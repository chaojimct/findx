//! 流式富文本渲染器：docx / md / pptx 共用（块模型 → GPUI 元素树）。
//!
//! 设计要点：
//! - 段落级样式：对齐、左缩进、首行缩进、段前后距、行距、标题字号。
//! - run 级样式：粗体/斜体/下划线/颜色/命中底色（GPUI `HighlightStyle` 全支持）。
//! - 命中高亮：调用方给 `plain` 的字节区间，这里按块映射到段内偏移。
//! - 表格：行×列网格，边框 + 单元格内段落。
//!
//! 不支持：per-run 字号（GPUI HighlightStyle 无 font_size）、分页、浮动图。

use std::ops::Range;

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::docx_blocks::{Align, DocxBlock, DocxDoc, StyledParagraph, StyledRun, TableBlock};
use crate::theme::FindxTokens;

/// 正文基准字号（px）。Word 默认 12pt ≈ 16px（96 DPI 逻辑像素）。
const BASE_PX: f32 = 16.0;
/// 渲染块数上限（GPUI 元素树过大影响布局性能；超出提示截断）。
const MAX_RENDER_BLOCKS: usize = 600;

/// 标题字号（px），按级别 1..=6。
fn heading_px(level: u8) -> f32 {
    match level {
        1 => BASE_PX * 1.9,
        2 => BASE_PX * 1.55,
        3 => BASE_PX * 1.3,
        4 => BASE_PX * 1.15,
        5 => BASE_PX * 1.05,
        _ => BASE_PX,
    }
}

/// 渲染整篇文档。`hit_ranges` 是 `doc.plain` 的字节区间（来自 match_spans）。
pub fn render_docx(
    doc: &DocxDoc,
    hit_ranges: &[Range<usize>],
    tokens: &FindxTokens,
) -> AnyElement {
    let mut container = v_flex().w_full().gap_0();
    let mut block_off = 0usize; // 当前块在 plain 中的起点
    let mut rendered = 0usize;

    for (i, block) in doc.blocks.iter().enumerate() {
        if rendered >= MAX_RENDER_BLOCKS {
            container = container.child(
                div()
                    .px_3()
                    .py_2()
                    .text_sm()
                    .text_color(tokens.text_secondary)
                    .child(format!(
                        "（已渲染前 {MAX_RENDER_BLOCKS} 块，后续省略；可切换文字摘录查看全文）"
                    )),
            );
            break;
        }
        let start = doc.block_starts.get(i).copied().unwrap_or(block_off);
        match block {
            DocxBlock::Paragraph(p) => {
                container = container.child(render_paragraph(p, start, hit_ranges, tokens));
                block_off = start + p.text().len() + 1;
            }
            DocxBlock::Table(t) => {
                container = container.child(render_table(t, start, hit_ranges, tokens));
                // 表格 plain 占位：每行 cells 用 \t 分隔 + \n
                let mut n = 0usize;
                for row in &t.rows {
                    for cell in row {
                        for p in &cell.paragraphs {
                            n += p.text().len() + 1;
                        }
                    }
                    n += 1;
                }
                block_off = start + n;
            }
        }
        rendered += 1;
    }

    container.into_any_element()
}

fn render_paragraph(
    p: &StyledParagraph,
    plain_start: usize,
    hits: &[Range<usize>],
    tokens: &FindxTokens,
) -> AnyElement {
    let text = p.text();
    if text.trim().is_empty() {
        // 空段保留一点垂直间距（Word 里的空行）。
        let h = (BASE_PX * 0.6).max(4.0);
        return div().h(px(h)).w_full().into_any_element();
    }

    let is_heading = p.heading.is_some();
    let font_px = match p.heading {
        Some(lv) => heading_px(lv),
        None => {
            // 段内最大字号（pt→px），用于正文里的大字强调。
            let max_pt = p
                .runs
                .iter()
                .map(|r| r.size_pt)
                .fold(0.0f32, f32::max);
            if max_pt > 0.0 {
                (max_pt * 4.0 / 3.0).clamp(BASE_PX * 0.75, BASE_PX * 2.2)
            } else {
                BASE_PX
            }
        }
    };

    // run 级样式 + 命中区间，统一转成相对本段的字节区间。
    let mut spans: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    let mut cursor = 0usize;
    for run in &p.runs {
        let len = run.text.len();
        if len == 0 {
            continue;
        }
        let run_range = cursor..cursor + len;
        // run 自身样式（整段区间）
        let run_style = run_highlight_style(run, tokens);
        if run_style.font_weight.is_some()
            || run_style.font_style.is_some()
            || run_style.underline.is_some()
            || run_style.color.is_some()
        {
            spans.push((run_range.clone(), run_style));
        }
        cursor += len;
    }

    // 命中区间（plain 绝对坐标 → 段内相对坐标）
    for h in hits {
        let rel_start = h.start.saturating_sub(plain_start);
        let rel_end = h.end.saturating_sub(plain_start);
        if rel_end <= rel_start || rel_start >= text.len() {
            continue;
        }
        let a = rel_start.min(text.len());
        let b = rel_end.min(text.len());
        if !text.is_char_boundary(a) || !text.is_char_boundary(b) {
            continue;
        }
        spans.push((
            a..b,
            HighlightStyle {
                background_color: Some(tokens.hit_bg),
                color: Some(tokens.hit_fg),
                ..Default::default()
            },
        ));
    }

    let styled = StyledText::new(text).with_highlights(spans);
    let mut el = div()
        .w_full()
        .text_size(px(font_px))
        .line_height(px(font_px * p.line_spacing.max(1.0) * 1.5))
        .text_color(tokens.text_primary)
        .child(styled);

    if is_heading {
        el = el.font_weight(FontWeight::BOLD);
    }
    if p.indent_left > 0.0 {
        el = el.pl(px(p.indent_left * 4.0 / 3.0));
    }
    if p.indent_first.abs() > 0.01 {
        // 首行缩进：GPUI 无 text-indent，用左侧 padding 近似（悬挂缩进不处理）。
        let v = p.indent_first * 4.0 / 3.0;
        if v > 0.0 {
            el = el.pl(px(p.indent_left * 4.0 / 3.0 + v));
        }
    }
    if p.space_before > 0.0 {
        el = el.pt(px(p.space_before * 4.0 / 3.0));
    }
    if p.space_after > 0.0 {
        el = el.pb(px(p.space_after * 4.0 / 3.0));
    }
    if let Some(level) = p.list_level {
        el = el.pl(px(16.0 + level as f32 * 18.0));
    }
    match p.align {
        Align::Center => el = el.text_center(),
        Align::Right => el = el.text_right(),
        Align::Both | Align::Left => {}
    }
    el.into_any_element()
}

fn run_highlight_style(run: &StyledRun, tokens: &FindxTokens) -> HighlightStyle {
    let mut style = HighlightStyle::default();
    if run.bold {
        style.font_weight = Some(FontWeight::BOLD);
    }
    if run.italic {
        style.font_style = Some(FontStyle::Italic);
    }
    if run.underline {
        style.underline = Some(UnderlineStyle {
            color: None,
            thickness: px(1.0),
            wavy: false,
        });
    }
    if let Some(rgb) = run.color {
        let r = ((rgb >> 16) & 0xFF) as f32 / 255.0;
        let g = ((rgb >> 8) & 0xFF) as f32 / 255.0;
        let b = (rgb & 0xFF) as f32 / 255.0;
        style.color = Some(gpui_kit::hsla(r, g, b, 1.0));
    }
    let _ = tokens;
    style
}

fn render_table(
    t: &TableBlock,
    plain_start: usize,
    hits: &[Range<usize>],
    tokens: &FindxTokens,
) -> AnyElement {
    let mut table = v_flex().w_full().border_1().border_color(tokens.border);
    let mut off = plain_start;
    for row in &t.rows {
        let mut row_el = h_flex().w_full();
        for cell in row {
            let mut cell_el = div()
                .flex_1()
                .min_w_0()
                .px_2()
                .py_1()
                .border_r_1()
                .border_b_1()
                .border_color(tokens.border_soft);
            if cell.paragraphs.is_empty() {
                cell_el = cell_el.child(div().h(px(BASE_PX)));
            } else {
                for p in &cell.paragraphs {
                    let el = render_paragraph(p, off, hits, tokens);
                    off += p.text().len() + 1;
                    cell_el = cell_el.child(el);
                }
            }
            row_el = row_el.child(cell_el);
        }
        table = table.child(row_el);
        off += 1; // 行末 \n
    }
    table.into_any_element()
}
