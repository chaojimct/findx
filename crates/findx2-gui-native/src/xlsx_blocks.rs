//! xlsx 表格模型：**流式解析 + 虚拟滚动**。
//!
//! 关键设计（性能）：
//! - 不把整表读进内存：用 quick-xml 流式读 `<row>`，读够 MAX_ROWS 立即停，
//!   即使 sheet 有 3.8 万行 / 27MB，也只解析前若干行（毫秒级）。
//! - sharedStrings 流式读成索引数组（顺序读取，比 calamine 建表快一个量级）。
//! - 渲染走 GPUI `uniform_list`（虚拟滚动），只渲染可见行，行数再多也不卡。
//!
//! 不再有"超大 sheet 跳过"——大表也能看（看到多少行由 MAX_ROWS 决定）。

use quick_xml::events::Event;
use quick_xml::Reader;
use std::io::{BufReader, Read};
use std::sync::Arc;

/// 单页表格。
#[derive(Debug, Clone, Default)]
pub struct Sheet {
    pub name: String,
    /// 行优先的单元格文本（Arc：虚拟滚动闭包可廉价捕获）。
    pub rows: Arc<Vec<Vec<String>>>,
    /// 命中格坐标 (row, col)。
    pub hits: Vec<(usize, usize)>,
    /// 该 sheet 真实总行数（来自 XML 的 row 数，用于提示"仅显示前 N 行"）。
    pub total_rows: usize,
}

impl Sheet {
    pub fn cols(&self) -> usize {
        self.rows.iter().map(|r| r.len()).max().unwrap_or(0)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Workbook {
    pub sheets: Vec<Sheet>,
    pub focus_sheet: usize,
}

/// 每页最多解析的行数：虚拟滚动负责渲染，这里只限制内存与解析时间。
const MAX_ROWS: usize = 5000;
const MAX_COLS: usize = 100;

/// 解析工作簿（流式，只读每页前 MAX_ROWS 行）。
pub fn parse(path: &str) -> Result<Workbook, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len > 128 * 1024 * 1024 {
        return Err("xlsx 超过 128 MiB 上限".into());
    }
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("xlsx 打开失败: {e}"))?;

    // 1) sheet 名（按 workbook.xml 顺序）
    let names = sheet_names(&mut zip)?;
    if names.is_empty() {
        return Err("工作簿没有工作表".into());
    }

    // 2) sharedStrings（流式，索引顺序）
    let shared = read_shared_strings(&mut zip);

    // 3) 逐 sheet 流式读（行数按 sheet 体量自适应：超大表少读几行，
    //    虚拟滚动只渲染可见区，够滚一屏即可，避免「原始数据」巨表拖慢首屏）。
    let mut out = Workbook::default();
    for (i, name) in names.iter().enumerate() {
        let part = format!("xl/worksheets/sheet{}.xml", i + 1);
        let sz = zip.by_name(&part).map(|f| f.size() as usize).unwrap_or(0);
        let max_rows = if sz > 8 * 1024 * 1024 {
            1200
        } else if sz > 2 * 1024 * 1024 {
            3000
        } else {
            MAX_ROWS
        };
        let (rows, total_rows) = match read_sheet_rows(&mut zip, &part, &shared, max_rows) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if rows.is_empty() {
            continue;
        }
        out.sheets.push(Sheet {
            name: name.clone(),
            rows: Arc::new(rows),
            hits: Vec::new(),
            total_rows,
        });
    }
    if out.sheets.is_empty() {
        return Err("工作簿没有可显示的单元格".into());
    }
    Ok(out)
}

/// 读 sheet 名列表（xl/workbook.xml 的 `<sheet name="...">`）。
fn sheet_names(zip: &mut zip::ZipArchive<std::fs::File>) -> Result<Vec<String>, String> {
    let f = zip
        .by_name("xl/workbook.xml")
        .map_err(|e| format!("缺少 workbook.xml: {e}"))?;
    let mut xml = String::new();
    let mut limited = f.take(4 * 1024 * 1024);
    limited
        .read_to_string(&mut xml)
        .map_err(|e| e.to_string())?;
    drop(limited);
    let mut reader = Reader::from_str(&xml);
    let mut names = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                if e.local_name().as_ref() == b"sheet" {
                    for a in e.attributes().flatten() {
                        if a.key.local_name().as_ref() == b"name" {
                            if let Ok(v) = a.unescape_value() {
                                names.push(v.into_owned());
                            }
                        }
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    Ok(names)
}

/// 流式读 sharedStrings → Vec<String>（按索引顺序）。
fn read_shared_strings(zip: &mut zip::ZipArchive<std::fs::File>) -> Vec<String> {
    let Ok(f) = zip.by_name("xl/sharedStrings.xml") else {
        return Vec::new();
    };
    let mut reader = Reader::from_reader(BufReader::new(f));
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_si = false;
    let mut in_t = false;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"si" => {
                    in_si = true;
                    cur.clear();
                }
                b"t" if in_si => in_t = true,
                _ => {}
            },
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"si" => {
                    in_si = false;
                    out.push(cur.clone());
                    if out.len() > 500_000 {
                        break;
                    }
                }
                b"t" => in_t = false,
                _ => {}
            },
            Ok(Event::Text(t)) => {
                if in_t {
                    if let Ok(s) = t.unescape() {
                        cur.push_str(&s);
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// 流式读 sheet 前 MAX_ROWS 行。返回 (rows, total_rows_seen)。
fn read_sheet_rows(
    zip: &mut zip::ZipArchive<std::fs::File>,
    part: &str,
    shared: &[String],
    max_rows: usize,
) -> Result<(Vec<Vec<String>>, usize), String> {
    let f = zip.by_name(part).map_err(|e| format!("缺少 {part}: {e}"))?;
    Ok(parse_rows(BufReader::new(f), shared, max_rows))
}

/// 通用流式行解析（正式路径与单测共用，避免两套逻辑不一致）。
fn parse_rows<R: std::io::BufRead>(
    src: R,
    shared: &[String],
    max_rows: usize,
) -> (Vec<Vec<String>>, usize) {
    let mut reader = Reader::from_reader(src);
    let mut rows: Vec<Vec<String>> = Vec::with_capacity(max_rows.min(256));
    let mut total_rows = 0usize;
    let mut cur_row: Vec<String> = Vec::new();
    let mut ctype: u8 = 0; // b's' = 共享串
    let mut is_inline = false;
    let mut in_v = false;
    let mut in_is_t = false;
    let mut cell_text = String::new();
    let mut buf = Vec::new();

    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"row" => cur_row.clear(),
                b"c" => {
                    ctype = 0;
                    is_inline = false;
                    for a in e.attributes().flatten() {
                        if a.key.local_name().as_ref() == b"t" {
                            match a.value.as_ref() {
                                b"s" => ctype = b's',
                                b"inlineStr" => is_inline = true,
                                _ => {}
                            }
                        }
                    }
                    cell_text.clear();
                }
                b"v" => in_v = true,
                b"t" if is_inline => in_is_t = true,
                _ => {}
            },
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"c" => {
                    let text = if ctype == b's' {
                        cell_text
                            .trim()
                            .parse::<usize>()
                            .ok()
                            .and_then(|i| shared.get(i))
                            .cloned()
                            .unwrap_or_default()
                    } else {
                        cell_text.clone()
                    };
                    if cur_row.len() < MAX_COLS {
                        cur_row.push(text.trim().to_string());
                    }
                    ctype = 0;
                    is_inline = false;
                    in_v = false;
                    in_is_t = false;
                    cell_text.clear();
                }
                b"row" => {
                    total_rows += 1;
                    if rows.len() < max_rows {
                        let mut r = std::mem::take(&mut cur_row);
                        while r.last().is_some_and(|s| s.is_empty()) {
                            r.pop();
                        }
                        rows.push(r);
                    } else {
                        // 读够即停：剩余行不再解析（这是大表提速的关键）。
                        break;
                    }
                }
                b"v" => in_v = false,
                b"t" => in_is_t = false,
                _ => {}
            },
            Ok(Event::Text(t)) => {
                if in_v || in_is_t {
                    if let Ok(s) = t.unescape() {
                        cell_text.push_str(&s);
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    (rows, total_rows)
}

/// 命中重算：给定实际命中词（来自 service 摘要 `<b>`），逐格 contains。
pub fn recompute_hits(wb: &mut Workbook, words: &[String]) {
    let words: Vec<String> = words
        .iter()
        .map(|w| w.trim().to_ascii_lowercase())
        .filter(|w| !w.is_empty() && w.len() <= 64)
        .collect();
    if words.is_empty() {
        return;
    }
    for sheet in &mut wb.sheets {
        let mut hits: Vec<(usize, usize)> = Vec::new();
        for (r, row) in sheet.rows.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                if cell.is_empty() {
                    continue;
                }
                let low = cell.to_ascii_lowercase();
                if words.iter().any(|w| low.contains(w.as_str())) {
                    hits.push((r, c));
                }
            }
        }
        sheet.hits = hits;
    }
    if let Some(i) = wb.sheets.iter().position(|s| !s.hits.is_empty()) {
        wb.focus_sheet = i;
    }
}

/// 列号 → Excel 列标（0 → A, 25 → Z, 26 → AA）。
pub fn col_label(mut c: usize) -> String {
    let mut s = String::new();
    loop {
        s.insert(0, (b'A' + (c % 26) as u8) as char);
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn col_labels() {
        assert_eq!(col_label(0), "A");
        assert_eq!(col_label(25), "Z");
        assert_eq!(col_label(26), "AA");
        assert_eq!(col_label(27), "AB");
    }

    #[test]
    fn row_parse_with_shared_and_number() {
        let xml = r#"<worksheet><sheetData>
            <row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c></row>
            <row r="2"><c r="A2"><v>42</v></c><c r="B2" t="inlineStr"><is><t>内联</t></is></c></row>
        </sheetData></worksheet>"#;
        // 直接测 XML 解析核心（不经 zip）
        let shared = vec!["甲乙".to_string(), "丙丁".to_string()];
        let (rows, total) = parse_rows(std::io::Cursor::new(xml.as_bytes()), &shared, MAX_ROWS);
        assert_eq!(total, 2);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], vec!["甲乙".to_string(), "丙丁".to_string()]);
        assert_eq!(rows[1], vec!["42".to_string(), "内联".to_string()]);
    }

}
