//! Read-only document previews. No Office automation, macros, external links or shell launches.
//! OOXML is deliberately text-only; PDF renders only page one through the Windows PDF engine.
use quick_xml::{Reader, events::Event};
use std::{
    fs::File,
    io::{Read, Seek},
    path::Path,
};
const MAX_FILE: u64 = 32 * 1024 * 1024;
const MAX_PART: u64 = 4 * 1024 * 1024;
const MAX_PARTS: usize = 100;
const MAX_OUTPUT: usize = 256 * 1024;

pub fn supported(ext: &str) -> bool {
    matches!(ext, "docx" | "xlsx" | "pptx" | "rtf" | "pdf")
}
pub fn load_text(path: &Path, ext: &str) -> Result<String, String> {
    let f = File::open(path).map_err(|e| e.to_string())?;
    if f.metadata().map_err(|e| e.to_string())?.len() > MAX_FILE {
        return Err("文档超过 32 MiB 预览上限".into());
    }
    if ext == "rtf" {
        let mut bytes = Vec::new();
        f.take(MAX_PART + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_PART {
            return Err("RTF 超过 4 MiB 预览上限".into());
        }
        return rtf_text(&bytes);
    }
    office_text(f, ext)
}
fn part<R: Read + Seek>(zip: &mut zip::ZipArchive<R>, name: &str) -> Result<String, String> {
    let f = zip
        .by_name(name)
        .map_err(|e| format!("缺少文档部件 {name}: {e}"))?;
    if f.size() > MAX_PART {
        return Err("文档 XML 部件超过 4 MiB 上限".into());
    }
    let mut bytes = Vec::new();
    f.take(MAX_PART + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_PART {
        return Err("解压部件超过安全上限".into());
    }
    String::from_utf8(bytes).map_err(|_| "文档 XML 不是 UTF-8".into())
}
fn office_text<R: Read + Seek>(file: R, ext: &str) -> Result<String, String> {
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|e| format!("无法读取 OOXML（加密或损坏的文档不支持）: {e}"))?;
    if zip.len() > 10000 {
        return Err("文档部件数量超过安全上限".into());
    }
    if ext == "docx" {
        return xml_text(&part(&mut zip, "word/document.xml")?);
    }
    let mut names: Vec<_> = zip
        .file_names()
        .filter(|n| match ext {
            "pptx" => {
                n.starts_with("ppt/slides/slide") && n.ends_with(".xml") && !n.contains("/_rels/")
            }
            "xlsx" => n.starts_with("xl/worksheets/sheet") && n.ends_with(".xml"),
            _ => false,
        })
        .map(str::to_owned)
        .collect();
    // Numeric ordering: slide2 before slide10.
    names.sort_by_key(|s| {
        s.chars()
            .filter(char::is_ascii_digit)
            .collect::<String>()
            .parse::<u32>()
            .unwrap_or(0)
    });
    if names.is_empty() {
        return Err("文档没有可提取的工作表或幻灯片".into());
    }
    let mut shared = Vec::new();
    if ext == "xlsx" && zip.file_names().any(|s| s == "xl/sharedStrings.xml") {
        shared = shared_strings(&part(&mut zip, "xl/sharedStrings.xml")?)?;
    }
    let mut out = String::new();
    let mut xml_budget = 16 * 1024 * 1024usize;
    for (i, name) in names.iter().take(MAX_PARTS).enumerate() {
        let xml = part(&mut zip, name)?;
        if xml.len() > xml_budget {
            out.push_str("\n[达到 16 MiB XML 解压总量上限]");
            break;
        }
        xml_budget -= xml.len();
        out.push_str(&format!(
            "\n── {} {} ──\n",
            if ext == "pptx" {
                "幻灯片"
            } else {
                "工作表"
            },
            i + 1
        ));
        out.push_str(&if ext == "xlsx" {
            sheet_text(&xml, &shared)?
        } else {
            xml_text(&xml)?
        });
        if out.len() >= MAX_OUTPUT {
            break;
        }
    }
    Ok(capped(out))
}
fn capped(mut s: String) -> String {
    if s.len() > MAX_OUTPUT {
        let mut n = MAX_OUTPUT;
        while !s.is_char_boundary(n) {
            n -= 1;
        }
        s.truncate(n);
        s.push_str("\n[内容已截断]");
    }
    s
}
fn xml_text(xml: &str) -> Result<String, String> {
    let mut reader = Reader::from_str(xml);
    let mut out = String::new();
    let mut in_text = false;
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(e) => {
                if e.local_name().as_ref() == b"t" {
                    in_text = true;
                }
            }
            Event::End(e) => match e.local_name().as_ref() {
                b"t" => in_text = false,
                b"p" | b"tr" => out.push('\n'),
                b"tc" => out.push('\t'),
                _ => {}
            },
            Event::Empty(e) => match e.local_name().as_ref() {
                b"tab" => out.push('\t'),
                b"br" | b"cr" => out.push('\n'),
                _ => {}
            },
            Event::Text(e) if in_text => out.push_str(&e.unescape().map_err(|e| e.to_string())?),
            Event::DocType(_) => return Err("不支持含 DTD 的文档".into()),
            Event::Eof => break,
            _ => {}
        }
        if out.len() > MAX_OUTPUT {
            break;
        }
    }
    Ok(capped(out))
}
fn shared_strings(xml: &str) -> Result<Vec<String>, String> {
    let mut reader = Reader::from_str(xml);
    let mut result = Vec::new();
    let mut item = String::new();
    let mut text = false;
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(e) => match e.local_name().as_ref() {
                b"si" => item.clear(),
                b"t" => text = true,
                _ => {}
            },
            Event::End(e) => match e.local_name().as_ref() {
                b"si" => result.push(item.clone()),
                b"t" => text = false,
                _ => {}
            },
            Event::Text(e) if text => item.push_str(&e.unescape().map_err(|e| e.to_string())?),
            Event::DocType(_) => return Err("不支持含 DTD 的文档".into()),
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(result)
}
fn sheet_text(xml: &str, shared: &[String]) -> Result<String, String> {
    let mut reader = Reader::from_str(xml);
    let mut out = String::new();
    let mut typ = String::new();
    let mut address = String::new();
    let mut value = String::new();
    let mut in_value = false;
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(e) => match e.local_name().as_ref() {
                b"c" => {
                    typ.clear();
                    address.clear();
                    value.clear();
                    for a in e.attributes() {
                        let a = a.map_err(|e| e.to_string())?;
                        if a.key.as_ref() == b"t" {
                            typ = a.unescape_value().map_err(|e| e.to_string())?.into_owned();
                        }
                        if a.key.as_ref() == b"r" {
                            address = a.unescape_value().map_err(|e| e.to_string())?.into_owned();
                        }
                    }
                }
                b"v" | b"t" => in_value = true,
                _ => {}
            },
            Event::End(e) => match e.local_name().as_ref() {
                b"v" | b"t" => in_value = false,
                b"c" => {
                    let val = if typ == "s" {
                        value
                            .parse::<usize>()
                            .ok()
                            .and_then(|i| shared.get(i))
                            .cloned()
                            .unwrap_or_else(|| "[共享字符串缺失]".into())
                    } else {
                        value.clone()
                    };
                    out.push_str(&format!("{address}: {val}\t"));
                }
                b"row" => out.push('\n'),
                _ => {}
            },
            Event::Text(e) if in_value => value.push_str(&e.unescape().map_err(|e| e.to_string())?),
            Event::DocType(_) => return Err("不支持含 DTD 的文档".into()),
            Event::Eof => break,
            _ => {}
        }
        if out.len() > MAX_OUTPUT {
            break;
        }
    }
    Ok(capped(out))
}

/// Conservative RTF text subset, skips embedded objects/pictures and metadata destinations.
fn rtf_text(bytes: &[u8]) -> Result<String, String> {
    if !bytes.starts_with(b"{\\rtf") {
        return Err("不是有效的 RTF 文档".into());
    }
    let mut out = String::new();
    let mut stack = vec![(false, 1usize)];
    let mut i = 0;
    let mut fallback = 0usize;
    while i < bytes.len() && out.len() < MAX_OUTPUT {
        let b = bytes[i];
        i += 1;
        match b {
            b'{' => {
                if stack.len() >= 256 {
                    return Err("RTF 嵌套过深".into());
                }
                stack.push(*stack.last().unwrap());
            }
            b'}' => {
                if stack.len() > 1 {
                    stack.pop();
                }
            }
            b'\\' => {
                if i >= bytes.len() {
                    break;
                }
                let c = bytes[i];
                i += 1;
                if matches!(c, b'\\' | b'{' | b'}') {
                    if fallback > 0 {
                        fallback -= 1;
                    } else if !stack.last().unwrap().0 {
                        out.push(c as char);
                    }
                    continue;
                }
                if c == b'*' {
                    stack.last_mut().unwrap().0 = true;
                    continue;
                }
                if c == b'\'' {
                    if i + 2 <= bytes.len() {
                        let h = std::str::from_utf8(&bytes[i..i + 2])
                            .ok()
                            .and_then(|s| u8::from_str_radix(s, 16).ok());
                        i += 2;
                        if fallback > 0 {
                            fallback -= 1;
                        } else if !stack.last().unwrap().0 {
                            if let Some(h) = h {
                                let a = [h];
                                let (s, _, _) = encoding_rs::WINDOWS_1252.decode(&a);
                                out.push_str(&s);
                            }
                        }
                    }
                    continue;
                }
                if !c.is_ascii_alphabetic() {
                    continue;
                }
                let start = i - 1;
                while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                    i += 1;
                }
                let word = &bytes[start..i];
                let nstart = i;
                if i < bytes.len() && bytes[i] == b'-' {
                    i += 1;
                }
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                let n = std::str::from_utf8(&bytes[nstart..i])
                    .ok()
                    .and_then(|s| s.parse::<i32>().ok());
                if i < bytes.len() && bytes[i] == b' ' {
                    i += 1;
                }
                match word {
                    b"fonttbl" | b"colortbl" | b"stylesheet" | b"info" | b"pict" | b"object"
                    | b"fldinst" => stack.last_mut().unwrap().0 = true,
                    b"bin" => {
                        let n = n.unwrap_or(0).max(0) as usize;
                        i = i.saturating_add(n).min(bytes.len());
                    }
                    b"uc" => stack.last_mut().unwrap().1 = n.unwrap_or(1).clamp(0, 16) as usize,
                    b"u" if !stack.last().unwrap().0 => {
                        if let Some(n) = n {
                            if let Some(c) = char::from_u32((n as i16 as u16) as u32) {
                                out.push(c);
                            }
                            fallback = stack.last().unwrap().1;
                        }
                    }
                    b"par" | b"line" if !stack.last().unwrap().0 => out.push('\n'),
                    b"tab" if !stack.last().unwrap().0 => out.push('\t'),
                    _ => {}
                }
            }
            b'\r' | b'\n' => {}
            _ => {
                if fallback > 0 {
                    fallback -= 1;
                } else if !stack.last().unwrap().0 {
                    if b < 128 {
                        out.push(b as char);
                    } else {
                        let a = [b];
                        let (s, _, _) = encoding_rs::WINDOWS_1252.decode(&a);
                        out.push_str(&s);
                    }
                }
            }
        }
    }
    Ok(capped(out))
}

#[cfg(windows)]
pub fn pdf_first_page(path: &Path) -> Result<(Vec<u8>, u32), String> {
    use windows::{
        Data::Pdf::{PdfDocument, PdfPageRenderOptions},
        Storage::{
            StorageFile,
            Streams::{DataReader, InMemoryRandomAccessStream},
        },
        core::HSTRING,
    };
    if std::fs::metadata(path).map_err(|e| e.to_string())?.len() > MAX_FILE {
        return Err("PDF 超过 32 MiB 预览上限".into());
    }
    let run = || -> windows::core::Result<(Vec<u8>, u32)> {
        let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(path.as_os_str()))?.get()?;
        let doc = PdfDocument::LoadFromFileAsync(&file)?.get()?;
        let count = doc.PageCount()?;
        let page = doc.GetPage(0)?;
        let stream = InMemoryRandomAccessStream::new()?;
        let options = PdfPageRenderOptions::new()?;
        let page_size = page.Size()?;
        let scale = (1200.0 / page_size.Width.max(1.0)).min(1600.0 / page_size.Height.max(1.0));
        options.SetDestinationWidth((page_size.Width * scale).max(1.0) as u32)?;
        options.SetDestinationHeight((page_size.Height * scale).max(1.0) as u32)?;
        page.RenderWithOptionsToStreamAsync(&stream, &options)?
            .get()?;
        let size = stream.Size()?.min(24 * 1024 * 1024) as u32;
        let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0)?)?;
        reader.LoadAsync(size)?.get()?;
        let mut bytes = vec![0; size as usize];
        reader.ReadBytes(&mut bytes)?;
        page.Close()?;
        Ok((bytes, count))
    };
    run().map_err(|e| format!("Windows PDF 引擎无法预览（可能加密、损坏或引擎不可用）：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn docx_text_and_entities() {
        assert_eq!(
            xml_text("<w:document><w:p><w:r><w:t>Safe &amp; local</w:t></w:r></w:p></w:document>")
                .unwrap(),
            "Safe & local\n"
        );
    }
    #[test]
    fn spreadsheet_shared_and_numeric() {
        let s = shared_strings("<sst><si><t>Test</t></si></sst>").unwrap();
        assert_eq!(sheet_text("<sheet><row><c r=\"A1\" t=\"s\"><v>0</v></c><c r=\"B1\"><v>42</v></c></row></sheet>",&s).unwrap(),"A1: Test\tB1: 42\t\n");
    }
    #[test]
    fn rtf_skips_objects_and_supports_unicode() {
        assert_eq!(
            rtf_text(br"{\rtf1\ansi Hello {\pict SECRET}world\par \u20013?}").unwrap(),
            "Hello world\n中"
        );
    }
    #[test]
    fn dtd_rejected() {
        assert!(xml_text("<!DOCTYPE x><x/>").is_err());
    }
    #[test]
    fn malformed_zip_is_error() {
        assert!(office_text(std::io::Cursor::new(b"not a zip"), "docx").is_err());
    }
    #[test]
    fn ooxml_fixture_roundtrip() {
        use std::io::{Cursor, Write};
        use zip::{ZipWriter, write::SimpleFileOptions};
        for (ext, name, xml, expected) in [
            (
                "docx",
                "word/document.xml",
                "<w:p><w:t>Public fixture</w:t></w:p>",
                "Public fixture",
            ),
            (
                "pptx",
                "ppt/slides/slide1.xml",
                "<a:p><a:t>Public slide</a:t></a:p>",
                "Public slide",
            ),
            (
                "xlsx",
                "xl/worksheets/sheet1.xml",
                "<row><c r=\"A1\"><v>42</v></c></row>",
                "A1: 42",
            ),
        ] {
            let mut z = ZipWriter::new(Cursor::new(Vec::new()));
            z.start_file(name, SimpleFileOptions::default()).unwrap();
            z.write_all(xml.as_bytes()).unwrap();
            let bytes = z.finish().unwrap().into_inner();
            assert!(
                office_text(Cursor::new(bytes), ext)
                    .unwrap()
                    .contains(expected)
            );
        }
    }
}

#[cfg(all(test, windows))]
mod pdf_tests {
    #[test]
    fn native_pdf_renders_public_fixture() {
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = vec![0];
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Count 1 /Kids [3 0 R] >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>",
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
            "<< /Length 52 >>\nstream\nBT /F1 16 Tf 30 100 Td (Public preview fixture) Tj ET\nendstream",
        ];
        for (i, obj) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", i + 1, obj).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
        for n in offsets.iter().skip(1) {
            pdf.extend_from_slice(format!("{n:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
        );
        let path = std::env::temp_dir().join(format!(
            "findx-native-public-preview-{}.pdf",
            std::process::id()
        ));
        std::fs::write(&path, pdf).unwrap();
        let result = super::pdf_first_page(&path);
        let _ = std::fs::remove_file(&path);
        let (bytes, pages) = result.expect("Windows PDF engine should render public fixture");
        assert_eq!(pages, 1);
        assert!(!bytes.is_empty());
    }
}
