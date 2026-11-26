//! docx 富文本块模型（管线 A'）：rdocx 解析 → 统一块模型 → GPUI 流式渲染。
//!
//! 与 md（`markdown_blocks`）共用同一套渲染器：块类型、run 样式、对齐、缩进。
//! 零外部进程：不启动 Word，不转 PDF，不转图——纯 Rust 解析，进程内自绘。
//!
//! 还原度边界（实测口径）：标题/段落/粗体/斜体/下划线/字号/颜色/对齐/缩进/
//! 段前后距/表格/列表 ✓；分页、浮动图、文本框、复杂样式继承 ✗。

use rdocx::Document;

/// 段落对齐（Word `w:jc`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
    Both,
}

/// 单个文本 run 的样式。
#[derive(Debug, Clone, PartialEq)]
pub struct StyledRun {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    /// 字号（pt）。Word 内部用半磅，这里换算成 pt。
    pub size_pt: f32,
    /// 文本颜色（RGB），None = 主题默认。
    pub color: Option<u32>,
}

impl StyledRun {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            bold: false,
            italic: false,
            underline: false,
            size_pt: 0.0,
            color: None,
        }
    }
}

/// 表格单元格（内部是段落列表，支持多段）。
#[derive(Debug, Clone, Default)]
pub struct TableCell {
    pub paragraphs: Vec<StyledParagraph>,
    /// 跨列数（gridSpan），1 = 不跨。
    pub colspan: usize,
}

/// 表格（行列 + 列宽比例）。
#[derive(Debug, Clone, Default)]
pub struct TableBlock {
    pub rows: Vec<Vec<TableCell>>,
    /// 每列相对宽度（按首行或网格列数归一），空 = 均分。
    pub col_weights: Vec<f32>,
}

/// 段落（run 列表 + 版式属性）。
#[derive(Debug, Clone, Default)]
pub struct StyledParagraph {
    pub runs: Vec<StyledRun>,
    pub align: Align,
    /// 左缩进（pt）。
    pub indent_left: f32,
    /// 首行缩进（pt，负值为悬挂缩进）。
    pub indent_first: f32,
    /// 段前距（pt）。
    pub space_before: f32,
    /// 段后距（pt）。
    pub space_after: f32,
    /// 行距倍数（1.0 = 单倍）；0 = 默认。
    pub line_spacing: f32,
    /// 列表层级（Some = 列表项，0 起）。
    pub list_level: Option<u8>,
    /// 列表是否有序（Word 编号 vs 项目符号）。
    pub list_ordered: bool,
    /// 标题级别（1..=6，Some = 标题）。
    pub heading: Option<u8>,
}

impl StyledParagraph {
    /// 段落纯文本（用于命中区间计算与空段判断）。
    pub fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
    pub fn is_empty(&self) -> bool {
        self.runs.iter().all(|r| r.text.trim().is_empty())
    }
}

/// 文档块（段落或表格，按文档顺序）。
#[derive(Debug, Clone)]
pub enum DocxBlock {
    Paragraph(StyledParagraph),
    Table(TableBlock),
}

/// 解析后的 docx 文档。
#[derive(Debug, Clone, Default)]
pub struct DocxDoc {
    pub blocks: Vec<DocxBlock>,
    /// 全文纯文本（块之间用 `\n` 分隔，供命中区间映射）。
    pub plain: String,
    /// 每块在 `plain` 中的字节起点。
    pub block_starts: Vec<usize>,
    pub truncated: bool,
}

const MAX_BLOCKS: usize = 4000;
const MAX_PLAIN: usize = 1 << 20; // 1 MiB 纯文本上限

/// 解析 docx。失败返回 Err（调用方降级文字摘录）。
pub fn parse(path: &str) -> Result<DocxDoc, String> {
    let doc = Document::open(path).map_err(|e| format!("docx 解析失败: {e}"))?;
    parse_document(&doc)
}

fn parse_document(doc: &Document) -> Result<DocxDoc, String> {
    let mut out = DocxDoc::default();
    let mut plain = String::new();
    for item in doc.body_items() {
        if out.blocks.len() >= MAX_BLOCKS || plain.len() >= MAX_PLAIN {
            out.truncated = true;
            break;
        }
        match item {
            rdocx::BodyItemRef::Paragraph(p) => {
                let para = convert_paragraph(doc, &p);
                if para.is_empty() {
                    continue;
                }
                out.block_starts.push(plain.len());
                plain.push_str(&para.text());
                plain.push('\n');
                out.blocks.push(DocxBlock::Paragraph(para));
            }
            rdocx::BodyItemRef::Table(t) => {
                let table = convert_table(doc, &t);
                if table.rows.is_empty() {
                    continue;
                }
                out.block_starts.push(plain.len());
                for row in &table.rows {
                    for cell in row {
                        for p in &cell.paragraphs {
                            plain.push_str(&p.text());
                            plain.push('\t');
                        }
                    }
                    plain.push('\n');
                }
                out.blocks.push(DocxBlock::Table(table));
            }
            _ => {}
        }
    }
    out.plain = plain;
    Ok(out)
}

fn convert_paragraph(doc: &Document, p: &rdocx::ParagraphRef<'_>) -> StyledParagraph {
    let ppr = doc.effective_paragraph_properties(p);
    let mut para = StyledParagraph {
        align: convert_align(ppr.jc),
        indent_left: twips_to_pt(ppr.ind_left),
        indent_first: twips_to_pt(ppr.ind_first_line),
        space_before: twips_to_pt(ppr.space_before),
        space_after: twips_to_pt(ppr.space_after),
        line_spacing: line_spacing_factor(ppr.line_spacing, ppr.line_rule.as_deref()),
        heading: heading_level(ppr.outline_lvl, ppr.style_id.as_deref()),
        list_level: ppr.num_ilvl.map(|v| v as u8),
        list_ordered: false, // 由 num_id 是否命中编号定义决定；先按无序展示
        ..Default::default()
    };
    for r in p.runs() {
        let rpr = doc.effective_run_properties(p, &r);
        let text = r.text();
        if text.is_empty() {
            continue;
        }
        para.runs.push(StyledRun {
            text,
            bold: rpr.bold.unwrap_or(false),
            italic: rpr.italic.unwrap_or(false),
            underline: rpr.underline.is_some(),
            size_pt: rpr
                .sz
                .as_ref()
                .map(|h| h.0 as f32 / 2.0)
                .unwrap_or(0.0),
            color: rpr.color.as_deref().and_then(parse_hex_color),
        });
    }
    para
}

fn convert_table(doc: &Document, t: &rdocx::TableRef<'_>) -> TableBlock {
    let rows_n = t.row_count();
    let cols_n = t.column_count();
    let mut rows = Vec::with_capacity(rows_n.min(200));
    for r in 0..rows_n.min(200) {
        let Some(row) = t.row(r) else { continue };
        let mut cells = Vec::with_capacity(cols_n);
        for c in 0..cols_n {
            let Some(cell) = row.cell(c) else { continue };
            let mut paragraphs = Vec::new();
            for cp in cell.paragraphs() {
                let para = convert_paragraph(doc, &cp);
                if !para.is_empty() {
                    paragraphs.push(para);
                }
            }
            cells.push(TableCell {
                paragraphs,
                colspan: 1,
            });
        }
        if !cells.is_empty() {
            rows.push(cells);
        }
    }
    TableBlock {
        rows,
        col_weights: Vec::new(),
    }
}

fn convert_align(jc: Option<rdocx_oxml::ST_Jc>) -> Align {
    match jc {
        Some(rdocx_oxml::ST_Jc::Center) => Align::Center,
        Some(rdocx_oxml::ST_Jc::Right) | Some(rdocx_oxml::ST_Jc::End) => Align::Right,
        Some(rdocx_oxml::ST_Jc::Both) | Some(rdocx_oxml::ST_Jc::Distribute) => Align::Both,
        _ => Align::Left,
    }
}

/// twips（1/20 pt）→ pt。
fn twips_to_pt(v: Option<rdocx_oxml::Twips>) -> f32 {
    v.map(|t| t.0 as f32 / 20.0).unwrap_or(0.0)
}

/// 行距：`line_rule=auto` 时 twips/240 = 倍数；exact/atLeast 时按 pt 处理。
fn line_spacing_factor(spacing: Option<rdocx_oxml::Twips>, rule: Option<&str>) -> f32 {
    let Some(s) = spacing else { return 0.0 };
    match rule {
        Some("auto") | None => {
            let f = s.0 as f32 / 240.0;
            if (0.8..=3.0).contains(&f) { f } else { 0.0 }
        }
        _ => 0.0,
    }
}

/// 标题级别：优先大纲级别（outlineLvl 0..8 → 1..9），其次样式名 Heading N。
fn heading_level(outline: Option<u32>, style_id: Option<&str>) -> Option<u8> {
    if let Some(l) = outline {
        if l < 9 {
            return Some((l + 1) as u8);
        }
    }
    let s = style_id?;
    let lower = s.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("heading") {
        let digits: String = rest.chars().filter(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = digits.parse::<u8>() {
            if (1..=6).contains(&n) {
                return Some(n);
            }
        }
    }
    None
}

fn parse_hex_color(s: &str) -> Option<u32> {
    let s = s.trim_start_matches('#');
    if s.len() == 6 {
        u32::from_str_radix(s, 16).ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_and_align_mapping() {
        assert_eq!(heading_level(Some(0), None), Some(1));
        assert_eq!(heading_level(None, Some("Heading2")), Some(2));
        assert_eq!(heading_level(None, Some("Normal")), None);
        assert_eq!(convert_align(Some(rdocx_oxml::ST_Jc::Center)), Align::Center);
        assert_eq!(convert_align(Some(rdocx_oxml::ST_Jc::Both)), Align::Both);
    }

    #[test]
    fn twips_and_color() {
        assert_eq!(twips_to_pt(None), 0.0);
        assert_eq!(parse_hex_color("FF0000"), Some(0xFF0000));
        assert_eq!(parse_hex_color("#00ff00"), Some(0x00FF00));
        assert_eq!(parse_hex_color("bad"), None);
    }

    #[test]
    fn paragraph_text_join() {
        let p = StyledParagraph {
            runs: vec![StyledRun::plain("abc"), StyledRun::plain("def")],
            ..Default::default()
        };
        assert_eq!(p.text(), "abcdef");
    }
}
