//! Virtualized Kit table: shared header/body geometry, resize handles and two scrollbars.

use crate::search_logic::{format_size, format_time};
use findx2_ipc::SearchHitDto;
use gpui_kit::component::table::{Column, ColumnSort, TableDelegate, TableState};
use gpui_kit::component::{Icon, IconName, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::PathBuf;

pub(super) const DEFAULT_WIDTHS: [f32; 4] = [360., 104., 112., 170.];
pub(super) const MIN_WIDTHS: [f32; 4] = [180., 88., 88., 150.];

pub(super) struct BrowserTable {
    pub owner: WeakEntity<super::main_window::MainWindow>,
    pub hits: Vec<SearchHitDto>,
    pub widths: [f32; 4],
    pub more: bool,
    pub allocated: bool,
}

/// App-internal file drag payload: the grabbed rows travel as DTO clones so
/// the drop target does not depend on selection surviving the gesture.
#[derive(Clone, Debug)]
pub(crate) struct FileDragPayload {
    pub hits: Vec<SearchHitDto>,
}

/// Minimal drag ghost: a count label; the platform draws nothing for us.
pub(crate) struct DragGhost {
    pub label: String,
}
impl Render for DragGhost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *cx.global::<crate::theme::FindxTokens>();
        div()
            .px_3()
            .py_1()
            .rounded_md()
            .bg(t.surface_raised)
            .border_1()
            .border_color(t.border)
            .text_sm()
            .text_color(t.text_primary)
            .child(self.label.clone())
    }
}
impl TableDelegate for BrowserTable {
    fn render_tr(
        &mut self,
        row: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let owner = self.owner.clone();
        let right_owner = owner.clone();
        let up_owner = owner.clone();
        let hover_owner = owner.clone();
        let painted_scope = owner.read_with(cx, |view, _| view.row_paint_scope(false)).ok().flatten();
        let selected = owner
            .read_with(cx, |view, cx| view.context_row_selected(false, row, cx))
            .unwrap_or(false);
        // Drag payload follows the click-selection rule; computed at paint
        // time (mousedown repaints before the drag threshold trips).
        let drag_hits = owner
            .read_with(cx, |view, _| {
                view.drag_hits_for_row(row, &self.hits)
            })
            .unwrap_or_default();
        let drop_dir = self
            .hits
            .get(row)
            .filter(|h| h.is_directory)
            .map(|h| PathBuf::from(h.path.clone()));
        let hover_dir = drop_dir.clone();
        let press_dir = drop_dir.clone();
        let drop_owner = owner.clone();
        let tokens = *cx.global::<crate::theme::FindxTokens>();
        div()
            .id(("row", row))
            .border_b_1()
            .border_color(tokens.border_soft)
            .when(selected, |v| v.bg(tokens.row_selected))
            .on_drag(
                FileDragPayload { hits: drag_hits },
                |payload, _, _, cx| {
                    cx.new(|_| DragGhost {
                        label: format!("移动 {} 项", payload.hits.len().max(1)),
                    })
                },
            )
            .on_drop::<FileDragPayload>(move |payload, _, cx| {
                if let Some(dir) = drop_dir.clone() {
                    let _ = drop_owner.update(cx, |view, cx| {
                        view.drop_files_on_directory(
                            payload.hits.clone(),
                            true,
                            dir,
                            cx,
                        )
                    });
                }
            })
            .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                let _ = owner.update(cx, |view, cx| {
                    if !crate::ui::async_publication::painted_event_current(painted_scope, view.row_paint_scope(false)) {
                        cx.stop_propagation(); return;
                    }
                    view.selection_pointer(false, row, event.modifiers, cx);
                    if !event.modifiers.control && !event.modifiers.shift {
                        view.press_row = Some((false, row, event.position));
                    }
                    // A press is definite interest (double-click navigates
                    // without any mouse move, so hover never fires): warm it.
                    if let Some(dir) = press_dir.clone() {
                        view.queue_prefetch(dir, cx);
                    }
                });
            })
            .on_mouse_up(MouseButton::Left, move |event, _, cx| {
                let _ = up_owner.update(cx, |view, cx| {
                    view.collapse_press_selection(false, row, event.modifiers, event.position, cx);
                });
            })
            .on_hover(move |hovering: &bool, _, cx| {
                if let Some(dir) = hover_dir.clone() {
                    let _ = hover_owner.update(cx, |view, cx| {
                        view.note_directory_hover(dir, *hovering, cx);
                    });
                }
            })
            .on_mouse_down(MouseButton::Right, move |_, _, cx| {
                let valid=right_owner.read_with(cx,|view,_| crate::ui::async_publication::painted_event_current(painted_scope,view.row_paint_scope(false))).unwrap_or(false);
                if !valid { cx.stop_propagation(); }
            })
    }
    /// Header click cycles asc -> desc -> default on the whole cell: the
    /// widget's own cycle starts with descending and only the tiny icon is
    /// clickable, so the header is owned here and the widget stays unsorted.
    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>
    ) -> impl IntoElement {
        let owner = self.owner.clone();
        let (active, descending) = owner
            .read_with(cx, |view, _| {
                view.directory_sort_spec()
                    .map(|(c, d)| (c == col_ix, d))
                    .unwrap_or((false, false))
            })
            .unwrap_or((false, false));
        let label = match col_ix {
            0 => "名称",
            1 => {
                if self.allocated {
                    "实际占用"
                } else {
                    "大小"
                }
            }
            2 => "类型",
            _ => "修改时间"
        };
        let tokens = *cx.global::<crate::theme::FindxTokens>();
        h_flex()
            .id(("th-sort", col_ix))
            .w_full()
            .items_center()
            .gap_1()
            .when(col_ix == 1, |v| v.justify_end())
            .cursor_pointer()
            .when(active && col_ix == 1, |v| {
                v.child(
                    Icon::new(if descending {
                        IconName::ArrowDown
                    } else {
                        IconName::ArrowUp
                    })
                    .size_3()
                    .text_color(tokens.accent),
                )
            })
            .child(
                div()
                    .text_sm()
                    .text_color(tokens.text_secondary)
                    .child(label),
            )
            .when(active && col_ix != 1, |v| {
                v.child(
                    Icon::new(if descending {
                        IconName::ArrowDown
                    } else {
                        IconName::ArrowUp
                    })
                    .size_3()
                    .text_color(tokens.accent),
                )
            })
            .on_click(move |_, _, cx| {
                let _ = owner.update(cx, |view, cx| {
                    let next = match view.directory_sort_spec() {
                        Some((c, false)) if c == col_ix => ColumnSort::Descending,
                        Some((c, true)) if c == col_ix => ColumnSort::Default,
                        _ => ColumnSort::Ascending,
                    };
                    view.apply_directory_sort(col_ix, next, cx);
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
                1 => format_size(hit.size, hit.is_directory && hit.size == 0 && !self.allocated),
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
                        .child(
                            StyledText::new(hit.name.clone()).with_highlights(
                                crate::search_logic::highlight_byte_ranges(
                                    &hit.name,
                                    &hit.name_highlight,
                                )
                                .into_iter()
                                .map(|range| {
                                    (
                                        range,
                                        HighlightStyle {
                                            color: Some(tokens.hit_fg),
                                            background_color: Some(tokens.hit_bg),
                                            ..Default::default()
                                        },
                                    )
                                }),
                            ),
                        ),
                )
                .into_any_element();
        }
        let text = match col {
            1 => format_size(hit.size, hit.is_directory && hit.size == 0 && !self.allocated),
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
