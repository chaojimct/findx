//! M1 富格式抽取：OOXML（docx/xlsx/pptx）+ PDF（原生文本）。
//!
//! 策略（纯 Rust，无 Office 依赖）：
//! - docx/xlsx/pptx 本质是 zip 包里的 XML：`zip` 解包 + `quick-xml` 只取文本节点。
//! - pdf 用 `pdf-extract`（排版文本抽取；扫描版图片型 PDF 无文本层，M2 走 OCR）。
//! - 毒文件（zip 炸弹/损坏 XML/加密 pdf）一律返回 `Error::Extract`，调用方计数跳过。

use std::io::Read;
use std::path::Path;

use crate::{Error, Result};

/// 富格式单文件解压/解析上限（ooxml 解压后可能膨胀 10 倍以上）。
pub const RICH_MAX_XML_BYTES: usize = 64 * 1024 * 1024;

/// `word/document.xml` 的 `<w:t>`、sheet/slide 的 `<t>`/`<a:t>`：后缀匹配即可，
/// 忽略命名空间前缀差异（`w:t` vs 裸 `t`），大小写不敏感。
fn is_text_element(name: &[u8]) -> bool {
    name.eq_ignore_ascii_case(b"w:t") || name.eq_ignore_ascii_case(b"t") || name.eq_ignore_ascii_case(b"a:t")
}

/// 从 zip 包内的 XML 文件收集所有文本节点（流式，不全量建 DOM）。
fn collect_zip_xml_text(zip_path: &Path, inner_names: &[&str]) -> Result<String> {
    let f = std::fs::File::open(zip_path)
        .map_err(|e| Error::Io(zip_path.display().to_string(), e))?;
    let mut zip = zip::ZipArchive::new(f)
        .map_err(|e| Error::Extract(zip_path.display().to_string(), format!("zip 打开失败: {e}")))?;
    let mut out = String::new();
    for inner in inner_names {
        let mut entry = match zip.by_name(inner) {
            Ok(e) => e,
            Err(_) => continue, // 缺某个 part（如无 sharedStrings）是合法现象，跳过
        };
        if entry.size() > RICH_MAX_XML_BYTES as u64 {
            return Err(Error::Extract(
                zip_path.display().to_string(),
                format!("{inner} 过大（{}B），疑似 zip 炸弹，跳过", entry.size()),
            ));
        }
        let mut xml = String::new();
        entry
            .read_to_string(&mut xml)
            .map_err(|e| Error::Extract(zip_path.display().to_string(), format!("{inner} 读取失败: {e}")))?;
        drop(entry);
        collect_xml_text(&xml, &mut out).map_err(|e| {
            Error::Extract(zip_path.display().to_string(), format!("{inner} 解析失败: {e}"))
        })?;
        out.push('\n');
    }
    Ok(out)
}

fn collect_xml_text(xml: &str, out: &mut String) -> std::result::Result<(), String> {
    use quick_xml::events::Event;
    use quick_xml::reader::Reader;
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut in_text = false;
    loop {
        match reader.read_event_into(&mut buf).map_err(|e| e.to_string())? {
            Event::Start(ref e) => {
                in_text = is_text_element(e.name().as_ref());
            }
            Event::Empty(ref e) => {
                let _ = is_text_element(e.name().as_ref());
            }
            Event::Text(e) => {
                if in_text {
                    if let Ok(s) = e.unescape() {
                        let s = s.trim();
                        if !s.is_empty() {
                            out.push_str(&s);
                            out.push(' ');
                        }
                    }
                }
            }
            Event::End(_) => {
                in_text = false;
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(())
}

pub fn extract_docx(path: &Path) -> Result<String> {
    // 正文 + 页眉页脚 + 批注（批注常含评审意见，值得搜）。
    let t = collect_zip_xml_text(
        path,
        &[
            "word/document.xml",
            "word/header1.xml",
            "word/header2.xml",
            "word/header3.xml",
            "word/footer1.xml",
            "word/footer2.xml",
            "word/footer3.xml",
            "word/comments.xml",
        ],
    )?;
    if t.trim().is_empty() {
        return Err(Error::Extract(
            path.display().to_string(),
            "docx 无正文文本（图片型/空文档走 M2 OCR）".into(),
        ));
    }
    Ok(t)
}

pub fn extract_xlsx(path: &Path) -> Result<String> {
    let f = std::fs::File::open(path).map_err(|e| Error::Io(path.display().to_string(), e))?;
    let mut zip = zip::ZipArchive::new(f)
        .map_err(|e| Error::Extract(path.display().to_string(), format!("zip 打开失败: {e}")))?;
    let mut out = String::new();
    // sharedStrings：按下标取（sheet 引用的 <v> 是下标）。
    let mut shared: Vec<String> = Vec::new();
    if let Ok(mut entry) = zip.by_name("xl/sharedStrings.xml") {
        if entry.size() <= RICH_MAX_XML_BYTES as u64 {
            let mut xml = String::new();
            if entry.read_to_string(&mut xml).is_ok() {
                let mut tmp = String::new();
                if collect_xml_text(&xml, &mut tmp).is_ok() {
                    // sharedStrings 按 <si> 分段；简化：整段切词即可搜（丢失单元格边界可接受）。
                    shared.push(tmp);
                }
            }
        }
    }
    for s in &shared {
        out.push_str(s);
        out.push('\n');
    }
    // sheet 内联字符串 + 行列文本（含数字原样保留，如金额/年份可搜）。
    let sheet_names: Vec<String> = zip
        .file_names()
        .filter(|n| {
            n.starts_with("xl/worksheets/sheet") || n.starts_with("xl/chartsheets/sheet")
        })
        .map(|s| s.to_owned())
        .collect();
    for name in sheet_names {
        let mut entry = match zip.by_name(&name) {
            Ok(e) => e,
            Err(_) => continue,
        };
        if entry.size() > RICH_MAX_XML_BYTES as u64 {
            continue;
        }
        let mut xml = String::new();
        if entry.read_to_string(&mut xml).is_err() {
            continue;
        }
        drop(entry);
        let mut tmp = String::new();
        if collect_xml_text(&xml, &mut tmp).is_ok() {
            out.push_str(&tmp);
            out.push('\n');
        }
    }
    // 工作表名也可搜（1.2.941 修过 .xls 表名搜不到的问题，教训：表名别丢）。
    if let Ok(mut entry) = zip.by_name("xl/workbook.xml") {
        let mut xml = String::new();
        if entry.read_to_string(&mut xml).is_ok() {
            let mut tmp = String::new();
            if collect_sheet_names(&xml, &mut tmp).is_ok() {
                out.push_str(&tmp);
            }
        }
    }
    if out.trim().is_empty() {
        return Err(Error::Extract(
            path.display().to_string(),
            "xlsx 无文本（纯数字表也可能全空）".into(),
        ));
    }
    Ok(out)
}

/// workbook.xml 的 `<sheet name="...">` 取名。
fn collect_sheet_names(xml: &str, out: &mut String) -> std::result::Result<(), String> {
    use quick_xml::events::Event;
    use quick_xml::reader::Reader;
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf).map_err(|e| e.to_string())? {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let n = e.name();
                if n.as_ref().eq_ignore_ascii_case(b"sheet") {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref().eq_ignore_ascii_case(b"name") {
                            if let Ok(v) = attr.decode_and_unescape_value(reader.decoder()) {
                                out.push_str(&v);
                                out.push(' ');
                            }
                        }
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(())
}

pub fn extract_pptx(path: &Path) -> Result<String> {
    let f = std::fs::File::open(path).map_err(|e| Error::Io(path.display().to_string(), e))?;
    let mut zip = zip::ZipArchive::new(f)
        .map_err(|e| Error::Extract(path.display().to_string(), format!("zip 打开失败: {e}")))?;
    let slides: Vec<String> = zip
        .file_names()
        .filter(|n| n.starts_with("ppt/slides/slide") || n.starts_with("ppt/notesSlides/notesSlide"))
        .map(|s| s.to_owned())
        .collect();
    if slides.is_empty() {
        return Err(Error::Extract(
            path.display().to_string(),
            "pptx 无幻灯片 part".into(),
        ));
    }
    let mut out = String::new();
    for name in slides {
        let mut entry = match zip.by_name(&name) {
            Ok(e) => e,
            Err(_) => continue,
        };
        if entry.size() > RICH_MAX_XML_BYTES as u64 {
            continue;
        }
        let mut xml = String::new();
        if entry.read_to_string(&mut xml).is_err() {
            continue;
        }
        drop(entry);
        let mut tmp = String::new();
        if collect_xml_text(&xml, &mut tmp).is_ok() {
            out.push_str(&tmp);
            out.push('\n');
        }
    }
    if out.trim().is_empty() {
        return Err(Error::Extract(
            path.display().to_string(),
            "pptx 无文本（图片型幻灯走 M2 OCR）".into(),
        ));
    }
    Ok(out)
}

pub fn extract_pdf(path: &Path) -> Result<String> {
    let bytes =
        std::fs::read(path).map_err(|e| Error::Io(path.display().to_string(), e))?;
    // pdf-extract 遇到不支持的编码（如中文 PDF 常见的 UniGB-UCS2-H）会直接 panic
    // 而不是返回 Err；一个毒文件不能把整个构建线程（rayon worker）带走，
    // 兜底转成 Extract 错误，由调用方计数跳过。
    // 注意：panic 触发时默认 hook 仍会往 stderr 打一行“thread panicked”，
    // 这是 catch 之前的无害噪音（毒 PDF 越多行数越多），真正的信号看 失败 计数。
    // 不换全局 hook：set_hook 进程级，多线程下会互相覆盖，因小失大。
    let text = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pdf_extract::extract_text_from_mem(&bytes)
    }))
    .map_err(|payload| {
        let msg = payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| {
                payload
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_string())
            })
            .unwrap_or_else(|| "pdf 解析器 panic".to_string());
        Error::Extract(
            path.display().to_string(),
            format!("pdf 解析失败（{msg}；加密/损坏/非常见编码走 M2）"),
        )
    })?
    .map_err(|e| {
        Error::Extract(
            path.display().to_string(),
            format!("pdf 解析失败（加密/损坏/图片型走 M2）: {e}"),
        )
    })?;
    if text.trim().is_empty() {
        return Err(Error::Extract(
            path.display().to_string(),
            "pdf 无文本层（扫描版走 M2 OCR）".into(),
        ));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// 内存合成最小 docx（zip + document.xml），不依赖外部样本。
    fn make_docx(dir: &std::path::Path, name: &str, body_xml: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        let f = std::fs::File::create(&p).unwrap();
        let mut zip = zip::ZipWriter::new(f);
        zip.start_file(
            "word/document.xml",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(
            format!(
                r#"<?xml version="1.0"?><w:document xmlns:w="x"><w:body>{body_xml}</w:body></w:document>"#
            )
            .as_bytes(),
        )
        .unwrap();
        zip.finish().unwrap();
        p
    }

    #[test]
    fn docx_text_nodes() {
        let d = tempfile::tempdir().unwrap();
        let p = make_docx(
            d.path(),
            "a.docx",
            "<w:p><w:r><w:t>全文搜索</w:t></w:r><w:r><w:t>hello</w:t></w:r></w:p>",
        );
        let t = extract_docx(&p).unwrap();
        assert!(t.contains("全文搜索"), "{t}");
        assert!(t.contains("hello"), "{t}");
    }

    #[test]
    fn docx_empty_is_error() {
        let d = tempfile::tempdir().unwrap();
        let p = make_docx(d.path(), "e.docx", "<w:p></w:p>");
        assert!(extract_docx(&p).is_err());
    }

    #[test]
    fn broken_zip_is_error_not_panic() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("bad.docx");
        std::fs::write(&p, b"not a zip at all").unwrap();
        assert!(extract_docx(&p).is_err());
        assert!(extract_xlsx(&p).is_err());
        assert!(extract_pptx(&p).is_err());
    }

    #[test]
    fn malformed_pdf_is_error_not_panic() {
        // 毒文件（非常见编码/损坏）只能 Err（记失败跳过），绝不能 panic
        // 把构建线程带走。回归：UniGB-UCS2-H 中文 PDF 曾崩掉整轮构建。
        let d = tempfile::tempdir().unwrap();
        let cases: &[&[u8]] = &[
            b"not a pdf at all",
            b"%PDF-1.4 truncated",
            b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n",
        ];
        for (i, data) in cases.iter().enumerate() {
            let p = d.path().join(format!("bad{i}.pdf"));
            std::fs::write(&p, data).unwrap();
            assert!(extract_pdf(&p).is_err(), "case {i}");
        }
    }
}
