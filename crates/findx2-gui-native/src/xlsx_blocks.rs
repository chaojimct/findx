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

/// `&entity;`（quick-xml 0.42 的 `Event::GeneralRef`）解析成字符串。
fn general_ref(r: quick_xml::events::BytesRef<'_>) -> String {
    let name = r.into_inner();
    let raw = format!("&{};", name.as_ref());
    quick_xml::escape::unescape(&raw)
        .map(|c| c.into_owned())
        .unwrap_or_default()
}


/// 单页表格。
#[derive(Debug, Clone, Default)]
pub struct Sheet {
    pub name: String,
    /// 行优先的单元格文本（Arc：虚拟滚动闭包可廉价捕获）。
    pub rows: Arc<Vec<Vec<String>>>,
    /// 命中格坐标 (row, col)：Arc 共享，渲染每帧克隆 Sheet 不复制命中表。
    /// 行主序（recompute_hits 按行推入），渲染用二分查找。
    pub hits: Arc<Vec<(usize, usize)>>,
    /// 该 sheet 真实总行数（来自 XML 的 row 数，用于提示"仅显示前 N 行"）。
    pub total_rows: usize,
    /// 最大列数（解析时算一次；渲染每帧调用 cols() 时不能重扫全表）。
    pub cols: usize,
}

impl Sheet {
    pub fn cols(&self) -> usize {
        if self.cols > 0 {
            self.cols
        } else {
            self.rows.iter().map(|r| r.len()).max().unwrap_or(0)
        }
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

/// 首屏快照：只读每页前 1200 行，毫秒级返回（用于立刻出内容）。
pub fn parse_fast(path: &str) -> Result<Workbook, String> {
    parse_inner(path, &[], 1200)
}

/// 全扫：读全表并保留「前 1200 行 + 含命中词的行」，用于补齐命中与深层跳转。
/// 大表（3 万行/70MB）需要十几秒，必须放后台线程，不能阻塞首屏。
pub fn parse_full(path: &str, keep_words: &[String]) -> Result<Workbook, String> {
    parse_inner(path, keep_words, 1200)
}

/// 解析工作簿（流式）。
pub fn parse_inner(path: &str, keep_words: &[String], base_rows: usize) -> Result<Workbook, String> {
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
        // 首屏行数：小表多读（一次给全），大表少读（快出内容）。
        let max_rows = if sz > 8 * 1024 * 1024 {
            base_rows
        } else if sz > 2 * 1024 * 1024 {
            base_rows.max(3000)
        } else {
            MAX_ROWS
        };
        let (rows, total_rows) = match read_sheet_rows(&mut zip, &part, &shared, max_rows, keep_words) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if rows.is_empty() {
            continue;
        }
        let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        out.sheets.push(Sheet {
            name: name.clone(),
            rows: Arc::new(rows),
            hits: Arc::new(Vec::new()),
            total_rows,
            cols,
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
                if e.local_name().as_ref() == "sheet" {
                    for a in e.attributes().flatten() {
                        if a.key.local_name().as_ref() == "name" {
                            if let Ok(v) = a.normalized_value(quick_xml::XmlVersion::Implicit1_0) {
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

/// 读 sharedStrings → Vec<String>（按索引顺序）。
///
/// 用**字节级扫描**而不是 quick-xml 逐 event：6MB / 619 万字符的字符串表
/// 在 debug 构建下逐 event 解析要十几秒，字节扫描快一个数量级。
fn read_shared_strings(zip: &mut zip::ZipArchive<std::fs::File>) -> Vec<String> {
    let Ok(mut f) = zip.by_name("xl/sharedStrings.xml") else {
        return Vec::new();
    };
    let mut data = Vec::new();
    if f.read_to_end(&mut data).is_err() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::with_capacity(1024);
    let mut pos = 0usize;
    while let Some(si) = find_bytes(&data, pos, b"<si") {
        let si_end = find_bytes(&data, si, b"</si>").unwrap_or(data.len());
        // <si> 内可能有多段 <t>（富文本 run），全部拼接。
        let mut text = String::new();
        let mut p = si;
        while let Some(t0) = find_bytes(&data, p, b"<t") {
            if t0 >= si_end {
                break;
            }
            let Some(gt) = find_bytes(&data, t0, b">") else {
                break;
            };
            if data.get(gt.wrapping_sub(1)) == Some(&b'/') {
                p = gt + 1;
                continue;
            }
            let te = find_bytes(&data, gt + 1, b"</t>").unwrap_or(si_end);
            if let Ok(s) = std::str::from_utf8(&data[gt + 1..te.min(data.len())]) {
                text.push_str(&unescape_xml(s));
            }
            p = te + 4;
            if te >= si_end {
                break;
            }
        }
        out.push(text);
        pos = si_end + 5;
        if out.len() > 500_000 {
            break;
        }
    }
    out
}

fn find_bytes(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from >= hay.len() || needle.is_empty() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| from + p)
}

/// 最小 XML 反转义（覆盖 sharedStrings 常见实体）。
fn unescape_xml(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        if let Some(semi) = tail.find(';') {
            let ent = &tail[1..semi];
            let rep = match ent {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => {
                    if let Some(hex) = ent.strip_prefix("#x").or_else(|| ent.strip_prefix("#X")) {
                        u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
                    } else if let Some(dec) = ent.strip_prefix('#') {
                        dec.parse::<u32>().ok().and_then(char::from_u32)
                    } else {
                        None
                    }
                }
            };
            match rep {
                Some(c) => {
                    out.push(c);
                    rest = &tail[semi + 1..];
                }
                None => {
                    out.push('&');
                    rest = &tail[1..];
                }
            }
        } else {
            out.push('&');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

/// 流式读 sheet 前 MAX_ROWS 行。返回 (rows, total_rows_seen)。
fn read_sheet_rows(
    zip: &mut zip::ZipArchive<std::fs::File>,
    part: &str,
    shared: &[String],
    max_rows: usize,
    keep_words: &[String],
) -> Result<(Vec<Vec<String>>, usize), String> {
    let f = zip.by_name(part).map_err(|e| format!("缺少 {part}: {e}"))?;
    Ok(parse_rows(BufReader::new(f), shared, max_rows, keep_words))
}

/// 通用流式行解析（正式路径与单测共用，避免两套逻辑不一致）。
///
/// `keep_words`：命中词（宽匹配，用于判断"这行要不要保留"）。
/// 行保留规则：**前 max_rows 行** 或 **含命中词的行**（无论多靠后）。
/// 结果按行号稀疏填充（未保留的行是空 Vec），渲染时空行显示占位，
/// 这样 3 万行的表也能：首屏有内容 + 命中行可跳转，且不做整表驻留。
fn parse_rows<R: std::io::BufRead>(
    src: R,
    shared: &[String],
    max_rows: usize,
    keep_words: &[String],
) -> (Vec<Vec<String>>, usize) {
    let lows: Vec<String> = keep_words
        .iter()
        .map(|w| w.trim().to_ascii_lowercase())
        .filter(|w| !w.is_empty())
        .collect();
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
                "row" => cur_row.clear(),
                "c" => {
                    ctype = 0;
                    is_inline = false;
                    for a in e.attributes().flatten() {
                        if a.key.local_name().as_ref() == "t" {
                            match a.value.as_ref() {
                                "s" => ctype = b's',
                                "inlineStr" => is_inline = true,
                                _ => {}
                            }
                        }
                    }
                    cell_text.clear();
                }
                "v" => in_v = true,
                "t" if is_inline => in_is_t = true,
                _ => {}
            },
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                "c" => {
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
                "row" => {
                    let r = total_rows;
                    total_rows += 1;
                    let mut row = std::mem::take(&mut cur_row);
                    while row.last().is_some_and(|s| s.is_empty()) {
                        row.pop();
                    }
                    // 行级一次小写 + 子串比较：每行 1 次分配，避免逐格分配
                    // （3 万行 × 60 列时逐格 to_ascii_lowercase 会要 20 秒+）。
                    let is_hit = !lows.is_empty() && {
                        let mut joined = String::new();
                        for c in &row {
                            if c.is_empty() {
                                continue;
                            }
                            joined.push_str(c);
                            joined.push('\u{1}');
                        }
                        let joined_low = joined.to_ascii_lowercase();
                        lows.iter().any(|w| joined_low.contains(w.as_str()))
                    };
                    let keep = r < max_rows || is_hit;
                    if keep {
                        // 稀疏填充到行号 r
                        while rows.len() < r {
                            rows.push(Vec::new());
                        }
                        if rows.len() == r {
                            rows.push(row);
                        } else {
                            rows[r] = row;
                        }
                    } else if r >= max_rows && !is_hit {
                        // 既不在首屏、又没命中：跳过（不再累积空行，避免 3 万个空 Vec）
                    }
                }
                "v" => in_v = false,
                "t" => in_is_t = false,
                _ => {}
            },
            Ok(Event::Text(t)) => {
                if in_v || in_is_t {
                    let raw = t.into_inner();
                    if let Ok(s) = quick_xml::escape::unescape(raw.as_ref()) {
                        cell_text.push_str(&s);
                    }
                }
            }
            Ok(Event::GeneralRef(r)) => {
                if in_v || in_is_t {
                    cell_text.push_str(&general_ref(r));
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
            if row.is_empty() {
                continue;
            }
            // 行级一次小写，再逐格判断（格内用原串 contains，中文无大小写问题；
            // 英文按小写后的行判断，漏判极少，换来 10 倍速度）。
            let joined: String = {
                let mut j = String::new();
                for c in row {
                    j.push_str(c);
                    j.push('\u{1}');
                }
                j.to_ascii_lowercase()
            };
            if !words.iter().any(|w| joined.contains(w.as_str())) {
                continue;
            }
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
        sheet.hits = Arc::new(hits);
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
        let (rows, total) = parse_rows(std::io::Cursor::new(xml.as_bytes()), &shared, MAX_ROWS, &[]);
        assert_eq!(total, 2);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], vec!["甲乙".to_string(), "丙丁".to_string()]);
        assert_eq!(rows[1], vec!["42".to_string(), "内联".to_string()]);
    }

}
