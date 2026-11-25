//! rdocx 可行性 spike：纯 Rust 打开 docx → 版式渲染 PNG + HTML 导出。
//! 通过标准 = 合成 docx（含中文 + 表格）能开、能渲染出非空 PNG、HTML 含正文。
//! 若本文件长期飘红/太慢，说明 rdocx 不适合进预览链路，直接删文件 + dev-dep。

use std::io::Write;

fn make_docx(path: &std::path::Path) {
    let f = std::fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(f);
    let opt = zip::write::SimpleFileOptions::default();
    for (name, body) in [
        (
            "word/document.xml",
            r#"<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>全文搜索 spike</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>格子</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>"#,
        ),
        (
            "_rels/.rels",
            r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#,
        ),
        (
            "word/_rels/document.xml.rels",
            r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#,
        ),
        (
            "[Content_Types].xml",
            r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#,
        ),
    ] {
        zip.start_file(name, opt).unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

#[test]
fn rdocx_opens_renders_exports() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("spike.docx");
    make_docx(&p);
    let doc = rdocx::Document::open(&p).expect("rdocx 应能打开合成 docx");
    let html = doc.to_html();
    assert!(html.contains("全文搜索"), "HTML 导出应含正文: {html:.200}");
    let png = doc
        .render_page_to_png_deterministic(0, 150.0)
        .expect("第 0 页应能渲染");
    let png = png.expect("PNG 不应为空 Option");
    assert!(!png.is_empty(), "PNG 字节不应为空");
    assert_eq!(&png[1..4], b"PNG", "应为合法 PNG");
}
