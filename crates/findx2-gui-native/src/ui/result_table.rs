//! Virtualized Kit table: shared header/body geometry, resize handles and two scrollbars.
use super::main_window::MainWindow;
use crate::search_logic::{format_size, format_time, highlight_segments, snippet_segments};
use findx2_ipc::SearchHitDto;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::component::table::{Column, TableDelegate, TableState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::PathBuf;

pub(super) const DEFAULT_WIDTHS: [f32; 4] = [280., 360., 90., 164.];
pub(super) const MIN_WIDTHS: [f32; 4] = [160., 160., 72., 150.];

pub(super) struct ResultsTable {
    pub hits: Vec<SearchHitDto>,
    pub widths: [f32; 4],
    pub more: bool,
    /// 设置页“命中摘要”开关（sync_table 每次从设置刷新；scoped 表恒 false）。
    pub show_snippet: bool,
    pub owner: WeakEntity<MainWindow>,
}
impl TableDelegate for ResultsTable {
    fn render_tr(
        &mut self,
        row: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let owner = self.owner.clone();
        let right_owner = owner.clone();
        let up_owner = owner.clone();
        let press_owner = owner.clone();
        let painted_scope = owner.read_with(cx, |view, _| view.row_paint_scope(true)).ok().flatten();
        let selected = owner
            .read_with(cx, |view, cx| view.context_row_selected(true, row, cx))
            .unwrap_or(false);
        let press_dir = self
            .hits
            .get(row)
            .filter(|h| h.is_directory)
            .map(|h| PathBuf::from(h.path.clone()));
        let tokens = *cx.global::<crate::theme::FindxTokens>();
        div()
            .id(("row", row))
            // Selection is a full-row fill, never an accent underline.
            .border_b_1()
            .border_color(tokens.border_soft)
            .when(selected, |v| v.bg(tokens.row_selected))
            .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                let _ = press_owner.update(cx, |view, cx| {
                    if !crate::ui::async_publication::painted_event_current(painted_scope, view.row_paint_scope(true)) {
                        cx.stop_propagation(); return;
                    }
                    view.selection_pointer(true, row, event.modifiers, cx);
                    if !event.modifiers.control && !event.modifiers.shift {
                        view.press_row = Some((true, row, event.position));
                    }
                    if let Some(dir) = press_dir.clone() {
                        view.queue_prefetch(dir, cx);
                    }
                });
            })
            .on_mouse_up(MouseButton::Left, move |event, _, cx| {
                let _ = up_owner.update(cx, |view, cx| {
                    view.collapse_press_selection(true, row, event.modifiers, event.position, cx);
                });
            })
            .on_mouse_down(MouseButton::Right, move |_, _, cx| {
                let valid=right_owner.read_with(cx,|view,_| crate::ui::async_publication::painted_event_current(painted_scope,view.row_paint_scope(true))).unwrap_or(false);
                if !valid { cx.stop_propagation(); }
            })
    }
    fn context_menu(
        &mut self,
        row: usize,
        menu: gpui_kit::component::menu::PopupMenu,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> gpui_kit::component::menu::PopupMenu {
        let owner = self.owner.clone();
        let table = cx.entity();
        cx.defer(move |cx| table.update(cx, |t, cx| t.set_selected_row(row, cx)));
        owner
            .update(cx, |view, cx| {
                view.prepare_context_row(true, row, cx);
                view.file_context_menu(menu, false, window, cx)
            })
            .unwrap_or_else(|_| panic!("menu owner closed during event"))
    }
    fn columns_count(&self, _: &App) -> usize {
        4
    }
    fn rows_count(&self, _: &App) -> usize {
        self.hits.len()
    }
    fn column(&self, ix: usize, _: &App) -> Column {
        let (key, label) = [
            ("name", "名称"),
            ("path", "路径"),
            ("size", "大小"),
            ("mtime", "修改时间"),
        ][ix];
        let mut column = Column::new(key, label)
            .width(px(self.widths[ix]))
            .min_width(px(MIN_WIDTHS[ix]))
            .max_width(px(1600.))
            .movable(false)
            .resizable(true);
        column.align = if ix == 2 {
            TextAlign::Right
        } else {
            TextAlign::Left
        };
        column
    }
    fn cell_label(&self, row: usize, col: usize, _: &App) -> Option<SharedString> {
        let hit = self.hits.get(row)?;
        Some(
            match col {
                0 => hit.name.clone(),
                1 => hit.path.clone(),
                2 => format_size(hit.size, hit.is_directory),
                _ => format_time(hit.mtime),
            }
            .into(),
        )
    }
    fn render_td(
        &mut self,
        row: usize,
        col: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(hit) = self.hits.get(row) else {
            return div().into_any_element();
        };
        let tokens = *cx.global::<crate::theme::FindxTokens>();
        if col == 0 {
            let full_path = hit.path.clone();
            // tooltip 放完整摘要（行内只显示省略片段，悬停读全）。
            let tip_text: SharedString = if hit.snippet.is_empty() {
                full_path.clone().into()
            } else {
                let plain: String = snippet_segments(&hit.snippet)
                    .into_iter()
                    .map(|(t, _)| t)
                    .collect::<Vec<_>>()
                    .join("");
                format!("{full_path}\n{plain}").into()
            };
            let menu_open = self
                .owner
                .read_with(cx, |view, _| view.file_menu_is_open())
                .unwrap_or(false);
            return div()
                .id(("search-name-cell", row))
                .when(!menu_open, |view| {
                    view.tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(tip_text.clone())
                            .build(window, cx)
                    })
                })
                .flex()
                .items_center()
                .w_full()
                .h_full()
                .min_w_0()
                .overflow_hidden()
                .child(super::file_icon::file_icon(
                    &hit.name,
                    hit.is_directory,
                    tokens,
                ))
                .child(
                    v_flex()
                        .ml_2()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .justify_center()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .w_full()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .children(
                                    highlight_segments(&hit.name, &hit.name_highlight)
                                        .into_iter()
                                        .map(|(text, on)| {
                                            div()
                                                .flex_shrink_0()
                                                .whitespace_nowrap()
                                                .when(on, |view| {
                                                    view.bg(tokens.hit_bg).text_color(tokens.hit_fg)
                                                })
                                                .child(text.to_owned())
                                        }),
                                ),
                        )
                        // 命中摘要第二行（设置页开关；仅 content: 查询有值）。
                        // 引用线样式 + StyledText 内联高亮，与文件名行明确区分。
                        .when(
                            self.show_snippet && !hit.snippet.is_empty(),
                            |view| {
                                let (plain, ranges) =
                                    crate::search_logic::snippet_plain_and_ranges(
                                        &hit.snippet,
                                    );
                                view.child(
                                    div()
                                        .w_full()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .border_l_1()
                                        .border_color(tokens.accent)
                                        .pl_1()
                                        .text_xs()
                                        .text_color(tokens.text_secondary)
                                        .child(StyledText::new(plain).with_highlights(
                                            ranges.into_iter().map(|range| {
                                                (
                                                    range,
                                                    HighlightStyle {
                                                        color: Some(tokens.hit_fg),
                                                        background_color: Some(
                                                            tokens.hit_bg,
                                                        ),
                                                        ..Default::default()
                                                    },
                                                )
                                            }),
                                        )),
                                )
                            },
                        ),
                )
                // 拼音辅库命中徽标（content: + ascii 拼音查到中文时）。
                .when(hit.pinyin_hit, |view| {
                    view.child(
                        div()
                            .ml_1()
                            .flex_shrink_0()
                            .px_1()
                            .rounded_sm()
                            .border_1()
                            .border_color(tokens.border)
                            .text_xs()
                            .text_color(tokens.text_secondary)
                            .child("拼"),
                    )
                })
                .into_any_element();
        }
        let text = match col {
            1 => hit.path.clone(),
            2 => format_size(hit.size, hit.is_directory),
            _ => format_time(hit.mtime),
        };
        div()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .text_ellipsis()
            .when(col == 2, |v| v.text_right())
            .text_color(tokens.text_secondary)
            .child(text)
            .into_any_element()
    }
    fn render_empty(&mut self, _: &mut Window, cx: &mut Context<TableState<Self>>) -> impl IntoElement {
        let label = self.owner.read_with(cx, |view, cx| view.search_empty_message(cx)).unwrap_or_default();
        let tokens = *cx.global::<crate::theme::FindxTokens>();
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p_6()
            .child(
                div()
                    .w_full()
                    .max_w(px(360.))
                    .text_center()
                    .whitespace_normal()
                    .text_color(tokens.text_secondary)
                    .child(label),
            )
    }
    fn has_more(&self, _: &App) -> bool {
        self.more
    }
    fn load_more(&mut self, _: &mut Window, cx: &mut Context<TableState<Self>>) {
        let owner = self.owner.clone();
        cx.defer(move |cx| {
            let _ = owner.update(cx, |view, cx| view.load_next_page(cx));
        });
    }
}
