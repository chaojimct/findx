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

use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode, ScrollableElement as _};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::docx_blocks::{Align, DocxBlock, DocxDoc, StyledParagraph, StyledRun, TableBlock};
use crate::theme::FindxTokens;

/// 正文基准字号（px）。Word 默认 12pt ≈ 16px（96 DPI 逻辑像素）。
const BASE_PX: f32 = 16.0;
/// 正常文档一次渲染整篇；超过这个块数才退回“按命中窗口渲染”。
/// 之前固定 600 块 + 永远按焦点窗口渲染，导致打开 docx 永远是
/// 「（前面还有 N 段，已按命中定位）」的半篇视图。
pub const DOCX_FULL_BLOCKS: usize = 5000;

/// 窗口模式下，命中块之后渲染多少块。
const WINDOW_AFTER: usize = 80;

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
/// 渲染 docx 的单个块（虚拟列表用：ListState 按需调用，不一次铺全文）。
pub fn render_docx_block(
    doc: &DocxDoc,
    index: usize,
    hit_ranges: &[Range<usize>],
    tokens: &FindxTokens,
) -> AnyElement {
    let Some(block) = doc.blocks.get(index) else {
        return div().into_any_element();
    };
    let start = doc.block_starts.get(index).copied().unwrap_or(0);
    match block {
        DocxBlock::Paragraph(p) => render_paragraph(p, start, hit_ranges, tokens),
        DocxBlock::Table(t) => render_table(t, start, hit_ranges, tokens),
    }
}

pub fn render_docx(
    doc: &DocxDoc,
    hit_ranges: &[Range<usize>],
    focus_block: Option<usize>,
    tokens: &FindxTokens,
) -> Vec<AnyElement> {
    // 命中窗口上方的上下文行数：只留 2 块，保证「窗口顶部」就能看到命中
    // （原来是 12 块，长段落时命中会被挤到第一屏之外）。
    const WINDOW_BEFORE: usize = 2;

    let total = doc.blocks.len();
    // 普通文档：整篇渲染（和 Word/预览器一样从第一段开始，不再半篇 + banner）。
    // 超过 DOCX_FULL_BLOCKS 的巨型文档才退到「命中窗口」，避免一次铺太多元素。
    let (lo, hi) = if total <= DOCX_FULL_BLOCKS {
        (0, total)
    } else if let Some(f) = focus_block {
        let lo = f.saturating_sub(WINDOW_BEFORE);
        (lo, (lo + WINDOW_BEFORE + WINDOW_AFTER).min(total))
    } else {
        (0, DOCX_FULL_BLOCKS)
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
        out.push(render_docx_block(doc, i, hit_ranges, tokens));
    }

    if truncated_tail {
        out.push(
            div()
                .px_3()
                .py_2()
                .text_xs()
                .text_color(tokens.text_tertiary)
                .child(format!(
                    "（后面还有 {} 段；点命中列表可定位到后面）",
                    total - hi
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
pub const SHEET_CELL_W: f32 = 112.0;
pub const SHEET_GUTTER_W: f32 = 46.0;
const CELL_W: f32 = SHEET_CELL_W;
const GUTTER_W: f32 = SHEET_GUTTER_W;

/// 渲染当前 sheet 网格：列标行（固定）+ 虚拟滚动数据区。
/// 数据区用 `uniform_list`，只渲染可见行——上万行 sheet 也不卡。
/// `cx` 用于构造行渲染闭包（闭包只捕获 `Arc<Sheet>`，无逐帧深拷贝）。
/// `hit_words`：当前查询的命中词，用于给命中单元格做**文字级**高亮。
pub fn render_sheet(
    book: &crate::xlsx_blocks::Workbook,
    tokens: &FindxTokens,
    selected: Option<(usize, usize)>,
    scroll: &gpui_kit::UniformListScrollHandle,
    hscroll: &gpui_kit::ScrollHandle,
    hit_words: &[String],
    popup_close: Option<std::rc::Rc<dyn Fn(&mut gpui_kit::App)>>,
    popup_max_h: f32,
    on_select_cell: Option<std::rc::Rc<dyn Fn(usize, usize, &mut gpui_kit::App)>>,
) -> AnyElement {
    let Some(sheet) = book.sheets.get(book.focus_sheet) else {
        return div().into_any_element();
    };
    let cols = sheet.cols().max(1);
    let row_h = 22.0;
    // 横向偏移：行号列用 -hx 反向补偿，视觉上固定在最左侧。
    let hx = hscroll.offset().x.as_f32();

    // 列标行（固定不滚动）
    let mut head = h_flex()
        .relative()
        .w(px(GUTTER_W + CELL_W * cols as f32))
        .flex_shrink_0();
    head = head.child(div().w(px(GUTTER_W)).h(px(24.)).flex_shrink_0());
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
    // 左上角：固定在视口最左侧（横向滚动时补偿 hx）。
    head = head.child(
        div()
            .absolute()
            .left(px(-hx))
            .top(px(0.))
            .w(px(GUTTER_W))
            .h(px(24.))
            .border_r_1()
            .border_b_1()
            .border_color(tokens.border)
            .bg(tokens.surface_raised),
    );

    let row_count = sheet.rows.len();
    // Sheet 内部 rows/hits 都是 Arc，clone 只加引用计数（每帧不复制大表）。
    let sheet_arc = std::sync::Arc::new(sheet.clone());
    let words: std::sync::Arc<Vec<String>> = std::sync::Arc::new(hit_words.to_vec());
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
                    .relative()
                    .w(px(GUTTER_W + CELL_W * cols as f32))
                    .h(px(row_h))
                    .flex_shrink_0();
                row_el = row_el.child(div().w(px(GUTTER_W)).h(px(row_h)).flex_shrink_0());
                for c in 0..cols {
                    let text = if row_empty && c == 0 {
                        "…（未加载行）"
                    } else {
                        row.and_then(|x| x.get(c)).map(|s| s.as_str()).unwrap_or("")
                    };
                    // hits 行主序 → 二分查找（O(log n)，无每帧 HashSet 分配）。
                    let is_hit = sheet_arc.hits.binary_search(&(r, c)).is_ok();
                    let is_sel = selected == Some((r, c));
                    let mut cell = div()
                        .id(SharedString::from(format!("sheet-cell-{r}-{c}")))
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
                    // 直接点单元格也能选中（显示高亮 + 展开浮层）。
                    if let Some(cb) = on_select_cell.as_ref() {
                        let cb = cb.clone();
                        cell = cell
                            .cursor_pointer()
                            .on_click(move |_, _, cx| cb(r, c, cx));
                    }
                    if is_sel {
                        // 选中格：主题强调色描边 + 淡底（AnyTXT 的蓝框效果）
                        cell = cell
                            .border_2()
                            .border_color(tokens_copy.accent)
                            .bg(tokens_copy.row_selected);
                    }
                    // 命中：只高亮**单元格内匹配到的文字**（不再是整格铺底色）。
                    let content: AnyElement = if is_hit && !text.is_empty() {
                        let ranges = crate::docx_blocks::match_hits_multi(text, &words);
                        if ranges.is_empty() {
                            text.to_string().into_any_element()
                        } else {
                            StyledText::new(text.to_string())
                                .with_highlights(ranges.into_iter().map(|range| {
                                    (
                                        range,
                                        HighlightStyle {
                                            color: Some(tokens_copy.hit_fg),
                                            background_color: Some(tokens_copy.hit_bg),
                                            ..Default::default()
                                        },
                                    )
                                }))
                                .into_any_element()
                        }
                    } else {
                        text.to_string().into_any_element()
                    };
                    row_el = row_el.child(cell.child(content));
                }
                // 行号列固定在左侧：横向滚动时用 -hx 抵消（覆盖在单元格之上）。
                row_el = row_el.child(
                    div()
                        .absolute()
                        .left(px(-hx))
                        .top(px(0.))
                        .w(px(GUTTER_W))
                        .h(px(row_h))
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
                // 选中格的展开浮层：挂在行内 → 位置/列宽天然对齐；
                // deferred 让它最后绘制（覆盖下面的行），并盖住原单元格。
                if let Some((sr, sc)) = selected {
                    if sr == r {
                        if let Some(cell_text) = row
                            .and_then(|x| x.get(sc))
                            .map(|s| s.trim())
                            .filter(|s| !s.is_empty())
                        {
                            let ranges =
                                crate::docx_blocks::match_hits_multi(cell_text, &words);
                            let addr = format!(
                                "{}{}",
                                crate::xlsx_blocks::col_label(sc),
                                r + 1
                            );
                            let cb = popup_close.clone();
                            let body = StyledText::new(cell_text.to_string())
                                .with_highlights(ranges.into_iter().map(|range| {
                                    (
                                        range,
                                        HighlightStyle {
                                            color: Some(gpui_kit::rgb(0x111827).into()),
                                            background_color: Some(
                                                gpui_kit::rgb(0xFFE58A).into(),
                                            ),
                                            ..Default::default()
                                        },
                                    )
                                }));
                            let panel = div()
                                .w(px(CELL_W))
                                .max_h(px(popup_max_h.max(row_h * 3.0)))
                                .flex()
                                .flex_col()
                                .overflow_hidden()
                                .rounded_sm()
                                .border_2()
                                .border_color(tokens_copy.accent)
                                .bg(gpui_kit::rgb(0xFFFFFF))
                                .text_xs()
                                .text_color(gpui_kit::rgb(0x1F2937))
                                .child(
                                    h_flex()
                                        .h(px(row_h))
                                        .flex_shrink_0()
                                        .items_center()
                                        .gap(px(4.))
                                        .px_1()
                                        .bg(gpui_kit::rgb(0xF3F4F6))
                                        .border_b_1()
                                        .border_color(gpui_kit::rgb(0xD1D5DB))
                                        .text_color(gpui_kit::rgb(0x374151))
                                        .child(div().flex_1().min_w_0().child(addr))
                                        .child(
                                            div()
                                                .id(SharedString::from(format!(
                                                    "sheet-popup-close-{r}-{sc}"
                                                )))
                                                .px_1()
                                                .rounded_sm()
                                                .cursor_pointer()
                                                .hover(|d| d.bg(gpui_kit::rgb(0xE5E7EB)))
                                                .child("×")
                                                .on_click(move |_, _, cx| {
                                                    if let Some(cb) = &cb {
                                                        cb(cx);
                                                    }
                                                }),
                                        ),
                                )
                                .child(
                                    div()
                                        .id(SharedString::from(format!(
                                            "sheet-popup-body-{r}-{sc}"
                                        )))
                                        .flex_shrink_0()
                                        .max_h(px((popup_max_h - row_h).max(row_h)))
                                        .overflow_y_scroll()
                                        .px_1()
                                        .py(px(2.))
                                        .child(body),
                                )
                                .into_any_element();
                            row_el = row_el.child(
                                div()
                                    .absolute()
                                    .left(px(GUTTER_W + CELL_W * sc as f32))
                                    .top(px(0.))
                                    .w(px(CELL_W))
                                    .child(gpui_kit::deferred(panel)),
                            );
                        }
                    }
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
                .h(px(12.))
                .child(
                    // 用 Scrollbar 组件 + viewport_from_layout：与文本预览同一套，
                    // 内置 .horizontal_scrollbar() 在嵌套虚拟列表里拿不到真实视口。
                    Scrollbar::horizontal(hscroll)
                        .viewport_from_layout()
                        .mode(ScrollbarMode::Always)
                        .styles(|styles| {
                            styles
                                .track(|style| style.bg(tokens.border_soft))
                                .thumb(|style| style.bg(tokens.text_tertiary))
                                .thumb_hover(|style| style.bg(tokens.text_secondary))
                                .thumb_active(|style| style.bg(tokens.accent))
                        }),
                ),
        )
        .child(
            div()
                .absolute()
                .top(px(24.))
                .right(px(0.))
                .bottom(px(12.))
                .w(px(12.))
                .child(
                    Scrollbar::vertical(scroll)
                        .viewport_from_layout()
                        .mode(ScrollbarMode::Always)
                        .styles(|styles| {
                            styles
                                .track(|style| style.bg(tokens.border_soft))
                                .thumb(|style| style.bg(tokens.text_tertiary))
                                .thumb_hover(|style| style.bg(tokens.text_secondary))
                                .thumb_active(|style| style.bg(tokens.accent))
                        }),
                ),
        )
        .into_any_element()
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
