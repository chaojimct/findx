//! Virtualized Kit table: shared header/body geometry, resize handles and two scrollbars.

use crate::search_logic::{format_size, format_time, highlight_segments};
use findx2_ipc::SearchHitDto;
use gpui_kit::component::table::{Column, TableDelegate, TableState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

pub(super) const DEFAULT_WIDTHS: [f32; 4] = [280., 360., 90., 164.];
pub(super) const MIN_WIDTHS: [f32; 4] = [160., 160., 72., 150.];

pub(super) struct BrowserTable {
    pub hits: Vec<SearchHitDto>,
    pub widths: [f32; 4],
    pub more: bool,
}
impl TableDelegate for BrowserTable {
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
        Column::new(key, label)
            .width(px(self.widths[ix]))
            .min_width(px(MIN_WIDTHS[ix]))
            .max_width(px(1600.))
            .movable(false)
            .resizable(true)
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
        _: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(hit) = self.hits.get(row) else {
            return div().into_any_element();
        };
        if col == 0 {
            return div()
                .flex()
                .items_center()
                .min_w_0()
                .w_full()
                .h_full()
                .overflow_hidden()
                .children(
                    highlight_segments(&hit.name, &hit.name_highlight)
                        .into_iter()
                        .map(|(text, on)| {
                            div()
                                .flex_shrink_0()
                                .whitespace_nowrap()
                                .when(on, |d| d.bg(rgb(0xffecb3)).text_color(rgb(0x0d47a1)))
                                .child(text.to_owned())
                        }),
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
