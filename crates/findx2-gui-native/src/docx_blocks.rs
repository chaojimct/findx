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

impl DocxDoc {
    /// plain 字节偏移落在第几个块（命中→块 的滚动定位用）。
    /// 返回 Some(block_index)；越界返回最后一个块。
    pub fn block_of_offset(&self, off: usize) -> Option<usize> {
        if self.blocks.is_empty() {
            return None;
        }
        match self.block_starts.binary_search(&off) {
            Ok(i) => Some(i),
            Err(0) => Some(0),
            Err(i) => Some(i - 1),
        }
    }
}

const MAX_BLOCKS: usize = 4000;
const MAX_PLAIN: usize = 1 << 20; // 1 MiB 纯文本上限

/// 解析 docx。失败返回 Err（调用方降级文字摘录）。
pub fn parse(path: &str) -> Result<DocxDoc, String> {
    let doc = Document::open(path).map_err(|e| format!("docx 解析失败: {e}"))?;
    parse_document(&doc)
}

/// 从字节解析 docx（供老 .doc 转 docx 后复用同一套块模型）。
pub fn parse_bytes(raw: &[u8]) -> Result<DocxDoc, String> {
    let doc = Document::from_bytes(raw).map_err(|e| format!("docx 解析失败: {e}"))?;
    parse_document(&doc)
}

/// 老式 .doc（Word 97-2003）：rwml 纯 Rust 解析 → 直接把 `DocModel` 映射成
/// 我们的块模型（段落、run 样式、表格、标题、列表），不再绕 `to_docx()` 重新
/// 打包 OOXML 再解一遍（省 ~0.5s / 十几 MB 中间分配）。
pub fn parse_legacy_doc(path: &str) -> Result<DocxDoc, String> {
    let t0 = std::time::Instant::now();
    let raw = std::fs::read(path).map_err(|e| e.to_string())?;
    let legacy = rwml::Document::open(&raw).map_err(|e| format!("doc 解析失败: {e}"))?;
    let model = legacy.model();

    let mut out = DocxDoc::default();
    let mut plain = String::new();
    for block in &model.blocks {
        if out.blocks.len() >= MAX_BLOCKS || plain.len() >= MAX_PLAIN {
            out.truncated = true;
            break;
        }
        match block {
            rwml::Block::Paragraph(p) => {
                let para = convert_rwml_paragraph(p);
                if para.is_empty() {
                    continue;
                }
                out.block_starts.push(plain.len());
                plain.push_str(&para.text());
                plain.push('\n');
                out.blocks.push(DocxBlock::Paragraph(para));
            }
            rwml::Block::Table(t) => {
                let table = convert_rwml_table(t);
                if table.rows.is_empty() {
                    continue;
                }
                out.block_starts.push(plain.len());
                for row in &table.rows {
                    for cell in row {
                        for p in &cell.paragraphs {
                            plain.push_str(&p.text());
                            plain.push(' ');
                        }
                    }
                    plain.push('\n');
                }
                out.blocks.push(DocxBlock::Table(table));
            }
            // 图片/图表暂不渲染；分页/分节忽略（连续视图）。
            rwml::Block::Image(_) | rwml::Block::Chart(_) => {}
            rwml::Block::PageBreak | rwml::Block::SectionBreak(_) => {}
        }
    }
    if out.blocks.is_empty() {
        return Err("doc 没有可显示的文本".into());
    }
    out.plain = plain;
    {
        use std::io::Write as _;
        let _ = writeln!(
            std::io::stderr(),
            "[doc] {} blocks={} plain={}B in {:?}",
            path,
            out.blocks.len(),
            out.plain.len(),
            t0.elapsed()
        );
    }
    Ok(out)
}

/// rwml 段落 → 我们自己的段落模型（run 样式、对齐、标题、列表、间距）。
fn convert_rwml_paragraph(p: &rwml::Paragraph) -> StyledParagraph {
    let mut sp = StyledParagraph {
        align: match p.props.align {
            rwml::Align::Left => Align::Left,
            rwml::Align::Center => Align::Center,
            rwml::Align::Right => Align::Right,
            rwml::Align::Justify => Align::Both,
        },
        heading: p.props.heading_level.map(|l| l.clamp(1, 6)),
        ..Default::default()
    };
    if let Some(list) = p.props.list.as_ref() {
        sp.list_level = Some(list.level.min(8));
        sp.list_ordered = list.ordered;
    }
    if let Some(v) = p.props.spacing.before_pt {
        sp.space_before = v.max(0.0);
    }
    if let Some(v) = p.props.spacing.after_pt {
        sp.space_after = v.max(0.0);
    }
    if let Some(v) = p.props.spacing.line_pct {
        sp.line_spacing = v;
    }
    if let Some(v) = p.props.indent.left_pt {
        sp.indent_left = v;
    }
    if let Some(v) = p.props.indent.first_line_pt {
        sp.indent_first = v;
    }
    sp.runs = p
        .runs
        .iter()
        .filter(|r| !r.text.is_empty())
        .map(|r| StyledRun {
            text: r.text.clone(),
            bold: r.props.bold,
            italic: r.props.italic,
            underline: r.props.underline,
            size_pt: r
                .props
                .size_half_pt
                .map(|h| h as f32 / 2.0)
                .unwrap_or(0.0),
            color: r
                .props
                .color
                .as_ref()
                .map(|c| ((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32),
        })
        .collect();
    sp
}

fn convert_rwml_table(t: &rwml::Table) -> TableBlock {
    let mut rows = Vec::new();
    for row in &t.rows {
        let mut cells = Vec::new();
        for cell in &row.cells {
            let paragraphs: Vec<StyledParagraph> = cell
                .blocks
                .iter()
                .filter_map(|b| match b {
                    rwml::Block::Paragraph(p) => Some(convert_rwml_paragraph(p)),
                    _ => None,
                })
                .collect();
            cells.push(TableCell {
                paragraphs,
                colspan: cell.col_span.max(1) as usize,
            });
        }
        rows.push(cells);
    }
    TableBlock {
        rows,
        col_weights: t.col_widths_pct.clone(),
    }
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

// ---------------------------------------------------------------------------
// 命中匹配（docx 侧自算，不依赖 service 的提取器）
// ---------------------------------------------------------------------------

/// 从查询串里取 `content:` / `utf8content:` 的**查询词列表**。
/// `content:"马春天 我与青松" glob:0` → ["马春天", "我与青松"]
/// `content:青松` → ["青松"]
pub fn content_needles_of(query: &str) -> Vec<String> {
    let lower = query.to_ascii_lowercase();
    for key in ["content:", "utf8content:"] {
        if let Some(i) = lower.find(key) {
            let rest = &query[i + key.len()..];
            let v = rest.trim_start();
            let inner: String = if let Some(stripped) = v.strip_prefix('"') {
                stripped.chars().take_while(|&c| c != '"').collect()
            } else {
                v.chars().take_while(|c| !c.is_whitespace()).collect()
            };
            let mut out: Vec<String> = inner
                .split_whitespace()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty() && s.len() <= 64 && !s.contains(':'))
                .collect();
            if out.is_empty() {
                let t = inner.trim();
                if !t.is_empty() {
                    out.push(t.to_string());
                }
            }
            return out;
        }
    }
    let q = query.trim();
    if q.is_empty() {
        Vec::new()
    } else {
        vec![q.to_string()]
    }
}

/// 多词命中：每个查询词分别匹配后合并（与 service 的 AND 语义一致）。
/// 用户输入 `马春天 我与青松` → 两个词各自在文中定位，
/// 而不是拿整串「马春天 我与青松」当子串找（那样永远不命中）。
pub fn match_hits_multi(text: &str, needles: &[String]) -> Vec<std::ops::Range<usize>> {
    let mut all: Vec<std::ops::Range<usize>> = Vec::new();
    for n in needles {
        all.extend(match_hits(text, n));
        if all.len() >= 4000 {
            break;
        }
    }
    all.sort_by_key(|r| r.start);
    let mut merged: Vec<std::ops::Range<usize>> = Vec::with_capacity(all.len());
    for r in all {
        if let Some(last) = merged.last_mut() {
            if r.start <= last.end {
                last.end = last.end.max(r.end);
                continue;
            }
        }
        merged.push(r);
    }
    merged.truncate(2000);
    merged
}

/// 在文本里查查询词的命中区间：先字面（ASCII 小写），失败再走拼音对齐。
/// 为什么要自算：service 用 `document_preview::xml_text`、文档侧用 rdocx，
/// 两套提取器的空白/表格顺序不同，跨提取器按字节定位会失败。
pub fn match_hits(text: &str, needle: &str) -> Vec<std::ops::Range<usize>> {
    let needle = needle.trim();
    if needle.is_empty() || text.is_empty() {
        return Vec::new();
    }
    // 1) 字面（大小写不敏感；中文大小写无影响）。
    let low = text.to_ascii_lowercase();
    let nl = needle.to_ascii_lowercase();
    let mut out: Vec<std::ops::Range<usize>> = Vec::new();
    let mut from = 0usize;
    while let Some(p) = low[from..].find(&nl) {
        let a = from + p;
        let b = a + nl.len();
        if text.is_char_boundary(a) && text.is_char_boundary(b) {
            out.push(a..b);
        }
        from = b.max(a + 1);
        if out.len() >= 2000 {
            break;
        }
    }
    if !out.is_empty() {
        return out;
    }
    // 2) 拼音对齐（仅纯 ASCII 查询，如 `machuntian` → 马春天）。
    if needle.is_ascii() && needle.len() >= 2 {
        return pinyin_hits(text, &nl);
    }
    out
}

/// 逐字拼音对齐：`machuntian` → `马春天`、`mct` → `马春天`（首字母）。
/// 逐字尝试「全拼」或「首字母」两种消费方式，命中连续区间。
fn pinyin_hits(text: &str, query: &str) -> Vec<std::ops::Range<usize>> {
    use pinyin::ToPinyin;
    const MAX_SCAN: usize = 2 << 20; // 2 MiB 扫描上限（超长文档只标前面）
    struct Item {
        start: usize,
        end: usize,
        full: String,
        first: u8,
    }
    let mut seq: Vec<Item> = Vec::new();
    let scan_end = text.len().min(MAX_SCAN);
    for (i, ch) in text[..scan_end].char_indices() {
        if let Some(py) = ch.to_pinyin() {
            let full = py.plain();
            let first = full.as_bytes().first().copied().unwrap_or(b' ');
            seq.push(Item {
                start: i,
                end: i + ch.len_utf8(),
                full: full.to_string(),
                first,
            });
        }
    }
    if seq.is_empty() {
        return Vec::new();
    }
    let qb = query.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < seq.len() {
        let mut qi = 0usize;
        let mut j = i;
        while j < seq.len() && qi < qb.len() {
            let it = &seq[j];
            if qb[qi..].starts_with(it.full.as_bytes()) {
                qi += it.full.len();
                j += 1;
                continue;
            }
            if qb[qi] == it.first {
                qi += 1;
                j += 1;
                continue;
            }
            break;
        }
        if qi == qb.len() && j > i {
            out.push(seq[i].start..seq[j - 1].end);
            if out.len() >= 2000 {
                break;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
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

    #[test]
    fn literal_hits_case_insensitive() {
        let t = "Hello 马春天 World 马春天";
        let hits = match_hits(t, "马春天");
        assert_eq!(hits.len(), 2);
        assert_eq!(&t[hits[0].clone()], "马春天");
    }

    #[test]
    fn pinyin_hits_full_and_initials() {
        let t = "我与青松——马春天采访文稿";
        // 全拼
        let hits = match_hits(t, "machuntian");
        assert_eq!(hits.len(), 1, "全拼应命中马春天");
        assert_eq!(&t[hits[0].clone()], "马春天");
        // 首字母
        let hits2 = match_hits(t, "mct");
        assert_eq!(hits2.len(), 1, "首字母应命中马春天");
        assert_eq!(&t[hits2[0].clone()], "马春天");
    }

    #[test]
    fn needles_from_query_forms() {
        // 全文模式：search_logic 会把多词包成引号
        assert_eq!(
            content_needles_of("content:\"马春天 我与青松\" glob:0"),
            vec!["马春天".to_string(), "我与青松".to_string()]
        );
        // 单词
        assert_eq!(
            content_needles_of("content:青松 glob:0"),
            vec!["青松".to_string()]
        );
        // utf8content
        assert_eq!(
            content_needles_of("utf8content:马春天"),
            vec!["马春天".to_string()]
        );
        // 无 content: 前缀 → 整串
        assert_eq!(
            content_needles_of("马春天 我与青松"),
            vec!["马春天 我与青松".to_string()]
        );
    }

    #[test]
    fn multi_needles_merge() {
        let t = "我与青松——马春天文案。开头：轻松工作，马春天加入青松。";
        let hits = match_hits_multi(t, &["马春天".into(), "青松".into()]);
        // 马春天×2 + 青松×2
        assert!(hits.len() >= 4, "got {}", hits.len());
        // 区间不重叠且有序
        for w in hits.windows(2) {
            assert!(w[0].end <= w[1].start);
        }
    }

    #[test]
    fn parse_legacy_doc_smoke_env() {
        let Ok(path) = std::env::var("FINDX_DOC_SMOKE") else {
            return;
        };
        let doc = parse_legacy_doc(&path).expect("parse_legacy_doc");
        assert!(!doc.blocks.is_empty());
        assert!(!doc.plain.trim().is_empty());
    }

    #[test]
    fn block_of_offset_maps_back() {
        let mut d = DocxDoc::default();
        d.plain = "aaa\nbbb\nccc\n".into();
        d.block_starts = vec![0, 4, 8];
        d.blocks = vec![
            DocxBlock::Paragraph(StyledParagraph::default()),
            DocxBlock::Paragraph(StyledParagraph::default()),
            DocxBlock::Paragraph(StyledParagraph::default()),
        ];
        assert_eq!(d.block_of_offset(0), Some(0));
        assert_eq!(d.block_of_offset(5), Some(1));
        assert_eq!(d.block_of_offset(9), Some(2));
    }
}
