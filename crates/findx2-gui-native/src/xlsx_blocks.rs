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
    /// 超大 sheet（原始数据等）：只保留名字，不解析内容（加载保护）。
    pub oversized: bool,
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

const MAX_ROWS: usize = 300;
const MAX_COLS: usize = 60;

/// 读工作簿。失败返回 Err（调用方降级文字摘录）。
///
/// 注意：这里**不做命中检测**。逐格做子串/拼音匹配会让加载慢一个数量级
/// （3 万格 × N 词 × 逐字拼音）。命中由 `recompute_hits` 在拿到实际命中词后
/// 一次性计算（纯 `contains`，不碰拼音）。
pub fn parse(path: &str) -> Result<Workbook, String> {
    let mut wb = open_workbook_auto(path).map_err(|e| format!("xlsx 打开失败: {e}"))?;
    let names = wb.sheet_names().to_vec();
    if names.is_empty() {
        return Err("工作簿没有工作表".into());
    }
    // 预先取各 sheet 的未压缩大小：原始数据 sheet 常有数万行/几十 MB，
    // calamine 会整表加载，单个就能拖到 6s+。超过阈值只保留名字不解析。
    const MAX_SHEET_BYTES: usize = 4 * 1024 * 1024;
    // 单独开一次 zip 读各 sheet 的未压缩大小（calamine 不暴露）。
    // sheet_names() 的顺序与 xl/worksheets/sheetN.xml 的顺序一致（Excel/WPS 都如此），
    // 这里按序号映射；取不到就当 0（不跳过，行为与之前一致）。
    let sizes: Vec<usize> = (|| {
        let f = std::fs::File::open(path).ok()?;
        let mut z = zip::ZipArchive::new(f).ok()?;
        Some(
            (1..=names.len())
                .map(|i| {
                    let p = format!("xl/worksheets/sheet{i}.xml");
                    z.by_name(&p).map(|f| f.size() as usize).unwrap_or(0)
                })
                .collect::<Vec<_>>(),
        )
    })()
    .unwrap_or_default();

    let mut out = Workbook::default();
    for (idx, name) in names.iter().take(20).enumerate() {
        if sizes.get(idx).copied().unwrap_or(0) > MAX_SHEET_BYTES {
            out.sheets.push(Sheet {
                name: name.clone(),
                rows: Vec::new(),
                hits: Vec::new(),
                oversized: true,
            });
            continue;
        }
        let range = match wb.worksheet_range(name) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let mut rows: Vec<Vec<String>> = Vec::new();
        for (r, row) in range.rows().enumerate().take(MAX_ROWS) {
            if r + 1 > MAX_ROWS {
                break;
            }
            let mut cells: Vec<String> = Vec::new();
            for (c, cell) in row.iter().enumerate().take(MAX_COLS) {
                cells.push(cell_text(cell));
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
        out.sheets.push(Sheet {
            name: name.clone(),
            rows,
            hits: Vec::new(),
            oversized: false,
        });
    }
    if out.sheets.is_empty() {
        return Err("工作簿没有可显示的单元格".into());
    }
    Ok(out)
}

/// 命中重算：给定**实际命中词**（来自 service 的摘要高亮），逐格做 contains。
/// 不碰拼音，纯 ASCII 小写比较，3 万格约几毫秒。
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
    // 默认聚焦第一个有命中的 sheet。
    if let Some(i) = wb.sheets.iter().position(|s| !s.hits.is_empty()) {
        wb.focus_sheet = i;
    }
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
