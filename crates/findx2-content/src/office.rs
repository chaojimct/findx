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
    extract_sheet_book(path)
}

/// 老二进制 `.xls`（BIFF）：calamine 原生支持，索引/预览与 xlsx 同路。
/// 此前该格式无文本后备，只能走系统预览处理器。
pub fn extract_xls(path: &Path) -> Result<String> {
    extract_sheet_book(path)
}

/// xls/xlsx（乃至 xlsb/ods，白名单只放行 xls/xlsx）统一走 calamine 结构化读：
/// 按行列输出 `{列字母}{行号}: 值`（与旧手写解析同格式，网格预览可直接复用地址），
/// 工作表名追加在末尾（旧行为：表名可搜，1.2.941 修过表名搜不到的教训）。
pub(crate) fn extract_sheet_book(path: &Path) -> Result<String> {
    use calamine::{open_workbook, open_workbook_auto, Data, Reader, Xlsx};
    let mut out = String::new();
    match open_workbook_auto(path) {
        Ok(mut b) => dump_sheet_book(&mut b, &mut out),
        Err(first) => {
            // 扩展名撒谎的常见情形：内容是 OOXML zip 却挂着 .xls 后缀
            //（calamine 按 BIFF 走直接报 Invalid OLE）。嗅探 ZIP 魔数后
            // 按 Xlsx 重试一次；仍失败则报原文（保留首错信息）。
            let is_zip = std::fs::File::open(path)
                .and_then(|mut f| {
                    use std::io::Read;
                    let mut magic = [0u8; 4];
                    f.read_exact(&mut magic).map(|_| magic)
                })
                .map(|m| m == *b"PK\x03\x04")
                .unwrap_or(false);
            if !is_zip {
                return Err(Error::Extract(
                    path.display().to_string(),
                    format!("表格打开失败: {first}"),
                ));
            }
            let mut xb = open_workbook::<Xlsx<_>, _>(path).map_err(|e| {
                Error::Extract(
                    path.display().to_string(),
                    format!("表格打开失败（zip 回退亦失败，首错: {first}；回退错: {e})"),
                )
            })?;
            dump_sheet_book(&mut xb, &mut out);
        }
    }
    // 工作表名也可搜（旧行为保留）。
    // 注：表名追加在 dump 之后，避免与单元格同行混淆（旧格式约定）。
    if out.trim().is_empty() {
        return Err(Error::Extract(
            path.display().to_string(),
            "表格无文本（纯空表走跳过）".into(),
        ));
    }
    Ok(out)
}

/// 工作簿 → `{列字母}{行号}: 值` 文本（与旧手写解析同格式）+ 末尾工作表名。
/// 泛型 over calamine `Reader`：auto 探测的 `Sheets` 枚举与显式 `Xlsx` 共用同一逻辑。
fn dump_sheet_book<RS: std::io::Read + std::io::Seek>(
    book: &mut impl calamine::Reader<RS>,
    out: &mut String,
) {
    use calamine::{Data, Reader};
    for name in book.sheet_names() {
        let range = match book.worksheet_range(&name) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for (ri, row) in range.rows().enumerate() {
            for (ci, cell) in row.iter().enumerate() {
                if matches!(cell, Data::Empty) {
                    continue;
                }
                out.push_str(&col_letters(ci));
                out.push_str(&(ri + 1).to_string());
                out.push_str(": ");
                out.push_str(&cell.to_string());
                out.push('\t');
            }
            out.push('\n');
        }
    }
    for name in book.sheet_names() {
        out.push_str(&name);
        out.push(' ');
    }
}

/// 0 基列下标 → A..Z, AA.. 字母（网格预览与索引共用同一地址写法）。
fn col_letters(mut ci: usize) -> String {
    let mut s = Vec::new();
    loop {
        s.push((b'A' + (ci % 26) as u8) as char);
        if ci < 26 {
            break;
        }
        ci = ci / 26 - 1;
    }
    s.iter().rev().collect()
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
    // 主路径：worker 进程池里的 pdfium（Chrome 同款，又快又不崩 CJK）。
    // `FINDX_PDFIUM=0` 强制走纯 Rust 兜底（hang 二分开关，默认开 pdfium）。
    if std::env::var("FINDX_PDFIUM").as_deref() != Ok("0") {
        if let Some(pool) = crate::pdf_pool::global() {
            match pool.extract(path) {
                Ok(t) => return nonempty_or_scan_err(path, t),
                Err(_) => return extract_pdf_fallback(path).and_then(|t| nonempty_or_scan_err(path, t)),
            }
        }
    }
    extract_pdf_fallback(path).and_then(|t| nonempty_or_scan_err(path, t))
}

/// worker 进程不可用/崩溃/超时时，失败直接记失败（调用方计数跳过），
/// 不再回退进程内 pdfium（进程内并发会挂起整个进程，见 pdf_pool 文档）。
fn nonempty_or_scan_err(path: &Path, text: String) -> Result<String> {
    if text.trim().is_empty() {
        return Err(Error::Extract(
            path.display().to_string(),
            "pdf 无文本层（扫描版走 M2 OCR）".into(),
        ));
    }
    Ok(text)
}

/// 纯 Rust 兜底（慢；非常见编码会 panic，已接住转失败计数）。
/// 公开仅供 benchmark 对比主路径；业务代码一律走 [`extract_pdf`]。
#[doc(hidden)]
pub fn extract_pdf_fallback(path: &Path) -> Result<String> {
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
    fn col_letters_boundaries() {
        assert_eq!(col_letters(0), "A");
        assert_eq!(col_letters(25), "Z");
        assert_eq!(col_letters(26), "AA");
        assert_eq!(col_letters(27), "AB");
        assert_eq!(col_letters(51), "AZ");
        assert_eq!(col_letters(52), "BA");
    }

    /// 内存合成最小 xlsx（sharedStrings + sheet + workbook），验证 calamine 路：
    /// 共享字符串/数字原样、A1 地址、表名可搜。
    fn make_xlsx(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        let f = std::fs::File::create(&p).unwrap();
        let mut zip = zip::ZipWriter::new(f);
        let opt = zip::write::SimpleFileOptions::default();
        let parts = [
            ("[Content_Types].xml", r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/></Types>"#),
            ("_rels/.rels", r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#),
            ("xl/workbook.xml", r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="费用表" sheetId="1" r:id="rId1"/></sheets></workbook>"#),
            ("xl/_rels/workbook.xml.rels", r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#),
            ("xl/sharedStrings.xml", r#"<?xml version="1.0"?><sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="1" uniqueCount="1"><si><t>全文搜索</t></si></sst>"#),
            ("xl/worksheets/sheet1.xml", r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1"><v>42</v></c></row></sheetData></worksheet>"#),
        ];
        for (n, body) in parts {
            zip.start_file(n, opt).unwrap();
            use std::io::Write;
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        p
    }

    #[test]
    fn xlsx_calamine_addresses_and_sheet_name() {
        let d = tempfile::tempdir().unwrap();
        let p = make_xlsx(d.path(), "a.xlsx");
        let t = extract_xlsx(&p).unwrap();
        assert!(t.contains("A1: 全文搜索"), "{t}");
        assert!(t.contains("B1: 42"), "{t}");
        assert!(t.contains("费用表"), "{t}");
    }

    #[test]
    fn xls_garbage_is_error_not_panic() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("bad.xls");
        std::fs::write(&p, b"not a BIFF stream at all").unwrap();
        assert!(extract_xls(&p).is_err());
    }

    #[test]
    fn xls_with_zip_content_falls_back_to_xlsx() {
        // 真实坏案：OOXML 内容挂着 .xls 后缀（calamine 按 BIFF 走直接报 Invalid OLE）。
        let d = tempfile::tempdir().unwrap();
        let p = make_xlsx(d.path(), "mislabeled.xls");
        let t = extract_xls(&p).unwrap();
        assert!(t.contains("A1: 全文搜索"), "{t}");
        assert!(t.contains("费用表"), "{t}");
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
