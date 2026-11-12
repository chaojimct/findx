use super::*;
use std::path::{Path, PathBuf};

pub(super) struct SearchSnapshot {
    input: Entity<InputState>,
    table: Entity<TableState<ResultsTable>>,
    hits: Vec<SearchHitDto>,
    selected_rows: std::collections::BTreeSet<usize>,
    selected: Option<usize>,
    total: u32,
    status: SharedString,
    type_filter: TypeFilter,
    time_filter: TimeFilter,
    path_prefix: String,
    size_min: String,
    size_max: String,
}
impl MainWindow {
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
        self.search_status = "范围索引搜索 · 含子目录 · 输入查询后开始；不扫描文件系统".into();
        self.workspace_switch(ViewMode::Search, cx);
        self.search.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    pub(super) fn return_global_search(&mut self, cx: &mut Context<Self>) {
        let Some(saved) = self.global_search.take() else {
            return;
        };
        self.search_gen = self.search_gen.wrapping_add(1);
        self._search_task = None;
        self._page_task = None;
        self.search_scope = None;
        self.search = saved.input;
        self.table = saved.table;
        self.hits = saved.hits;
        self.search_selection = saved.selected_rows;
        self.selected = saved.selected;
        self.match_total = saved.total;
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
