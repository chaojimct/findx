use findx2_core::index::{filetime_to_unix_secs, hash_ext8, IndexBuilder, IndexStore};
use findx2_core::platform::RawEntry;
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
