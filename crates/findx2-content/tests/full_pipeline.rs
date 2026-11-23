//! 全链路回归：建文件名索引 → 落盘 index.bin → 重载 → 全量建内容索引 → 查。
//! 覆盖 CLI `content-index` 与 service 启动挂载走的同一条代码路径。

use std::io::Write;
use std::sync::Arc;

use findx2_content::{build_from_store, content_dir_for, open_if_exists};
use findx2_core::{IndexBuilder, QueryParser, RawEntry, SearchEngine, SearchOptions};

fn raw(file_id: u64, parent_id: u64, name: &str, is_dir: bool) -> RawEntry {
    RawEntry {
        file_id,
        file_id_128: None,
        parent_id,
        name: name.to_owned(),
        size: 0,
        mtime: 0,
        ctime: 0,
        attrs: 0,
        is_dir,
    }
}

#[test]
fn full_pipeline_index_bin_to_content_search() {
    let case = tempfile::tempdir().unwrap();
    let root = case.path().join("vol");
    std::fs::create_dir(&root).unwrap();
    std::fs::File::create(root.join("alpha.txt"))
        .unwrap()
        .write_all("findx 全文搜索 alpha".as_bytes())
        .unwrap();
    std::fs::File::create(root.join("beta.txt"))
        .unwrap()
        .write_all("nothing relevant beta".as_bytes())
        .unwrap();

    // unix 卷，root_prefix 指向 tempdir（display_path 即真实路径）。
    let rp = root.display().to_string().replace('\\', "/");
    let store = IndexBuilder::new(0, 1, 1, 1)
        .with_unix_volume("C", rp)
        .build_from_raw(
            vec![
                raw(1001, 100, "alpha.txt", false),
                raw(1002, 100, "beta.txt", false),
            ],
            vec![raw(100, 0, "", true)],
            true,
        )
        .unwrap();
    let index_bin = case.path().join("index.bin");
    findx2_core::save_index_bin(&index_bin, &store).unwrap();

    // CLI content-index 同款调用。
    let store2 = findx2_core::load_store_auto(&index_bin).unwrap();
    let cdir = content_dir_for(&index_bin);
    let st = build_from_store(&store2, &cdir, false, |_| true).unwrap();
    assert_eq!(st.extracted, 2, "{st:?}");
    assert_eq!(st.docs, 2);

    // service 启动挂载同款 + search 快路径。
    let cidx = open_if_exists(&index_bin).expect("内容索引应已建成");
    assert_eq!(cidx.num_docs(), 2);
    let engine = SearchEngine::new(store2);
    let opt = SearchOptions {
        allow_pinyin: false,
        content: Some(Arc::new(cidx)),
        ..Default::default()
    };
    let pq = QueryParser::parse("content:全文搜索").unwrap();
    let (hits, total) = engine.search(&pq, &opt).unwrap();
    assert_eq!(total, 1);
    assert!(hits[0].path.ends_with("alpha.txt"), "{}", hits[0].path);
}
