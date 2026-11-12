//! Distinct search, directory-tab and usage pages share file identity and explicit actions, not navigation histories.
//! Filesystem and allocation work never run on the render/input thread.
#[path = "workspace_pages.rs"]
mod pages;
use super::super::browser_table::{BrowserTable, MIN_WIDTHS as BROWSER_MIN_WIDTHS};
use super::*;
use crate::{
    browser::{self, FileAction},
    disk_usage::{self, Limits, Progress, Scan},
};
use gpui_kit::base::InteractiveElementExt as _;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub(super) const DIRECTORY_SIDEBAR_WIDTH: f32 = 184.;
/// Reserve actual table minimums, not an unrelated 620px guess that hid filters at 1024px.
pub(super) fn panel_budget(
    width: f32,
    rail: f32,
    list_min: f32,
    mut sidebar: bool,
    mut preview: bool,
    wanted: f32,
) -> (bool, bool, f32) {
    if preview && sidebar && width < rail + list_min + 250. {
        sidebar = false;
    }
    if preview && width < 566. {
        preview = false;
    }
    let pane = wanted.min((width - if sidebar { rail } else { 0. } - list_min - 10.).max(240.));
    (sidebar, preview, pane)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ViewMode {
    Search,
    Directory,
    Usage,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Route {
    mode: ViewMode,
    path: PathBuf,
    pub(in crate::ui) selected: Option<String>,
}
#[derive(Clone, Default)]
struct History {
    back: Vec<Route>,
    forward: Vec<Route>,
}
impl History {
    fn push(&mut self, r: Route) {
        if self.back.last() != Some(&r) {
            self.back.push(r);
            if self.back.len() > 128 {
                self.back.remove(0);
            }
        }
        self.forward.clear();
    }
    fn travel(&mut self, current: Route, forward: bool) -> Option<Route> {
        if forward {
            let next = self.forward.pop()?;
            self.back.push(current);
            Some(next)
        } else {
            let previous = self.back.pop()?;
            self.forward.push(current);
            Some(previous)
        }
    }
}
pub(super) fn display_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    if let Some(s) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{}", s)
    } else {
        s.strip_prefix(r"\\?\").unwrap_or(&s).to_owned()
    }
}
fn key(p: &Path) -> String {
    display_path(p)
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}
fn root_hit(path: &Path) -> SearchHitDto {
    SearchHitDto {
        entry_idx: 0,
        name: path
            .file_name()
            .map(|v| v.to_string_lossy().into_owned())
            .unwrap_or_else(|| display_path(path)),
        path: display_path(path),
        size: 0,
        mtime: 0,
        is_directory: true,
        name_highlight: vec![],
    }
}

#[derive(Clone)]
struct PageContext {
    pub(in crate::ui) location: PathBuf,
    pub(in crate::ui) table: Entity<TableState<BrowserTable>>,
    pub(in crate::ui) rows: Vec<SearchHitDto>,
    all_rows: Vec<SearchHitDto>,
    pub(in crate::ui) selected_rows: std::collections::BTreeSet<usize>,
    pub(in crate::ui) selected: Option<usize>,
    history: History,
    status: String,
    filter: String,
    needs_load: bool,
    preview_force: Option<ForceState>,
}

fn fit_columns(
    mut widths: [f32; 4],
    minimum: [f32; 4],
    flex: [usize; 2],
    available: f32,
) -> [f32; 4] {
    for i in 0..4 {
        widths[i] = if widths[i].is_finite() {
            widths[i].clamp(minimum[i], 1600.)
        } else {
            minimum[i]
        };
    }
    for group in [
        flex,
        [0, 1, 2, 3]
            .into_iter()
            .filter(|i| !flex.contains(i))
            .collect::<Vec<_>>()
            .try_into()
            .unwrap_or([2, 3]),
    ] {
        let deficit = (widths.iter().sum::<f32>() - available).max(0.);
        let capacity = group.iter().map(|&i| widths[i] - minimum[i]).sum::<f32>();
        if capacity > 0. {
            let amount = deficit.min(capacity);
            for i in group {
                widths[i] -= amount * (widths[i] - minimum[i]) / capacity;
            }
        }
    }
    let spare = (available - widths.iter().sum::<f32>()).max(0.);
    widths[flex[0]] += (1600. - widths[flex[0]]).max(0.).min(spare);
    widths.map(|v| (v * 2.).floor() / 2.)
}

pub(super) fn preview_stamp(path: &str, size: u64, mtime: u64) -> (String, u64, u64) {
    let seconds = if mtime > 100_000_000_000_000 {
        mtime.saturating_sub(116_444_736_000_000_000) / 10_000_000
    } else {
        mtime
    };
    (key(Path::new(path)), size, seconds)
}

pub(super) struct Workspace {
    pub(super) show_hidden: bool,
    locations: Vec<(&'static str, &'static str, PathBuf)>,
    sidebar_source: Option<(&'static str, String)>,
    collapsed_groups: std::collections::HashSet<&'static str>,
    pub(super) mode: ViewMode,
    tabs: Vec<PageContext>,
    active_tab: usize,
    usage_page: Option<PageContext>,
    search_preview: Option<ForceState>,
    filter_input: Entity<InputState>,
    filter_text: String,
    all_rows: Vec<SearchHitDto>,
    pub(super) editing_path: bool,
    pub(super) show_actions: bool,
    pub(super) path: Entity<InputState>,
    pub(super) status: String,
    pub(in crate::ui) location: PathBuf,
    pub(in crate::ui) table: Entity<TableState<BrowserTable>>,
    pub(in crate::ui) rows: Vec<SearchHitDto>,
    pub(in crate::ui) selected_rows: std::collections::BTreeSet<usize>,
    pub(in crate::ui) selected: Option<usize>,
    pending_hit: Option<SearchHitDto>,
    history: History,
    pub(in crate::ui) cache: HashMap<String, (Vec<SearchHitDto>, bool)>,
    list_generation: u64,
    list_cancel: Arc<AtomicBool>,
    pub(super) listing: bool,
    window: AnyWindowHandle,
    scan: Option<Scan>,
    scan_lookup: HashMap<String, usize>,
    scan_generation: u64,
    scan_cancel: Arc<AtomicBool>,
    progress: Arc<Progress>,
    pub(in crate::ui) scanning: bool,
    pub(in crate::ui) scan_stale: bool,
    usage_focus: Option<String>,
    suspend_preview: bool,
    scan_status: String,
    operation: Option<FileAction>,
    source: Option<SearchHitDto>,
    source_identity: Option<crate::file_identity::FileStamp>,
    target: Entity<InputState>,
    confirmed: bool,
    pub(in crate::ui) op_busy: bool,
    op_cancel: Arc<AtomicBool>,
    op_hint: String,
    _tick: Option<Task<()>>,
}
impl Workspace {
    pub(super) fn new(widths: [f32; 4], window: &mut Window, cx: &mut Context<MainWindow>) -> Self {
        let widths = std::array::from_fn(|i| {
            if widths[i].is_finite() {
                widths[i].clamp(BROWSER_MIN_WIDTHS[i], 1600.)
            } else {
                super::super::browser_table::DEFAULT_WIDTHS[i]
            }
        });
        let location = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\"));
        let path = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("目录路径 · Ctrl+L")
                .default_value(display_path(&location))
        });
        let filter_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("过滤当前目录（仅本层）"));
        let target =
            cx.new(|cx| InputState::new(window, cx).placeholder("完整目标路径 / 新文件名"));
        let owner = cx.weak_entity();
        let table = cx.new(|cx| {
            TableState::new(
                BrowserTable {
                    owner: owner.clone(),
                    hits: vec![],
                    widths,
                    more: false,
                    allocated: false,
                },
                window,
                cx,
            )
            .col_movable(false)
            .col_resizable(true)
            .row_selectable(true)
            .col_selectable(false)
        });
        let mut this = Self {
            show_hidden: false,
            locations: Vec::new(),
            sidebar_source: None,
            collapsed_groups: Default::default(),
            mode: ViewMode::Search,
            tabs: vec![],
            active_tab: 0,
            usage_page: None,
            search_preview: None,
            filter_input,
            filter_text: String::new(),
            all_rows: vec![],
            editing_path: false,
            show_actions: false,
            path,
            status: "选择位置后浏览".into(),
            location,
            table,
            rows: vec![],
            selected_rows: Default::default(),
            selected: None,
            pending_hit: None,
            history: History::default(),
            cache: HashMap::new(),
            list_generation: 0,
            list_cancel: Arc::new(AtomicBool::new(false)),
            listing: false,
            window: window.window_handle(),
            scan: None,
            scan_lookup: HashMap::new(),
            scan_generation: 0,
            scan_cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(Progress::default()),
            scanning: false,
            scan_stale: false,
            usage_focus: None,
            suspend_preview: false,
            scan_status: "尚未扫描；不会把搜索结果当作全盘统计".into(),
            operation: None,
            source: None,
            source_identity: None,
            target,
            confirmed: false,
            op_busy: false,
            op_cancel: Arc::new(AtomicBool::new(false)),
            op_hint: String::new(),
            _tick: None,
        };
        let initial = this.snapshot(Some(ForceState::Hide), true);
        this.tabs.push(initial.clone());
        let mut usage = initial;
        usage.table = cx.new(|cx| {
            TableState::new(
                BrowserTable {
                    owner: owner.clone(),
                    hits: vec![],
                    widths,
                    more: false,
                    allocated: true,
                },
                window,
                cx,
            )
            .col_movable(false)
            .col_resizable(true)
            .row_selectable(true)
            .col_selectable(false)
        });
        this.usage_page = Some(usage);
        this
    }

    fn snapshot(&self, preview_force: Option<ForceState>, needs_load: bool) -> PageContext {
        PageContext {
            location: self.location.clone(),
            table: self.table.clone(),
            rows: self.rows.clone(),
            all_rows: self.all_rows.clone(),
            selected_rows: self.selected_rows.clone(),
            selected: self.selected,
            history: self.history.clone(),
            status: self.status.clone(),
            filter: self.filter_text.clone(),
            needs_load,
            preview_force,
        }
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        self.list_cancel.store(true, Ordering::Release);
        self.scan_cancel.store(true, Ordering::Release);
        self.op_cancel.store(true, Ordering::Release);
    }
}

impl MainWindow {
    pub(super) fn init_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let locations = cx
                .background_spawn(async { browser::standard_locations() })
                .await;
            this.update(cx, |this, cx| {
                this.workspace.locations = locations;
                cx.notify();
            })
            .ok();
        })
        .detach();

        self.subscribe_workspace_table(self.workspace.table.clone(), cx);
        let filter = self.workspace.filter_input.clone();
        self._subscriptions
            .push(cx.subscribe(&filter, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) && this.workspace.mode == ViewMode::Directory
                {
                    this.workspace.filter_text =
                        this.workspace.filter_input.read(cx).value().to_string();
                    let focus = this.active_workspace_hit().map(|h| h.path);
                    let all = this.workspace.all_rows.clone();
                    this.set_workspace_rows(all, focus, false, cx);
                }
            }));
        let target = self.workspace.target.clone();
        self._subscriptions
            .push(cx.subscribe(&target, |this, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    this.workspace.confirmed = false;
                    cx.notify();
                }
            }));
        let path = self.workspace.path.clone();
        self._subscriptions.push(cx.subscribe_in(
            &path,
            window,
            |this, _, ev: &InputEvent, _, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    let p = PathBuf::from(this.workspace.path.read(cx).value().to_string());
                    this.workspace_navigate(p, this.workspace.mode, None, true, cx);
                }
            },
        ));
        self.workspace._tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                if this
                    .update(cx, |this, cx| {
                        if this.workspace.scanning {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }
    pub(super) fn focus_directory_filter(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .filter_input
            .update(cx, |input, cx| input.focus(window, cx));
    }

    pub(super) fn fit_active_columns(&self, available: f32, cx: &mut Context<Self>) {
        let prefs = &self.session.read(cx).prefs;
        if self.workspace.mode == ViewMode::Search && prefs.search_fit_columns {
            let natural = prefs.column_widths.unwrap_or(DEFAULT_WIDTHS);
            let desired = fit_columns(natural, MIN_WIDTHS, [0, 1], available - 4.);
            if self.table.read(cx).delegate().widths != desired {
                self.table.update(cx, |table, cx| {
                    table.delegate_mut().widths = desired;
                    table.refresh(cx);
                });
            }
        } else if self.workspace.mode == ViewMode::Directory && prefs.browser_fit_columns {
            let natural = prefs
                .browser_column_widths
                .map(|w| {
                    super::super::browser_table::migrate_widths(w, prefs.browser_column_schema)
                })
                .unwrap_or(super::super::browser_table::DEFAULT_WIDTHS);
            let desired = fit_columns(natural, BROWSER_MIN_WIDTHS, [0, 2], available - 4.);
            if self.workspace.table.read(cx).delegate().widths != desired {
                self.workspace.table.update(cx, |table, cx| {
                    table.delegate_mut().widths = desired;
                    table.refresh(cx);
                });
            }
        }
    }

    fn subscribe_workspace_table(
        &mut self,
        table: Entity<TableState<BrowserTable>>,
        cx: &mut Context<Self>,
    ) {
        self._subscriptions
            .push(cx.subscribe(&table, |this, table, ev: &TableEvent, cx| {
                if table.entity_id() != this.workspace.table.entity_id() {
                    return;
                }
                match ev {
                    TableEvent::SelectRow(ix) => this.table_selected_event(false, *ix, cx),
                    TableEvent::DoubleClickedRow(ix) => {
                        this.workspace_select(*ix, cx);
                        this.workspace_enter(cx);
                    }
                    TableEvent::ColumnWidthsChanged(w) if w.len() == 4 => {
                        let widths = std::array::from_fn(|i| {
                            w[i].as_f32().clamp(BROWSER_MIN_WIDTHS[i], 1600.)
                        });
                        this.workspace
                            .table
                            .update(cx, |t, _| t.delegate_mut().widths = widths);
                        this.session.update(cx, |s, cx| {
                            s.prefs.browser_column_widths = Some(widths);
                            s.prefs.browser_column_schema = 2;
                            s.prefs.browser_fit_columns = false;
                            crate::session::persist_prefs(s.prefs.clone(), cx);
                        });
                    }
                    _ => {}
                }
            }));
    }

    fn save_page_context(&mut self) {
        let state = self
            .workspace
            .snapshot(self.preview_force, self.workspace.listing);
        match self.workspace.mode {
            ViewMode::Directory => self.workspace.tabs[self.workspace.active_tab] = state,
            ViewMode::Usage => self.workspace.usage_page = Some(state),
            ViewMode::Search => self.workspace.search_preview = self.preview_force,
        }
    }

    fn restore_page_context(&mut self, state: PageContext, cx: &mut Context<Self>) -> bool {
        self.workspace.location = state.location;
        self.workspace.table = state.table;
        self.workspace.rows = state.rows;
        self.workspace.all_rows = state.all_rows;
        self.workspace.selected_rows = state.selected_rows;
        self.workspace.selected = state.selected;
        self.workspace.pending_hit = None;
        self.workspace.history = state.history;
        self.workspace.status = state.status;
        self.workspace.filter_text = state.filter.clone();
        self.workspace.listing = false;
        self.preview_force = state.preview_force;
        self.update_location_input(cx);
        let input = self.workspace.filter_input.clone();
        let _ = self.workspace.window.update(cx, move |_, window, cx| {
            input.update(cx, |input, cx| input.set_value(state.filter, window, cx))
        });
        state.needs_load
    }

    fn select_directory_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.workspace.tabs.len() || index == self.workspace.active_tab {
            return;
        }
        self.save_page_context();
        self.workspace.list_cancel.store(true, Ordering::Release);
        self.workspace.list_generation = self.workspace.list_generation.wrapping_add(1);
        self.workspace.active_tab = index;
        let state = self.workspace.tabs[index].clone();
        let needs_load = self.restore_page_context(state, cx);
        if needs_load {
            self.workspace_navigate(
                self.workspace.location.clone(),
                ViewMode::Directory,
                None,
                false,
                cx,
            );
        }
        self.preview_active_selection(cx);
        cx.notify();
    }

    fn new_directory_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.tabs.len() >= 12 {
            self.workspace.status = "最多同时打开 12 个目录标签".into();
            cx.notify();
            return;
        }
        self.save_page_context();
        let widths = self.workspace.table.read(cx).delegate().widths;
        let owner = cx.weak_entity();
        let table = cx.new(|cx| {
            TableState::new(
                BrowserTable {
                    owner: owner.clone(),
                    hits: vec![],
                    widths,
                    more: false,
                    allocated: false,
                },
                window,
                cx,
            )
            .col_movable(false)
            .col_resizable(true)
            .row_selectable(true)
            .col_selectable(false)
        });
        self.subscribe_workspace_table(table.clone(), cx);
        let mut state = self.workspace.snapshot(Some(ForceState::Hide), true);
        state.table = table;
        state.rows.clear();
        state.all_rows.clear();
        state.selected_rows.clear();
        state.selected = None;
        state.filter.clear();
        state.history = History::default();
        self.workspace.tabs.push(state);
        self.select_directory_tab(self.workspace.tabs.len() - 1, cx);
    }

    fn close_directory_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.workspace.tabs.len() == 1 {
            return;
        }
        if index == self.workspace.active_tab {
            let next = if index == 0 { 1 } else { index - 1 };
            self.select_directory_tab(next, cx);
        }
        self.workspace.tabs.remove(index);
        if self.workspace.active_tab > index {
            self.workspace.active_tab -= 1;
        }
        cx.notify();
    }

    pub(super) fn active_workspace_hit(&self) -> Option<SearchHitDto> {
        if self.workspace.mode == ViewMode::Search {
            self.selected.and_then(|i| self.hits.get(i).cloned())
        } else if self.workspace.mode == ViewMode::Usage {
            if let Some(node) = self
                .workspace
                .usage_focus
                .as_ref()
                .and_then(|path| self.workspace.scan_lookup.get(&key(Path::new(path))))
                .and_then(|&i| self.workspace.scan.as_ref()?.nodes.get(i))
            {
                Some(SearchHitDto {
                    entry_idx: 0,
                    name: node
                        .path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    path: display_path(&node.path),
                    size: node.logical,
                    mtime: node.mtime,
                    is_directory: node.directory,
                    name_highlight: vec![],
                })
            } else {
                Some(root_hit(&self.workspace.location))
            }
        } else {
            self.workspace
                .selected
                .and_then(|i| self.workspace.rows.get(i).cloned())
                .or_else(|| self.workspace.pending_hit.clone())
                .or_else(|| Some(root_hit(&self.workspace.location)))
        }
    }
    fn current_route(&self) -> Route {
        Route {
            mode: self.workspace.mode,
            path: self.workspace.location.clone(),
            selected: self.active_workspace_hit().map(|h| h.path),
        }
    }
    pub(super) fn preview_active_selection(&mut self, cx: &mut Context<Self>) {
        self.refresh_selection_snapshot(cx);
        if self.selected_file_hits().len() > 1 {
            self.pause_preview();
            cx.notify();
            return;
        }
        if self.workspace.suspend_preview {
            return;
        }
        if !self.ensure_selection_identity(cx) {
            self.pause_preview();
            cx.notify();
            return;
        }
        let visible = self.settings_page.is_none()
            && self.workspace.mode != ViewMode::Usage
            && match self.preview_force {
                Some(ForceState::Hide) => false,
                Some(ForceState::Show) => true,
                None => self.session.read(cx).prefs.preview_open,
            };
        if !visible {
            self.pause_preview();
            cx.notify();
            return;
        }
        let target = self.active_workspace_hit();
        if target.as_ref().is_some_and(|h| {
            self.preview.meta.as_ref().is_some_and(|m| {
                preview_stamp(&m.path, m.size, m.mtime) == preview_stamp(&h.path, h.size, h.mtime)
            })
        }) && !matches!(self.preview.kind, PreviewKind::Empty | PreviewKind::Loading)
        {
            cx.notify();
            return;
        }
        self.pdf_options = target
            .as_ref()
            .and_then(|hit| {
                self.preview_memo
                    .get(&preview_stamp(&hit.path, hit.size, hit.mtime))
            })
            .copied()
            .unwrap_or_default();
        self.office_text_only = false;
        self.request_preview(cx);
        cx.notify();
    }
    pub(super) fn workspace_switch(&mut self, mode: ViewMode, cx: &mut Context<Self>) {
        if mode == self.workspace.mode {
            return;
        }
        self.save_page_context();
        self.workspace.list_cancel.store(true, Ordering::Release);
        self.workspace.list_generation = self.workspace.list_generation.wrapping_add(1);
        self.workspace.listing = false;
        self.workspace.operation = None;
        self.workspace.show_actions = false;
        self.workspace.mode = mode;
        match mode {
            ViewMode::Search => self.preview_force = self.workspace.search_preview,
            ViewMode::Directory => {
                let state = self.workspace.tabs[self.workspace.active_tab].clone();
                let needs_load = self.restore_page_context(state, cx);
                if needs_load {
                    self.workspace_navigate(self.workspace.location.clone(), mode, None, false, cx);
                }
            }
            ViewMode::Usage => {
                if let Some(state) = self.workspace.usage_page.clone() {
                    self.restore_page_context(state, cx);
                }
                let focus = self.active_workspace_hit().map(|h| h.path);
                self.show_scanned_directory(focus, cx);
            }
        }
        self.preview_active_selection(cx);
        cx.notify();
    }
    fn update_location_input(&self, cx: &mut Context<Self>) {
        let input = self.workspace.path.clone();
        let text = display_path(&self.workspace.location);
        let _ = self.workspace.window.update(cx, move |_, w, cx| {
            input.update(cx, |i, cx| i.set_value(text, w, cx))
        });
    }
    fn set_workspace_rows(
        &mut self,
        rows: Vec<SearchHitDto>,
        focus: Option<String>,
        allocated: bool,
        cx: &mut Context<Self>,
    ) {
        self.workspace.all_rows = rows.clone();
        let filter = self.workspace.filter_text.to_lowercase();
        let rows = if self.workspace.mode == ViewMode::Directory && !filter.is_empty() {
            rows.into_iter()
                .filter(|row| row.name.to_lowercase().contains(&filter))
                .collect::<Vec<_>>()
        } else {
            rows
        };
        self.workspace.selected = focus.as_ref().and_then(|p| {
            rows.iter()
                .position(|h| key(Path::new(&h.path)) == key(Path::new(p)))
        });
        self.workspace.selected_rows.clear();
        self.workspace.selected_rows.extend(self.workspace.selected);
        self.workspace.rows = rows;
        self.workspace.pending_hit = None;
        let rows = self.workspace.rows.clone();
        let selected = self.workspace.selected;
        self.workspace.table.update(cx, |t, cx| {
            t.delegate_mut().hits = rows;
            t.delegate_mut().allocated = allocated;
            t.refresh(cx);
            if let Some(ix) = selected {
                t.set_selected_row(ix, cx);
                t.scroll_to_row(ix, cx);
            } else {
                t.clear_selection(cx);
            }
        });
        if self.workspace.mode != ViewMode::Search {
            self.preview_active_selection(cx);
        }
        cx.notify();
    }
    fn scan_node(&self) -> Option<usize> {
        self.workspace
            .scan_lookup
            .get(&key(&self.workspace.location))
            .copied()
    }
    fn show_scanned_directory(&mut self, focus: Option<String>, cx: &mut Context<Self>) -> bool {
        let Some(ix) = self.scan_node() else {
            return false;
        };
        let Some(scan) = &self.workspace.scan else {
            return false;
        };
        let mut children = scan.nodes[ix].children.clone();
        children.sort_by_key(|&i| std::cmp::Reverse(scan.nodes[i].allocated));
        let rows = children
            .iter()
            .map(|&i| {
                let n = &scan.nodes[i];
                SearchHitDto {
                    entry_idx: i as u32,
                    name: n
                        .path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    path: display_path(&n.path),
                    size: n.allocated,
                    mtime: n.mtime,
                    is_directory: n.directory,
                    name_highlight: vec![],
                }
            })
            .collect();
        self.workspace.status = format!(
            "占用 · {} 个子项 · 逻辑 {} · 实际 {}{}",
            children.len(),
            search_logic::format_size(scan.nodes[ix].logical, false),
            search_logic::format_size(scan.nodes[ix].allocated, false),
            if scan.partial { " · 部分结果" } else { "" }
        );
        self.set_workspace_rows(rows, focus, true, cx);
        true
    }
    fn workspace_navigate(
        &mut self,
        path: PathBuf,
        mode: ViewMode,
        focus: Option<String>,
        remember: bool,
        cx: &mut Context<Self>,
    ) {
        let source_hit = self.active_workspace_hit();
        if self.workspace.mode != mode {
            self.workspace.suspend_preview = true;
            self.workspace_switch(mode, cx);
            self.workspace.suspend_preview = false;
        }
        if remember {
            let route = self.current_route();
            self.workspace.history.push(route);
        }
        self.workspace.list_cancel.store(true, Ordering::Release);
        self.workspace.list_cancel = Arc::new(AtomicBool::new(false));
        self.workspace.list_generation = self.workspace.list_generation.wrapping_add(1);
        let generation = self.workspace.list_generation;
        let pending = source_hit.filter(|h| {
            focus
                .as_ref()
                .is_some_and(|p| key(Path::new(p)) == key(Path::new(&h.path)))
        });
        self.workspace.editing_path = false;
        self.workspace.show_actions = false;
        self.workspace.mode = mode;
        self.workspace.pending_hit = pending;
        self.workspace.location = PathBuf::from(display_path(&path));
        self.workspace.operation = None;
        self.update_location_input(cx);
        if mode == ViewMode::Usage {
            self.workspace.usage_focus = focus.clone();
        }
        if mode == ViewMode::Usage {
            if !self.show_scanned_directory(focus.clone(), cx) {
                self.workspace.status = "待扫描 · 只有明确开始才会读取占用".into();
                self.set_workspace_rows(vec![], None, true, cx);
            }
            cx.notify();
            return;
        }
        if let Some((rows, cap)) = self.workspace.cache.get(&key(&path)).cloned() {
            self.workspace.listing = false;
            self.workspace.status = format!(
                "{} 项{} · 已保留目录上下文",
                rows.len(),
                if cap { "（已达安全上限）" } else { "" }
            );
            self.remember_directory(cx);
            self.set_workspace_rows(rows, focus, false, cx);
            return;
        }
        self.workspace.listing = true;
        self.workspace.status = "读取目录中 · 搜索仍可使用".into();
        self.workspace.rows.clear();
        self.workspace.all_rows.clear();
        self.workspace.selected_rows.clear();
        self.workspace.selected = None;
        self.workspace.table.update(cx, |t, cx| {
            t.delegate_mut().hits.clear();
            t.refresh(cx);
        });
        self.preview_active_selection(cx);
        let cancel = self.workspace.list_cancel.clone();
        let show_hidden = self.workspace.show_hidden;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    browser::list_with_hidden(&path, &cancel, show_hidden).map(
                        |(path, mut rows, cap)| {
                            for h in &mut rows {
                                h.path = display_path(Path::new(&h.path));
                            }
                            (path, rows, cap)
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                if this.workspace.list_generation != generation {
                    return;
                }
                this.workspace.listing = false;
                match result {
                    Ok((path, rows, cap)) => {
                        this.workspace.location = PathBuf::from(display_path(&path));
                        this.update_location_input(cx);
                        this.remember_directory(cx);
                        if this.workspace.cache.len() >= 4 {
                            this.workspace.cache.clear();
                        }
                        this.workspace.cache.insert(key(&path), (rows.clone(), cap));
                        this.workspace.status = format!(
                            "{} 项{} · 选中预览，双击目录进入",
                            rows.len(),
                            if cap {
                                "（达到 100000 项安全上限）"
                            } else {
                                ""
                            }
                        );
                        if this.workspace.mode != ViewMode::Search {
                            this.set_workspace_rows(rows, focus, false, cx);
                        } else {
                            this.workspace.selected_rows.clear();
                            this.workspace.rows = rows;
                        }
                    }
                    Err(e) => this.workspace.status = format!("目录读取失败：{e}"),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
    pub(super) fn workspace_history(&mut self, forward: bool, cx: &mut Context<Self>) {
        let current = self.current_route();
        let Some(route) = self.workspace.history.travel(current, forward) else {
            return;
        };
        if route.mode == ViewMode::Search {
            self.workspace.mode = ViewMode::Search;
            self.workspace.operation = None;
            self.preview_active_selection(cx);
        } else {
            self.workspace_navigate(route.path, route.mode, route.selected, false, cx);
        }
    }
    pub(super) fn workspace_up(&mut self, cx: &mut Context<Self>) {
        let path = if self.workspace.mode == ViewMode::Search {
            self.active_workspace_hit()
                .and_then(|h| PathBuf::from(h.path).parent().map(Path::to_path_buf))
        } else {
            self.workspace.location.parent().map(Path::to_path_buf)
        };
        if let Some(p) = path {
            let focus = Some(display_path(&self.workspace.location));
            let mode = if self.workspace.mode == ViewMode::Search {
                ViewMode::Directory
            } else {
                self.workspace.mode
            };
            self.workspace_navigate(p, mode, focus, true, cx);
        }
    }
    pub(super) fn workspace_refresh(&mut self, cx: &mut Context<Self>) {
        self.reset_selected_identity_baselines();
        self.identity_key = None;
        self.selected_identity = None;
        self.identity_generation = self.identity_generation.wrapping_add(1);
        self.identity_pending = false;
        self.identity_error = None;
        if self.workspace.mode == ViewMode::Search {
            self.run_search(cx);
        } else if self.workspace.mode == ViewMode::Usage {
            self.workspace.scan_status =
                "请使用「开始扫描 / 重新扫描」明确发起；普通刷新不重扫".into();
            cx.notify();
        } else {
            let p = self.workspace.location.clone();
            let focus = self.active_workspace_hit().map(|h| h.path);
            self.workspace.cache.remove(&key(&p));
            self.workspace_navigate(p, ViewMode::Directory, focus, false, cx);
        }
    }
    pub(in crate::ui) fn workspace_analyze_selected(&mut self, cx: &mut Context<Self>) {
        let path = self
            .active_workspace_hit()
            .map(|hit| {
                if hit.is_directory {
                    PathBuf::from(hit.path)
                } else {
                    PathBuf::from(hit.path)
                        .parent()
                        .unwrap_or(Path::new("C:\\"))
                        .to_path_buf()
                }
            })
            .unwrap_or_else(|| self.workspace.location.clone());
        self.workspace.usage_focus = None;
        self.workspace_navigate(path, ViewMode::Usage, None, true, cx);
        // Root preparation is not permission to start scanning.
    }
    pub(super) fn workspace_reveal(&mut self, cx: &mut Context<Self>) {
        if let Some(h) = self.active_workspace_hit() {
            let p = PathBuf::from(&h.path);
            let parent = p.parent().unwrap_or(Path::new("C:\\")).to_path_buf();
            self.workspace_navigate(parent, ViewMode::Directory, Some(h.path), true, cx);
        }
    }
    pub(super) fn workspace_enter(&mut self, cx: &mut Context<Self>) {
        if let Some(h) = self.active_workspace_hit() {
            if h.is_directory {
                let mode = if self.workspace.mode == ViewMode::Usage {
                    ViewMode::Usage
                } else {
                    ViewMode::Directory
                };
                self.workspace_navigate(h.path.into(), mode, None, true, cx);
            } else {
                self.activate_system_file(h, cx);
            }
        }
    }
    pub(in crate::ui) fn workspace_select(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.workspace.rows.len() {
            return;
        }
        self.record_single_selection(false, Some(ix));
        self.workspace.selected = Some(ix);
        self.workspace.operation = None;
        self.preview_active_selection(cx);
    }
    pub(super) fn workspace_move_selection(&mut self, key: ResultNavKey, cx: &mut Context<Self>) {
        if let Some(ix) =
            move_result_selection(self.workspace.selected, key, self.workspace.rows.len())
        {
            self.workspace_select(ix, cx);
            self.workspace.table.update(cx, |t, cx| {
                t.set_selected_row(ix, cx);
                t.scroll_to_row(ix, cx);
            });
        }
    }
    fn select_usage_node(&mut self, index: usize, enter: bool, cx: &mut Context<Self>) {
        let Some(scan) = self.workspace.scan.as_ref() else {
            return;
        };
        let Some(n) = scan.nodes.get(index) else {
            return;
        };
        let path = display_path(&n.path);
        let directory = n.directory;
        self.workspace.usage_focus = Some(path.clone());
        if let Some(ix) = self
            .workspace
            .rows
            .iter()
            .position(|h| key(Path::new(&h.path)) == key(Path::new(&path)))
        {
            self.workspace_select(ix, cx);
            self.workspace.table.update(cx, |t, cx| {
                t.set_selected_row(ix, cx);
                t.scroll_to_row(ix, cx);
            });
        }
        if enter && directory {
            self.workspace.usage_focus = None;
            self.workspace_navigate(path.into(), ViewMode::Usage, None, true, cx);
        } else if enter {
            // Explicit preview action reuses the main host; never executes a file from the map.
            self.workspace_reveal(cx);
            self.preview_force = Some(ForceState::Show);
            self.preview_active_selection(cx);
        } else {
            cx.notify();
        }
    }
    fn start_workspace_scan(&mut self, cx: &mut Context<Self>) {
        if self.workspace.op_busy || self.workspace.scanning {
            return;
        }
        self.workspace.scan_cancel.store(true, Ordering::Release);
        self.workspace.scan_cancel = Arc::new(AtomicBool::new(false));
        self.workspace.progress = Arc::new(Progress::default());
        self.workspace.scan_generation = self.workspace.scan_generation.wrapping_add(1);
        let generation = self.workspace.scan_generation;
        let path = self.workspace.location.clone();
        let cancel = self.workspace.scan_cancel.clone();
        let progress = self.workspace.progress.clone();
        self.workspace.scanning = true;
        self.workspace.scan_status = "扫描当前目录 · 搜索可用 · 不跟随链接/挂载".into();
        self.workspace.scan_stale = false;
        cx.spawn(async move|this,cx|{let result=cx.background_spawn(async move{disk_usage::scan(&path,cancel,progress,Limits::default()).map(|s|{let lookup=s.nodes.iter().enumerate().map(|(i,n)|(key(&n.path),i)).collect::<HashMap<_,_>>();(s,lookup)})}).await;this.update(cx,|this,cx|{if this.workspace.scan_generation!=generation{return;}this.workspace.scanning=false;match result{Ok((s,lookup))=>{this.workspace.scan_status=format!("{} · {} 路径 / {} 秒 · 硬链接去重 {} · 跳过边界 {} · 读取错误 {} · 未知占用 {}",if s.cancelled{"已取消，部分结果"}else if s.partial{"部分扫描（不是全盘结论）"}else{"目录扫描完成"},s.nodes.len(),s.elapsed.as_secs(),s.hardlinks,s.skipped_links,s.errors,s.unknown);this.workspace.scan=Some(s);this.workspace.scan_lookup=lookup;if this.workspace.mode==ViewMode::Usage{let focus=this.active_workspace_hit().map(|h|h.path);this.show_scanned_directory(focus,cx);}},Err(e)=>this.workspace.scan_status=format!("扫描失败：{e}")};cx.notify();}).ok();}).detach();
        cx.notify();
    }
    pub(in crate::ui) fn begin_workspace_operation(
        &mut self,
        action: FileAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(hit) = self.active_workspace_hit() else {
            return;
        };
        if !self.ensure_selection_identity(cx) {
            self.workspace.status = "正在核对身份，请稍后重试；不可用项需刷新后重新选择。".into();
            self.file_actions.summary = self.workspace.status.clone();
            cx.notify();
            return;
        }
        self.workspace.source_identity = self.selected_identity;
        self.workspace.source = Some(hit.clone());
        self.workspace.operation = Some(action);
        self.workspace.confirmed = false;
        let text = if action == FileAction::Rename {
            hit.name
        } else {
            Path::new(&hit.path)
                .parent()
                .unwrap_or(Path::new("C:\\"))
                .join(format!("副本-{}", hit.name))
                .to_string_lossy()
                .into_owned()
        };
        self.workspace
            .target
            .update(cx, |i, cx| i.set_value(text, window, cx));
        self.workspace.op_hint = "不会覆盖、不会跟随重解析点；请核对源与目标。".into();
        cx.notify();
    }
    fn execute_workspace_operation(&mut self, cx: &mut Context<Self>) {
        let Some(action) = self.workspace.operation else {
            return;
        };
        let Some(hit) = self.workspace.source.clone() else {
            return;
        };
        let src = PathBuf::from(hit.path);
        let dst = match browser::destination(action, &src, &self.workspace.target.read(cx).value())
        {
            Ok(p) => p,
            Err(e) => {
                self.workspace.op_hint = e;
                cx.notify();
                return;
            }
        };
        if !self.workspace.confirmed {
            self.workspace.confirmed = true;
            self.workspace.op_hint = format!(
                "确认{}：{} → {}。不覆盖已有项；请再次点击确认执行。",
                action.label(),
                src.display(),
                dst.display()
            );
            cx.notify();
            return;
        }
        let Some(source_identity) = self.workspace.source_identity else {
            return;
        };
        self.pause_preview();
        self.workspace.op_busy = true;
        self.workspace.op_cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.workspace.op_cancel.clone();
        self.workspace.op_hint = "正在后台执行；复制可取消，已提交的重命名/移动不可撤回。".into();
        let selected = display_path(&dst);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    crate::file_identity::revalidate(&src, source_identity)?;
                    browser::operate(action, &src, &dst, &cancel)
                })
                .await;
            this.update(cx, |this, cx| {
                this.workspace.op_busy = false;
                match result {
                    Ok(()) => {
                        this.file_actions.summary =
                            format!("{}完成；搜索索引稍后同步。", action.label());
                        this.workspace.op_hint.clear();
                        this.workspace.operation = None;
                        this.workspace.scan_stale = true;
                        this.workspace.cache.clear();
                        if this.workspace.mode == ViewMode::Directory {
                            this.workspace_navigate(
                                this.workspace.location.clone(),
                                ViewMode::Directory,
                                Some(selected),
                                false,
                                cx,
                            );
                        }
                    }
                    Err(e) => {
                        this.workspace.op_hint = format!("未完成：{e}");
                        this.workspace.confirmed = false;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub(super) fn workspace_scan_status(&self) -> String {
        if self.workspace.scanning {
            format!(
                "后台扫描 · {} 路径 · {} 目录 · 已测 {} · 总量未知",
                self.workspace.progress.entries.load(Ordering::Relaxed),
                self.workspace.progress.directories.load(Ordering::Relaxed),
                search_logic::format_size(
                    self.workspace.progress.allocated.load(Ordering::Relaxed),
                    false
                )
            )
        } else if self.workspace.scan_stale {
            format!("缓存可能过期 · {}", self.workspace.scan_status)
        } else {
            self.workspace.scan_status.clone()
        }
    }

    pub(super) fn render_workspace_title(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let tokens = Self::tokens(cx);
        let brand_width = (window.viewport_size().width.as_f32() / 2. - 96.).max(64.);
        let settings = Button::new("titlebar-settings")
            .ghost()
            .icon(IconName::Settings)
            .accessibility_label("设置")
            .tooltip("设置 · Ctrl+,")
            .w(px(36.))
            .h(px(32.))
            .mr_2()
            .on_click(cx.listener(|this, _, window, cx| this.open_settings_page(window, cx)));
        crate::ui::chrome::title_bar_with_tools(
            h_flex()
                .h_full()
                .child(
                    div()
                        .w(px(brand_width))
                        .px_3()
                        .h_full()
                        .flex()
                        .items_center()
                        .window_control_area(WindowControlArea::Drag)
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(tokens.text_primary)
                                .child("FindX"),
                        ),
                )
                .children(
                    [
                        (ViewMode::Search, "搜索"),
                        (ViewMode::Directory, "文件"),
                        (ViewMode::Usage, "占用"),
                    ]
                    .into_iter()
                    .map(|(mode, label)| {
                        let active = self.settings_page.is_none() && self.workspace.mode == mode;
                        div()
                            .h_full()
                            .border_b_2()
                            .border_color(if active {
                                tokens.accent
                            } else {
                                tokens.surface
                            })
                            .child(
                                Button::new(SharedString::from(format!("mode-{label}")))
                                    .ghost()
                                    .rounded_none()
                                    .w(px(64.))
                                    .h_full()
                                    .label(label)
                                    .text_color(if active {
                                        tokens.text_primary
                                    } else {
                                        tokens.text_secondary
                                    })
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.request_mode(mode, window, cx)
                                    })),
                            )
                    }),
                ),
            settings,
            window,
            cx,
        )
    }

    pub(super) fn render_workspace_actions(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Self::tokens(cx);
        let entity = cx.entity();
        v_flex()
            .id("workspace-actions-menu")
            .absolute()
            .top(px(76.))
            .right(px(8.))
            .w(px(218.))
            .p_1()
            .gap_1()
            .bg(tokens.surface_raised)
            .border_1()
            .border_color(tokens.border)
            .rounded_md()
            .shadow_md()
            .occlude()
            .on_mouse_down_out({
                let e = entity.clone();
                move |_, _, cx| {
                    e.update(cx, |this, cx| {
                        this.workspace.show_actions = false;
                        cx.notify();
                    })
                }
            })
            .child(
                Button::new("auto-fit-columns")
                    .ghost()
                    .label("自动适应列宽")
                    .selected(if self.workspace.mode == ViewMode::Search {
                        self.session.read(cx).prefs.search_fit_columns
                    } else {
                        self.session.read(cx).prefs.browser_fit_columns
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        let search = this.workspace.mode == ViewMode::Search;
                        this.session.update(cx, |session, cx| {
                            if search {
                                session.prefs.search_fit_columns =
                                    !session.prefs.search_fit_columns;
                            } else {
                                session.prefs.browser_fit_columns =
                                    !session.prefs.browser_fit_columns;
                            }
                            crate::session::persist_prefs(session.prefs.clone(), cx);
                        });
                        this.workspace.show_actions = false;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("explicit-locate")
                    .ghost()
                    .label("在文件中定位")
                    .disabled(self.active_workspace_hit().is_none())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.workspace.show_actions = false;
                        this.workspace_reveal(cx);
                    })),
            )
            .child(
                Button::new("explicit-analyze")
                    .ghost()
                    .label("分析此目录 / 所在目录")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.workspace.show_actions = false;
                        this.workspace_analyze_selected(cx);
                    })),
            )
            .child(
                Button::new("explicit-scope-search")
                    .ghost()
                    .label("在此范围搜索（含子目录）")
                    .on_click(cx.listener(|this, _, window, cx| {
                        let path = this
                            .active_workspace_hit()
                            .map(|hit| {
                                if hit.is_directory {
                                    PathBuf::from(hit.path)
                                } else {
                                    PathBuf::from(hit.path)
                                        .parent()
                                        .unwrap_or(Path::new("C:\\"))
                                        .to_path_buf()
                                }
                            })
                            .unwrap_or_else(|| this.workspace.location.clone());
                        this.workspace.show_actions = false;
                        this.begin_scoped_search(path, window, cx);
                    })),
            )
            .children(
                [FileAction::Copy, FileAction::Move, FileAction::Rename]
                    .into_iter()
                    .map(|action| {
                        let e = entity.clone();
                        Button::new(SharedString::from(format!("op-{}", action.label())))
                            .ghost()
                            .label(action.label())
                            .disabled(
                                self.workspace.op_busy
                                    || self.workspace.scanning
                                    || self.active_workspace_hit().is_none(),
                            )
                            .on_click(move |_, w, cx| {
                                e.update(cx, |this, cx| {
                                    this.workspace.show_actions = false;
                                    this.begin_workspace_operation(action, w, cx);
                                })
                            })
                    }),
            )
            .child({
                let e = entity.clone();
                Button::new("workspace-refresh")
                    .ghost()
                    .label("刷新")
                    .on_click(move |_, _, cx| {
                        e.update(cx, |this, cx| {
                            this.workspace.show_actions = false;
                            this.workspace_refresh(cx);
                        })
                    })
            })
    }
    pub(super) fn render_preview_separator(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let tokens = Self::tokens(cx);
        let _ = window;
        div()
            .id("preview-width-handle")
            .accessibility_id("preview-width-handle")
            .role(gpui_kit::accesskit::Role::Splitter)
            .aria_label("调整预览宽度")
            .w(px(6.))
            .h_full()
            .flex_shrink_0()
            .cursor_col_resize()
            .bg(tokens.border_soft)
            .hover(|s| s.bg(tokens.accent))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, w, cx| {
                    this.preview_resize =
                        Some((event.position.x.as_f32(), this.actual_preview_width));
                    crate::ui::chrome::capture_pointer(w);
                    w.prevent_default();
                    cx.stop_propagation();
                }),
            )
    }
    pub(super) fn resize_preview_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if let Some((start, width)) = self.preview_resize {
            let next = (width + start - event.position.x.as_f32())
                .clamp(240., 600.)
                .min((self.last_content_width - 326.).max(240.));
            self.session
                .update(cx, |s, _| s.prefs.preview_width = Some(next));
            cx.notify();
        }
    }
    pub(super) fn finish_preview_resize(&mut self, cx: &mut Context<Self>) {
        if self.preview_resize.take().is_some() {
            let handle = self.workspace.window;
            let _ = handle.update(cx, |_, w, _| crate::ui::chrome::release_pointer(w));
            let prefs = self.session.read(cx).prefs.clone();
            crate::session::persist_prefs(prefs, cx);
            cx.notify();
        }
    }
    pub(super) fn render_workspace_center(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.workspace.mode {
            ViewMode::Search => DataTable::new(&self.table)
                .scrollbar_visible(
                    true,
                    self.table.read(cx).delegate().widths.iter().sum::<f32>()
                        > self.table_viewport_width + 0.5,
                )
                .bordered(false)
                .into_any_element(),
            ViewMode::Directory => self.render_directory_table(cx),
            ViewMode::Usage => self.render_workspace_usage(cx).into_any_element(),
        }
    }
    pub(super) fn render_workspace_operation(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Self::tokens(cx);
        let Some(action) = self.workspace.operation else {
            return div().into_any_element();
        };
        let entity = cx.entity();
        v_flex()
            .w_full()
            .flex_shrink_0()
            .p_2()
            .gap_1()
            .bg(tokens.surface_raised)
            .border_t_1()
            .border_color(tokens.border)
            .child(div().text_sm().child(format!(
                    "{} · {}",
                    action.label(),
                    self.workspace
                        .source
                        .as_ref()
                        .map(|h| h.path.as_str())
                        .unwrap_or("")
                )))
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.workspace.target)),
                    )
                    .child({
                        let e = entity.clone();
                        Button::new("confirm-workspace-operation")
                            .label(if self.workspace.confirmed {
                                "确认执行"
                            } else {
                                "检查目标"
                            })
                            .disabled(self.workspace.op_busy)
                            .on_click(move |_, _, cx| {
                                e.update(cx, |this, cx| this.execute_workspace_operation(cx))
                            })
                    })
                    .child({
                        let e = entity.clone();
                        Button::new("cancel-workspace-operation")
                            .label("取消")
                            .on_click(move |_, _, cx| {
                                e.update(cx, |this, cx| {
                                    this.workspace.op_cancel.store(true, Ordering::Release);
                                    if !this.workspace.op_busy {
                                        this.workspace.operation = None;
                                    }
                                    cx.notify();
                                })
                            })
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(tokens.warning)
                    .child(self.workspace.op_hint.clone()),
            )
            .into_any_element()
    }
}
#[cfg(test)]
mod tests {
    use super::{History, Route, ViewMode, display_path, key};
    use core::prelude::v1::test;
    use std::path::Path;
    #[test]
    fn history_keeps_search_route_and_selection() {
        let a = Route {
            mode: ViewMode::Search,
            path: "C:\\".into(),
            selected: Some("C:\\public.docx".into()),
        };
        let b = Route {
            mode: ViewMode::Directory,
            path: "C:\\public".into(),
            selected: Some("C:\\public\\fixture.txt".into()),
        };
        let c = Route {
            mode: ViewMode::Usage,
            path: "C:\\public".into(),
            selected: None,
        };
        let mut h = History::default();
        h.push(a.clone());
        h.push(b.clone());
        assert_eq!(h.travel(c.clone(), false), Some(b.clone()));
        assert_eq!(h.travel(b, false), Some(a.clone()));
        assert_eq!(h.travel(a, true).unwrap().mode, ViewMode::Directory);
    }
    #[test]
    fn windows_keys_match_live_index_and_canonical_paths() {
        assert_eq!(
            key(Path::new(r"\\?\C:\Users\Public")),
            key(Path::new(r"c:\users\public\"))
        );
        assert_eq!(
            display_path(Path::new(r"\\?\UNC\server\public")),
            r"\\server\public"
        );
    }
    #[test]
    fn new_navigation_discards_forward_branch() {
        let a = Route {
            mode: ViewMode::Search,
            path: "C:\\".into(),
            selected: None,
        };
        let mut h = History::default();
        h.push(a.clone());
        h.travel(a.clone(), false);
        h.push(a);
        assert!(h.forward.is_empty());
    }
}

#[cfg(test)]
mod interaction_contract_tests {
    use super::*;
    use core::prelude::v1::test;
    #[test]
    fn auto_fit_preserves_size_time_and_never_makes_name_path_unreadable() {
        let natural = [400., 600., 90., 164.];
        let fit = fit_columns(natural, [160., 160., 72., 150.], [0, 1], 800.);
        assert!(fit.iter().sum::<f32>() <= 800.);
        assert_eq!(fit[2], 90.);
        assert_eq!(fit[3], 164.);
        assert!(fit[0] >= 160. && fit[1] >= 160.);
        assert_eq!(natural, [400., 600., 90., 164.]);
    }
    #[test]
    fn compact_search_keeps_filters_preview_and_readable_columns_at_1024() {
        let (sidebar, preview, pane) = panel_budget(
            1016.,
            SIDEBAR_WIDTH_PX,
            MIN_WIDTHS.iter().sum(),
            true,
            true,
            320.,
        );
        assert!(sidebar && preview);
        assert!(pane >= 240. && pane <= 320.);
        assert!(1016. - SIDEBAR_WIDTH_PX - pane - 6. >= MIN_WIDTHS.iter().sum::<f32>());
        let (sidebar, preview, pane) = panel_budget(
            720.,
            SIDEBAR_WIDTH_PX,
            MIN_WIDTHS.iter().sum(),
            true,
            true,
            320.,
        );
        assert!(!sidebar && preview && pane >= 240.);
        assert!(720. - pane - 6. >= 300.);
    }
    #[test]
    fn too_narrow_keeps_minimums_and_requires_horizontal_scroll() {
        assert_eq!(
            fit_columns(
                [400., 600., 90., 164.],
                [160., 160., 72., 150.],
                [0, 1],
                300.
            ),
            [160., 160., 72., 150.]
        );
    }
    #[test]
    fn preview_identity_normalizes_path_and_filetime_but_not_content_version() {
        let seconds = 1_790_000_000u64;
        let filetime = (seconds + 11_644_473_600) * 10_000_000;
        assert_eq!(
            preview_stamp(r"\\?\C:\A\doc.pdf", 20, filetime),
            preview_stamp("c:/a/doc.pdf", 20, seconds)
        );
        assert_ne!(
            preview_stamp(r"C:\A\doc.pdf", 20, filetime),
            preview_stamp("c:/a/doc.pdf", 21, seconds)
        );
    }
}

impl MainWindow {
    fn remember_directory(&mut self, cx: &mut Context<Self>) {
        let path = display_path(&self.workspace.location);
        self.session.update(cx, |s, cx| {
            s.prefs
                .browser_recent
                .retain(|p| key(Path::new(p)) != key(Path::new(&path)));
            s.prefs.browser_recent.insert(0, path);
            s.prefs.browser_recent.truncate(12);
            crate::session::persist_prefs(s.prefs.clone(), cx);
        });
    }
    fn toggle_favorite(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.session.update(cx, |s, cx| {
            let k = key(&path);
            if s.prefs
                .browser_favorites
                .iter()
                .any(|p| key(Path::new(p)) == k)
            {
                s.prefs.browser_favorites.retain(|p| key(Path::new(p)) != k);
            } else if s.prefs.browser_favorites.len() < 24 {
                s.prefs.browser_favorites.push(display_path(&path));
            }
            crate::session::persist_prefs(s.prefs.clone(), cx);
        });
        cx.notify();
    }
    pub(super) fn set_show_hidden(&mut self, show: bool, cx: &mut Context<Self>) {
        self.workspace.show_hidden = show;
        self.workspace.cache.clear();
        self.session.update(cx, |s, cx| {
            s.prefs.browser_show_hidden = show;
            crate::session::persist_prefs(s.prefs.clone(), cx);
        });
        self.workspace_refresh(cx);
    }
}

impl MainWindow {
    pub(in crate::ui) fn analyze_directory(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.workspace.usage_focus = None;
        self.workspace_navigate(path, ViewMode::Usage, None, true, cx);
    }
}
