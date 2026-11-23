//! M0 验收：抽取 → 建索引 → 中英文查询 → snippet 全链路。

use std::io::Write;

use findx2_content::{doc_key, extract_text, open_or_create, DocMeta};

fn write_txt(dir: &std::path::Path, name: &str, data: &[u8]) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::File::create(&p).unwrap().write_all(data).unwrap();
    p
}

#[test]
fn m0_roundtrip_zh_en_with_snippet() {
    let case = tempfile::tempdir().unwrap();
    let zh = write_txt(
        case.path(),
        "readme.txt",
        "全文搜索是本地磁盘的Google，findx 要做得更快".as_bytes(),
    );
    let en = write_txt(
        case.path(),
        "notes.md",
        b"# hello world from findx content index",
    );

    let idx_dir = case.path().join("tantivy");
    let idx = open_or_create(&idx_dir).unwrap();

    for (i, p) in [zh, en].iter().enumerate() {
        let e = extract_text(p).unwrap();
        idx.index_doc(
            &DocMeta {
                frn: 1000 + i as u64,
                volume_id: "C:".into(),
                path: p.display().to_string(),
                mtime: 0,
                size: e.raw_bytes,
            },
            &e.text,
        )
        .unwrap();
    }
    idx.commit().unwrap();
    assert_eq!(idx.num_docs(), 2);

    // 中文分词命中。
    let hits = idx.search("全文搜索", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].frn, 1000);
    assert!(!hits[0].snippet.is_empty(), "snippet 不得为空");

    // 英文命中另一篇。
    let hits = idx.search("hello", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].frn, 1001);

    // upsert 同 frn 不产生重复 doc。
    let e = extract_text(&case.path().join("notes.md")).unwrap();
    idx.index_doc(
        &DocMeta {
            frn: 1001,
            volume_id: "C:".into(),
            path: "notes.md".into(),
            mtime: 1,
            size: e.raw_bytes,
        },
        &e.text,
    )
    .unwrap();
    idx.commit().unwrap();
    assert_eq!(idx.num_docs(), 2);

    // 删除。
    idx.delete_by_frn("C:", 1000).unwrap();
    idx.commit().unwrap();
    assert_eq!(idx.num_docs(), 1);
    assert!(idx.search("全文搜索", 10).unwrap().is_empty());
}
