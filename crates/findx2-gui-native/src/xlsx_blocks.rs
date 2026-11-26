//! xlsx 表格网格模型：calamine 读单元格 → 行列网格 → GPUI 自绘。
//!
//! 对标 AnyTXT：行号列标、sheet 页签、命中格高亮、单元格选中。
//! 不做公式计算（读缓存值），不做图表/条件格式。

use calamine::{open_workbook_auto, Data, Reader};

/// 单页表格。
#[derive(Debug, Clone, Default)]
pub struct Sheet {
    pub name: String,
    /// 行优先的单元格文本（空串 = 空格）。
    pub rows: Vec<Vec<String>>,
    /// 命中格坐标 (row, col)，1-based 显示。
    pub hits: Vec<(usize, usize)>,
}

impl Sheet {
    pub fn cols(&self) -> usize {
        self.rows.iter().map(|r| r.len()).max().unwrap_or(0)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Workbook {
    pub sheets: Vec<Sheet>,
    /// 命中所在 sheet 索引（默认 0）。
    pub focus_sheet: usize,
}

const MAX_ROWS: usize = 500;
const MAX_COLS: usize = 60;

/// 读工作簿。失败返回 Err（调用方降级文字摘录）。
pub fn parse(path: &str, needles: &[String]) -> Result<Workbook, String> {
    let mut wb = open_workbook_auto(path).map_err(|e| format!("xlsx 打开失败: {e}"))?;
    let names = wb.sheet_names().to_vec();
    if names.is_empty() {
        return Err("工作簿没有工作表".into());
    }
    let mut out = Workbook::default();
    for name in names.iter().take(20) {
        let range = match wb.worksheet_range(name) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let mut rows: Vec<Vec<String>> = Vec::new();
        let mut hits: Vec<(usize, usize)> = Vec::new();
        for (r, row) in range.rows().enumerate().take(MAX_ROWS) {
            if r + 1 > MAX_ROWS {
                break;
            }
            let mut cells: Vec<String> = Vec::new();
            for (c, cell) in row.iter().enumerate().take(MAX_COLS) {
                let text = cell_text(cell);
                if !text.is_empty() && !needles.is_empty() {
                    let low = text.to_ascii_lowercase();
                    if needles.iter().any(|n| {
                        !n.is_empty()
                            && (low.contains(&n.to_ascii_lowercase())
                                || crate::docx_blocks::match_hits(&text, n).iter().next().is_some())
                    }) {
                        hits.push((r, c));
                    }
                }
                cells.push(text);
            }
            // 去掉行尾空格
            while cells.last().is_some_and(|s| s.is_empty()) {
                cells.pop();
            }
            if !cells.is_empty() {
                rows.push(cells);
            } else if !rows.is_empty() {
                rows.push(Vec::new()); // 保留空行占位
            }
        }
        // 去掉尾部空行
        while rows.last().is_some_and(|r| r.is_empty()) {
            rows.pop();
        }
        if rows.is_empty() {
            continue;
        }
        if !hits.is_empty() && out.focus_sheet == 0 {
            out.focus_sheet = out.sheets.len();
        }
        out.sheets.push(Sheet {
            name: name.clone(),
            rows,
            hits,
        });
    }
    if out.sheets.is_empty() {
        return Err("工作簿没有可显示的单元格".into());
    }
    Ok(out)
}

fn cell_text(cell: &Data) -> String {
    match cell {
        Data::Empty => String::new(),
        Data::String(s) => s.trim().to_string(),
        Data::Float(f) => {
            if f.fract() == 0.0 && f.abs() < 1e15 {
                format!("{}", *f as i64)
            } else {
                format!("{f}")
            }
        }
        Data::Int(i) => i.to_string(),
        Data::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        Data::DateTime(dt) => dt.to_string(),
        Data::DateTimeIso(s) => s.clone(),
        Data::DurationIso(s) => s.clone(),
        Data::Error(e) => format!("#{e:?}"),
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
}
