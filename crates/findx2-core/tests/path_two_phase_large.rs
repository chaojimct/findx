use findx2_core::index::IndexBuilder;
use findx2_core::platform::RawEntry;
use findx2_core::{load_index_bin, save_index_bin, QueryParser, SearchEngine, SearchOptions};

/// `path:` 两级候选（exact/check）+ trigram 名字剪枝的等价性回归。
///
/// 现有 `path_two_phase_filter_matches_naive_full_path_scan` 的 fixture 只有 9 条目，
/// hits 恒走「hits 小路」（逐条校验）——两段式主体（建表 + `path_candidate_ids` 的
/// exact/check 分级 + trigram 剪枝）在等价性意义上**没有被它覆盖**。本用例构造
/// 超过小路阈值（50_000）的条目数，强制走两段式大路，并分别在 trigram 边车
/// 挂载 / 摘除两种状态下与朴素全路径扫描对齐。
#[test]
fn path_two_phase_large_hits_exact_check_split_matches_naive() {
    // 目录树：data / deep（deep 下挂 target_* 文件，data 下挂 fill_* 填量）。
    let dir_entry = |id: u64, parent: u64, name: &str| RawEntry {
        file_id: id,
        file_id_128: None,
        parent_id: parent,
        name: name.into(),
        size: 0,
        mtime: 0,
        ctime: 0,
        attrs: 0x10,
        is_dir: true,
    };
    let dirs = vec![dir_entry(10, 0, "data"), dir_entry(11, 10, "deep")];

    const TARGETS: u32 = 100;
    const FILLS: u32 = 60_000;
    let mut files: Vec<RawEntry> = Vec::with_capacity((TARGETS + FILLS) as usize);
    for i in 0..TARGETS {
        files.push(RawEntry {
            file_id: 1000 + i as u64,
            file_id_128: None,
            parent_id: 11,
            name: format!("target_report_{i:04}.log"),
            size: 10,
            mtime: 1,
            ctime: 1,
            attrs: 0,
            is_dir: false,
        });
    }
    for i in 0..FILLS {
        files.push(RawEntry {
            file_id: 200_000 + i as u64,
            file_id_128: None,
            parent_id: 10,
            name: format!("fill_{i:06}.bin"),
            size: 10,
            mtime: 1,
            ctime: 1,
            attrs: 0,
            is_dir: false,
        });
    }

    // 条目数必须大于小路阈值，否则本用例退化成测小路、两段式主体空转。
    assert!(
        files.len() + dirs.len() > 50_000,
        "fixture 条目数必须超过 SMALL_HITS_DIRECT，否则等价性检查没有覆盖两段式"
    );

    let store = IndexBuilder::new(b'C', 1, 2, 3)
        .build_from_raw(files, dirs, true)
        .unwrap();

    let dir = std::env::temp_dir().join(format!(
        "findx2-path-large-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let index = dir.join("index.bin");
    save_index_bin(&index, &store).unwrap();
    findx2_core::build_trigram_sidecar(&store, &index).unwrap();
    let store = load_index_bin(&index).unwrap();
    assert!(
        store.trigram.is_some(),
        "边车应挂载成功，否则 trigram 剪枝分支没有被覆盖"
    );

    let engine = SearchEngine::new(store);

    // 朴素基准：与 naive 定义一致（小写、卷符前缀、`\` 分隔）。
    //
    // ⚠️ 读锁只在闭包内拿放：`engine.index_store()` 返回 RwLockReadGuard，
    // 若跨轮持有（尤其持有到 `index_store_mut().trigram = None`），写锁会
    // 永久死等——本测试前版本就是这么死锁的。search() 内部自取读锁，
    // 与这里的短读锁互不阻塞。
    let naive = |needle: &str| -> Vec<String> {
        let store = engine.index_store();
        let nb = needle.to_ascii_lowercase().into_bytes();
        (0..store.entries.len() as u32)
            .filter(|&idx| {
                let e = &store.entries[idx as usize];
                let name = store
                    .name_bytes(e)
                    .iter()
                    .map(|b| b.to_ascii_lowercase())
                    .collect::<Vec<u8>>();
                let dir_path = store.dir_path_bytes(e);
                let mut full = Vec::new();
                full.push(b'c');
                full.push(b':');
                full.extend_from_slice(dir_path.as_ref());
                full.push(b'\\');
                full.extend_from_slice(&name);
                full.windows(nb.len().max(1)).any(|w| w == nb.as_slice())
            })
            .map(|idx| {
                String::from_utf8_lossy(store.name_bytes(&store.entries[idx as usize])).into_owned()
            })
            .collect()
    };

    let run = |query: &str| -> Vec<String> {
        let pq = QueryParser::parse(query).unwrap();
        let (hits, _) = engine.search(&pq, &SearchOptions::default()).unwrap();
        hits.iter().map(|h| h.name.clone()).collect()
    };

    // 覆盖矩阵：
    // - `target`：无分隔符 → 目录侧 check 恒空，纯 exact 直通；
    // - `fill_5`：纯名字命中（批量）；
    // - `data\deep`：完整 needle 落在目录路径 → FULL 子树直通；
    // - `a\deep\t`：骑缝 + 右段单字节（不进变体表）→ 靠 VARIANT 子树兜底进 check；
    // - `a\d`：骑缝变体（左尾 + 右头都 ≥2）；
    // - `zzz_nope`：无命中。
    let cases = [
        "target",
        "fill_5",
        "data\\deep",
        "a\\deep\\t",
        "a\\d",
        "zzz_nope",
    ];

    for with_tri in [true, false] {
        if !with_tri {
            engine.index_store_mut().trigram = None;
        }
        for needle in cases {
            let mut expect: Vec<String> = naive(needle);
            let mut got = run(&format!("path:{needle}"));
            expect.sort();
            got.sort();
            assert_eq!(
                got, expect,
                "with_tri={with_tri} path:{needle} 两级候选与朴素全路径扫描结果不一致"
            );
        }
        // 摘除后挂回（下一轮 with_tri=true 重新走剪枝路径）。
        if !with_tri {
            let snap = load_index_bin(&index).unwrap();
            engine.index_store_mut().trigram = snap.trigram;
        }
    }

    // sanity：各语义分组必须有命中，防止 fixture 退化后断言空转。
    assert!(!run("path:target").is_empty(), "target 用例应有命中");
    assert!(!run("path:a\\d").is_empty(), "骑缝用例应有命中");

    let _ = std::fs::remove_dir_all(&dir);
}
