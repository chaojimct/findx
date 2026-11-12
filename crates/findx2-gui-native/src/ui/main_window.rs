#[path = "file_actions.rs"]
mod file_actions;
use file_actions::FileActions;
#[path = "file_context.rs"]
mod file_context;
#[path = "search_session.rs"]
mod search_session;
use search_session::SearchSnapshot;
#[path = "workspace.rs"]
mod workspace;
use gpui_kit::component::Disableable as _;
use std::time::Duration;
use workspace::{ViewMode, Workspace};

use findx2_ipc::SearchHitDto;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Icon, IconName, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::result_table::{DEFAULT_WIDTHS, MIN_WIDTHS, ResultsTable};
use crate::preview::{self, PreviewKind, PreviewState};
use crate::search_logic::{
    self, ForceState, PREVIEW_DEFAULT_WIDTH_PX, ResultNavKey, SIDEBAR_WIDTH_PX, TimeFilter,
    TypeFilter, explain_panels, move_result_selection,
};
use crate::session::AppSession;
use crate::status_logic::{self, IndexStatusView};
use crate::theme::FindxTokens;
use gpui_kit::component::table::{DataTable, TableEvent, TableState};

actions!(
    findx_native,
    [
        SelectNextResult,
        SelectPrevResult,
        SelectFirstResult,
        SelectLastResult,
        OpenSelected,
        RevealSelected,
        ToggleDiag,
        FocusSearch,
        OpenSettings,
        ShowFileMenu,
        ContextOpen,
        ContextOpenWith,
        ContextLocate,
        ContextCopyPath,
        ContextCopy,
        ContextCut,
        ContextPaste,
        ContextRename,
        ContextProperties,
        ContextAnalyze,
        ContextScope,
        ContextSelectAll,
        ContextAutoFit,
        WorkspaceBack,
        WorkspaceForward,
        WorkspaceUp,
        WorkspaceFocusPath,
        WorkspaceRefresh,
        DirectoryNewTab,
        DirectoryCloseTab,
        DirectoryNextTab,
        DirectoryPreviousTab,
    ]
);

const PAGE_SIZE: usize = 200;
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(60);

pub struct MainWindow {
    file_actions: FileActions,
    search_selection: std::collections::BTreeSet<usize>,
    focus: FocusHandle,
    identity_key: Option<(String, u64, u64)>,
    identity_generation: u64,
    selected_identity: Option<crate::file_identity::FileStamp>,
    identity_pending: bool,
    identity_error: Option<String>,
    pending_open: Option<SearchHitDto>,
    run_pending: Option<(SearchHitDto, crate::file_identity::FileStamp)>,
    workspace: Workspace,
    settings_page: Option<Entity<super::settings_window::SettingsWindow>>,
    settings_pending_mode: Option<ViewMode>,
    preview_resize: Option<(f32, f32)>,
    actual_preview_width: f32,
    table: Entity<TableState<ResultsTable>>,
    page_loading: bool,
    page_error: bool,
    _page_task: Option<Task<()>>,
    session: Entity<AppSession>,
    search: Entity<InputState>,
    search_scope: Option<std::path::PathBuf>,
    global_search: Option<SearchSnapshot>,
    type_filter: TypeFilter,
    time_filter: TimeFilter,
    path_prefix: String,
    size_min: String,
    size_max: String,
    hits: Vec<SearchHitDto>,
    selected: Option<usize>,
    match_total: u32,
    search_status: SharedString,
    search_gen: u64,
    status_gen: u64,
    preview_diagnostics: bool,
    preview: PreviewState,
    preview_memo: std::collections::HashMap<(String, u64, u64), crate::preview_worker::PdfOptions>,
    preview_gen: u64,
    preview_cancel: crate::preview_worker::Cancel,
    pdf_options: crate::preview_worker::PdfOptions,
    pdf_count: u32,
    office_text_only: bool,
    preview_file: Option<String>,
    loading: bool,
    sidebar_force: Option<ForceState>,
    preview_force: Option<ForceState>,
    last_content_width: f32,
    table_viewport_width: f32,
    last_diag: String,
    key_traces: Vec<String>,
    selection_resets: Vec<String>,
    _subscriptions: Vec<Subscription>,
    _search_task: Option<Task<()>>,
    _status_task: Option<Task<()>>,
    _preview_task: Option<Task<()>>,
}

impl MainWindow {
    fn open_settings_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_path_edit(cx);
        self.dismiss_address_menu(cx);
        self.dismiss_file_menu(cx);
        if self.settings_page.is_some() {
            return;
        }
        self.pause_preview();
        self.workspace.show_actions = false;
        let session = self.session.clone();
        let page =
            cx.new(|cx| super::settings_window::SettingsWindow::new_embedded(session, window, cx));
        self._subscriptions.push(
            cx.subscribe_in(&page, window, |this, _, event, window, cx| {
                use super::settings_window::SettingsEvent;
                match event {
                    SettingsEvent::Return => {
                        this.settings_page = None;
                        if let Some(mode) = this.settings_pending_mode.take() {
                            this.workspace_switch(mode, cx);
                        }
                        cx.notify();
                    }
                    SettingsEvent::CloseApp => window.remove_window(),
                    SettingsEvent::Stay => {
                        this.settings_pending_mode = None;
                        cx.notify();
                    }
                }
            }),
        );
        self.settings_page = Some(page);
        cx.notify();
    }

    fn request_mode(&mut self, mode: ViewMode, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(page) = self.settings_page.clone() {
            self.settings_pending_mode = Some(mode);
            page.update(cx, |page, cx| page.request_leave(window, cx));
        } else {
            self.workspace_switch(mode, cx);
            if mode == ViewMode::Search {
                self.search.update(cx, |input, cx| input.focus(window, cx));
            } else {
                window.focus(&self.focus, cx);
            }
        }
    }

    pub fn new(session: Entity<AppSession>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.bind_keys([
            KeyBinding::new("shift-f10", ShowFileMenu, Some("FindXMain")),
            KeyBinding::new("menu", ShowFileMenu, Some("FindXMain")),
            KeyBinding::new("ctrl-c", ContextCopy, Some("FindXMain")),
            KeyBinding::new("ctrl-x", ContextCut, Some("FindXMain")),
            KeyBinding::new("ctrl-v", ContextPaste, Some("FindXMain")),
            KeyBinding::new("ctrl-shift-c", ContextCopyPath, Some("FindXMain")),
            KeyBinding::new("f2", ContextRename, Some("FindXMain")),
            KeyBinding::new("alt-enter", ContextProperties, Some("FindXMain")),
            KeyBinding::new("ctrl-a", ContextSelectAll, Some("FindXMain")),
            KeyBinding::new("alt-left", WorkspaceBack, Some("FindXMain")),
            KeyBinding::new("alt-right", WorkspaceForward, Some("FindXMain")),
            KeyBinding::new("alt-up", WorkspaceUp, Some("FindXMain")),
            KeyBinding::new("ctrl-l", WorkspaceFocusPath, Some("FindXMain")),
            KeyBinding::new("f5", WorkspaceRefresh, Some("FindXMain")),
            KeyBinding::new("ctrl-t", DirectoryNewTab, Some("FindXMain")),
            KeyBinding::new("ctrl-w", DirectoryCloseTab, Some("FindXMain")),
            KeyBinding::new("ctrl-tab", DirectoryNextTab, Some("FindXMain")),
            KeyBinding::new("ctrl-shift-tab", DirectoryPreviousTab, Some("FindXMain")),
            KeyBinding::new("down", SelectNextResult, Some("FindXMain")),
            KeyBinding::new("up", SelectPrevResult, Some("FindXMain")),
            KeyBinding::new("home", SelectFirstResult, Some("FindXMain")),
            KeyBinding::new("end", SelectLastResult, Some("FindXMain")),
            KeyBinding::new("enter", OpenSelected, Some("FindXMain")),
            KeyBinding::new("ctrl-enter", RevealSelected, Some("FindXMain")),
            KeyBinding::new("ctrl-shift-d", ToggleDiag, Some("FindXMain")),
            KeyBinding::new("ctrl-f", FocusSearch, Some("FindXMain")),
            KeyBinding::new("ctrl-comma", OpenSettings, Some("FindXMain")),
        ]);

        let search = cx.new(|cx| InputState::new(window, cx).placeholder("搜索文件…"));

        let widths = session
            .read(cx)
            .prefs
            .column_widths
            .unwrap_or(DEFAULT_WIDTHS);
        let widths = std::array::from_fn(|i| {
            if widths[i].is_finite() {
                widths[i].clamp(MIN_WIDTHS[i], 1600.)
            } else {
                DEFAULT_WIDTHS[i]
            }
        });
        let owner = cx.weak_entity();
        let table = cx.new(|cx| {
            TableState::new(
                ResultsTable {
                    hits: Vec::new(),
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
        let browser_widths = session
            .read(cx)
            .prefs
            .browser_column_widths
            .unwrap_or(super::browser_table::DEFAULT_WIDTHS);
        let browser_widths = if session.read(cx).prefs.browser_column_widths.is_some() {
            super::browser_table::migrate_widths(
                browser_widths,
                session.read(cx).prefs.browser_column_schema,
            )
        } else {
            browser_widths
        };
        let mut workspace = Workspace::new(browser_widths, window, cx);
        workspace.show_hidden = session.read(cx).prefs.browser_show_hidden;
        let mut this = Self {
            file_actions: FileActions::default(),
            search_selection: Default::default(),
            focus: cx.focus_handle(),
            identity_key: None,
            identity_generation: 0,
            selected_identity: None,
            identity_pending: false,
            identity_error: None,
            pending_open: None,
            run_pending: None,
            workspace,
            settings_page: None,
            settings_pending_mode: None,
            preview_resize: None,
            actual_preview_width: PREVIEW_DEFAULT_WIDTH_PX,
            table: table.clone(),
            page_loading: false,
            page_error: false,
            _page_task: None,
            session: session.clone(),
            search: search.clone(),
            search_scope: None,
            global_search: None,
            type_filter: TypeFilter::All,
            time_filter: TimeFilter::All,
            path_prefix: String::new(),
            size_min: String::new(),
            size_max: String::new(),
            hits: Vec::new(),
            selected: None,
            match_total: 0,
            search_status: "就绪".into(),
            search_gen: 0,
            status_gen: 0,
            preview_diagnostics: false,
            preview: PreviewState::empty(),
            preview_memo: std::collections::HashMap::new(),
            preview_gen: 0,
            preview_cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            pdf_options: Default::default(),
            pdf_count: 0,
            office_text_only: false,
            preview_file: session.read(cx).preview_file.clone(),
            loading: false,
            sidebar_force: None,
            preview_force: None,
            last_content_width: 1100.0,
            table_viewport_width: 1100.0,
            last_diag: String::new(),
            key_traces: Vec::new(),
            selection_resets: Vec::new(),
            _subscriptions: Vec::new(),
            _search_task: None,
            _status_task: None,
            _preview_task: None,
        };

        this.subscribe_search_input(search.clone(), window, cx);
        this._subscriptions
            .push(cx.observe(&session, |this, _, cx| {
                cx.notify();
                let _ = this;
            }));

        this.subscribe_results_table(table.clone(), cx);
        let session_for_appearance = session.clone();
        this._subscriptions
            .push(window.observe_window_appearance(move |window, cx| {
                let dark = matches!(
                    window.appearance(),
                    WindowAppearance::Dark | WindowAppearance::VibrantDark
                );
                session_for_appearance.update(cx, |s, cx| {
                    s.system_dark = dark;
                    if s.preview_theme
                        .map(|(_, pref)| pref)
                        .unwrap_or(s.prefs.theme)
                        == crate::settings_logic::UiThemePref::System
                    {
                        s.apply_theme(cx);
                        cx.refresh_windows();
                    }
                    cx.notify();
                });
            }));
        let closing = cx.entity();
        window.on_window_should_close(cx, move |window, cx| {
            closing.update(cx, |this, cx| {
                if this.cancel_active_batch_before_close(cx) {
                    return false;
                }
                if let Some(settings) = this.settings_page.clone() {
                    settings.update(cx, |page, cx| page.request_app_close(window, cx))
                } else {
                    true
                }
            })
        });
        this.init_workspace(window, cx);
        if let Some(path) = crate::isolation::isolated_usage_open_path(
            this.session.read(cx).live,
            std::env::var_os("FINDX_NATIVE_USAGE_OPEN")
                .map(std::path::PathBuf::from)
                .as_deref(),
        ) {
            this.workspace_navigate(path, ViewMode::Usage, None, true, cx);
            this.start_workspace_scan(true, cx);
        }
        this.schedule_status_poll(cx);
        if this.preview_file.is_some() {
            this.request_preview(cx);
        }
        this
    }

    fn subscribe_search_input(
        &mut self,
        input: Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self._subscriptions.push(cx.subscribe_in(
            &input,
            window,
            |this, input, event, window, cx| {
                if input.entity_id() != this.search.entity_id() {
                    return;
                }
                if let InputEvent::Change = event {
                    this.schedule_search(window, cx);
                }
            },
        ));
    }
    fn subscribe_results_table(
        &mut self,
        table: Entity<TableState<ResultsTable>>,
        cx: &mut Context<Self>,
    ) {
        self._subscriptions
            .push(cx.subscribe(&table, |this, table, event: &TableEvent, cx| {
                if table.entity_id() != this.table.entity_id() {
                    return;
                }
                match event {
                    TableEvent::SelectRow(ix) => this.table_selected_event(true, *ix, cx),
                    TableEvent::DoubleClickedRow(ix) => {
                        this.set_selected(Some(*ix), cx);
                        this.open_selected(cx);
                    }
                    TableEvent::ColumnWidthsChanged(widths) if widths.len() == 4 => {
                        let widths =
                            std::array::from_fn(|i| widths[i].as_f32().clamp(MIN_WIDTHS[i], 1600.));
                        this.session.update(cx, |s, cx| {
                            s.prefs.column_widths = Some(widths);
                            s.prefs.search_fit_columns = false;
                            crate::session::persist_prefs(s.prefs.clone(), cx);
                        });
                        // Keep delegate widths current so a later header refresh cannot undo a drag.
                        this.table
                            .update(cx, |t, _| t.delegate_mut().widths = widths);
                    }
                    _ => {}
                }
            }));
    }

    fn current_query(&self, cx: &App) -> String {
        let raw = self.search.read(cx).value().to_string();
        search_logic::build_full_query(
            &raw,
            self.type_filter,
            self.time_filter,
            search_logic::today_local(),
            &self.path_prefix,
            &self.size_min,
            &self.size_max,
        )
    }

    fn schedule_search(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.mode != ViewMode::Search {
            self.workspace_switch(ViewMode::Search, cx);
        }
        self.search_gen = self.search_gen.wrapping_add(1);
        let generation = self.search_gen;
        self._search_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            this.update(cx, |this, cx| {
                if this.search_gen == generation {
                    this.run_search(cx);
                }
            })
            .ok();
        }));
    }

    fn run_search(&mut self, cx: &mut Context<Self>) {
        let query = self.current_query(cx);
        if query.trim().is_empty() {
            self.record_reset("查询变空");
            self.hits.clear();
            self.match_total = 0;
            self.search_selection.clear();
            self.selected = None;
            self.loading = false;
            self.search_status = "就绪".into();
            self.request_preview(cx);
            self.sync_table(true, cx);
            cx.notify();
            return;
        }
        self.page_loading = false;
        self.page_error = false;
        self._page_task = None;
        self.hits.clear();
        self.match_total = 0;
        self.sync_table(true, cx);
        self.record_reset("重新查询");
        self.search_selection.clear();
        self.selected = None;
        self.request_preview(cx);
        self.loading = true;
        self.search_status = "搜索中…".into();
        self.search_gen = self.search_gen.wrapping_add(1);
        let generation = self.search_gen;
        let (endpoint, pinyin, limit, ipc) = self.session.read_with(cx, |s, _| {
            (
                s.endpoint.clone(),
                s.settings.pinyin_default,
                s.settings.search_limit.max(1).min(PAGE_SIZE as u32) as usize,
                s.ipc.clone(),
            )
        });
        // PAGE_SIZE is the first-page fetch; search_limit is the ceiling shown in status.
        let fetch = PAGE_SIZE.min(limit.max(1));
        let rx = ipc.search(endpoint, query, pinyin, fetch, 0);
        self._search_task = Some(cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("IPC 通道已关闭".into()));
            this.update(cx, |this, cx| {
                if this.search_gen != generation {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(out) => {
                        this.hits = out
                            .hits
                            .into_iter()
                            .map(|mut hit| {
                                hit.path = workspace::display_path(std::path::Path::new(&hit.path));
                                hit
                            })
                            .collect();
                        this.match_total = out.total;
                        let shown = this.hits.len();
                        let total = out.total as usize;
                        let head = if total == 0 {
                            "无匹配".to_string()
                        } else if shown >= total {
                            format!("匹配 {} 条", total)
                        } else {
                            format!("匹配 {total} 条 · 已显示 {shown} 条")
                        };
                        this.search_status = if out.elapsed_ms > 0 {
                            format!("{head} · {} ms", out.elapsed_ms).into()
                        } else {
                            head.into()
                        };
                    }
                    Err(err) => {
                        this.hits.clear();
                        this.match_total = 0;
                        this.record_reset("查询失败");
                        this.search_selection.clear();
                        this.selected = None;
                        this.search_status = format!("错误: {err}").into();
                        this.request_preview(cx);
                    }
                }
                this.sync_table(false, cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn sync_table(&mut self, reset: bool, cx: &mut Context<Self>) {
        let hits = self.hits.clone();
        let more = hits.len() < self.match_total as usize && !self.page_loading && !self.page_error;
        self.table.update(cx, |table, cx| {
            table.delegate_mut().hits = hits;
            table.delegate_mut().more = more;
            if reset {
                table.clear_selection(cx);
                table.scroll_to_row(0, cx);
                table.scroll_to_col(0, cx);
            }
            cx.notify();
        });
    }

    pub(super) fn load_next_page(&mut self, cx: &mut Context<Self>) {
        if self.loading
            || self.page_loading
            || self.page_error
            || self.hits.len() >= self.match_total as usize
        {
            return;
        }
        self.page_loading = true;
        self.sync_table(false, cx);
        let generation = self.search_gen;
        let offset = self.hits.len();
        let query = self.current_query(cx);
        let (endpoint, pinyin, ipc) = self.session.read_with(cx, |s, _| {
            (s.endpoint.clone(), s.settings.pinyin_default, s.ipc.clone())
        });
        let rx = ipc.search(endpoint, query, pinyin, PAGE_SIZE, offset);
        self._page_task = Some(cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("IPC 通道已关闭".into()));
            let _ = this.update(cx, |this, cx| {
                if this.search_gen != generation || this.hits.len() != offset {
                    return;
                }
                this.page_loading = false;
                match result {
                    Ok(out) => {
                        if out.hits.is_empty() {
                            this.match_total = this.hits.len() as u32;
                        } else {
                            this.hits.extend(out.hits.into_iter().map(|mut hit| {
                                hit.path = workspace::display_path(std::path::Path::new(&hit.path));
                                hit
                            }));
                            this.match_total = out.total;
                        }
                        this.search_status = format!(
                            "匹配 {} 条 · 已加载 {} 条",
                            this.match_total,
                            this.hits.len()
                        )
                        .into();
                    }
                    Err(err) => {
                        this.page_error = true;
                        this.search_status =
                            format!("加载下一页失败（重新搜索可重试）: {err}").into();
                    }
                }
                this.sync_table(false, cx);
                cx.notify();
            });
        }));
    }

    fn schedule_status_poll(&mut self, cx: &mut Context<Self>) {
        self.poll_status(cx);
        self._status_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                if this.update(cx, |this, cx| this.poll_status(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    fn poll_status(&mut self, cx: &mut Context<Self>) {
        self.status_gen = self.status_gen.wrapping_add(1);
        let generation = self.status_gen;
        let (endpoint, ipc, backfill) = self.session.read_with(cx, |s, _| {
            (
                s.endpoint.clone(),
                s.ipc.clone(),
                if s.live {
                    true
                } else {
                    s.settings.enable_metadata_backfill
                },
            )
        });
        let rx = ipc.status(endpoint);
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("IPC 通道已关闭".into()));
            this.update(cx, |this, cx| {
                if this.status_gen != generation {
                    return;
                }
                this.session.update(cx, |session, cx| {
                    match result {
                        Ok(resp) => {
                            session.connected = true;
                            session.status_raw = serde_json::to_string_pretty(&resp)
                                .unwrap_or_else(|e| e.to_string());
                            session.status_checked = search_logic::format_time(
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs(),
                            );
                            session.last_error = None;
                            session.index_status = status_logic::summarize_status(&resp, backfill);
                        }
                        Err(err) => {
                            session.connected = false;
                            session.last_error = Some(err.clone());
                            session.index_status = status_logic::disconnected_status(&err);
                        }
                    }
                    cx.notify();
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn record_reset(&mut self, reason: &str) {
        self.selection_resets.push(format!(
            "{reason}@{}",
            self.selected
                .map(|i| i.to_string())
                .unwrap_or_else(|| "null".into())
        ));
        if self.selection_resets.len() > 8 {
            let drop = self.selection_resets.len() - 8;
            self.selection_resets.drain(0..drop);
        }
    }

    fn record_key(&mut self, key: &str, decision: &str, after: Option<usize>) {
        self.key_traces.push(format!(
            "keydown key={key} composing=false decision={decision} before={} after={} n={}",
            self.selected
                .map(|i| i.to_string())
                .unwrap_or_else(|| "null".into()),
            after
                .map(|i| i.to_string())
                .unwrap_or_else(|| "null".into()),
            self.hits.len()
        ));
        if self.key_traces.len() > 20 {
            let drop = self.key_traces.len() - 20;
            self.key_traces.drain(0..drop);
        }
    }

    fn set_selected(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        self.record_single_selection(true, index);
        if self.selected != index {
            if index.is_some() {
                self.preview_file = None;
            }
            self.office_text_only = false;
            self.pdf_count = 0;
            self.selected = index;
            self.preview_active_selection(cx);
        } else {
            self.selected = index;
        }
        cx.notify();
    }

    fn pause_preview(&mut self) {
        self.preview_cancel
            .store(true, std::sync::atomic::Ordering::Release);
        self.preview_gen = self.preview_gen.wrapping_add(1);
        self._preview_task = None;
        self.preview = PreviewState::empty();
    }

    fn request_preview(&mut self, cx: &mut Context<Self>) {
        self.preview_cancel
            .store(true, std::sync::atomic::Ordering::Release);
        self.preview_cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel = self.preview_cancel.clone();
        let options = self.pdf_options;
        let prefer_native = !self.office_text_only;
        self.preview_gen = self.preview_gen.wrapping_add(1);
        let generation = self.preview_gen;
        let visible = self.settings_page.is_none()
            && self.workspace.mode != ViewMode::Usage
            && match self.preview_force {
                Some(ForceState::Hide) => false,
                Some(ForceState::Show) => true,
                None => self.session.read(cx).prefs.preview_open,
            };
        if !visible {
            self._preview_task = None;
            self.preview = PreviewState::empty();
            return;
        }
        let local_file = self.preview_file.is_some() || self.workspace.mode != ViewMode::Search;
        let target = self.active_workspace_hit().or_else(|| {
            self.preview_file.as_ref().map(|path| SearchHitDto {
                entry_idx: 0,
                name: std::path::Path::new(path)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.clone()),
                path: path.clone(),
                size: 0,
                mtime: 0,
                is_directory: false,
                name_highlight: Vec::new(),
            })
        });
        let Some(hit) = target else {
            self.pdf_count = 0;
            self.pdf_options = Default::default();
            self.office_text_only = false;
            self.preview = PreviewState::empty();
            self._preview_task = None;
            return;
        };
        if self.preview_memo.len() > 64 {
            self.preview_memo.clear();
        }
        self.preview_memo.insert(
            workspace::preview_stamp(&hit.path, hit.size, hit.mtime),
            self.pdf_options,
        );
        let same_path = self
            .preview
            .meta
            .as_ref()
            .is_some_and(|meta| meta.path == hit.path);
        let changed_page =
            self.preview.pdf.is_some() && !self.preview.pdf_frame_matches(&hit.path, options.page);
        if !same_path {
            self.pdf_count = 0;
        }
        // Never label the old raster as the new PDF page while a worker is decoding.
        if !same_path || changed_page {
            self.preview = PreviewState::loading(
                generation,
                preview::meta_from_hit(&hit.name, &hit.path, hit.size, hit.mtime, hit.is_directory),
            );
        }
        self._preview_task = Some(cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_spawn(async move {
                    let mut hit = hit;
                    if local_file {
                        if let Ok(meta) = std::fs::metadata(&hit.path) {
                            hit.size = meta.len();
                            hit.mtime = meta
                                .modified()
                                .ok()
                                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                                .map(|t| t.as_secs())
                                .unwrap_or(0);
                            hit.is_directory = meta.is_dir();
                        }
                    }
                    preview::load_preview_with_options(
                        generation,
                        &hit.name,
                        &hit.path,
                        hit.size,
                        hit.mtime,
                        hit.is_directory,
                        options,
                        prefer_native,
                        cancel,
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                if !preview::is_current_generation(this.preview_gen, generation, loaded.generation)
                {
                    return;
                }
                this.pdf_count = loaded.pdf.map(|pdf| pdf.count).unwrap_or(0);
                if let Some(pdf) = loaded.pdf {
                    this.pdf_options.page = pdf.page;
                }
                this.preview = loaded;
                cx.notify();
            })
            .ok();
        }));
    }

    fn move_sel(&mut self, key: ResultNavKey, cx: &mut Context<Self>) {
        if self.workspace.mode != ViewMode::Search {
            self.workspace_move_selection(key, cx);
            return;
        }
        if let Some(index) = move_result_selection(self.selected, key, self.hits.len()) {
            let label = match key {
                ResultNavKey::ArrowDown => "down",
                ResultNavKey::ArrowUp => "up",
                ResultNavKey::Home => "home",
                ResultNavKey::End => "end",
            };
            self.record_key(label, "move", Some(index));
            self.set_selected(Some(index), cx);
            self.table.update(cx, |table, cx| {
                table.set_selected_row(index, cx);
                table.scroll_to_row(index, cx);
            });
        }
    }

    fn open_selected(&mut self, cx: &mut Context<Self>) {
        self.workspace_enter(cx);
    }
    fn reveal_selected(&mut self, cx: &mut Context<Self>) {
        self.workspace_reveal(cx);
    }

    fn toggle_diag(&mut self, cx: &mut Context<Self>) {
        self.session.update(cx, |session, cx| {
            let next = !session.prefs.diag_enabled;
            session.set_diag(next, cx);
        });
        cx.notify();
    }

    fn toggle_panel(
        &mut self,
        kind: search_logic::PanelKind,
        currently_open: bool,
        cx: &mut Context<Self>,
    ) {
        let force = if currently_open {
            Some(ForceState::Hide)
        } else {
            Some(ForceState::Show)
        };
        match kind {
            search_logic::PanelKind::Sidebar => self.sidebar_force = force,
            search_logic::PanelKind::Preview => self.preview_force = force,
        }
        self.session.update(cx, |session, cx| match kind {
            search_logic::PanelKind::Sidebar => session.set_panels(Some(!currently_open), None, cx),
            search_logic::PanelKind::Preview => session.set_panels(None, Some(!currently_open), cx),
        });
        if matches!(kind, search_logic::PanelKind::Preview) {
            if currently_open {
                self.pause_preview();
            } else {
                self.preview_active_selection(cx);
            }
        }
        cx.notify();
    }

    fn tokens(cx: &App) -> FindxTokens {
        if cx.has_global::<FindxTokens>() {
            *cx.global::<FindxTokens>()
        } else {
            FindxTokens::for_theme(crate::settings_logic::EffectiveTheme::Light)
        }
    }

    fn type_icon(filter: TypeFilter) -> Icon {
        Icon::default().path(match filter {
            TypeFilter::All => "icons/files.svg",
            TypeFilter::Folder => "icons/folder.svg",
            TypeFilter::File => "icons/file.svg",
            TypeFilter::Doc => "icons/file-text.svg",
            TypeFilter::Image => "icons/image.svg",
            TypeFilter::Video => "icons/film.svg",
            TypeFilter::Audio => "icons/music.svg",
        })
    }

    fn render_section_header(label: &'static str, tokens: FindxTokens) -> impl IntoElement {
        div()
            .w_full()
            .px_2()
            .pt_2()
            .pb_1()
            .text_color(tokens.text_tertiary)
            .text_xs()
            .child(label)
    }

    fn render_filter_row(
        id: SharedString,
        icon: Icon,
        label: &'static str,
        selected: bool,
        tokens: FindxTokens,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        Button::new(id)
            .ghost()
            .w_full()
            .h(px(28.))
            .px_0()
            .accessibility_label(label)
            .selected(selected)
            .on_click(on_click)
            .child(
                h_flex()
                    .w(px(SIDEBAR_WIDTH_PX - 16.))
                    .h(px(28.))
                    .px_1()
                    .gap_2()
                    .items_center()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |this| this.bg(tokens.row_selected))
                    .when(!selected, |this| this.hover(|s| s.bg(tokens.hover)))
                    .child(
                        div()
                            .w(px(3.))
                            .h(px(14.))
                            .rounded_sm()
                            .when(selected, |this| this.bg(tokens.accent))
                            .when(!selected, |this| this.bg(hsla(0., 0., 0., 0.))),
                    )
                    .child(
                        icon.with_size(gpui_kit::component::Size::Small)
                            .flex_shrink_0()
                            .text_color(if selected {
                                tokens.accent
                            } else {
                                tokens.text_secondary
                            }),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .text_sm()
                            .text_ellipsis()
                            .text_color(if selected {
                                tokens.accent
                            } else {
                                tokens.text_primary
                            })
                            .child(label),
                    ),
            )
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Self::tokens(cx);
        let entity = cx.entity();
        let type_section = v_flex()
            .w_full()
            .child(Self::render_section_header("类型", tokens))
            .children(TypeFilter::ALL.iter().map(|filter| {
                let selected = self.type_filter == *filter;
                let f = *filter;
                let entity = entity.clone();
                Self::render_filter_row(
                    SharedString::from(format!("type-{}", f.label())),
                    Self::type_icon(f),
                    f.label(),
                    selected,
                    tokens,
                    move |_, window, cx| {
                        entity.update(cx, |this, cx| {
                            this.type_filter = f;
                            this.schedule_search(window, cx);
                            cx.notify();
                        });
                    },
                )
            }));
        let time_section = v_flex()
            .w_full()
            .child(
                div()
                    .w_full()
                    .px_2()
                    .py_2()
                    .child(div().h(px(1.)).w_full().bg(tokens.border_soft)),
            )
            .child(Self::render_section_header("时间", tokens))
            .children(TimeFilter::ALL.iter().map(|filter| {
                let selected = self.time_filter == *filter;
                let f = *filter;
                let entity = entity.clone();
                Self::render_filter_row(
                    SharedString::from(format!("time-{}", f.label())),
                    Icon::new(IconName::Calendar),
                    f.label(),
                    selected,
                    tokens,
                    move |_, window, cx| {
                        entity.update(cx, |this, cx| {
                            this.time_filter = f;
                            this.schedule_search(window, cx);
                            cx.notify();
                        });
                    },
                )
            }));

        v_flex()
            .w(px(SIDEBAR_WIDTH_PX))
            .min_h_0()
            .flex_shrink_0()
            .bg(tokens.surface)
            .border_r_1()
            .border_color(tokens.border)
            .overflow_hidden()
            .child(
                v_flex()
                    .id("sidebar-filter-rail")
                    .flex_1()
                    .min_h_0()
                    .px_2()
                    .pt_1()
                    .pb_2()
                    .overflow_y_scrollbar()
                    .child(type_section)
                    .child(time_section),
            )
    }

    fn pdf_action(&mut self, action: &str, cx: &mut Context<Self>) {
        let previous_page = self.pdf_options.page;
        let previous_edge = if self.pdf_options.fit_width {
            1600u32
        } else {
            (1600. * self.pdf_options.zoom).clamp(400., 3072.) as u32
        };
        match action {
            "first" => self.pdf_options.page = 0,
            "previous" => self.pdf_options.navigate(-1, self.pdf_count),
            "next" => self.pdf_options.navigate(1, self.pdf_count),
            "last" => self.pdf_options.page = self.pdf_count.saturating_sub(1),
            "minus" => self.pdf_options.scale(1. / 1.25),
            "plus" => self.pdf_options.scale(1.25),
            "fit" => self.pdf_options.fit_width = true,
            "actual" => {
                self.pdf_options.fit_width = false;
                self.pdf_options.zoom = 1.;
            }
            _ => return,
        }
        let edge = if self.pdf_options.fit_width {
            1600u32
        } else {
            (1600. * self.pdf_options.zoom).clamp(400., 3072.) as u32
        };
        // Fit/100%/zoom can relayout the current raster immediately. Reload only when
        // the page changed or a sharper (or smaller) decode is actually required.
        if self.pdf_options.page != previous_page || edge != previous_edge {
            self.request_preview(cx);
        }
        cx.notify();
    }
    fn render_pdf_controls(&self, cx: &Context<Self>) -> impl IntoElement {
        let page = self.pdf_options.page;
        h_flex()
            .w_full()
            .flex_shrink_0()
            .px_1()
            .py_1()
            .gap(px(2.))
            .items_center()
            .children(
                [
                    ("first", "|‹", "首页", page == 0),
                    ("previous", "‹", "上一页", page == 0),
                    ("next", "›", "下一页", page + 1 >= self.pdf_count),
                    ("last", "›|", "末页", page + 1 >= self.pdf_count),
                ]
                .into_iter()
                .map(|(id, _label, name, disabled)| {
                    let e = cx.entity();
                    Button::new(id)
                        .ghost()
                        .icon(Icon::default().path(match id {
                            "first" => "icons/chevrons-left.svg",
                            "previous" => "icons/chevron-left.svg",
                            "next" => "icons/chevron-right.svg",
                            _ => "icons/chevrons-right.svg",
                        }))
                        .tooltip(name)
                        .accessibility_label(name)
                        .w(px(20.))
                        .h(px(24.))
                        .px_0()
                        .text_xs()
                        .disabled(disabled)
                        .on_click(move |_, _, cx| e.update(cx, |this, cx| this.pdf_action(id, cx)))
                }),
            )
            .child(
                div()
                    .id("pdf-page-counter")
                    .role(gpui_kit::accesskit::Role::Label)
                    .aria_label(format!(
                        "PDF 第 {} / {} 页 · {}",
                        page + 1,
                        self.pdf_count,
                        if self.preview.pdf.is_some() {
                            "已加载"
                        } else {
                            "正在加载"
                        }
                    ))
                    .w(px(30.))
                    .text_xs()
                    .whitespace_nowrap()
                    .child(format!("{}/{}", page + 1, self.pdf_count)),
            )
            .children(
                [("minus", "−"), ("plus", "+")]
                    .into_iter()
                    .map(|(id, _label)| {
                        let e = cx.entity();
                        Button::new(id)
                            .ghost()
                            .icon(if id == "minus" {
                                IconName::Minus
                            } else {
                                IconName::Plus
                            })
                            .tooltip(if id == "minus" { "缩小" } else { "放大" })
                            .accessibility_label(if id == "minus" { "缩小" } else { "放大" })
                            .w(px(20.))
                            .h(px(24.))
                            .px_0()
                            .on_click(move |_, _, cx| {
                                e.update(cx, |this, cx| this.pdf_action(id, cx))
                            })
                    }),
            )
            .child({
                let e = cx.entity();
                Button::new("fit")
                    .ghost()
                    .icon(Icon::default().path("icons/move-horizontal.svg"))
                    .tooltip("适应宽度")
                    .accessibility_label("适宽")
                    .selected(self.pdf_options.fit_width)
                    .w(px(22.))
                    .h(px(24.))
                    .px_0()
                    .text_xs()
                    .on_click(move |_, _, cx| e.update(cx, |this, cx| this.pdf_action("fit", cx)))
            })
            .child({
                let e = cx.entity();
                Button::new("actual")
                    .ghost()
                    .icon(Icon::default().path("icons/scan.svg"))
                    .tooltip("恢复 100% 原始大小")
                    .accessibility_label("100%")
                    .w(px(22.))
                    .h(px(24.))
                    .px_0()
                    .text_xs()
                    .on_click(move |_, _, cx| {
                        e.update(cx, |this, cx| this.pdf_action("actual", cx))
                    })
            })
    }
    fn render_single_preview(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Self::tokens(cx);
        let header = match &self.preview.meta {
            Some(meta) => v_flex()
                .w_full()
                .min_w_0()
                .flex_shrink_0()
                .px_3()
                .py_2()
                .gap_1()
                .border_b_1()
                .border_color(tokens.border)
                .child(
                    div()
                        .id("preview-filename")
                        .role(gpui_kit::accesskit::Role::Label)
                        .aria_label(meta.name.clone())
                        .w_full()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_sm()
                        .aria_description(meta.path.clone())
                        .child(meta.name.clone()),
                )
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_xs()
                        .text_color(tokens.text_secondary)
                        .child(preview::format_meta_line(meta)),
                )
                .into_any_element(),
            None => div().into_any_element(),
        };

        let body: AnyElement = match self.preview.kind {
            PreviewKind::Empty => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .px_4()
                .child(
                    Icon::new(IconName::Eye)
                        .w(px(22.))
                        .h(px(22.))
                        .text_color(tokens.text_tertiary),
                )
                .child(
                    div()
                        .text_color(tokens.text_tertiary)
                        .text_sm()
                        .child("选择一条结果以预览"),
                )
                .into_any_element(),
            PreviewKind::NativeOffice => {
                #[cfg(windows)]
                {
                    if let Some(client) = &self.preview.native {
                        let surface = client.surface.clone();
                        if surface.is_alive() {
                            canvas(
                                |_, _, _| (),
                                move |bounds, _, window, _| {
                                    // Placement cannot release a newer menu/page suppression.
                                    surface.place(bounds, window.scale_factor());
                                },
                            )
                            .size_full()
                            .into_any_element()
                        } else {
                            div()
                                .p_3()
                                .child("系统处理器已退出。可切换文字摘录或重试系统版式。")
                                .into_any_element()
                        }
                    } else {
                        div().child("系统预览未就绪").into_any_element()
                    }
                }
                #[cfg(not(windows))]
                {
                    div().child("系统预览需要 Windows").into_any_element()
                }
            }
            PreviewKind::Loading => div()
                .p_3()
                .text_sm()
                .text_color(tokens.text_secondary)
                .child(
                    self.preview
                        .message
                        .clone()
                        .unwrap_or_else(|| "正在加载预览…".into()),
                )
                .into_any_element(),
            PreviewKind::Error => v_flex()
                .p_3()
                .gap_3()
                .child(
                    div()
                        .text_sm()
                        .text_color(tokens.text_secondary)
                        .child("暂时无法显示此文件。请检查本地可读性、文件关联，或稍后重试。"),
                )
                .child(
                    Button::new("preview-error-diagnostics")
                        .ghost()
                        .icon(IconName::Info)
                        .label(if self.preview_diagnostics {
                            "收起诊断"
                        } else {
                            "诊断详情"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.preview_diagnostics = !this.preview_diagnostics;
                            cx.notify();
                        })),
                )
                .when(self.preview_diagnostics, |view| {
                    view.child(
                        div()
                            .max_h(px(160.))
                            .overflow_y_scrollbar()
                            .text_xs()
                            .text_color(tokens.text_secondary)
                            .child(
                                self.preview
                                    .message
                                    .clone()
                                    .unwrap_or_else(|| "未提供额外诊断".into()),
                            ),
                    )
                })
                .into_any_element(),
            PreviewKind::Unsupported => v_flex()
                .p_3()
                .gap_2()
                .child(
                    div().text_sm().text_color(tokens.text_secondary).child(
                        self.preview
                            .message
                            .clone()
                            .unwrap_or_else(|| "暂不支持此格式的内容预览".into()),
                    ),
                )
                .into_any_element(),
            PreviewKind::Image if self.preview.pdf.is_some() => {
                let pdf = self.preview.pdf.unwrap();
                let width = if self.pdf_options.fit_width {
                    self.actual_preview_width - 24.
                } else {
                    pdf.width * self.pdf_options.zoom
                };
                let height = width * pdf.height / pdf.width.max(1.);
                div()
                    .p_3()
                    .child(
                        img(self.preview.image.clone().unwrap())
                            .w(px(width))
                            .h(px(height))
                            .object_fit(ObjectFit::Contain),
                    )
                    .into_any_element()
            }
            PreviewKind::Image => {
                let mut col = v_flex()
                    .w_full()
                    .min_h_0()
                    .flex_1()
                    .px_3()
                    .pt_2()
                    .pb_3()
                    .gap_2();
                if let Some(note) = &self.preview.image_note {
                    col = col.child(
                        div()
                            .text_xs()
                            .text_color(tokens.text_tertiary)
                            .child(note.clone()),
                    );
                }
                if let Some(image) = self.preview.image.clone() {
                    col = col.child(
                        img(image)
                            .id("preview-image")
                            .object_fit(ObjectFit::Contain)
                            .w_full()
                            .flex_1()
                            .min_h(px(80.)),
                    );
                }
                col.into_any_element()
            }
            PreviewKind::Text => {
                let mut col = v_flex().w_full().px_3().py_2().gap_2();
                if let Some(enc) = &self.preview.text_encoding {
                    let cap = if self.preview.truncated {
                        format!("{enc} · 仅显示开头部分")
                    } else {
                        enc.clone()
                    };
                    col = col.child(div().text_xs().text_color(tokens.text_tertiary).child(cap));
                }
                col.child(
                    div()
                        .w_full()
                        .text_sm()
                        .text_color(tokens.text_primary)
                        .child(self.preview.text.clone().unwrap_or_default()),
                )
                .into_any_element()
            }
            PreviewKind::Directory => {
                let mut col = v_flex().w_full().px_2().py_2().gap_1();
                let cap = if self.preview.truncated {
                    format!(
                        "显示前 {} 项（共 {} 项）",
                        self.preview.dir_entries.len(),
                        self.preview.dir_total
                    )
                } else {
                    format!("{} 项", self.preview.dir_total)
                };
                col = col.child(
                    div()
                        .px_1()
                        .text_xs()
                        .text_color(tokens.text_tertiary)
                        .child(cap),
                );
                for (i, entry) in self.preview.dir_entries.iter().enumerate() {
                    let icon = if entry.is_dir {
                        IconName::Folder
                    } else {
                        IconName::File
                    };
                    let kind = if entry.is_dir { "文件夹" } else { "文件" };
                    col = col.child(
                        h_flex()
                            .id(("preview-dir", i))
                            .w_full()
                            .h(px(24.))
                            .px_1()
                            .gap_2()
                            .items_center()
                            .child(
                                Icon::new(icon)
                                    .w(px(13.))
                                    .h(px(13.))
                                    .text_color(tokens.text_secondary),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .text_sm()
                                    .text_ellipsis()
                                    .child(entry.name.clone()),
                            )
                            .child(div().text_xs().text_color(tokens.text_tertiary).child(kind)),
                    );
                }
                col.into_any_element()
            }
        };

        let scrollable_body = matches!(
            self.preview.kind,
            PreviewKind::Text
                | PreviewKind::Directory
                | PreviewKind::Unsupported
                | PreviewKind::Error
        );

        v_flex()
            .w(px(self.actual_preview_width))
            .h_full()
            .min_h_0()
            .flex_shrink_0()
            .bg(tokens.surface)
            .border_l_1()
            .border_color(tokens.border)
            .child(header)
            .when(self.pdf_count > 0, |view| {
                view.child(self.render_pdf_controls(cx))
            })
            .when(
                self.preview.meta.as_ref().is_some_and(|m| {
                    let ext = std::path::Path::new(&m.path)
                        .extension()
                        .and_then(|x| x.to_str())
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    matches!(
                        ext.as_str(),
                        "docx" | "xlsx" | "pptx" | "rtf" | "doc" | "xls" | "ppt"
                    )
                }),
                |view| {
                    view.child(
                        h_flex().px_2().py_1().gap_1().flex_shrink_0().children(
                            [
                                ("office-native", "系统版式", false),
                                ("office-text", "文字摘录", true),
                            ]
                            .into_iter()
                            .map(|(id, label, text_only)| {
                                let entity = cx.entity();
                                Button::new(id)
                                    .ghost()
                                    .label(label)
                                    .on_click(move |_, _, cx| {
                                        entity.update(cx, |this, cx| {
                                            this.office_text_only = text_only;
                                            this.request_preview(cx);
                                            cx.notify();
                                        })
                                    })
                            }),
                        ),
                    )
                },
            )
            .child(if self.preview.pdf.is_some() {
                div()
                    .id("pdf-page-scroll")
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_scrollbar()
                    .child(body)
                    .into_any_element()
            } else if scrollable_body {
                div()
                    .id("preview-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(body)
                    .into_any_element()
            } else {
                div()
                    .id("preview-body")
                    .flex_1()
                    .min_h_0()
                    .child(body)
                    .into_any_element()
            })
    }

    fn render_diag(
        &self,
        content_width: f32,
        sidebar: bool,
        preview: bool,
        sidebar_reason: &str,
        preview_reason: &str,
        cx: &App,
    ) -> impl IntoElement {
        let tokens = Self::tokens(cx);
        let session = self.session.read(cx);
        let text = format!(
            "这段文字只报告测量，不改变折叠。原生窗口用逻辑像素，不是 CSS。\n\
决策宽度 contentWidth={content_width}\n\
viewport 见窗口当前尺寸，devicePixelRatio 由系统缩放决定\n\
{preview_reason}\n\
{sidebar_reason}\n\
手动 force preview={:?} sidebar={:?}  保存 preview={} sidebar={}\n\
selectedIndex={}  results={}\n\
{}\n最近按键：\n{}",
            self.preview_force,
            self.sidebar_force,
            if session.prefs.preview_open {
                "开"
            } else {
                "关"
            },
            if session.prefs.sidebar_open {
                "开"
            } else {
                "关"
            },
            self.selected
                .map(|i| i.to_string())
                .unwrap_or_else(|| "null".into()),
            self.hits.len(),
            if self.selection_resets.is_empty() {
                "选中未被查询清掉".into()
            } else {
                format!("选中被清掉: {}", self.selection_resets.join(" | "))
            },
            if self.key_traces.is_empty() {
                "（空则说明方向键还没进视图）".into()
            } else {
                self.key_traces.join("\n")
            },
        );
        let _ = (sidebar, preview);
        div()
            .absolute()
            .top_8()
            .right_4()
            .w(px(420.))
            .max_h(px(360.))
            .p_3()
            .rounded_md()
            .bg(tokens.surface_raised)
            .border_1()
            .border_color(tokens.border)
            .text_color(tokens.text_secondary)
            .text_sm()
            .overflow_hidden()
            .child(text)
    }
}

impl Render for MainWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Self::tokens(cx);
        let viewport = window.viewport_size();
        let content_width = viewport.width.as_f32();
        self.last_content_width = content_width;
        self.ensure_usage_scene(window, cx);
        let rail_width = if self.workspace.mode == ViewMode::Directory {
            workspace::DIRECTORY_SIDEBAR_WIDTH
        } else {
            SIDEBAR_WIDTH_PX
        };
        // Viewport can report the outer frame; leave slack for title bar + borders
        // so the pinned status bar is never pushed off-screen.
        let (saved_sidebar, saved_preview, diag_on, index_view) =
            self.session.read_with(cx, |s, _| {
                (
                    s.prefs.sidebar_open,
                    s.prefs.preview_open,
                    s.prefs.diag_enabled,
                    s.index_status.clone(),
                )
            });
        let (mut sidebar, mut preview, sidebar_reason, preview_reason) = explain_panels(
            content_width,
            PREVIEW_DEFAULT_WIDTH_PX,
            saved_sidebar,
            saved_preview,
            self.sidebar_force,
            self.preview_force,
        );
        let wanted = self
            .session
            .read(cx)
            .prefs
            .preview_width
            .unwrap_or(PREVIEW_DEFAULT_WIDTH_PX)
            .clamp(240., 600.);
        let list_min = if self.workspace.mode == ViewMode::Directory {
            super::browser_table::MIN_WIDTHS.iter().sum()
        } else {
            super::result_table::MIN_WIDTHS.iter().sum()
        };
        (sidebar, preview, self.actual_preview_width) = workspace::panel_budget(
            content_width,
            rail_width,
            list_min,
            sidebar,
            preview,
            wanted,
        );
        self.table_viewport_width = content_width
            - if sidebar { rail_width } else { 0. }
            - if preview {
                self.actual_preview_width + 6.
            } else {
                0.
            };
        self.fit_active_columns(self.table_viewport_width, cx);
        self.last_diag = format!("{sidebar_reason}\n{preview_reason}");
        if self.workspace.mode == ViewMode::Usage {
            sidebar = false;
            preview = false;
        }
        if self.settings_page.is_some() {
            preview = false;
            sidebar = false;
        }
        #[cfg(windows)]
        if let Some(client) = &self.preview.native {
            let mounted = preview && matches!(self.preview.kind, PreviewKind::NativeOffice);
            let allowed = crate::preview_visibility::surface_allowed(
                mounted,
                self.workspace.show_actions || self.file_actions.native_menu.is_open(),
                self.workspace.address_menu.is_some(),
                self.file_actions.keyboard_menu.is_some(),
            );
            client.surface.set_suppressed(!allowed);
        }

        v_flex()
            .id("findx-main")
            .key_context("FindXMain")
            .track_focus(&self.focus)
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .relative()
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                this.resize_preview_move(event, cx)
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.finish_preview_resize(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, _, cx| {
                    if event.button == MouseButton::Left {
                        this.finish_preview_resize(cx);
                    }
                }),
            )
            .overflow_hidden()
            .bg(tokens.bg)
            .text_color(tokens.text_primary)
            .on_action(
                cx.listener(|this, _: &WorkspaceBack, _, cx| this.workspace_history(false, cx)),
            )
            .on_action(
                cx.listener(|this, _: &WorkspaceForward, _, cx| this.workspace_history(true, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ShowFileMenu, window, cx| this.show_file_menu(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ContextOpen, window, cx| this.context_action(0, window, cx)),
            )
            .on_action(cx.listener(|this, _: &ContextOpenWith, window, cx| {
                this.context_action(1, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ContextLocate, window, cx| {
                    this.context_action(2, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &ContextCopyPath, window, cx| {
                this.context_action(3, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ContextCopy, window, cx| this.context_action(4, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ContextCut, window, cx| this.context_action(5, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ContextPaste, window, cx| {
                    this.context_action(6, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &ContextRename, window, cx| {
                    this.context_action(7, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &ContextProperties, window, cx| {
                this.context_action(8, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ContextAnalyze, window, cx| {
                this.context_action(9, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ContextScope, window, cx| {
                    this.context_action(10, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &ContextSelectAll, window, cx| {
                this.context_action(11, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ContextAutoFit, window, cx| {
                this.context_action(12, window, cx)
            }))
            .on_action(cx.listener(|this, _: &WorkspaceUp, _, cx| this.workspace_up(cx)))
            .on_action(cx.listener(|this, _: &WorkspaceRefresh, _, cx| this.workspace_refresh(cx)))
            .on_action(cx.listener(|this, _: &DirectoryNewTab, window, cx| {
                this.directory_tab_shortcut(0, window, cx)
            }))
            .on_action(cx.listener(|this, _: &DirectoryCloseTab, window, cx| {
                this.directory_tab_shortcut(1, window, cx)
            }))
            .on_action(cx.listener(|this, _: &DirectoryNextTab, window, cx| {
                this.directory_tab_shortcut(2, window, cx)
            }))
            .on_action(cx.listener(|this, _: &DirectoryPreviousTab, window, cx| {
                this.directory_tab_shortcut(3, window, cx)
            }))
            .on_action(cx.listener(|this, _: &WorkspaceFocusPath, window, cx| {
                if this.settings_page.is_some() || this.workspace.mode == ViewMode::Search {
                    return;
                }
                this.begin_path_edit(window, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectNextResult, _, cx| {
                this.move_sel(ResultNavKey::ArrowDown, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectPrevResult, _, cx| {
                this.move_sel(ResultNavKey::ArrowUp, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectFirstResult, window, cx| {
                // Home/End in the search box should stay with the input.
                if focus_is_search(&this.search, window, cx) {
                    return;
                }
                this.move_sel(ResultNavKey::Home, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectLastResult, window, cx| {
                if focus_is_search(&this.search, window, cx) {
                    return;
                }
                this.move_sel(ResultNavKey::End, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenSelected, window, cx| {
                if focus_is_search(&this.search, window, cx)
                    || focus_is_search(&this.workspace.path, window, cx)
                {
                    return;
                }
                this.open_selected(cx)
            }))
            .on_action(cx.listener(|this, _: &RevealSelected, _, cx| this.reveal_selected(cx)))
            .on_action(cx.listener(|this, _: &ToggleDiag, _, cx| this.toggle_diag(cx)))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                if this.settings_page.is_some() {
                    return;
                }
                if this.workspace.mode == ViewMode::Directory {
                    this.focus_directory_filter(window, cx);
                } else {
                    this.workspace_switch(ViewMode::Search, cx);
                    window.focus(&this.search.read(cx).focus_handle(cx), cx);
                }
            }))
            .on_action(
                cx.listener(|this, _: &OpenSettings, window, cx| {
                    this.open_settings_page(window, cx)
                }),
            )
            .child(self.render_workspace_title(window, cx))
            .when_some(self.settings_page.clone(), |view, page| {
                view.child(div().flex_1().min_h_0().child(page))
            })
            .when(self.settings_page.is_none(), |view| {
                view.child(
                    v_flex()
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .w_full()
                        .overflow_hidden()
                        .child(self.render_workspace_page(sidebar, preview, window, cx)),
                )
                .child(self.render_workspace_operation(cx))
                .when(self.has_file_activity(), |view| {
                    view.child(self.render_file_activity(cx))
                })
                .child(self.render_status_bar(&index_view, cx))
                .when(self.workspace.show_actions, |view| {
                    view.child(self.render_workspace_actions(cx))
                })
            })
            .when_some(self.workspace.address_menu.clone(), |view, menu| {
                view.child(
                    deferred(
                        anchored()
                            .position(self.workspace.address_menu_position)
                            .snap_to_window_with_margin(px(8.))
                            .child(menu),
                    )
                    .with_priority(gpui_kit::base::POPUP_PRIORITY),
                )
            })
            .when_some(self.file_actions.keyboard_menu.clone(), |view, menu| {
                view.child(
                    deferred(
                        anchored()
                            .position(self.file_actions.keyboard_menu_position)
                            .snap_to_window_with_margin(px(8.))
                            .child(menu),
                    )
                    .with_priority(gpui_kit::base::POPUP_PRIORITY),
                )
            })
            .when(self.file_actions.confirm_target.is_some(), |view| {
                view.child(self.render_paste_confirmation(cx))
            })
            .when(self.run_pending.is_some(), |view| {
                view.child(self.render_run_confirmation(cx))
            })
            .when(diag_on, |this| {
                this.child(self.render_diag(
                    content_width,
                    sidebar,
                    preview,
                    &sidebar_reason,
                    &preview_reason,
                    cx,
                ))
            })
    }
}

impl MainWindow {
    fn render_status_bar(&self, index: &IndexStatusView, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Self::tokens(cx);
        if self.workspace.mode == ViewMode::Directory {
            let count = self.workspace.rows.len();
            let selected = self.selected_file_hits().len();
            let summary = if self.workspace.listing {
                "正在读取目录…".to_string()
            } else if self.workspace.status.starts_with("目录读取失败") {
                "目录不可用：请检查权限或磁盘连接".to_string()
            } else {
                self.file_actions.summary.clone()
            };
            return h_flex()
                .w_full()
                .h(px(32.))
                .flex_shrink_0()
                .px_3()
                .gap_3()
                .border_t_1()
                .border_color(tokens.border)
                .bg(tokens.surface)
                .text_xs()
                .text_color(tokens.text_secondary)
                .child(div().flex_shrink_0().child(if selected > 0 {
                    format!("{selected} 个项目已选择 · 共 {count} 个项目")
                } else {
                    format!("{count} 个项目")
                }))
                .child(div().flex_1().min_w_0().text_ellipsis().child(summary))
                .child(
                    gpui_kit::component::switch::Switch::new("browser-show-hidden")
                        .small()
                        .checked(self.workspace.show_hidden)
                        .label("显示隐藏文件")
                        .on_click(cx.listener(|this, show, _, cx| this.set_show_hidden(*show, cx))),
                )
                .child(
                    Button::new("browser-index-status")
                        .ghost()
                        .icon(Icon::default().path(if self.session.read(cx).connected {
                            "icons/circle-check.svg"
                        } else {
                            "icons/circle-alert.svg"
                        }))
                        .w(px(24.))
                        .h(px(24.))
                        .px_0()
                        .tooltip(index.summary.clone())
                        .accessibility_label("索引连接状态"),
                )
                .into_any_element();
        }
        let left: String = if self.workspace.mode == ViewMode::Search {
            if self.file_actions.summary.is_empty() {
                self.search_status.to_string()
            } else {
                format!("{} · {}", self.search_status, self.file_actions.summary)
            }
        } else {
            self.workspace_scan_status()
        };
        h_flex()
            .w_full()
            .h(px(32.))
            .flex_shrink_0()
            .overflow_hidden()
            .px_3()
            .gap_3()
            .items_center()
            .bg(tokens.surface)
            .border_t_1()
            .border_color(tokens.border)
            .text_xs()
            .text_color(tokens.text_secondary)
            .child(div().flex_1().min_w_0().text_ellipsis().child(left))
            .when(
                self.workspace.mode == ViewMode::Search && self.workspace.scanning,
                |v| {
                    v.child(
                        Button::new("background-scan-status")
                            .ghost()
                            .icon(IconName::ChartPie)
                            .label("后台扫描中")
                            .tooltip(self.workspace_scan_status())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.workspace_switch(ViewMode::Usage, cx)
                            })),
                    )
                },
            )
            .child(
                Button::new("workspace-index-status")
                    .ghost()
                    .icon(Icon::default().path(if self.session.read(cx).connected {
                        "icons/circle-check.svg"
                    } else {
                        "icons/circle-alert.svg"
                    }))
                    .w(px(24.))
                    .h(px(24.))
                    .px_0()
                    .tooltip(index.summary.clone())
                    .accessibility_label("索引连接状态"),
            )
            .into_any_element()
    }
}

fn focus_is_search(search: &Entity<InputState>, window: &Window, cx: &App) -> bool {
    let Some(focused) = window.focused(cx) else {
        return false;
    };
    focused == search.read(cx).focus_handle(cx)
}

impl Drop for MainWindow {
    fn drop(&mut self) {
        self.cancel_file_background();
        self.preview_cancel
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

#[cfg(test)]
mod key_binding_tests {
    #[test]
    fn context_and_file_shortcuts_parse_before_startup() {
        for key in [
            "menu",
            "shift-f10",
            "ctrl-c",
            "ctrl-x",
            "ctrl-v",
            "ctrl-shift-c",
            "f2",
            "alt-enter",
            "ctrl-a",
            "ctrl-t",
            "ctrl-w",
            "ctrl-tab",
            "ctrl-shift-tab",
        ] {
            assert!(
                gpui_kit::Keystroke::parse(key).is_ok(),
                "invalid shortcut {key}"
            );
        }
    }
}
