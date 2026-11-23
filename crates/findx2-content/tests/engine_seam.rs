//! M0 缝合验收：`content:` 经 `SearchOptions.content` 走 tantivy 快路径，
//! 命中正确映射到 `IndexStore` 条目；无 content 索引时回退旧慢路径。

use std::io::Write;
use std::sync::Arc;

use findx2_content::{extract_text, open_or_create, DocMeta};
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

fn build_store(tmp: &std::path::Path) -> findx2_core::IndexStore {
    // 磁盘文件名必须与索引条目名一致（慢路径回退要读真实文件）；
    // 用 unix 卷把 root_prefix 指向 tempdir，display_path 即真实路径。
    std::fs::File::create(tmp.join("alpha.txt"))
        .unwrap()
        .write_all("findx 全文搜索 alpha".as_bytes())
        .unwrap();
    std::fs::File::create(tmp.join("beta.txt"))
        .unwrap()
        .write_all("nothing relevant here beta".as_bytes())
        .unwrap();
    // unix 卷要求 root_prefix 含 '/'（见 is_unix），Windows 上把分隔符翻转即可；
    // fs::read 对正斜杠路径同样可用。
    let root = tmp.display().to_string().replace('\\', "/");
    let builder = IndexBuilder::new(0, 1, 1, 1).with_unix_volume("C", root);
    builder
        .build_from_raw(
            vec![
                raw(1001, 100, "alpha.txt", false),
                raw(1002, 100, "beta.txt", false),
            ],
            vec![raw(100, 0, "", true)],
            true,
        )
        .unwrap()
}

#[test]
fn content_fast_path_end_to_end() {
    let case = tempfile::tempdir().unwrap();
    let store = build_store(case.path());

    // 内容索引：frn/volume 与文件名索引对齐。
    let cidx = open_or_create(&case.path().join("tantivy")).unwrap();
    for (frn, name) in [(1001u64, "alpha.txt"), (1002u64, "beta.txt")] {
        let p = case.path().join(name);
        let t = extract_text(&p).unwrap();
        cidx
            .index_doc(
                &DocMeta {
                    frn,
                    volume_id: "C".into(),
                    path: p.display().to_string(),
                    mtime: 0,
                    size: t.raw_bytes,
                },
                &t.text,
            )
            .unwrap();
    }
    cidx.commit().unwrap();

    let engine = SearchEngine::new(store);
    let opt = SearchOptions {
        allow_pinyin: false,
        content: Some(Arc::new(cidx)),
        ..Default::default()
    };

    // 快路径：只命中 alpha。
    let pq = QueryParser::parse("content:全文搜索").unwrap();
    let (hits, total) = engine.search(&pq, &opt).unwrap();
    assert_eq!(total, 1, "快路径应只命中 alpha.txt");
    assert_eq!(hits.len(), 1);
    assert!(hits[0].path.ends_with("alpha.txt"), "{}", hits[0].path);

    // 与文件名谓词求交：content 命中 ∩ name 命中。
    let pq = QueryParser::parse("content:beta alpha").unwrap();
    let (hits, total) = engine.search(&pq, &opt).unwrap();
    assert_eq!(total, 0, "交集应为空（beta 内容无 alpha 文件名）");
    assert!(hits.is_empty());

    // 无 content 索引：回退慢路径，同样只命中 alpha（读真实文件）。
    let engine2_store = build_store(case.path());
    let engine2 = SearchEngine::new(engine2_store);
    let pq = QueryParser::parse("content:全文搜索").unwrap();
    let (hits, total) = engine2
        .search(&pq, &SearchOptions::default())
        .unwrap();
    assert_eq!(total, 1);
    assert!(hits[0].path.ends_with("alpha.txt"));
}
