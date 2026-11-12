//! Virtualized Kit table: shared header/body geometry, resize handles and two scrollbars.
use super::main_window::MainWindow;
use crate::search_logic::{format_size, format_time, highlight_segments};
use findx2_ipc::SearchHitDto;
use gpui_kit::component::table::{Column, TableDelegate, TableState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

pub(super) const DEFAULT_WIDTHS: [f32; 4] = [280., 360., 90., 164.];
pub(super) const MIN_WIDTHS: [f32; 4] = [160., 160., 72., 150.];

pub(super) struct ResultsTable {
    pub hits: Vec<SearchHitDto>,
    pub widths: [f32; 4],
    pub more: bool,
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
        let selected = owner
            .read_with(cx, |view, cx| view.context_row_selected(true, row, cx))
            .unwrap_or(false);
        let tokens = *cx.global::<crate::theme::FindxTokens>();
        div()
            .id(("row", row))
            // Selection is a full-row fill, never an accent underline.
            .border_b_1()
            .border_color(tokens.border_soft)
            .when(selected, |v| v.bg(tokens.row_selected))
            .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                let _ = owner.update(cx, |view, cx| {
                    view.selection_pointer(true, row, event.modifiers, cx)
                });
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
            let menu_open = self
                .owner
                .read_with(cx, |view, _| view.file_menu_is_open())
                .unwrap_or(false);
            return div()
                .id(("search-name-cell", row))
                .when(!menu_open, |view| {
                    view.tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(full_path.clone())
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
                    div()
                        .ml_2()
                        .flex()
                        .items_center()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
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
    fn render_empty(
        &mut self,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child("输入关键词搜索文件")
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
