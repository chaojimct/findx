use super::*;
use std::path::{Path, PathBuf};

pub(super) struct SearchSnapshot {
    input: Entity<InputState>,
    table: Entity<TableState<ResultsTable>>,
    hits: Vec<SearchHitDto>,
    selected_rows: std::collections::BTreeSet<usize>,
    selected: Option<usize>,
    total: u32,
    keyboard_ready: bool,
    status: SharedString,
    type_filter: TypeFilter,
    time_filter: TimeFilter,
    path_prefix: String,
    size_min: String,
    size_max: String,
}
impl MainWindow {
    pub(super) fn query_key_allowed(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.settings_page.is_none()
            && self.workspace.mode == ViewMode::Search
            && self.search.read(cx).focus_handle(cx).is_focused(window)
            && self
                .search
                .update(cx, |i, cx| i.marked_text_range(window, cx).is_none())
    }
    pub(super) fn request_search_key(
        &mut self,
        intent: crate::interaction_logic::SearchKeyIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if crate::isolation::development_var_os("FINDX_NATIVE_KEY_DIAGNOSTICS").is_some() {
            eprintln!(
                "[query-key] intent={intent:?} generation={} published={} loading={} rows={}",
                self.search_gen,
                self.search_keyboard.ready(self.search_gen),
                self.loading,
                self.hits.len()
            );
        }
        if self.search_keyboard.ready(self.search_gen) && !self.loading {
            self.apply_search_key(intent, window, cx);
        } else if !self.current_query(cx).trim().is_empty() {
            self.search_keyboard
                .queue(self.search_gen, intent, self.current_query(cx));
            if !self.loading {
                self.run_search(cx);
            }
        }
    }
    pub(super) fn apply_search_key(
        &mut self,
        intent: crate::interaction_logic::SearchKeyIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::interaction_logic::SearchKeyIntent;
        // A queued Enter must not steal focus from settings, another page or
        // an IME composition begun while the IPC request was in flight.
        if !self.query_key_allowed(window, cx) { return; }
        if self.workspace.mode != ViewMode::Search
            || !self.search_keyboard.ready(self.search_gen)
            || self.hits.is_empty()
        {
            return;
        }
        let index = match intent {
            SearchKeyIntent::Next => {
                move_result_selection(self.selected, ResultNavKey::ArrowDown, self.hits.len())
            }
            SearchKeyIntent::Previous => {
                move_result_selection(self.selected, ResultNavKey::ArrowUp, self.hits.len())
            }
            SearchKeyIntent::Open => {
                Some(self.selected.filter(|i| *i < self.hits.len()).unwrap_or(0))
            }
        }
        .unwrap();
        self.set_selected(Some(index), cx);
        let focus = self.table.read(cx).focus_handle(cx);
        self.table.update(cx, |table, cx| {
            table.set_selected_row(index, cx);
            table.scroll_to_row(index, cx);
        });
        focus.focus(window, cx);
        if crate::isolation::development_var_os("FINDX_NATIVE_KEY_DIAGNOSTICS").is_some() {
            eprintln!(
                "[query-key] selected={index} table_focus={} intent={intent:?}",
                focus.is_focused(window)
            );
        }
        if intent == SearchKeyIntent::Open {
            self.open_selected(cx);
        }
        cx.notify();
    }
    pub(super) fn begin_scoped_search(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.global_search.is_none() {
            self.global_search = Some(SearchSnapshot {
                input: self.search.clone(),
                table: self.table.clone(),
                hits: self.hits.clone(),
                selected_rows: self.search_selection.clone(),
                selected: self.selected,
                total: self.match_total,
                keyboard_ready: self.search_keyboard.ready(self.search_gen),
                status: if self.loading {
                    "原查询已暂停；可按 F5 刷新".into()
                } else {
                    self.search_status.clone()
                },
                type_filter: self.type_filter,
                time_filter: self.time_filter,
                path_prefix: self.path_prefix.clone(),
                size_min: self.size_min.clone(),
                size_max: self.size_max.clone(),
            });
        }
        self.search_gen = self.search_gen.wrapping_add(1);
        self.search_keyboard.invalidate();
        self._search_task = None;
        self._page_task = None;
        self.search_scope = Some(path.clone());
        self.path_prefix = scope_prefix(&path);
        self.type_filter = TypeFilter::All;
        self.time_filter = TimeFilter::All;
        self.size_min.clear();
        self.size_max.clear();
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("搜索此范围中的已索引文件（含子目录）")
        });
        let widths = self.table.read(cx).delegate().widths;
        let owner = cx.weak_entity();
        let table = cx.new(|cx| {
            TableState::new(
                ResultsTable {
                    hits: vec![],
                    widths,
                    more: false,
                    owner,
                },
                window,
                cx,
            )
            .col_movable(false)
            .col_resizable(true)
            .row_selectable(true)
            .col_selectable(false)
        });
        self.subscribe_search_input(input.clone(), window, cx);
        self.subscribe_results_table(table.clone(), cx);
        self.search = input;
        self.table = table;
        self.hits.clear();
        self.search_selection.clear();
        self.selected = None;
        self.match_total = 0;
        self.loading = false;
        self.page_loading = false;
        self.page_error = false;
        self.search_status = "范围索引搜索，含子目录。输入查询后开始，不扫描文件系统".into();
        self.workspace_switch(ViewMode::Search, cx);
        self.search.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    /// Export the currently published result set to CSV.
    ///
    /// The rows are exactly what the table shows: the same filtered hits the
    /// user is looking at, not the whole index. Nothing is written until the
    /// platform save dialog returns a path, and a cancelled dialog only
    /// updates the status line.
    ///
    /// The document is rendered before the dialog opens so that a query
    /// finishing mid-dialog cannot export a different result set than the one
    /// the user saw when pressing the button.
    pub(super) fn export_results_csv(&mut self, cx: &mut Context<Self>) {
        if self.hits.is_empty() {
            self.search_status = "没有可导出的结果".into();
            cx.notify();
            return;
        }
        let rows = self.hits.len();
        let doc = crate::csv_export::render(&self.hits);
        let dir = crate::csv_export::default_directory();
        let name = crate::csv_export::suggested_file_name(crate::csv_export::now_unix_secs());
        let picked = cx.prompt_for_new_path(&dir, Some(&name));
        self.search_status = format!("选择保存位置…（{rows} 条）").into();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = match picked.await {
                Ok(Ok(Some(path))) => std::fs::write(&path, doc.as_bytes())
                    .map(|()| Some(path))
                    .map_err(|e| format!("写入失败：{e}")),
                // Cancelled dialogs are not failures.
                Ok(Ok(None)) => Ok(None),
                Ok(Err(e)) => Err(format!("无法打开保存对话框：{e}")),
                Err(_) => Err("保存对话框已关闭".to_string()),
            };
            this.update(cx, |this, cx| {
                this.search_status = match outcome {
                    Ok(Some(path)) => crate::csv_export::done_message(rows, &path).into(),
                    Ok(None) => format!("已取消导出（{rows} 条）").into(),
                    Err(e) => format!("导出失败：{e}").into(),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn return_global_search(&mut self, cx: &mut Context<Self>) {        let Some(saved) = self.global_search.take() else {
            return;
        };
        self.search_gen = self.search_gen.wrapping_add(1);
        self.search_keyboard.invalidate();
        self._search_task = None;
        self._page_task = None;
        self.search_scope = None;
        self.search = saved.input;
        self.table = saved.table;
        self.hits = saved.hits;
        self.search_selection = saved.selected_rows;
        self.selected = saved.selected;
        self.match_total = saved.total;
        if saved.keyboard_ready {
            self.search_keyboard.publish(self.search_gen);
        }
        self.search_status = saved.status;
        self.type_filter = saved.type_filter;
        self.time_filter = saved.time_filter;
        self.path_prefix = saved.path_prefix;
        self.size_min = saved.size_min;
        self.size_max = saved.size_max;
        self.loading = false;
        self.page_loading = false;
        self.page_error = false;
        self.workspace_switch(ViewMode::Search, cx);
        self.preview_active_selection(cx);
        cx.notify();
    }
}
/// A terminal separator is mandatory: C:\A must not accidentally include C:\AB.
fn scope_prefix(path: &Path) -> String {
    let raw = super::workspace::display_path(path).replace('/', "\\");
    format!("{}\\", raw.trim_end_matches('\\'))
}
#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    #[test]
    fn scope_is_segment_bounded_and_normalizes_extended_and_mixed_paths() {
        assert_eq!(scope_prefix(Path::new(r"C:\A")), r"C:\A\");
        assert_eq!(scope_prefix(Path::new(r"\\?\C:\A/")), r"C:\A\");
        assert_eq!(scope_prefix(Path::new(r"C:\")), r"C:\");
        assert!(!r"c:\ab\x".starts_with(&scope_prefix(Path::new(r"C:\A")).to_lowercase()));
    }
}
