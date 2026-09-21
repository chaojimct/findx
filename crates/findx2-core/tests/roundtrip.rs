use findx2_core::index::{filetime_to_unix_secs, hash_ext8, IndexBuilder, IndexStore};
use findx2_core::platform::{ChangeEvent, RawEntry};
use findx2_core::{load_index_bin, save_index_bin, QueryParser, SearchEngine, SearchOptions};
#[test]
fn query_parse_ext() {
    let q = QueryParser::parse("ext:txt readme").unwrap();
    assert_eq!(q.ext.as_deref(), Some("txt"));
    assert_eq!(q.substring.as_deref(), Some("readme"));
    assert!(q.name_terms.iter().any(|t| t == "readme"));
}

#[test]
fn query_or_branch() {
    let q = QueryParser::parse("ext:txt | ext:md").unwrap();
    assert_eq!(q.ext.as_deref(), Some("txt"));
    assert_eq!(q.or_branches.len(), 1);
    assert_eq!(q.or_branches[0].ext.as_deref(), Some("md"));
}

#[test]
fn query_unknown_modifier_fails() {
    assert!(QueryParser::parse("notakey:blah").is_err());
}

#[test]
fn query_size_empty() {
    let q = QueryParser::parse("size:empty").unwrap();
    assert!(q.size_empty);
    assert_eq!(q.size_min, Some(0));
    assert_eq!(q.size_max, Some(0));
}

#[test]
fn query_dm_gt_date() {
    let q = QueryParser::parse("dm:>2024-06-01").unwrap();
    assert!(q.mtime_min.is_some());
    assert!(q.mtime_max.is_none());
    let u = filetime_to_unix_secs(q.mtime_min.unwrap());
    assert!(u > 1_000_000_000, "dm:> 阈值 unix 不得为 0，否则全表通过时间下界");
}

/// 与 GUI 一致：`dm:>日期` 后接关键词时阈值须非 0，否则热路径里 `mt < 0` 永假，等于未筛时间。
#[test]
fn query_dm_gt_date_with_keyword_threshold_unix_sane() {
    let q = QueryParser::parse("dm:>2026-04-21 mctjl").unwrap();
    let ft = q.mtime_min.expect("mtime_min");
    let u = filetime_to_unix_secs(ft);
    assert!(
        u > 1_700_000_000,
        "threshold unix expected ~1776729600 (2026-04-21 UTC 0:00), got {}",
        u
    );
}

/// GUI 类型筛选拼成 `folder: 关键词` / `file: 关键词`，冒号右侧须参与文件名匹配，不得仅保留类型位。
#[test]
fn query_unix_path_token_and_modifiers() {
    let q = QueryParser::parse("/tmp/findx-fixture shanghai").unwrap();
    assert_eq!(q.path_match.as_deref(), Some("/tmp/findx-fixture"));
    assert!(q.name_terms.iter().any(|t| t == "shanghai"));

    let q = QueryParser::parse("parent:/Users/foo beijing").unwrap();
    assert_eq!(q.parent_path.as_deref(), Some("users/foo"));
    assert!(!q.parent_path_substring);
    assert!(q.name_terms.iter().any(|t| t == "beijing"));

    let q = QueryParser::parse(r"parent:C:\Users\foo").unwrap();
    assert_eq!(q.drive, Some('C'));
    assert_eq!(q.parent_path.as_deref(), Some("users/foo"));

    let q = QueryParser::parse("file:beijing;py").unwrap();
    assert!(q.only_files);
    assert!(q.pinyin_only);
    assert!(q.name_terms.iter().any(|t| t == "beijing"));

    let q = QueryParser::parse("startwith:bei;py").unwrap();
    assert_eq!(q.starts_with.as_deref(), Some("bei"));
    assert!(q.pinyin_only);
}

#[test]
fn query_folder_file_modifier_keeps_keyword() {
    let q = QueryParser::parse("folder: 149").unwrap();
    assert!(q.only_dirs);
    assert_eq!(q.substring.as_deref(), Some("149"));
    assert!(q.name_terms.iter().any(|t| t == "149"));

    let qf = QueryParser::parse("file: readme").unwrap();
    assert!(qf.only_files);
    assert_eq!(qf.substring.as_deref(), Some("readme"));

    let q_empty = QueryParser::parse("folder:").unwrap();
    assert!(q_empty.only_dirs);
    assert!(q_empty.substring.is_none());
    assert!(q_empty.name_terms.is_empty());
}

#[test]
fn index_persist_roundtrip() {
    let dirs = vec![
        RawEntry {
            file_id: 100,
            file_id_128: None,
            parent_id: 0,
            name: "Users".into(),
            size: 0,
            mtime: 0,
            ctime: 0,
            attrs: 0x10,
            is_dir: true,
        },
        RawEntry {
            file_id: 101,
            file_id_128: None,
            parent_id: 100,
            name: "Alice".into(),
            size: 0,
            mtime: 0,
            ctime: 0,
            attrs: 0x10,
            is_dir: true,
        },
    ];
    let files = vec![RawEntry {
        file_id: 1,
        file_id_128: None,
        parent_id: 101,
        name: "note.txt".into(),
        size: 10,
        mtime: 1,
        ctime: 1,
        attrs: 0,
        is_dir: false,
    }];
    let b = IndexBuilder::new(b'C', 1, 2, 3);
    let store = b.build_from_raw(files, dirs, true).unwrap();
    let mut tmp = std::env::temp_dir();
    tmp.push("findx2_test_index.bin");
    save_index_bin(&tmp, &store).unwrap();
    let loaded: IndexStore = load_index_bin(&tmp).unwrap();
    assert_eq!(loaded.entry_count(), store.entry_count());
    assert_eq!(loaded.frns.len(), loaded.entries.len());
    let _ = std::fs::remove_file(&tmp);
}

/// `frns` 段的计数字段有一个"防垃圾"上限。它曾经是 64 Mi（6710 万），
/// 在 3 亿条目级的真实库上会把**合法**索引误判成损坏（实测 309,031,075 条被拒），
/// 界面表现为"卡在建库中"。
///
/// 这个测试不构建上亿条目（那会吃掉几十 GB 内存），而是直接手工定位
/// index.bin 里 `frns` 计数字段的偏移并改写它，从而钉住两件事：
///   1. 远大于旧阈值 64 Mi 的合法计数必须能加载（回归：修复前这里会报
///      "frns 计数异常（过大）"）；
///   2. 真正的天文数字（随机字节/段错位造成的）仍然要被拒。
///
/// 偏移推导严格照 `load_index_bin_with_progress` 的解析顺序：
///   header(64) + nvol*32 + [v6: 每卷 (2+len) 两次] + names_buf_len
///   + 8 + dir_paths_len + 8 + nr*8 + entry_count*32 + dir_count*24
/// 之后紧跟的就是 `frns` 的 u64 计数。
#[test]
fn frns_count_guard_accepts_large_but_rejects_absurd() {
    fn frns_count_offset(bytes: &[u8]) -> usize {
        assert_eq!(&bytes[0..4], &0x4644_5832u32.to_le_bytes(), "magic 不符");
        let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        let entry_count = u64::from_le_bytes(bytes[8..16].try_into().unwrap()) as usize;
        let dir_count = u64::from_le_bytes(bytes[16..24].try_into().unwrap()) as usize;
        let names_buf_len = u64::from_le_bytes(bytes[24..32].try_into().unwrap()) as usize;
        let nvol = u32::from_le_bytes(bytes[36..40].try_into().unwrap()) as usize;

        let mut off = 64 + nvol * 32;
        if version >= 6 {
            for _ in 0..nvol {
                for _ in 0..2 {
                    let len = u16::from_le_bytes(bytes[off..off + 2].try_into().unwrap()) as usize;
                    off += 2 + len;
                }
            }
        }
        off += names_buf_len;
        if version >= 2 {
            let dlen = u64::from_le_bytes(bytes[off..off + 8].try_into().unwrap()) as usize;
            off += 8 + dlen;
            let nr = u64::from_le_bytes(bytes[off..off + 8].try_into().unwrap()) as usize;
            off += 8 + nr * 8;
        }
        off += entry_count * 32;
        off += dir_count * 24;
        off
    }

    let dirs = vec![RawEntry {
        file_id: 100,
        file_id_128: None,
        parent_id: 0,
        name: "Users".into(),
        size: 0,
        mtime: 0,
        ctime: 0,
        attrs: 0x10,
        is_dir: true,
    }];
    let files = vec![RawEntry {
        file_id: 1,
        file_id_128: None,
        parent_id: 100,
        name: "note.txt".into(),
        size: 10,
        mtime: 1,
        ctime: 1,
        attrs: 0,
        is_dir: false,
    }];
    let b = IndexBuilder::new(b'C', 1, 2, 3);
    let store = b.build_from_raw(files, dirs, true).unwrap();
    let mut tmp = std::env::temp_dir();
    tmp.push("findx2_test_frns_guard.bin");
    save_index_bin(&tmp, &store).unwrap();

    let mut bytes = std::fs::read(&tmp).unwrap();
    let off = frns_count_offset(&bytes);
    let bytes_of_original = bytes.clone();

    // 先确认推导出的偏移确实落在 `frns` 计数上：原始值应等于 entry_count。
    let original = u64::from_le_bytes(bytes[off..off + 8].try_into().unwrap());
    assert_eq!(
        original as usize,
        store.entry_count(),
        "偏移推导有误：该位置不是 frns 计数（推导偏移 {off}）"
    );

    // 场景 1：合法计数（等于 entry_count）必须能通过阈值检查。
    // 这条是本体的回归：旧代码的对应阈值远大于它，所以它也过；真正钉住"不再用
    // 固定小常量"的是场景 2。
    assert_eq!(
        original as usize,
        store.entry_count(),
        "frns 计数应等于 entry_count"
    );

    // 场景 2：**按 entry_count 相对放大**的计数。
    //
    // 真实的 3 亿条目库就是中了这里：旧上限是固定常量 64 Mi（6710 万），
    // 而 3 亿 > 6710 万，于是合法索引被拒。这个测试的索引只有 2 条，没法在
    // 绝对值上复现 3 亿；但阈值现在是 `entry_count * 8 + 1 MiB` —— 只要
    // 计数字段超出这个相对上限，就必须被拒；落在其内就必须放行。
    // 下面用比 threshold = entry_count * 8 + 1 MiB 略大的值来验证"拒绝"生效。
    let entry_count = store.entry_count() as u64;
    let threshold = entry_count * 8 + (1 << 20);
    let just_over = threshold + 1;
    bytes[off..off + 8].copy_from_slice(&just_over.to_le_bytes());
    std::fs::write(&tmp, &bytes).unwrap();
    let err2 = load_index_bin(&tmp).err().map(|e| e.to_string());
    assert!(
        matches!(&err2, Some(m) if m.contains("frns 计数异常")),
        "计数 {just_over} 超过阈值 {threshold}，应被拒绝，实际：{err2:?}"
    );

    // 场景 3：旧代码会误杀的"绝对值很大但在相对阈值内"的情形。
    // 这里没法让 entry_count 真的到 3 亿，于是反过来验证另一侧：
    // 阈值内的大计数不应触发**阈值**错误（可能因段越界失败，那不属阈值检查）。
    // 用 just_under 而非 64 Mi，是因为本测试 entry_count=2、阈值仅 ~1 MiB。
    let just_under = threshold.saturating_sub(1);
    bytes[off..off + 8].copy_from_slice(&just_under.to_le_bytes());
    std::fs::write(&tmp, &bytes).unwrap();
    let err3 = load_index_bin(&tmp).err().map(|e| e.to_string());
    assert!(
        !matches!(&err3, Some(m) if m.contains("frns 计数异常")),
        "计数 {just_under} 未超阈值 {threshold}，不应被阈值检查拒绝：{err3:?}"
    );

    // 场景 4（本体）：不改文件，正常往返必须成功且长度已对齐。
    std::fs::write(&tmp, &bytes_of_original).unwrap();
    let loaded = load_index_bin(&tmp).expect("原始索引应能正常加载");
    assert_eq!(loaded.frns.len(), loaded.entries.len());

    let _ = std::fs::remove_file(&tmp);
}

/// USN 枚举常不把「卷根目录」本身作为一条目录记录吐出；`findx2-windows` 会打开 `X:\`
/// 用 `GetFileInformationByHandle` 取**完整** 64 位 FRN（含序列号，不是裸 MFT 序号 5），
/// 补一条 parent=0、name 为空的卷根目录，使根下文件的 `parent_id` 能正确解析为 `C:\xxx`。
#[test]
fn ntfs_volume_root_file_searchable_after_root_dir_injected() {
    // 模拟真实 NTFS：低 48 位为 MFT 记录号 5，高位非零（与 USN parent_id 形态一致）。
    const NTFS_ROOT_FRN: u64 = 0x0000_0000_0000_0005 | (7u64 << 48);
    let dirs = vec![
        RawEntry {
            file_id: NTFS_ROOT_FRN,
            file_id_128: None,
            parent_id: 0,
            name: String::new(),
            size: 0,
            mtime: 0,
            ctime: 0,
            attrs: 0x10,
            is_dir: true,
        },
        RawEntry {
            file_id: 200,
            file_id_128: None,
            parent_id: NTFS_ROOT_FRN,
            name: "Windows".into(),
            size: 0,
            mtime: 0,
            ctime: 0,
            attrs: 0x10,
            is_dir: true,
        },
        RawEntry {
            file_id: 201,
            file_id_128: None,
            parent_id: 200,
            name: "System32".into(),
            size: 0,
            mtime: 0,
            ctime: 0,
            attrs: 0x10,
            is_dir: true,
        },
    ];
    let files = vec![
        RawEntry {
            file_id: 10,
            file_id_128: None,
            parent_id: NTFS_ROOT_FRN,
            name: "vfcompat.dll".into(),
            size: 100,
            mtime: 1,
            ctime: 1,
            attrs: 0,
            is_dir: false,
        },
        RawEntry {
            file_id: 11,
            file_id_128: None,
            parent_id: 201,
            name: "vfcompat.dll".into(),
            size: 200,
            mtime: 1,
            ctime: 1,
            attrs: 0,
            is_dir: false,
        },
    ];
    let b = IndexBuilder::new(b'C', 1, 2, 3);
    let store = b.build_from_raw(files, dirs, true).unwrap();
    let engine = SearchEngine::new(store);
    let pq = QueryParser::parse("vfcompat").unwrap();
    let (hits, _total) = engine
        .search(&pq, &SearchOptions::default())
        .expect("search");
    let root_hit = hits
        .iter()
        .find(|h| h.path.eq_ignore_ascii_case("C:\\vfcompat.dll"))
        .expect("应能解析出 C:\\vfcompat.dll");
    assert_eq!(root_hit.name, "vfcompat.dll");
    let sys_hit = hits
        .iter()
        .find(|h| {
            h.path
                .eq_ignore_ascii_case("C:\\Windows\\System32\\vfcompat.dll")
        })
        .expect("应能解析出 System32 路径");
    assert_eq!(sys_hit.name, "vfcompat.dll");
}

/// 名字 interning：重复文件名/目录名共享同一段 `names_buf` 字节（`name_offset` 相同），
/// 文件与目录同名同样共享；名字切片、搜索与持久化 roundtrip 均不受影响。
#[test]
fn index_build_interns_duplicate_names() {
    let dirs = vec![
        RawEntry {
            file_id: 100,
            file_id_128: None,
            parent_id: 0,
            name: "proj".into(),
            size: 0,
            mtime: 0,
            ctime: 0,
            attrs: 0x10,
            is_dir: true,
        },
        RawEntry {
            file_id: 101,
            file_id_128: None,
            parent_id: 100,
            name: "src".into(),
            size: 0,
            mtime: 0,
            ctime: 0,
            attrs: 0x10,
            is_dir: true,
        },
        RawEntry {
            file_id: 102,
            file_id_128: None,
            parent_id: 101,
            name: "src".into(),
            size: 0,
            mtime: 0,
            ctime: 0,
            attrs: 0x10,
            is_dir: true,
        },
    ];
    let files = vec![
        RawEntry {
            file_id: 1,
            file_id_128: None,
            parent_id: 101,
            name: "index.js".into(),
            size: 1,
            mtime: 1,
            ctime: 1,
            attrs: 0,
            is_dir: false,
        },
        RawEntry {
            file_id: 2,
            file_id_128: None,
            parent_id: 102,
            name: "index.js".into(),
            size: 2,
            mtime: 1,
            ctime: 1,
            attrs: 0,
            is_dir: false,
        },
        RawEntry {
            file_id: 3,
            file_id_128: None,
            parent_id: 101,
            name: "index.js".into(),
            size: 3,
            mtime: 1,
            ctime: 1,
            attrs: 0,
            is_dir: false,
        },
        RawEntry {
            file_id: 4,
            file_id_128: None,
            parent_id: 100,
            name: "src".into(),
            size: 4,
            mtime: 1,
            ctime: 1,
            attrs: 0,
            is_dir: false,
        },
    ];
    let store = IndexBuilder::new(b'C', 1, 2, 3)
        .build_from_raw(files, dirs, true)
        .unwrap();

    // entries 布局：[0..4) 文件（index.js×3, src），[4..7) 目录（proj, src, src）。
    let offsets: Vec<u32> = store.entries.iter().map(|e| e.name_offset).collect();
    assert_eq!(
        store.entries.len(),
        7,
        "4 文件 + 3 目录（目录也有 FileEntry）"
    );
    assert_eq!(offsets[0], offsets[1]);
    assert_eq!(offsets[1], offsets[2], "三份 index.js 应共享同段字节");
    assert_eq!(
        offsets[3], offsets[6],
        "文件 src 与目录 src 同名应共享（目录 FileEntry 复用 DirEntry offset）"
    );
    assert_eq!(offsets[5], offsets[6], "两个目录 src 应共享");
    let unique = {
        let mut s = offsets.clone();
        s.sort_unstable();
        s.dedup();
        s.len()
    };
    assert_eq!(unique, 3, "全部名字只有 proj/src/index.js 三段");

    // 共享不改变切片语义：每个条目仍能取回自己的名字。
    for (e, expect) in store
        .entries
        .iter()
        .zip(["index.js", "index.js", "index.js", "src", "proj", "src", "src"])
    {
        assert_eq!(store.name_str(e).unwrap(), expect);
    }

    // 持久化 roundtrip：offset 原样落盘，共享关系在 load 后保持。
    let mut tmp = std::env::temp_dir();
    tmp.push("findx2_test_intern.bin");
    save_index_bin(&tmp, &store).unwrap();
    let loaded: IndexStore = load_index_bin(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);
    assert_eq!(loaded.entries.len(), store.entries.len());
    for (a, b) in loaded.entries.iter().zip(store.entries.iter()) {
        assert_eq!(a.name_offset, b.name_offset, "offset 应原样持久化");
        assert_eq!(a.n_len, b.n_len);
    }
    let loaded_offsets: Vec<u32> = loaded.entries.iter().map(|e| e.name_offset).collect();
    assert_eq!(loaded_offsets[0], loaded_offsets[2]);
    for (e, expect) in loaded
        .entries
        .iter()
        .zip(["index.js", "index.js", "index.js", "src", "proj", "src", "src"])
    {
        assert_eq!(loaded.name_str(e).unwrap(), expect);
    }

    // 搜索照常工作（store 最后消费进 engine）。
    let engine = SearchEngine::new(store);
    let pq = QueryParser::parse("index.js").unwrap();
    let (hits, total) = engine
        .search(&pq, &SearchOptions::default())
        .expect("search");
    assert_eq!(total, 3, "三份 index.js 都可被搜到");
    assert_eq!(hits.len(), 3);
}

/// `hash_ext8` 仅 8 位：`pdf` 与 `yml` 会碰撞；`ext:` 过滤必须在位图后再按真实后缀收紧。
#[test]
fn ext_pdf_does_not_return_colliding_yml_bucket() {
    assert_eq!(
        hash_ext8("x.pdf"),
        hash_ext8("x.yml"),
        "测试前提：若哈希算法变更导致不碰撞，请换一对仍碰撞的扩展名"
    );
    const ROOT: u64 = 100;
    let dirs = vec![RawEntry {
        file_id: ROOT,
        file_id_128: None,
        parent_id: 0,
        name: "root".into(),
        size: 0,
        mtime: 0,
        ctime: 0,
        attrs: 0x10,
        is_dir: true,
    }];
    let files = vec![
        RawEntry {
            file_id: 1,
            file_id_128: None,
            parent_id: ROOT,
            name: "a.pdf".into(),
            size: 1,
            mtime: 1,
            ctime: 1,
            attrs: 0,
            is_dir: false,
        },
        RawEntry {
            file_id: 2,
            file_id_128: None,
            parent_id: ROOT,
            name: "b.yml".into(),
            size: 1,
            mtime: 1,
            ctime: 1,
            attrs: 0,
            is_dir: false,
        },
    ];
    let store = IndexBuilder::new(b'C', 1, 1, 1)
        .build_from_raw(files, dirs, true)
        .unwrap();
    let engine = SearchEngine::new(store);
    let pq = QueryParser::parse("ext:pdf").unwrap();
    let (hits, _) = engine
        .search(&pq, &SearchOptions::default())
        .expect("search");
    assert_eq!(hits.len(), 1, "应只命中 .pdf，不得因桶碰撞带入 .yml");
    assert_eq!(hits[0].name, "a.pdf");
}

/// trigram 剪枝正确性回归：`startwith:` / `ends_with:` / 普通子串查询在
/// 「挂边车剪枝」与「无边车全表扫描」两条路径下结果必须完全一致。
/// （三者的 needle 都进了 `trigram_candidate_ids` 的候选源，剪枝只是必要条件过滤。）
#[test]
fn starts_with_ends_with_trigram_pruning_matches_full_scan() {
    let root = 5u64;
    let dirs = vec![RawEntry {
        file_id: root,
        file_id_128: None,
        parent_id: 0,
        name: "C:".into(),
        size: 0,
        mtime: 0,
        ctime: 0,
        attrs: 0x10,
        is_dir: true,
    }];
    let names = [
        "readme.md",
        "readme.txt",
        "already_read.md",
        "config.json",
        "conftest.py",
        "覆盖readme.md",
    ];
    let files: Vec<RawEntry> = names
        .iter()
        .enumerate()
        .map(|(i, n)| RawEntry {
            file_id: 100 + i as u64,
            file_id_128: None,
            parent_id: root,
            name: (*n).into(),
            size: 1,
            mtime: 1,
            ctime: 1,
            attrs: 0,
            is_dir: false,
        })
        .collect();

    let dir = std::env::temp_dir().join(format!(
        "findx2-tri-search-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let index = dir.join("index.bin");

    let store = IndexBuilder::new(b'C', 1, 1, 1)
        .build_from_raw(files, dirs, true)
        .unwrap();
    save_index_bin(&index, &store).unwrap();
    findx2_core::build_trigram_sidecar(&store, &index).unwrap();

    let store = load_index_bin(&index).unwrap();
    assert!(
        store.trigram.is_some(),
        "边车应挂载成功，否则本测试没有覆盖剪枝路径"
    );
    let engine = SearchEngine::new(store);

    let run = |query: &str| -> Vec<(String, String)> {
        let pq = QueryParser::parse(query).unwrap();
        let (hits, _) = engine.search(&pq, &SearchOptions::default()).unwrap();
        hits.into_iter().map(|h| (h.name, h.path)).collect()
    };

    for query in [
        "startwith:readme",
        "startwith:read",
        "endwith:md",
        "endwith:.json",
        "readme",          // 普通子串（含前缀命中）
        "startwith:覆盖",  // 非 ASCII 前缀（UTF-8 字节窗口）
    ] {
        let pruned = run(query);
        // 关掉边车走全表扫描对照。
        engine.index_store_mut().trigram = None;
        let full = run(query);
        engine.index_store_mut().trigram = {
            // 重新挂回（下一轮 query 继续测剪枝路径）。
            let snap = load_index_bin(&index).unwrap();
            snap.trigram
        };
        assert_eq!(
            pruned, full,
            "query `{query}`：trigram 剪枝路径与全表扫描结果不一致"
        );
        assert!(!pruned.is_empty(), "query `{query}` 应有命中，否则对照无意义");
    }

    // spot check：具体语义抽查两个。
    let names: Vec<String> = run("startwith:readme")
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert!(names.contains(&"readme.md".to_string()));
    assert!(names.contains(&"readme.txt".to_string()));
    assert!(
        !names.contains(&"already_read.md".to_string()),
        "starts_with 是前缀匹配，不得把子串命中带进来"
    );
    let md_count = run("endwith:md").len();
    assert_eq!(md_count, 3, "readme.md / already_read.md / 覆盖readme.md");

    std::fs::remove_dir_all(&dir).ok();
}

/// P0：CreatePending 先以 0 元数据入库（名字立刻可搜），DataOrMeta 再补 size，
/// 且不得清掉已有名字。对应 USN watch 热路径「先可见、后台 stat」。
#[test]
fn create_pending_searchable_then_stat_refresh() {
    const ROOT: u64 = 5;
    let dirs = vec![RawEntry {
        file_id: ROOT,
        file_id_128: None,
        parent_id: 0,
        name: "C:".into(),
        size: 0,
        mtime: 0,
        ctime: 0,
        attrs: 0x10,
        is_dir: true,
    }];
    let store = IndexBuilder::new(b'C', 1, 1, 1)
        .build_from_raw(vec![], dirs, true)
        .unwrap();
    let engine = SearchEngine::new(store);
    {
        let mut g = engine.index_store_mut();
        g.apply_change_event(&ChangeEvent::CreatePending {
            file_id: 42,
            file_id_128: None,
            parent_id: ROOT,
            name: "burst.tmp".into(),
            attrs: 0x20,
            is_dir: false,
        })
        .unwrap();
    }
    engine.note_external_mutation();
    let pq = QueryParser::parse("burst").unwrap();
    let (hits, total) = engine
        .search(&pq, &SearchOptions::default())
        .expect("search after CreatePending");
    assert_eq!(total, 1, "CreatePending 入库后名字应立刻可搜");
    assert_eq!(hits[0].name, "burst.tmp");
    assert_eq!(hits[0].size, 0);

    {
        let mut g = engine.index_store_mut();
        g.apply_change_event(&ChangeEvent::DataOrMeta {
            file_id: 42,
            size: Some(4096),
            mtime: Some(findx2_core::index::unix_secs_to_filetime(1_700_000_000)),
            ctime: None,
        })
        .unwrap();
    }
    engine.note_external_mutation();
    let (hits, _) = engine
        .search(&pq, &SearchOptions::default())
        .expect("search after DataOrMeta");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].name, "burst.tmp", "补 meta 不得改名");
    assert_eq!(hits[0].size, 4096);
}

#[test]
fn index_v6_unix_volume_roundtrip() {
    let dirs = vec![RawEntry {
        file_id: 100,
        file_id_128: None,
        parent_id: 0,
        name: "Users".into(),
        size: 0,
        mtime: 0,
        ctime: 0,
        attrs: 0x10,
        is_dir: true,
    }];
    let files = vec![RawEntry {
        file_id: 1,
        file_id_128: None,
        parent_id: 100,
        name: "note.txt".into(),
        size: 10,
        mtime: 1,
        ctime: 1,
        attrs: 0,
        is_dir: false,
    }];
    let store = IndexBuilder::new(0, 0, 7, 99)
        .with_unix_volume("apfs-uuid", "/System/Volumes/Data")
        .build_from_raw(files, dirs, true)
        .unwrap();
    let mut tmp = std::env::temp_dir();
    tmp.push("findx2_test_index_v6_unix.bin");
    save_index_bin(&tmp, &store).unwrap();
    let loaded: IndexStore = load_index_bin(&tmp).unwrap();
    let _ = std::fs::remove_file(&tmp);
    let v = loaded.volumes.first().expect("volume");
    assert_eq!(v.root_prefix, "/System/Volumes/Data");
    assert_eq!(v.volume_id, "apfs-uuid");
    assert_eq!(v.usn_journal_id, 7);
    assert_eq!(v.last_usn, 99);
    let path = loaded.entry_display_path(0).unwrap();
    assert!(
        path.contains("Users") || path.contains("note.txt"),
        "unix display path: {path}"
    );
    assert!(path.starts_with('/'), "unix path must be absolute: {path}");
}

/// `compact_tombstones` 的回归：物理移除墓碑后，索引的**全部**耦合下标都必须仍然自洽。
///
/// 压缩要同时搬 entries / frns / dirs / dir_index / frn_to_entry / ext_filter /
/// dir_path_ranges / volumes.first_entry_idx / 位图 —— 任何一处漏改都会让索引在
/// 「看起来能加载」的同时悄悄给出错误结果（比如路径为空、ext 筛选漏条目）。
/// 所以这里不满足于「条数变少了」，而是逐项验证：
///   1. 三个平行数组长度一致，且 `deleted` 清空；
///   2. ext_filter 每个桶的下标都落在新范围内、且与 entries 的 ext_hash 一致；
///   3. 每个存活条目的 `dir_idx` 都能解析出非空路径（父链没被搬断）；
///   4. `frn_to_entry` 与 `frns` 双向一致；
///   5. 压缩前后**搜索结果等价**：存活的仍能命中，墓碑的不再返回；
///   6. 压缩后再存盘→加载仍能往返（格式自洽）。
#[test]
fn compact_tombstones_keeps_index_consistent_and_search_equivalent() {
    let dirs = vec![
        RawEntry {
            file_id: 10,
            file_id_128: None,
            parent_id: 0,
            name: "Users".into(),
            size: 0,
            mtime: 0,
            ctime: 0,
            attrs: 0x10,
            is_dir: true,
        },
        RawEntry {
            file_id: 11,
            file_id_128: None,
            parent_id: 10,
            name: "Alice".into(),
            size: 0,
            mtime: 0,
            ctime: 0,
            attrs: 0x10,
            is_dir: true,
        },
        // 这个目录专门用来「整目录被删」：它自己与它下面的文件都该消失
        RawEntry {
            file_id: 12,
            file_id_128: None,
            parent_id: 10,
            name: "Gone".into(),
            size: 0,
            mtime: 0,
            ctime: 0,
            attrs: 0x10,
            is_dir: true,
        },
    ];
    let mk = |id: u64, parent: u64, name: &str| RawEntry {
        file_id: id,
        file_id_128: None,
        parent_id: parent,
        name: name.into(),
        size: 100,
        mtime: 1,
        ctime: 1,
        attrs: 0,
        is_dir: false,
    };
    let files = vec![
        mk(1, 11, "keep_alpha.txt"),
        mk(2, 11, "keep_beta.md"),
        mk(3, 12, "inside_gone_dir.txt"), // 父目录被删 → 连带消失
        mk(4, 11, "tomb_me.txt"),         // 直接打墓碑
        mk(5, 11, "tomb_me_too.md"),      // 直接打墓碑
    ];

    let mut store = IndexBuilder::new(b'C', 1, 2, 3)
        .build_from_raw(files, dirs, true)
        .unwrap();

    // 建库后没有任何墓碑
    assert_eq!(store.deleted.len(), 0);
    assert!(!store.should_compact(), "无墓碑时不该建议压缩");

    // 定位要删的条目下标（按名字找，避免依赖建库顺序）
    let idx_of = |s: &IndexStore, name: &str| -> u32 {
        (0..s.entries.len())
            .find(|&i| s.name_bytes(&s.entries[i]) == name.as_bytes())
            .unwrap_or_else(|| panic!("找不到条目 {name}")) as u32
    };
    let i_tomb1 = idx_of(&store, "tomb_me.txt");
    let i_tomb2 = idx_of(&store, "tomb_me_too.md");
    // Gone 目录条目 + 它下面的文件：模拟「整目录被重建摘掉」
    let i_gone_file = idx_of(&store, "inside_gone_dir.txt");
    let i_gone_dir = idx_of(&store, "Gone");

    let n_before = store.entry_count();
    let dirs_before = store.dirs.len();

    // 打墓碑（含那个目录及其子文件，模拟重建摘卷）
    for i in [i_tomb1, i_tomb2, i_gone_file, i_gone_dir] {
        store.delete_entry(i);
    }
    assert_eq!(store.deleted.len(), 4);

    // ── 压缩 ──
    let removed = store.compact_tombstones();
    assert!(removed >= 4, "至少该移除 4 条墓碑，实际 {removed}");
    assert_eq!(store.entry_count(), n_before - removed);

    // 1) 平行数组长度一致 + deleted 清空
    assert_eq!(store.frns.len(), store.entries.len(), "frns 与 entries 必须等长");
    assert_eq!(store.deleted.len(), 0, "压缩后不再有墓碑");
    assert_eq!(
        store.dir_path_ranges.len(),
        store.dirs.len(),
        "dir_path_ranges 与 dirs 必须等长"
    );
    assert!(!store.should_compact(), "压缩后不该再建议压缩");

    // 2) ext_filter 自洽：桶内下标在范围内且与 entries 的 ext_hash 对得上
    let n = store.entries.len();
    for (h, bm) in store.ext_filter.iter().enumerate() {
        let Some(bm) = bm else { continue };
        for idx in bm.iter() {
            assert!((idx as usize) < n, "ext_filter[{h}] 含越界下标 {idx}");
            assert_eq!(
                store.entries[idx as usize].ext_hash_u8() as usize,
                h,
                "ext_filter[{h}] 里的条目 ext_hash 不是 {h}"
            );
        }
    }

    // 3) 每个存活条目都能解析出非空路径（父链没被搬断）
    for (i, _) in store.entries.iter().enumerate() {
        let p = store.entry_display_path(i).unwrap_or_default();
        assert!(
            !p.is_empty(),
            "压缩后条目 {i} 路径解析为空 —— dir_idx 父链被搬断了"
        );
    }

    // 4) frn_to_entry 与 frns 双向一致
    for (i, fr) in store.frns.iter().enumerate() {
        if *fr != 0 {
            assert_eq!(
                store.frn_to_entry.get_idx(*fr),
                Some(i as u32),
                "frns[{i}]={fr} 在 frn_to_entry 里查不到或指向别处"
            );
        }
    }

    // 5) 搜索结果等价
    let engine = SearchEngine::new(store);
    let search = |q: &str| {
        let pq = QueryParser::parse(q).unwrap();
        engine.search(&pq, &SearchOptions::default()).unwrap()
    };
    let (hits, total) = search("keep_alpha");
    assert_eq!(total, 1, "存活的文件必须仍能搜到");
    assert_eq!(hits[0].name, "keep_alpha.txt");
    let (hits, _) = search("tomb_me");
    assert!(
        hits.is_empty(),
        "墓碑条目压缩后必须彻底消失，实际仍返回 {:?}",
        hits.iter().map(|h| &h.name).collect::<Vec<_>>()
    );
    let (hits, _) = search("inside_gone_dir");
    assert!(hits.is_empty(), "父目录被删的条目应连带消失");
    let (hits, _) = search("folder: Gone");
    assert!(hits.is_empty(), "被删的目录本身不该再出现在结果里");

    // ext 筛选也要正确（走 ext_filter 桶，是压缩最容易搬错的路径）
    let (hits, total) = search("ext:md");
    assert_eq!(total, 1, "ext:md 只剩 keep_beta.md，实际 {}", hits.len());
    assert_eq!(hits[0].name, "keep_beta.md");

    // 6) 压缩后能正常存盘 + 加载（格式自洽）
    //    注意：SearchEngine 拿走 store 的所有权，这里从 engine 借回来。
    let mut tmp = std::env::temp_dir();
    tmp.push("findx2_test_compacted.bin");
    {
        let g = engine.index_store();
        save_index_bin(&tmp, &g).unwrap();
    }
    let reloaded: IndexStore = load_index_bin(&tmp).expect("压缩后的索引必须能加载");
    assert_eq!(reloaded.entry_count(), n, "往返后条目数必须一致");
    assert_eq!(reloaded.frns.len(), reloaded.entries.len());
    assert_eq!(reloaded.deleted.len(), 0);
    let _ = std::fs::remove_file(&tmp);

    // 目录区确实缩小了（Gone 及其父链判定生效）
    assert!(
        reloaded.dirs.len() < dirs_before,
        "被删目录应从 dirs 里移除：压缩前 {dirs_before}，压缩后 {}",
        reloaded.dirs.len()
    );
}

/// `remove_volume_entries` 的**区间安全**回归。
///
/// 真实事故：对本机 3.09 亿条目的索引跑 `findx2 compact` 时，`remove_volume_entries`
/// 把唯一卷（`first_entry_idx == 0`、且没有下一个卷）的区间推成 `0..entry_count`，
/// 于是「摘掉该卷」被解释成「清空整库」，309,031,075 条**全部**被物理删光，
/// 压缩后的文件只剩 `names_buf`（`entry_count == 0`）。
///
/// 这里把那个区间推算法钉死：单卷、`first_entry_idx == 0` 时，
/// `remove_volume_entries` **必须拒绝执行**（返回 0 且不动任何条目），
/// 而不是把整库当旧卷摘掉。
#[test]
fn remove_volume_entries_refuses_when_volume_range_covers_whole_index() {
    let dirs = vec![RawEntry {
        file_id: 10,
        file_id_128: None,
        parent_id: 0,
        name: "Users".into(),
        size: 0,
        mtime: 0,
        ctime: 0,
        attrs: 0x10,
        is_dir: true,
    }];
    let mk = |id: u64, name: &str| RawEntry {
        file_id: id,
        file_id_128: None,
        parent_id: 10,
        name: name.into(),
        size: 100,
        mtime: 1,
        ctime: 1,
        attrs: 0,
        is_dir: false,
    };
    let files = vec![
        mk(1, "a.txt"),
        mk(2, "b.txt"),
        mk(3, "c.txt"),
        mk(4, "d.txt"),
    ];

    let mut store = IndexBuilder::new(b'C', 1, 2, 3)
        .build_from_raw(files, dirs, true)
        .unwrap();

    // 前置条件：单卷且区间起点为 0（正是触发条件）
    assert_eq!(store.volumes.len(), 1, "本用例要求单卷");
    assert_eq!(
        store.volumes[0].first_entry_idx, 0,
        "本用例要求卷区间起点为 0（即区间覆盖整库）"
    );
    let n_before = store.entry_count();
    assert!(n_before > 0);

    // 该调用必须**拒绝**——否则就是把整库清空
    let removed = store.remove_volume_entries('C');
    assert_eq!(removed, 0, "区间覆盖整库时必须拒绝摘卷，返回 0");
    assert_eq!(
        store.entry_count(),
        n_before,
        "拒绝后条目数不得变化（绝不能被清空）"
    );
    assert_eq!(store.deleted.len(), 0, "拒绝后不该留下墓碑");
    assert_eq!(store.dirs.len(), 1, "拒绝后目录区不得变化");
    assert!(!store.volumes.is_empty(), "拒绝后卷表不得被清掉");
}

/// `path:` **两段式过滤**的等价性回归。
///
/// 旧实现：对每条命中沿 `dir_idx` 父链拼全路径再子串比对 —— 正确但 O(n × 路径长度)。
/// 新实现：先用目录路径 + 名字建立候选**超集**，只在候选上拼路径精确校验。
///
/// 这里用「显式物化目录路径的索引」跑遍一组 needle，把两段式的命中集合与
/// **朴素全表拼路径**的命中集合逐一对齐。覆盖的关键形态：
///   - 纯目录段命中（`users`）
///   - 纯文件名命中（`readme`）
///   - **骑缝命中**：needle 跨越 `c:` 与 `\`、或跨越目录分隔符（`c:\us`、`s\alice`）
///   - needle 以分隔符结尾 / 开头
///   - 无命中、超长无命中
///   - 大小写（全路径比较按小写语义）
#[test]
fn path_two_phase_filter_matches_naive_full_path_scan() {
    let dir = |id: u64, parent: u64, name: &str| RawEntry {
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
    let file = |id: u64, parent: u64, name: &str| RawEntry {
        file_id: id,
        file_id_128: None,
        parent_id: parent,
        name: name.into(),
        size: 100,
        mtime: 1,
        ctime: 1,
        attrs: 0,
        is_dir: false,
    };

    // 目录树：Users / Alice / Projects，以及 Users / Bob
    let dirs = vec![
        dir(10, 0, "Users"),
        dir(11, 10, "Alice"),
        dir(12, 11, "Projects"),
        dir(13, 10, "Bob"),
    ];
    let files = vec![
        file(1, 12, "readme.md"),
        file(2, 12, "notes.txt"),
        file(3, 13, "readme.txt"),
        file(4, 10, "alice_profile.dat"),
        file(5, 13, "USERS_INDEX.db"),
    ];

    let store = IndexBuilder::new(b'C', 1, 2, 3)
        .build_from_raw(files, dirs, true)
        .unwrap();
    // 两段式过滤必须在这种「无物化」布局下也能建立候选表，否则优化在生产里是空的。
    assert!(
        store.dir_paths_buf.is_empty(),
        "本用例要求目录路径池未被物化——优化必须工作在这个前提下"
    );

    let engine = SearchEngine::new(store);
    let store_ref = engine.index_store();

    // 朴素基准：逐条拼全路径，语义与旧实现一致（小写、卷符前缀、`\` 分隔）。
    let naive = |needle: &str| -> Vec<u32> {
        let nb = needle.to_ascii_lowercase().into_bytes();
        (0..store_ref.entries.len() as u32)
            .filter(|&idx| {
                let e = &store_ref.entries[idx as usize];
                let name = store_ref
                    .name_bytes(e)
                    .iter()
                    .map(|b| b.to_ascii_lowercase())
                    .collect::<Vec<u8>>();
                let dir_path = store_ref.dir_path_bytes(e);
                let mut full = Vec::new();
                full.push(b'c');
                full.push(b':');
                full.extend_from_slice(dir_path.as_ref());
                full.push(b'\\');
                full.extend_from_slice(&name);
                full.windows(nb.len().max(1)).any(|w| w == nb.as_slice())
                    || (nb.is_empty())
            })
            .collect()
    };

    let search = |q: &str| -> Vec<String> {
        let pq = QueryParser::parse(q).unwrap();
        let (hits, _) = engine.search(&pq, &SearchOptions::default()).unwrap();
        hits.iter().map(|h| h.name.clone()).collect()
    };

    let cases = [
        "users",      // 纯目录段
        "readme",     // 纯文件名（多处）
        "c:\\us",     // 骑缝：跨 c: 与 \Users 的 \users
        "s\\alice",   // 骑缝：跨 Users 与 Alice 的 \users\alice
        "alice",      // 目录名 + 文件名同时命中
        "projects\\", // needle 以分隔符结尾
        "\\alice",    // needle 以分隔符开头
        "bob",        // 浅层目录
        "zzz_nope",   // 无命中
        "c:\\users\\alice\\projects\\readme.md", // 完整路径
    ];

    for needle in cases {
        let mut expect: Vec<String> = naive(needle)
            .into_iter()
            .map(|i| {
                String::from_utf8_lossy(store_ref.name_bytes(&store_ref.entries[i as usize]))
                    .into_owned()
            })
            .collect();
        let mut got = search(&format!("path:{needle}"));
        expect.sort();
        got.sort();
        assert_eq!(
            got, expect,
            "path:{needle} 两段式过滤与朴素全路径扫描结果不一致"
        );
    }

    // ── `parent_idx` 语义的专项回归 ──
    //
    // `path:` 的 haystack 是 `dir_path(e.dir_idx) + \ + name(e)`，父链上溯的终止条件是
    // 「当前节点的 `parent_idx == 0`」——这条规则被朴素实现与优化实现**共同**遵守，
    // 二者必须一致。这里钉住几个具体形态，防止后续有人改动传播逻辑时静默走偏：
    //
    //  - `Users`（`dir[0]`，`parent_idx` 自指 0）→ haystack `c:\users`：命中；
    //  - `Alice`（目录条目，`dir_idx` 指向 `Users`）→ `c:\users\alice`：命中；
    //  - `alice_profile.dat`（`Users` 下的文件）→ `c:\users\alice_profile.dat`：命中；
    //  - `Projects`（`Alice` 的子目录）→ 父链在 `Alice` 处止步、haystack 只有
    //    `c:\alice\projects`（**不含** `users`）→ **不**命中。
    //    这正是既有 `build_dir_path_lower_owned` 的行为，优化实现不得擅自“修正”它。
    let hits = search("path:users");
    for must in ["Users", "Alice", "Bob", "alice_profile.dat", "USERS_INDEX.db"] {
        assert!(
            hits.iter().any(|h| h == must),
            "path:users 必须包含 {must}，实际 {hits:?}"
        );
    }
    assert!(
        !hits.iter().any(|h| h == "Projects"),
        "path:users 不该包含 Projects（其父链在 Alice 处止步，haystack 不含 users），\
         实际 {hits:?}——若这里失败，说明父链上溯规则被改动、与既有路径解析不一致"
    );
}

/// Unix 卷 `path:` 的等价性回归。
///
/// unix 的 haystack 是 `root_prefix + / + dir_path + / + name`（`/` 分隔、无盘符），
/// 与 Windows 的 `c: + dir_path + \ + name` 结构不同；两段式过滤的候选表只含 dir_path
/// 部分，「needle 跨过 root_prefix 与目录路径边界」的命中（如 `fixture users`）不被
/// 变体覆盖——曾因未回退而漏报全部命中（见 `path_candidate_ids` 的 unix 回退注释）。
/// 本用例以独立拼接的朴素全路径扫描为基准逐 needle 对齐，钉住回退行为的正确性。
#[test]
fn path_unix_volume_matches_naive_full_path_scan() {
    let dir = |id: u64, parent: u64, name: &str| RawEntry {
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
    let file = |id: u64, parent: u64, name: &str| RawEntry {
        file_id: id,
        file_id_128: None,
        parent_id: parent,
        name: name.into(),
        size: 100,
        mtime: 1,
        ctime: 1,
        attrs: 0,
        is_dir: false,
    };

    // 卷根：空名目录（unix 惯例），子树与 Windows 版同构。
    let dirs = vec![
        dir(100, 0, ""),
        dir(10, 100, "Users"),
        dir(11, 10, "Alice"),
        dir(12, 11, "Projects"),
        dir(13, 10, "Bob"),
    ];
    let files = vec![
        file(1, 12, "readme.md"),
        file(2, 12, "notes.txt"),
        file(3, 13, "readme.txt"),
        file(4, 10, "alice_profile.dat"),
        file(5, 13, "USERS_INDEX.db"),
    ];

    let store = IndexBuilder::new(0, 0, 1, 1)
        .with_unix_volume("dev:test", "/tmp/findx-fixture")
        .build_from_raw(files, dirs, true)
        .unwrap();
    assert!(store.dir_paths_buf.is_empty(), "要求目录路径池未物化");

    let engine = SearchEngine::new(store);
    let store_ref = engine.index_store();

    // 朴素基准：严格按 `compose_volume_dir_path` / `join_unix_prefix` 的拼接规则独立重建
    // haystack（root_prefix 尾部去 `/`、dir_rel 内 `\` 归一 `/`、rel 去前导 `/` 后单斜杠连接）。
    let naive = |needle: &str| -> Vec<u32> {
        let nb = needle.to_ascii_lowercase().into_bytes();
        (0..store_ref.entries.len() as u32)
            .filter(|&idx| {
                let e = &store_ref.entries[idx as usize];
                let name_lc = String::from_utf8_lossy(
                    &store_ref
                        .name_bytes(e)
                        .iter()
                        .map(|b| b.to_ascii_lowercase())
                        .collect::<Vec<u8>>(),
                )
                .into_owned();
                let dir_rel: String = store_ref
                    .dir_path_bytes(e)
                    .iter()
                    .map(|b| if *b == b'\\' { b'/' } else { *b })
                    .collect::<Vec<u8>>()
                    .into_iter()
                    .map(|b| b as char)
                    .collect();
                let rel_trim = dir_rel.trim_start_matches('/');
                let mut full = String::from("/tmp/findx-fixture");
                if !rel_trim.is_empty() {
                    full.push('/');
                    full.push_str(rel_trim);
                }
                full.push('/');
                full.push_str(&name_lc);
                let full_lc = full.to_ascii_lowercase().into_bytes();
                full_lc.windows(nb.len().max(1)).any(|w| w == nb.as_slice())
            })
            .collect()
    };

    let search = |q: &str| -> Vec<String> {
        let pq = QueryParser::parse(q).unwrap();
        let (hits, _) = engine.search(&pq, &SearchOptions::default()).unwrap();
        hits.iter().map(|h| h.name.clone()).collect()
    };

    let cases = [
        "/tmp/findx-fixture", // 纯 root_prefix
        "findx",              // prefix 内段
        "users",              // 纯目录段
        "fixture/users",      // ⚠️ 骑缝跨 root_prefix 尾与首目录边界（曾被漏报的形态）
        "tmp/f",              // 骑缝跨 prefix 内部 `/`
        "alice/readme",       // 跨目录分隔符
        "readme",             // 纯文件名（多处）
        "/tmp/findx-fixture/users/alice/projects/readme.md", // 完整路径
        "zzz_nope",           // 无命中
    ];

    for needle in cases {
        let mut expect: Vec<String> = naive(needle)
            .into_iter()
            .map(|i| {
                String::from_utf8_lossy(store_ref.name_bytes(&store_ref.entries[i as usize]))
                    .into_owned()
            })
            .collect();
        let mut got = search(&format!("path:{needle}"));
        expect.sort();
        got.sort();
        assert_eq!(
            got, expect,
            "unix path:{needle} 与朴素全路径扫描结果不一致"
        );
    }

    // 前置 sanity：骑缝用例必须真有命中，防止 fixture 退化后等价性检查变空转。
    assert!(
        !search("path:fixture/users").is_empty(),
        "跨 root_prefix 边界的 needle 应命中（这是本用例存在的意义）"
    );
}

