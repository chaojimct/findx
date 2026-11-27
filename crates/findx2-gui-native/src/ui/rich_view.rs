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

use gpui_kit::component::scroll::ScrollableElement as _;
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

// ---------------------------------------------------------------------------
// pptx 幻灯片渲染（按页分组 + 段落 + 命中高亮）
// ---------------------------------------------------------------------------

/// 渲染 pptx：每页一个标题条 + 段落流。命中按 para_starts 映射到段内。
pub fn render_pptx(
    deck: &crate::pptx_blocks::Deck,
    hits: &[Range<usize>],
    tokens: &FindxTokens,
) -> Vec<AnyElement> {
    let mut out: Vec<AnyElement> = Vec::with_capacity(deck.slides.len() * 4);
    let mut para_idx = 0usize;
    for slide in &deck.slides {
        out.push(
            div()
                .w_full()
                .pt(px(10.))
                .pb(px(4.))
                .border_b_1()
                .border_color(tokens.border_soft)
                .text_xs()
                .text_color(tokens.text_tertiary)
                .child(format!("第 {} 页", slide.number))
                .into_any_element(),
        );
        for para in &slide.paragraphs {
            let start = deck.para_starts.get(para_idx).copied().unwrap_or(0);
            para_idx += 1;
            let spans = simple_spans(para, start, hits, tokens);
            // 首段当作标题（PowerPoint 习惯），字号略大。
            let is_title = para_idx == 1 || slide.paragraphs.first().is_some_and(|f| std::ptr::eq(f, para));
            let size = if is_title {
                BASE_PX * 1.25
            } else {
                BASE_PX
            };
            out.push(
                div()
                    .w_full()
                    .py(px(2.))
                    .text_size(px(size))
                    .line_height(px(size * 1.55))
                    .when(is_title, |d| d.font_weight(FontWeight::BOLD))
                    .text_color(tokens.text_primary)
                    .child(StyledText::new(para.clone()).with_highlights(spans))
                    .into_any_element(),
            );
        }
    }
    out
}

// ---------------------------------------------------------------------------
// xlsx 表格网格渲染（对标 AnyTXT：行列标 + 命中格高亮）
// ---------------------------------------------------------------------------

/// 单元格宽度 / 行号列宽度（px）。
const CELL_W: f32 = 112.0;
const GUTTER_W: f32 = 46.0;

/// 渲染当前 sheet 网格：列标行（固定）+ 虚拟滚动数据区。
/// 数据区用 `uniform_list`，只渲染可见行——上万行 sheet 也不卡。
/// `cx` 用于构造行渲染闭包（闭包只捕获 `Arc<Sheet>`，无逐帧深拷贝）。
pub fn render_sheet(
    book: &crate::xlsx_blocks::Workbook,
    tokens: &FindxTokens,
    selected: Option<(usize, usize)>,
    scroll: &gpui_kit::UniformListScrollHandle,
    hscroll: &gpui_kit::ScrollHandle,
) -> AnyElement {
    let Some(sheet) = book.sheets.get(book.focus_sheet) else {
        return div().into_any_element();
    };
    let cols = sheet.cols().max(1);
    let row_h = 22.0;

    // 列标行（固定不滚动）
    let mut head = h_flex().w(px(GUTTER_W + CELL_W * cols as f32)).flex_shrink_0();
    head = head.child(
        div()
            .w(px(GUTTER_W))
            .h(px(24.))
            .flex_shrink_0()
            .border_r_1()
            .border_b_1()
            .border_color(tokens.border)
            .bg(tokens.surface_raised),
    );
    for c in 0..cols {
        head = head.child(
            div()
                .w(px(CELL_W))
                .h(px(24.))
                .flex_shrink_0()
                .px_1()
                .flex()
                .items_center()
                .border_r_1()
                .border_b_1()
                .border_color(tokens.border_soft)
                .bg(tokens.surface_raised)
                .text_xs()
                .text_color(tokens.text_tertiary)
                .child(crate::xlsx_blocks::col_label(c)),
        );
    }

    let row_count = sheet.rows.len();
    let sheet_arc = std::sync::Arc::new(sheet.clone());
    let hits: std::sync::Arc<Vec<(usize, usize)>> = std::sync::Arc::new(sheet.hits.clone());
    let tokens_copy = *tokens;
    let processor =
        move |range: std::ops::Range<usize>,
              _window: &mut gpui_kit::Window,
              _cx: &mut gpui_kit::App|
              -> Vec<AnyElement> {
            let mut out: Vec<AnyElement> = Vec::with_capacity(range.len());
            for r in range {
                let row = sheet_arc.rows.get(r);
                let row_empty = row.is_none_or(|x| x.is_empty());
                let mut row_el = h_flex()
                    .w(px(GUTTER_W + CELL_W * cols as f32))
                    .h(px(row_h))
                    .flex_shrink_0();
                row_el = row_el.child(
                    div()
                        .w(px(GUTTER_W))
                        .h(px(row_h))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .border_r_1()
                        .border_b_1()
                        .border_color(tokens_copy.border_soft)
                        .bg(tokens_copy.surface_raised)
                        .text_xs()
                        .text_color(tokens_copy.text_tertiary)
                        .child(format!("{}", r + 1)),
                );
                for c in 0..cols {
                    let text = if row_empty && c == 0 {
                        "…（未加载行）"
                    } else {
                        row.and_then(|x| x.get(c)).map(|s| s.as_str()).unwrap_or("")
                    };
                    let is_hit = hits.contains(&(r, c));
                    let is_sel = selected == Some((r, c));
                    let mut cell = div()
                        .w(px(CELL_W))
                        .h(px(row_h))
                        .flex_shrink_0()
                        .px_1()
                        .flex()
                        .items_center()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .border_r_1()
                        .border_b_1()
                        .border_color(tokens_copy.border_soft)
                        .text_xs()
                        .text_color(tokens_copy.text_primary);
                    if is_hit {
                        cell = cell.bg(tokens_copy.hit_bg).text_color(tokens_copy.hit_fg);
                    }
                    if is_sel {
                        // 选中格：主题强调色描边 + 淡底（AnyTXT 的蓝框效果）
                        cell = cell
                            .border_2()
                            .border_color(tokens_copy.accent)
                            .bg(tokens_copy.row_selected);
                    }
                    row_el = row_el.child(cell.child(text.to_string()));
                }
                out.push(row_el.into_any_element());
            }
            out
        };

    let total_w = GUTTER_W + CELL_W * cols as f32;
    let list = uniform_list("sheet-rows", row_count, processor)
        .track_scroll(scroll)
        .w(px(total_w))
        .flex_1()
        .min_h_0();

    // 表格区：同一块 host 里叠三层（都相对 host 定位）：
    //   1) 横向滚动容器（表头 + 数据行一起左右滚）
    //   2) 横向滚动条（必须在滚动容器之外，否则会随内容一起滚走）
    //   3) 纵向滚动条（同样在滚动容器之外；top(24) 与表头同高，
    //      这样 Scrollbar::viewport_from_layout 拿到的视口正好是数据区）
    div()
        .relative()
        .w_full()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .child(
            div()
                .id("sheet-hscroll")
                .size_full()
                .track_scroll(hscroll)
                .overflow_x_scroll()
                .restrict_scroll_to_axis()
                .child(
                    v_flex()
                        .w(px(total_w))
                        .h_full()
                        .min_h_0()
                        .child(head)
                        .child(list),
                ),
        )
        .child(
            div()
                .absolute()
                .left(px(0.))
                .right(px(0.))
                .bottom(px(0.))
                .h(px(16.))
                .horizontal_scrollbar(hscroll),
        )
        .child(
            div()
                .absolute()
                .top(px(24.))
                .right(px(0.))
                .bottom(px(0.))
                .w(px(16.))
                .vertical_scrollbar(scroll),
        )
        .into_any_element()
}

/// sheet 页签行（点击切换由调用方用 Button 包裹）。
pub fn sheet_tabs(wb: &crate::xlsx_blocks::Workbook, tokens: &FindxTokens) -> AnyElement {
    let mut tabs = h_flex().w_full().flex_shrink_0().gap(px(2.)).px_2().py(px(3.));
    for (i, sh) in wb.sheets.iter().enumerate() {
        let active = i == wb.focus_sheet;
        let label = if sh.hits.is_empty() {
            sh.name.clone()
        } else {
            format!("{} ({})", sh.name, sh.hits.len())
        };
        tabs = tabs.child(
            div()
                .px_2()
                .py(px(2.))
                .rounded_sm()
                .text_xs()
                .when(active, |d| {
                    d.bg(tokens.row_selected)
                        .text_color(tokens.text_primary)
                        .font_weight(FontWeight::BOLD)
                })
                .when(!active, |d| d.text_color(tokens.text_secondary))
                .child(label),
        );
    }
    tabs.into_any_element()
}

// ---------------------------------------------------------------------------
// md 渲染（与 docx 共用版式与命中映射）
// ---------------------------------------------------------------------------

/// 渲染 md 块模型。命中区间是 `doc.plain` 的字节坐标，按块映射到段内。
pub fn render_md(
    doc: &crate::markdown_blocks::MdDoc,
    hit_ranges: &[Range<usize>],
    tokens: &FindxTokens,
) -> Vec<AnyElement> {
    use crate::markdown_blocks::MdBlock;
    let mut out: Vec<AnyElement> = Vec::with_capacity(doc.blocks.len());
    for (i, block) in doc.blocks.iter().enumerate() {
        let start = doc.block_starts.get(i).copied().unwrap_or(0);
        let el: AnyElement = match block {
            MdBlock::Heading { level, text } => {
                let px_size = heading_px(*level);
                let spans = simple_spans(text, start, hit_ranges, tokens);
                div()
                    .w_full()
                    .pt(px(10.))
                    .pb(px(4.))
                    .text_size(px(px_size))
                    .line_height(px(px_size * 1.45))
                    .font_weight(FontWeight::BOLD)
                    .text_color(tokens.text_primary)
                    .child(StyledText::new(text.clone()).with_highlights(spans))
                    .into_any_element()
            }
            MdBlock::Paragraph { text } => {
                let spans = simple_spans(text, start, hit_ranges, tokens);
                div()
                    .w_full()
                    .py(px(3.))
                    .text_size(px(BASE_PX))
                    .line_height(px(BASE_PX * 1.6))
                    .text_color(tokens.text_primary)
                    .child(StyledText::new(text.clone()).with_highlights(spans))
                    .into_any_element()
            }
            MdBlock::Code { lang, text } => {
                let spans = simple_spans(text, start, hit_ranges, tokens);
                let mut body = v_flex()
                    .w_full()
                    .my(px(4.))
                    .px_3()
                    .py_2()
                    .rounded_sm()
                    .bg(tokens.surface_raised)
                    .border_1()
                    .border_color(tokens.border_soft);
                if !lang.is_empty() {
                    body = body.child(
                        div()
                            .text_xs()
                            .text_color(tokens.text_tertiary)
                            .pb(px(2.))
                            .child(lang.clone()),
                    );
                }
                body.child(
                    div()
                        .text_size(px(BASE_PX * 0.9))
                        .line_height(px(BASE_PX * 1.45))
                        .font_family("Consolas")
                        .text_color(tokens.text_primary)
                        .child(StyledText::new(text.clone()).with_highlights(spans)),
                )
                .into_any_element()
            }
            MdBlock::List { ordered, items } => {
                let mut list = v_flex().w_full().py(px(2.)).gap(px(2.));
                let mut off = start;
                for (n, item) in items.iter().enumerate() {
                    let marker = if *ordered {
                        format!("{}.", n + 1)
                    } else {
                        "•".to_string()
                    };
                    let spans = simple_spans(item, off, hit_ranges, tokens);
                    list = list.child(
                        h_flex()
                            .w_full()
                            .items_start()
                            .gap(px(6.))
                            .pl(px(8.))
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .w(px(20.))
                                    .text_size(px(BASE_PX))
                                    .text_color(tokens.text_tertiary)
                                    .child(marker),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(BASE_PX))
                                    .line_height(px(BASE_PX * 1.55))
                                    .text_color(tokens.text_primary)
                                    .child(StyledText::new(item.clone()).with_highlights(spans)),
                            ),
                    );
                    off += item.len() + 1;
                }
                list.into_any_element()
            }
            MdBlock::Table { headers, rows } => {
                let mut table = v_flex().w_full().my(px(6.)).border_1().border_color(tokens.border);
                let mut off = start;
                let mut head = h_flex().w_full();
                for hcell in headers {
                    let spans = simple_spans(hcell, off, hit_ranges, tokens);
                    head = head.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .px_2()
                            .py_1()
                            .border_r_1()
                            .border_b_1()
                            .border_color(tokens.border_soft)
                            .bg(tokens.surface_raised)
                            .text_size(px(BASE_PX * 0.92))
                            .font_weight(FontWeight::BOLD)
                            .child(StyledText::new(hcell.clone()).with_highlights(spans)),
                    );
                    off += hcell.len() + 1;
                }
                table = table.child(head);
                for row in rows {
                    let mut r = h_flex().w_full();
                    for cell in row {
                        let spans = simple_spans(cell, off, hit_ranges, tokens);
                        r = r.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .px_2()
                                .py_1()
                                .border_r_1()
                                .border_b_1()
                                .border_color(tokens.border_soft)
                                .text_size(px(BASE_PX * 0.92))
                                .child(StyledText::new(cell.clone()).with_highlights(spans)),
                        );
                        off += cell.len() + 1;
                    }
                    table = table.child(r);
                }
                table.into_any_element()
            }
            MdBlock::Rule => div()
                .w_full()
                .my(px(8.))
                .h(px(1.))
                .bg(tokens.border_soft)
                .into_any_element(),
        };
        out.push(el);
    }
    out
}

/// 单段纯文本 + 命中高亮（md 没有 run 级样式，只需命中覆盖）。
fn simple_spans(
    text: &str,
    plain_start: usize,
    hits: &[Range<usize>],
    tokens: &FindxTokens,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut spans = Vec::new();
    for h in hits {
        let a = h.start.saturating_sub(plain_start);
        let b = h.end.saturating_sub(plain_start);
        if b > a && b <= text.len() && text.is_char_boundary(a) && text.is_char_boundary(b) {
            spans.push((
                a..b,
                HighlightStyle {
                    background_color: Some(tokens.hit_bg),
                    color: Some(tokens.hit_fg),
                    ..Default::default()
                },
            ));
        }
    }
    spans
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
