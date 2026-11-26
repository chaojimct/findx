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

/// 渲染整篇文档（`focus` 为 None）或**以某个命中块为中心的窗口**。
///
/// `focus_block` 定向渲染：只输出该块前后各 `WINDOW_BEFORE` / `WINDOW_AFTER` 块，
/// 命中总能落在可见区第一屏——比依赖 GPUI 滚动 API 可靠（实测 `set_offset`
/// 会被内部滚动状态覆盖）。点击列表行/翻页只需切换 focus 并重绘。
/// `hit_ranges` 是 `doc.plain` 的字节区间（来自 match_spans）。
pub fn render_docx(
    doc: &DocxDoc,
    hit_ranges: &[Range<usize>],
    focus_block: Option<usize>,
    tokens: &FindxTokens,
) -> Vec<AnyElement> {
    const WINDOW_BEFORE: usize = 12;
    const WINDOW_AFTER: usize = 80;

    let total = doc.blocks.len();
    let (lo, hi) = match focus_block {
        Some(f) if total > WINDOW_BEFORE + WINDOW_AFTER => {
            let lo = f.saturating_sub(WINDOW_BEFORE);
            (lo, (lo + WINDOW_BEFORE + WINDOW_AFTER).min(total))
        }
        _ => (0, total.min(MAX_RENDER_BLOCKS)),
    };
    let truncated_head = lo > 0;
    let truncated_tail = hi < total;

    let mut out: Vec<AnyElement> = Vec::with_capacity(hi - lo + 2);
    if truncated_head {
        out.push(
            div()
                .px_3()
                .py_2()
                .text_xs()
                .text_color(tokens.text_tertiary)
                .child(format!("（前面还有 {lo} 段，已按命中定位）"))
                .into_any_element(),
        );
    }

    for i in lo..hi {
        let block = &doc.blocks[i];
        let start = doc.block_starts.get(i).copied().unwrap_or(0);
        match block {
            DocxBlock::Paragraph(p) => {
                out.push(render_paragraph(p, start, hit_ranges, tokens));
            }
            DocxBlock::Table(t) => {
                out.push(render_table(t, start, hit_ranges, tokens));
            }
        }
    }

    if truncated_tail {
        out.push(
            div()
                .px_3()
                .py_2()
                .text_xs()
                .text_color(tokens.text_tertiary)
                .child(format!("（后面还有 {} 段）", total - hi))
                .into_any_element(),
        );
    } else if total > MAX_RENDER_BLOCKS && focus_block.is_none() {
        out.push(
            div()
                .px_3()
                .py_2()
                .text_sm()
                .text_color(tokens.text_secondary)
                .child(format!(
                    "（已渲染前 {MAX_RENDER_BLOCKS} 段；点命中列表可定位到后面）"
                ))
                .into_any_element(),
        );
    }

    out
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

    // 命中区间转到段内坐标（只保留落在本段、边界合法的）。
    let hit_ranges: Vec<Range<usize>> = hits
        .iter()
        .filter_map(|h| {
            let a = h.start.saturating_sub(plain_start);
            let b = h.end.saturating_sub(plain_start);
            if b > a
                && b <= text.len()
                && text.is_char_boundary(a)
                && text.is_char_boundary(b)
            {
                Some(a..b)
            } else {
                None
            }
        })
        .collect();

    // 关键：GPUI 的 compute_runs 按顺序消费 highlights，**假设严格有序且不重叠**。
    // 若把「run 整段样式」和「命中局部样式」直接堆在一起，区间重叠会让它把
    // ix 回退、总长度算错（文字重复、高亮丢失）。这里按 run 边界切分，
    // 命中样式覆盖 run 样式，保证输出有序不重叠。
    let mut spans: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    let mut cursor = 0usize;
    for run in &p.runs {
        let len = run.text.len();
        if len == 0 {
            continue;
        }
        let run_start = cursor;
        let run_end = cursor + len;
        cursor = run_end;
        let base = run_highlight_style(run, tokens);
        let mut cuts = vec![run_start, run_end];
        for h in &hit_ranges {
            let a = h.start.max(run_start);
            let b = h.end.min(run_end);
            if b > a {
                cuts.push(a);
                cuts.push(b);
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        for w in cuts.windows(2) {
            let (a, b) = (w[0], w[1]);
            if b <= a {
                continue;
            }
            let mut st = base.clone();
            if hit_ranges.iter().any(|h| h.start <= a && h.end >= b) {
                st.background_color = Some(tokens.hit_bg);
                st.color = Some(tokens.hit_fg);
            }
            if st.font_weight.is_some()
                || st.font_style.is_some()
                || st.underline.is_some()
                || st.color.is_some()
                || st.background_color.is_some()
            {
                spans.push((a..b, st));
            }
        }
    }

    if !hits.is_empty() && plain_start < 200 {
        use std::io::Write as _;
        let _ = writeln!(
            std::io::stderr(),
            "[render] start={plain_start} text_len={} hits={} spans={:?} heading={is_heading}",
            text.len(),
            hits.len(),
            spans.iter().map(|(r, _)| (r.start, r.end)).collect::<Vec<_>>()
        );
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
