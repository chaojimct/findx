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
