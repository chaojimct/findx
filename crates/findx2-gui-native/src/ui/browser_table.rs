//! Virtualized Kit table: shared header/body geometry, resize handles and two scrollbars.

use crate::search_logic::{format_size, format_time};
use findx2_ipc::SearchHitDto;
use gpui_kit::component::table::{Column, TableDelegate, TableState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

pub(super) const DEFAULT_WIDTHS: [f32; 4] = [360., 104., 112., 170.];
pub(super) const MIN_WIDTHS: [f32; 4] = [180., 88., 88., 150.];

pub(super) struct BrowserTable {
    pub owner: WeakEntity<super::main_window::MainWindow>,
    pub hits: Vec<SearchHitDto>,
    pub widths: [f32; 4],
    pub more: bool,
    pub allocated: bool,
}
impl TableDelegate for BrowserTable {
    fn render_tr(
        &mut self,
        row: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let owner = self.owner.clone();
        let selected = owner
            .read_with(cx, |view, cx| view.context_row_selected(false, row, cx))
            .unwrap_or(false);
        let tokens = *cx.global::<crate::theme::FindxTokens>();
        div()
            .id(("row", row))
            .border_b_1()
            .border_color(tokens.border_soft)
            .when(selected, |v| v.bg(tokens.row_selected))
            .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                let _ = owner.update(cx, |view, cx| {
                    view.selection_pointer(false, row, event.modifiers, cx)
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
                view.prepare_context_row(false, row, cx);
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
            ("size", "大小"),
            ("type", "类型"),
            ("mtime", "修改时间"),
        ][ix];
        let mut column = Column::new(
            key,
            if ix == 1 && self.allocated {
                "实际占用"
            } else {
                label
            },
        )
        .width(px(self.widths[ix]))
        .min_width(px(MIN_WIDTHS[ix]))
        .max_width(px(1600.))
        .movable(false)
        .resizable(true);
        column.align = if ix == 1 {
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
                1 => format_size(hit.size, hit.is_directory && !self.allocated),
                2 => type_label(hit),
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
                .id(("browser-name-cell", row))
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
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(hit.name.clone()),
                )
                .into_any_element();
        }
        let text = match col {
            1 => format_size(hit.size, hit.is_directory && !self.allocated),
            2 => type_label(hit),
            _ => format_time(hit.mtime),
        };
        div()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .text_ellipsis()
            .when(col == 1, |v| v.text_right())
            .when(col > 0, |v| v.text_color(tokens.text_secondary))
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
            .child("空目录（或尚未加载）")
    }
    fn has_more(&self, _: &App) -> bool {
        self.more
    }
}

fn type_label(hit: &SearchHitDto) -> String {
    if hit.is_directory {
        "文件夹".into()
    } else {
        match std::path::Path::new(&hit.name)
            .extension()
            .and_then(|s| s.to_str())
        {
            Some(ext) if !ext.is_empty() => ext.to_ascii_uppercase(),
            _ => "文件".into(),
        }
    }
}

pub(super) fn migrate_widths(widths: [f32; 4], schema: u8) -> [f32; 4] {
    if schema >= 2 {
        widths
    } else {
        [widths[0], widths[1], DEFAULT_WIDTHS[2], widths[2]]
    }
}
#[cfg(test)]
mod column_tests {
    #[test]
    fn old_widths_preserve_name_size_and_date() {
        assert_eq!(
            super::migrate_widths([300., 100., 180., 900.], 0),
            [300., 100., 112., 180.]
        );
    }
}
