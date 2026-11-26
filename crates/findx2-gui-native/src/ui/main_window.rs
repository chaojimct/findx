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
use std::path::PathBuf;
use std::time::Duration;
use workspace::{ViewMode, Workspace, key};

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
use crate::settings_logic::ContentDefaultMode;
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
        PreviewSelected,
        DismissTransient,
        ParentSelected,
        RevealSelected,
        ToggleDiag,
        ToggleCommandPalette,
        DismissCommandPalette,
        FocusSearch,
        OpenSettings,
        ShowFileMenu,
        ContextOpen,
        ContextOpenWith,
        ContextLocate,
        ContextOpenInNewTab,
        ContextToggleFavorite,
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
        DirectoryLastTab,
        DirectoryTab1,
        DirectoryTab2,
        DirectoryTab3,
        DirectoryTab4,
        DirectoryTab5,
        DirectoryTab6,
        DirectoryTab7,
        DirectoryTab8,
        DirectoryTab9,
        ToggleSidebar,
    ]
);

// Sidebar context-menu actions carry their own path. That removes the need for
// an `on_mouse_down(Right)` handler on every row, which made GPUI apply the
// pressed/hover style on right-click and leave a stale highlight behind.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = findx_native, no_json)]
pub struct SidebarOpenNewTab {
    pub path: SharedString,
}
#[derive(Clone, PartialEq, Action)]
#[action(namespace = findx_native, no_json)]
pub struct SidebarToggleFavorite {
    pub path: SharedString,
}
#[derive(Clone, PartialEq, Action)]
#[action(namespace = findx_native, no_json)]
pub struct SidebarReveal {
    pub path: SharedString,
}
#[derive(Clone, PartialEq, Action)]
#[action(namespace = findx_native, no_json)]
pub struct SidebarCopyPath {
    pub path: SharedString,
}
#[derive(Clone, PartialEq, Action)]
#[action(namespace = findx_native, no_json)]
pub struct SidebarToggleExpand {
    pub path: SharedString,
}

const FILE_SHORTCUT_CONTEXT: &str = "FindXMain && !Input";

// Match the same ancestor depth as GPUI's keymap. `eval` only examines the
// deepest node; a focused DataTable would otherwise fail FindXMain and make
// every raw Space/Enter/Escape look like text editing.
fn file_shortcuts_allowed(contexts: &[gpui_kit::KeyContext]) -> bool {
    KeyBindingContextPredicate::parse(FILE_SHORTCUT_CONTEXT)
        .expect("static file shortcut context")
        .depth_of(contexts)
        .is_some()
}

// gpui-component binds Escape at DataTable depth, ahead of the main context.
// Override only our table; nested text editors/IME remain excluded.
const TABLE_RAW_ESCAPE_CONTEXT: &str = "(FindXMain > DataTable) && !Input";
const PAGE_SIZE: usize = 200;
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(60);

/// Shared with routing regressions: never add repeat-blind bare activation
/// actions here; their raw KeyDownEvent path retains the platform held bit.
pub(crate) fn main_key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("shift-f10", ShowFileMenu, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("menu", ShowFileMenu, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-c", ContextCopy, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-x", ContextCut, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-v", ContextPaste, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-shift-c", ContextCopyPath, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("f2", ContextRename, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("alt-enter", ContextProperties, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-a", ContextSelectAll, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("alt-left", WorkspaceBack, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("alt-right", WorkspaceForward, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("alt-up", WorkspaceUp, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-l", WorkspaceFocusPath, Some("FindXMain")),
        KeyBinding::new("f5", WorkspaceRefresh, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-t", DirectoryNewTab, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-w", DirectoryCloseTab, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-tab", DirectoryNextTab, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new(
            "ctrl-shift-tab",
            DirectoryPreviousTab,
            Some(FILE_SHORTCUT_CONTEXT),
        ),
        KeyBinding::new("down", SelectNextResult, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("up", SelectPrevResult, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("home", SelectFirstResult, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("end", SelectLastResult, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-enter", RevealSelected, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-shift-d", ToggleDiag, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-f", FocusSearch, Some("FindXMain")),
        KeyBinding::new("ctrl-k", ToggleCommandPalette, Some("FindXMain")),
        KeyBinding::new("ctrl-comma", OpenSettings, Some("FindXMain")),
        KeyBinding::new("escape", NoAction, Some(TABLE_RAW_ESCAPE_CONTEXT)),
        KeyBinding::new("backspace", ParentSelected, Some(FILE_SHORTCUT_CONTEXT)),
        // Familiar aliases from browsers / VS Code / Explorer. All map to the
        // same actions, so they inherit the existing action guards.
        KeyBinding::new("ctrl-n", DirectoryNewTab, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-r", WorkspaceRefresh, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("alt-d", WorkspaceFocusPath, Some("FindXMain")),
        KeyBinding::new("ctrl-backspace", ParentSelected, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("f1", ToggleCommandPalette, Some("FindXMain")),
        KeyBinding::new("ctrl-shift-p", ToggleCommandPalette, Some("FindXMain")),
        KeyBinding::new("ctrl-i", PreviewSelected, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-b", ToggleSidebar, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-0", DirectoryLastTab, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-1", DirectoryTab1, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-2", DirectoryTab2, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-3", DirectoryTab3, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-4", DirectoryTab4, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-5", DirectoryTab5, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-6", DirectoryTab6, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-7", DirectoryTab7, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-8", DirectoryTab8, Some(FILE_SHORTCUT_CONTEXT)),
        KeyBinding::new("ctrl-9", DirectoryTab9, Some(FILE_SHORTCUT_CONTEXT)),
    ]
}

pub struct MainWindow {
    #[cfg(windows)]
    tray: Option<crate::native_tray::Tray>,
    exit_requested: bool,
    file_actions: FileActions,
    search_selection: std::collections::BTreeSet<usize>,
    focus: FocusHandle,
    table_navigation_focus: crate::ui::native_focus::TableNavigationFocus,
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
    settings_pending_tab: Option<String>,
    // Command palette overlay state.
    command_palette_open: bool,
    command_palette_query: Entity<InputState>,
    command_palette_selected: usize,
    preview_resize: Option<(f32, f32)>,
    actual_preview_width: f32,
    sidebar_resize: Option<(f32, f32)>,
    actual_sidebar_width: f32,
    /// Plain mousedown anchor for click-vs-drag disambiguation: (search page, row, position).
    pub(super) press_row: Option<(bool, usize, Point<Pixels>)>,
    table: Entity<TableState<ResultsTable>>,
    page_loading: bool,
    page_error: bool,
    _page_task: Option<Task<()>>,
    session: Entity<AppSession>,
    /// Backdrop actually applied to this window's HWND. Render converges
    /// it toward prefs: cross-window updates cannot touch a window that is
    /// mid-dispatch, so each window reconciles itself.
    applied_material: Option<crate::window_material::WindowMaterial>,
    search: Entity<InputState>,
    search_scope: Option<std::path::PathBuf>,
    global_search: Option<SearchSnapshot>,
    type_filter: TypeFilter,
    time_filter: TimeFilter,
    path_prefix: String,
    size_min: String,
    size_max: String,
    /// 搜索框全文模式（文件名/全文检索/混合检索；`content_enabled` 关时恒为文件名）。
    /// 初值取自设置页“默认搜索模式”。
    content_mode: ContentDefaultMode,
    /// 三段 switch 滑块当前位置（px）；点击后逐帧划向新位置，杜绝哑火。
    content_mode_glide_px: f32,
    /// 滑块动画代次：快速连点时旧任务自动认输，保证只剩最后一个在跑。
    content_mode_glide_gen: u64,
    hits: Vec<SearchHitDto>,
    selected: Option<usize>,
    match_total: u32,
    search_status: SharedString,
    service_action_busy: bool,
    resume_search_when_connected: bool,
    search_gen: u64,
    search_keyboard: crate::interaction_logic::SearchKeyboard,
    /// P0-1：最后一次发出的查询 ID（每次击键搜索 +1；翻页沿用，不自增）。
    /// 回包 `request_id` 对不上即过期丢弃；`__CANCELLED__` 静默丢弃。
    search_request_id: u64,
    /// P0-2：当前输入框固定的会话 ID（主框 1；范围框每次新建递增；返回全局恢复）。
    /// 同会话 + needle 收窄时 service 侧复用基结果集免全表扫描。
    search_session_id: u64,
    /// 会话 ID 分配器（0 保留 = 未跟踪）。
    next_search_session_id: u64,
    status_gen: u64,
    status_inflight: bool,
    preview_diagnostics: bool,
    preview: PreviewState,
    preview_memo: std::collections::HashMap<(String, u64, u64), crate::preview_worker::PdfOptions>,
    preview_gen: u64,
    preview_cancel: crate::preview_worker::Cancel,
    pdf_options: crate::preview_worker::PdfOptions,
    preview_options: crate::preview_worker::PdfOptions,
    pdf_count: u32,
    office_text_only: bool,
    preview_file: Option<String>,
    /// 当前预览命中的摘要（content: 查询 service 回填；预览窗展示用）。
    preview_snippet: String,
    /// 当前预览摘要所处的匹配窗口序号（上一个/下一个用；新目标重置为 0）。
    snippet_window: usize,
    /// 当前预览摘要的全文匹配点总数（0 = 未知；首屏命中自带，补取时回填）。
    snippet_total: u32,
    /// AnyTXT 式编号命中列表：全部窗口摘要（window, html），按需拉取后缓存。
    /// 点击行即跳窗（复用 fetch_snippet_window）；新目标/新查询时清空。
    snippet_list: Vec<(usize, String)>,
    full_text: String,
    full_spans: Vec<[usize; 2]>,
    full_total: u32,
    full_truncated: bool,
    match_list_h: f32,
    split_drag: Option<(f32, f32)>,
    /// docx 块模型（命中上下文富文本自绘：纯 Rust 解析，零外部进程）。
    docx_doc: Option<crate::docx_blocks::DocxDoc>,
    /// docx 命中区间（docx.plain 字节坐标，由命中词在 plain 内字面定位）。
    docx_hits: Vec<std::ops::Range<usize>>,
    /// docx 解析代（防止旧解析覆盖新目标）。
    docx_gen: u64,
    /// docx 内容区滚动句柄：块与子元素一一对应，点击列表行/翻页可精确定位。
    hit_scroll: gpui_kit::ScrollHandle,
    /// docx 定向渲染焦点块（点击列表/翻页时切换，只渲染该块附近的窗口）。
    docx_focus: Option<usize>,
    /// 搜索选项：区分大小写（Aa）。标题搜索精确生效；全文搜索后置过滤。
    search_match_case: bool,
    /// 搜索选项：全匹配（ab）。整串连续出现才算命中。
    search_exact: bool,
    /// 搜索选项：通配符（*）。开 = `*`/`?` 生效；关 = 按字面。
    search_wildcard: bool,
    /// 搜索选项：OR 模式。默认 AND（分词词全要）；开 = 任一命中。
    search_or: bool,
    /// 列表面板展开态（默认收起，只占 pager 行一个按钮）。
    snippet_list_open: bool,
    /// 列表拉取中（按钮置灰防重复请求）。
    snippet_list_loading: bool,
    /// 摘要 tab 是否激活（office 预览 [命中上下文|系统版式|文字摘录] 三态之一）。
    preview_snippet_view: bool,
    /// 用户手动点过预览展示 tab 后锁定：后到的摘要不再抢回激活态。
    preview_snippet_lock: bool,
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

/// 原生预览宿主处理的文件类型（与 preview.rs 走同一条路：office 文档 + pdf）。
/// 这类预览才有展示形态 tab 行（命中上下文/系统版式/文字摘录）。
/// 单行化：把空白折叠成一个空格，同时按字符重映射高亮区间（保持对齐）。
fn flatten_with_ranges(
    text: &str,
    ranges: &[std::ops::Range<usize>],
) -> (String, Vec<std::ops::Range<usize>>) {
    let mut out = String::with_capacity(text.len());
    // 旧字节偏移 → 新字节偏移
    let mut map: Vec<(usize, usize)> = Vec::with_capacity(text.len() + 1);
    let mut prev_space = false;
    for (i, ch) in text.char_indices() {
        if ch.is_whitespace() {
            if prev_space {
                // 折叠：不产出，但记录映射（指向当前位置）
                map.push((i, out.len()));
                continue;
            }
            prev_space = true;
            map.push((i, out.len()));
            out.push(' ');
        } else {
            prev_space = false;
            map.push((i, out.len()));
            out.push(ch);
        }
    }
    map.push((text.len(), out.len()));
    let remap = |b: usize| -> usize {
        // 二分找最后一个 <= b 的映射
        match map.binary_search_by_key(&b, |&(old, _)| old) {
            Ok(i) => map[i].1,
            Err(i) => {
                if i == 0 {
                    0
                } else {
                    map[i - 1].1
                }
            }
        }
    };
    let new_ranges = ranges
        .iter()
        .filter_map(|r| {
            let a = remap(r.start.min(text.len()));
            let b = remap(r.end.min(text.len()));
            (b > a).then_some(a..b)
        })
        .collect();
    (out, new_ranges)
}

/// 从查询串里取 `content:` / `utf8content:` 的**查询词列表**。
/// `content:"马春天 我与青松" glob:0` → ["马春天", "我与青松"]
/// `content:马春天` → ["马春天"]
fn content_needles_of(query: &str) -> Vec<String> {
    let lower = query.to_ascii_lowercase();
    for key in ["content:", "utf8content:"] {
        if let Some(i) = lower.find(key) {
            let rest = &query[i + key.len()..];
            let v = rest.trim_start();
            let inner: String = if let Some(stripped) = v.strip_prefix('"') {
                stripped.chars().take_while(|&c| c != '"').collect()
            } else {
                v.chars()
                    .take_while(|c| !c.is_whitespace())
                    .collect::<String>()
            };
            let mut out: Vec<String> = inner
                .split_whitespace()
                .map(|s| s.trim().to_string())
                // 跳过修饰符（如 glob:0 / exact:1 / sort:name）
                .filter(|s| !s.is_empty() && s.len() <= 64 && !s.contains(':'))
                .collect();
            if out.is_empty() {
                let t = inner.trim();
                if !t.is_empty() {
                    out.push(t.to_string());
                }
            }
            return out;
        }
    }
    // 非 content: 查询（标题/混合未包前缀）：整串当一个词。
    let q = query.trim();
    if q.is_empty() {
        Vec::new()
    } else {
        vec![q.to_string()]
    }
}

fn preview_has_mode_tabs(path: &str) -> bool {    matches!(
        std::path::Path::new(path)
            .extension()
            .and_then(|x| x.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "docx" | "xlsx" | "pptx" | "rtf" | "doc" | "xls" | "ppt" | "pdf"
    )
}

impl MainWindow {
    fn open_settings_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings_page_at(None, window, cx)
    }

    fn open_settings_page_at(&mut self, tab: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        let tab = tab.map(str::to_string).or_else(|| self.settings_pending_tab.take());
        self.cancel_path_edit(cx);
        self.dismiss_address_menu(cx);
        self.dismiss_file_menu(cx);
        if self.settings_page.is_some() {
            // Already open: just switch the tab.
            if let Some(tab) = tab {
                if let Some(page) = self.settings_page.clone() {
                    let t = tab.to_string();
                    page.update(cx, |p, cx| {
                        p.tab = super::settings_window::SettingsTab::from_canonical(&t);
                        cx.notify();
                    });
                }
            }
            return;
        }
        self.pause_preview();
        self.workspace.show_actions = false;
        let session = self.session.clone();
        let tab_for_page = tab.clone();
        let page = cx.new(|cx| {
            super::settings_window::SettingsWindow::new_embedded_at(session, window, cx, tab_for_page.as_deref())
        });
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
                    SettingsEvent::CloseApp => {
                        if this.exit_requested {
                            cx.quit();
                        } else {
                            window.remove_window();
                        }
                    }
                    SettingsEvent::Stay => {
                        this.exit_requested = false;
                        this.settings_pending_mode = None;
                        cx.notify();
                    }
                    SettingsEvent::ServiceChanged => this.poll_status(cx),
                    SettingsEvent::ServiceReady => {
                        this.resume_search_when_connected = true;
                        this.poll_status(cx);
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
        cx.bind_keys(main_key_bindings());
        // The first render converges the saved backdrop (see
        // `applied_material`); that single mechanism owns DWM state.
        // Saved user shortcuts are applied on top of the factory bindings.
        let saved_bindings = session.read(cx).prefs.key_bindings.clone();
        crate::keybindings::apply(&saved_bindings, cx);

        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder(session.read(cx).settings.content_default_mode.hint())
        });

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
                    content_suffix: SharedString::default(),
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
        workspace.usage_depth = session.read(cx).prefs.usage_preferred_depth.clamp(1,6);
        workspace.show_hidden = session.read(cx).prefs.browser_show_hidden;
        workspace.browser_view = session.read(cx).prefs.browser_view_mode;
        workspace.filter_recursive = session.read(cx).prefs.browser_recursive;
        let command_palette_query = cx.new(|cx| InputState::new(window, cx));
        let mut this = Self {
            #[cfg(windows)]
            tray: None,
            exit_requested: false,
            table_navigation_focus: Default::default(),
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
            settings_pending_tab: None,
            command_palette_open: false,
            command_palette_query: command_palette_query.clone(),
            command_palette_selected: 0,
            preview_resize: None,
            actual_preview_width: PREVIEW_DEFAULT_WIDTH_PX,
            sidebar_resize: None,
            actual_sidebar_width: SIDEBAR_WIDTH_PX,
            press_row: None,
            table: table.clone(),
            page_loading: false,
            page_error: false,
            _page_task: None,
            session: session.clone(),
            applied_material: None,
            search: search.clone(),
            search_scope: None,
            global_search: None,
            type_filter: TypeFilter::All,
            time_filter: TimeFilter::All,
            path_prefix: String::new(),
            size_min: String::new(),
            size_max: String::new(),
            content_mode: session.read(cx).settings.content_default_mode,
            content_mode_glide_px: crate::settings_logic::content_mode_left(
                session.read(cx).settings.content_default_mode,
            ),
            content_mode_glide_gen: 0,
            hits: Vec::new(),
            selected: None,
            match_total: 0,
            search_status: "就绪".into(),
            service_action_busy: false,
            resume_search_when_connected: false,
            search_gen: 0,
            search_keyboard: Default::default(),
            search_request_id: 0,
            search_session_id: 1,
            next_search_session_id: 2,
            status_gen: 0,
            status_inflight: false,
            preview_diagnostics: false,
            preview: PreviewState::empty(),
            preview_memo: std::collections::HashMap::new(),
            preview_gen: 0,
            preview_cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            pdf_options: Default::default(),
            preview_options: Default::default(),
            pdf_count: 0,
            office_text_only: false,
            preview_file: session.read(cx).preview_file.clone(),
            loading: false,
            sidebar_force: None,
            preview_force: None,
            preview_snippet: String::new(),
            snippet_window: 0,
            snippet_total: 0,
            snippet_list: Vec::new(),
            snippet_list_open: false,
            snippet_list_loading: false,
            full_text: String::new(),
            full_spans: Vec::new(),
            full_total: 0,
            full_truncated: false,
            match_list_h: 220.0,
            split_drag: None,
            docx_doc: None,
            docx_hits: Vec::new(),
            docx_gen: 0,
            hit_scroll: gpui_kit::ScrollHandle::new(),
            docx_focus: None,
            search_match_case: false,
            search_exact: false,
            search_wildcard: false,
            search_or: false,
            preview_snippet_view: false,
            preview_snippet_lock: false,
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

        // Observe resolved keystrokes too: Tab can be consumed by SDK actions
        // before raw capture_key_down handlers are called. Never consume input.
        let owner_window = window.window_handle();
        this._subscriptions
            .push(cx.observe_keystrokes(move |this, event, window, cx| {
                if window.window_handle() == owner_window
                    && this.table_navigation_focus.keystroke(&event.keystroke)
                {
                    cx.notify();
                }
            }));
        // Complete a splitter drag if activation is lost; do not retain a
        // captured pointer/resize state when the user switches applications.
        this._subscriptions
            .push(cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() {
                    this.finish_preview_resize(window, cx);
                    this.finish_sidebar_resize(window, cx);
                    this.reset_side_buttons();
                    this.dismiss_usage_facts();
                    cx.notify();
                }
            }));
        this.subscribe_search_input(search.clone(), window, cx);
        this._subscriptions
            .push(cx.observe(&session, |this, _, cx| {
                cx.notify();
                let _ = this;
            }));

        this.subscribe_results_table(table.clone(), cx);
        let palette_q = command_palette_query.clone();
        this._subscriptions.push(cx.subscribe_in(&palette_q, window, |this, _, ev, window, cx| {
            match ev {
                gpui_kit::component::input::InputEvent::Change => {
                    this.command_palette_selected = 0;
                    cx.notify();
                }
                gpui_kit::component::input::InputEvent::PressEnter { .. } => {
                    this.run_command_palette_selected(window, cx);
                }
                _ => {}
            }
        }));
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
                this.persist_workspace_resume(cx);
                // Hiding is not shutdown: retain active work and unsaved settings.
                #[cfg(windows)]
                if let Some(tray) = this.tray.as_ref().filter(|t| t.ready()) {
                    tray.hide();
                    return false;
                }
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
        #[cfg(windows)]
        {
            let title = if this.session.read(cx).official_product {
                "FindX"
            } else {
                "FindX — isolated native candidate"
            };
            this.tray = crate::native_tray::Tray::new(window, title);
            if this.tray.is_some() {
                let handle = window.window_handle();
                cx.spawn(async move |this,cx| {
                    loop {
                        cx.background_executor().timer(Duration::from_millis(100)).await;
                        let Ok(event)=this.update(cx,|this,_|this.tray.as_ref().and_then(|t|t.event())) else {break};
                        if let Some(event)=event {
                            let _=handle.update(cx,|_,window,cx|{
                                let _=this.update(cx,|this,cx|match event {
                                    crate::native_tray::Event::Show=>{if let Some(t)=&this.tray{t.show();}window.activate_window();},
                                    crate::native_tray::Event::Exit=>{
                                        // A hidden window must reveal cancellation/unsaved prompts.
                                        if let Some(t)=&this.tray{t.show();}window.activate_window();
                                        if this.cancel_active_batch_before_close(cx){return;}
                                        if this.session.read(cx).settings_window.is_some(){
                                            if let Some(t)=&this.tray{t.show();}
                                            this.transient_file_notice("请先关闭独立设置窗口，再退出 FindX；索引服务不受影响".into(),cx);return;
                                        }
                                        this.exit_requested=true;
                                        if let Some(page)=this.settings_page.clone(){
                                            if !page.update(cx,|page,cx|page.request_app_close(window,cx)){return;}
                                        }
                                        cx.quit();
                                    }
                                });
                            });
                        }
                    }
                }).detach();
            }
        }
        this.init_workspace(window, cx);
        cx.defer_in(window,|this,window,cx|this.restore_workspace_resume(window,cx));
        if let Some(path) = crate::isolation::isolated_usage_open_path(
            this.session.read(cx).live,
            crate::isolation::development_var_os("FINDX_NATIVE_USAGE_OPEN")
                .map(std::path::PathBuf::from)
                .as_deref(),
        ) {
            this.workspace_navigate(path, ViewMode::Usage, None, true, cx);
            if crate::isolation::development_var_os("FINDX_NATIVE_USAGE_INDEXED").is_none() {
                this.start_workspace_scan(true, cx);
            }
        }
        // Read-only live startup route; never enables a filesystem scan.
        if this.session.read(cx).live {
            if let Some(path) = crate::isolation::development_var_os("FINDX_NATIVE_LIVE_DIRECTORY_PATH") {
                this.workspace_navigate(
                    std::path::PathBuf::from(path),
                    ViewMode::Directory,
                    None,
                    true,
                    cx,
                );
            }
            if let Some(path) = crate::isolation::development_var_os("FINDX_NATIVE_LIVE_USAGE_PATH") {
                this.workspace_navigate(
                    std::path::PathBuf::from(path),
                    ViewMode::Usage,
                    None,
                    true,
                    cx,
                );
            }
        }
        // Establish a keyboard route on a freshly opened window, before any
        // pointer click. Otherwise even Ctrl-F has no FindXMain focus context.
        if this.workspace.mode == ViewMode::Search && this.preview_file.is_none() {
            window.focus(&this.search.read(cx).focus_handle(cx), cx);
        } else {
            window.focus(&this.focus, cx);
        }
        this.schedule_status_poll(cx);
        if this.preview_file.is_some() {
            this.request_preview(cx, true);
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

    pub(super) fn search_empty_message(&self, cx: &App) -> String {
        if self.service_down(cx) {
            return "索引服务未运行".into();
        }
        crate::search_logic::empty_result_label(
            !self.current_query(cx).trim().is_empty(),
            self.loading,
            self.search_status.as_ref(),
        )
        .into()
    }

    pub(super) fn service_down(&self, cx: &App) -> bool {
        if self.loading || !self.hits.is_empty() {
            return false;
        }
        let session = self.session.read(cx);
        !session.connected
            && session
                .last_error
                .as_deref()
                .is_some_and(status_logic::is_service_not_listening)
    }

    pub(super) fn render_service_down(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Self::tokens(cx);
        let official = self.session.read(cx).official_product;
        let busy = self.service_action_busy;
        v_flex()
            .absolute()
            .inset_0()
            .bg(tokens.bg)
            .items_center()
            .justify_center()
            .p_6()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(360.))
                    .items_center()
                    .gap_2()
                    .child(div().text_base().text_center().child("索引服务未运行"))
                    .child(
                        div()
                            .text_sm()
                            .text_center()
                            .whitespace_normal()
                            .text_color(tokens.text_secondary)
                            .child(if official {
                                "启动后台索引服务后即可开始搜索。"
                            } else {
                                "请先启动已有的索引服务，再重新搜索。"
                            }),
                    )
                    .when(official, |view| {
                        view.child(
                            Button::new("search-start-service")
                                .primary()
                                .w(px(128.))
                                .h(px(32.))
                                .label("启动服务")
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, _, cx| this.start_official_service(cx))),
                        )
                    })
                    .child(
                        div()
                            .h(px(20.))
                            .text_xs()
                            .text_color(tokens.text_tertiary)
                            .child(if busy { "正在启动服务" } else { "" }),
                    ),
            )
    }

    fn start_official_service(&mut self, cx: &mut Context<Self>) {
        if self.service_action_busy || !self.session.read(cx).official_product {
            return;
        }
        self.service_action_busy = true;
        self.resume_search_when_connected = true;
        self.search_status = "服务未运行".into();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async { crate::service_control::start_official() })
                .await;
            this.update(cx, |this, cx| {
                this.service_action_busy = false;
                match result {
                    Ok(_) => this.poll_status(cx),
                    Err(error) => {
                        this.resume_search_when_connected = false;
                        this.search_status = "服务未运行".into();
                        this.session.update(cx, |session, _| {
                            session.last_error = Some(error);
                        });
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
    fn current_query(&self, cx: &App) -> String {
        let raw = self.search.read(cx).value().to_string();
        let raw = search_logic::apply_content_mode(&raw, self.effective_content_mode(cx));
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

    /// 设置总开关关时恒为文件名（搜索框模式按钮同步隐藏）。
    fn effective_content_mode(&self, cx: &App) -> ContentDefaultMode {
        if self.session.read(cx).settings.content_enabled {
            self.content_mode
        } else {
            ContentDefaultMode::Name
        }
    }

    /// 含 `content:` 且索引未就绪时的状态栏后缀（慢速逐文件提示）。
    /// 就绪/旧服务（状态未知）返回空串。
    fn content_slow_hint(sent_query: &str, content_state: &str) -> &'static str {
        if !sent_query.to_ascii_lowercase().contains("content:") {
            return "";
        }
        match content_state {
            s if s == findx2_ipc::content_state::MISSING => "（全文索引未建，走慢速逐文件）",
            s if s == findx2_ipc::content_state::BUILDING => "（全文索引构建中，结果可能不全）",
            s if s == findx2_ipc::content_state::STALE => "（全文索引过期，建议重建；走慢速）",
            s if s == findx2_ipc::content_state::FAILED => "（全文索引失败，走慢速）",
            _ => "",
        }
    }

    fn set_content_mode(
        &mut self,
        m: ContentDefaultMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.content_mode == m {
            return;
        }
        let from = self.content_mode_glide_px;
        self.content_mode = m;
        self.animate_content_glide(cx, from, crate::settings_logic::content_mode_left(m));
        self.schedule_search(window, cx);
        // 占位提示跟随检索模式（全文顺带提示拼音）。
        let hint: SharedString = m.hint().into();
        self.search.update(cx, |input, cx| {
            input.set_placeholder(hint, window, cx);
        });
        cx.notify();
    }

    /// 滑块逐帧滑动：确定性重绘，不依赖声明式动画排期。
    /// 快速连点时只有最新一代跑完，旧任务中途认输。
    fn animate_content_glide(&mut self, cx: &mut Context<Self>, from: f32, to: f32) {
        self.content_mode_glide_gen = self.content_mode_glide_gen.wrapping_add(1);
        let round = self.content_mode_glide_gen;
        cx.spawn(async move |this, cx| {
            let t0 = std::time::Instant::now();
            let total = std::time::Duration::from_millis(
                crate::settings_logic::CONTENT_MODE_GLIDE_MS,
            );
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(16))
                    .await;
                let t = (t0.elapsed().as_secs_f32() / total.as_secs_f32()).min(1.0);
                let eased = crate::ui::motion::ease_in_out_cubic(t);
                let cont = this
                    .update(cx, |this, cx| {
                        if this.content_mode_glide_gen != round {
                            return false;
                        }
                        this.content_mode_glide_px = from + (to - from) * eased;
                        cx.notify();
                        t < 1.0
                    })
                    .unwrap_or(false);
                if !cont {
                    break;
                }
            }
        })
        .detach();
    }

    fn schedule_search(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.mode != ViewMode::Search {
            self.workspace_switch(ViewMode::Search, cx);
        }
        self.search_gen = self.search_gen.wrapping_add(1);
        self.search_keyboard.invalidate();
        self.selected = None;
        self.search_selection.clear();
        self.freeze_preview();
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

    /// 把搜索选项拼进查询串（只用于发请求；输入框显示不变）。
    /// `case:1` 大小写、`exact:1` 全匹配（优先）、`or:1` 分词任一、`glob:0` 关通配。
    fn effective_query(&self, cx: &mut Context<Self>) -> String {
        let mut q = self.current_query(cx);
        if q.trim().is_empty() {
            return q;
        }
        if self.search_match_case {
            q.push_str(" case:1");
        }
        if self.search_exact {
            q.push_str(" exact:1");
        } else if self.search_or {
            q.push_str(" or:1");
        }
        if !self.search_wildcard {
            q.push_str(" glob:0");
        }
        q
    }

    fn run_search(&mut self, cx: &mut Context<Self>) {
        let query = self.effective_query(cx);
        if query.trim().is_empty() {
            self.record_reset("查询变空");
            self.hits.clear();
            self.match_total = 0;
            self.search_selection.clear();
            self.selected = None;
            self.loading = false;
            self.search_keyboard.invalidate();
            self.search_status = "就绪".into();
            // 清空也取消在飞搜索（服务端立刻停扫；无在飞则幂等无操作）。
            // 此前清空后旧扫描继续烧 CPU 且其回包只能靠代检查丢弃。
            let rid = self.search_request_id;
            if rid > 0 {
                let (endpoint, ipc) = self
                    .session
                    .read_with(cx, |s, _| (s.endpoint.clone(), s.ipc.clone()));
                ipc.cancel_search(endpoint, rid);
            }
            self.request_preview(cx, true);
            self.sync_table(true, cx);
            cx.notify();
            return;
        }
        self.page_loading = false;
        self.page_error = false;
        self._page_task = None;
        // Retain published rows and scroll geometry until this generation completes.
        self.record_reset("重新查询");
        self.search_selection.clear();
        self.selected = None;
        self.freeze_preview();
        self.loading = true;
        self.search_status = "搜索中…".into();
        // 新查询覆盖旧的操作反馈（陈旧的位置/身份警告不再 relevant）。
        self.file_actions.summary.clear();
        let old_generation = self.search_gen;
        self.search_gen = self.search_gen.wrapping_add(1);
        let generation = self.search_gen;
        self.search_keyboard
            .begin(old_generation, generation, &query);
        let (endpoint, pinyin, pinyin_initials, pinyin_full, limit, mix_limit, content_state, ipc) =
            self.session.read_with(cx, |s, _| {
                (
                    s.endpoint.clone(),
                    s.settings.pinyin_default,
                    s.settings.content_pinyin_initials,
                    s.settings.content_pinyin_full,
                    s.settings.search_limit.max(1).min(PAGE_SIZE as u32) as usize,
                    s.settings.content_mix_limit,
                    s.content_index.state.clone(),
                    s.ipc.clone(),
                )
            });
        // PAGE_SIZE is the first-page fetch; search_limit is the ceiling shown in status.
        // 全文/混合模式再按“混合模式内容上限”截断单次取回（防 TopDocs 爆炸）。
        let mode = self.effective_content_mode(cx);
        let fetch = if mode == ContentDefaultMode::Name {
            PAGE_SIZE.min(limit.max(1))
        } else {
            PAGE_SIZE
                .min(limit.max(1))
                .min(mix_limit.max(1) as usize)
        };
        let slow_hint = Self::content_slow_hint(&query, &content_state);
        // P0-1/P0-2：每次击键搜索 request_id +1，会话按输入框固定；
        // 先取消在飞的上一查询（fire-and-forget，service 幂等），防堆积。
        self.search_request_id = self.search_request_id.wrapping_add(1).max(1);
        let rid = self.search_request_id;
        let sid = self.search_session_id;
        if rid > 1 {
            ipc.cancel_search(endpoint.clone(), rid - 1);
        }
        let rx = ipc.search_tracked(
            endpoint,
            query,
            pinyin,
            pinyin_initials,
            pinyin_full,
            fetch,
            0,
            rid,
            sid,
        );
        self._search_task = Some(cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("IPC 通道已关闭".into()));
            this.update(cx, |this, cx| {
                if this.search_gen != generation {
                    return;
                }
                // P0-1：过期回包 / 取消确认一律静默丢弃（不清空旧结果、不弹错）。
                match &result {
                    Ok(out)
                        if crate::ipc::is_stale_response(out.request_id, this.search_request_id) =>
                    {
                        return
                    }
                    // 能走到这里说明本代仍是最新（顶部 gen 检查已过）却收到取消确认：
                    // 没有更新查询在途的迹象，立即收掉 loading，状态栏不得永远
                    //「搜索中…」（实测「搜不了」的即时自愈；兜底看门狗在 run_search）。
                    Err(e) if crate::ipc::is_cancelled_err(e) => {
                        this.loading = false;
                        this.search_status = "查询已取消".into();
                        cx.notify();
                        return;
                    }
                    _ => {}
                }
                this.loading = false;
                let mut pending_key = None;
                match result {
                    Ok(out) => {
                        pending_key = this.search_keyboard.publish(generation);
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
                            format!("匹配 {total} 条")
                        } else {
                            format!("匹配 {total} 条，已显示 {shown} 条")
                        };
                        this.search_status = if out.elapsed_ms > 0 {
                            format!("{head}，{} ms{slow_hint}", out.elapsed_ms).into()
                        } else {
                            format!("{head}{slow_hint}").into()
                        };
                    }
                    Err(err) => {
                        // 诊断：搜索失败必须留痕（突发连击下偶发 Error 却无服务端记录，
                        // stderr 是定位 connect/超时/通道三类错误的唯一现场）。
                        eprintln!("[findx-gui] search failed gen={} rid={} err={}", generation, this.search_request_id, err.as_str());
                        this.search_keyboard.invalidate();
                        this.hits.clear();
                        this.match_total = 0;
                        this.record_reset("查询失败");
                        this.search_selection.clear();
                        this.selected = None;
                        this.search_status = if status_logic::is_service_not_listening(&err) {
                            "服务未运行".into()
                        } else {
                            "搜索失败".into()
                        };
                        this.session.update(cx, |session, _| {
                            session.engine_footer.observe(&Err(err.clone()));
                            session.connected = false;
                            session.last_error = Some(err.clone());
                            session.index_status = status_logic::disconnected_status(&err);
                        });
                        this.request_preview(cx, true);
                    }
                }
                this.search_selection.clear();
                this.selected = None;
                this.sync_table(true, cx);
                if pending_key.is_none() { this.request_preview(cx, true); }
                if let Some(intent) = pending_key {
                    let handle = this.native_window_handle();
                    let owner = cx.weak_entity();
                    cx.defer(move |cx| {
                        let _ = handle.update(cx, |_, window, cx| {
                            let _ = owner.update(cx, |this, cx| {
                                if this.search_gen == generation
                                    && this.search_keyboard.ready(generation)
                                {
                                    this.apply_search_key(intent, window, cx);
                                    // Lost focus / new IME composition may reject the queued key.
                                    // Then publish the legitimate no-selection preview, not old data forever.
                                    if this.selected.is_none() { this.request_preview(cx, true); }
                                }
                            });
                        });
                    });
                }
                cx.notify();
            })
            .ok();
        }));
        // 看门狗（P0-1 兜底）：取消/过期分支不回写状态，响应若因竞态彻底未达
        //（连接消失、任务替换、服务端无响应），loading 会永远挂 true——本次
        //「搜不了」的实测形态。16s（> IPC 15s 超时）后按代收口；正常路径
        // 早已清掉 loading，看门狗无事可做。
        let wd_gen = generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(16))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.search_gen == wd_gen && this.loading {
                    this.loading = false;
                    this.search_status = "搜索响应超时（已自动收口）；请重试".into();
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn sync_table(&mut self, reset: bool, cx: &mut Context<Self>) {
        let hits = self.hits.clone();
        let more = hits.len() < self.match_total as usize && !self.page_loading && !self.page_error;
        // 全文/混合模式名称列头加低调后缀（搜索框模式键的列表侧呼应）。
        let content_suffix: SharedString = {
            let enabled = self.session.read(cx).settings.content_enabled;
            if !enabled {
                "".into()
            } else {
                match self.content_mode {
                    ContentDefaultMode::Content => "·全文".into(),
                    ContentDefaultMode::Mix => "·混合".into(),
                    ContentDefaultMode::Name => "".into(),
                }
            }
        };
        self.table.update(cx, |table, cx| {
            table.delegate_mut().hits = hits;
            table.delegate_mut().more = more;
            table.delegate_mut().content_suffix = content_suffix;
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
        let (endpoint, pinyin, pinyin_initials, pinyin_full, mix_limit, content_state, ipc) =
            self.session.read_with(cx, |s, _| {
                (
                    s.endpoint.clone(),
                    s.settings.pinyin_default,
                    s.settings.content_pinyin_initials,
                    s.settings.content_pinyin_full,
                    s.settings.content_mix_limit,
                    s.content_index.state.clone(),
                    s.ipc.clone(),
                )
            });
        let fetch = if self.effective_content_mode(cx) == ContentDefaultMode::Name {
            PAGE_SIZE
        } else {
            PAGE_SIZE.min(mix_limit.max(1) as usize)
        };
        let slow_hint = Self::content_slow_hint(&query, &content_state);
        // 翻页沿用当前 request_id/session_id（不自增、不取消；回包照样验旧丢弃）。
        let rid = self.search_request_id;
        let sid = self.search_session_id;
        let rx = ipc.search_tracked(
            endpoint,
            query,
            pinyin,
            pinyin_initials,
            pinyin_full,
            fetch,
            offset,
            rid,
            sid,
        );
        self._page_task = Some(cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("IPC 通道已关闭".into()));
            let _ = this.update(cx, |this, cx| {
                if this.search_gen != generation || this.hits.len() != offset {
                    return;
                }
                // P0-1：翻页回包同样过期/取消丢弃（不置 page_error，避免误报）。
                match &result {
                    Ok(out)
                        if crate::ipc::is_stale_response(out.request_id, this.search_request_id) =>
                    {
                        return
                    }
                    Err(e) if crate::ipc::is_cancelled_err(e) => return,
                    _ => {}
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
                            "匹配 {} 条，已加载 {} 条{slow_hint}",
                            this.match_total,
                            this.hits.len()
                        )
                        .into();
                    }
                    Err(err) => {
                        this.page_error = true;
                        this.search_status = if status_logic::is_service_not_listening(&err) {
                            "服务未运行".into()
                        } else {
                            "加载下一页失败，请重新搜索".into()
                        };
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
        // At most one status request. Otherwise the 2s timer can continually
        // supersede a 5s timeout and leave a genuinely offline engine "online".
        if self.status_inflight {
            return;
        }
        self.status_inflight = true;
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
        self.session
            .update(cx, |s, _| s.engine_footer.set_source(&endpoint));
        let rx = ipc.status(endpoint.clone());
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or_else(|_| Err("IPC 通道已关闭".into()));
            this.update(cx, |this, cx| {
                if this.status_gen != generation {
                    return;
                }
                this.status_inflight = false;
                if this.session.read(cx).endpoint != endpoint {
                    return;
                }
                let resume = this.resume_search_when_connected;
                let mut connected_now = false;
                this.session.update(cx, |session, cx| {
                    session.engine_footer.observe(&result);
                    session.connected = session.engine_footer.connected;
                    match result {
                        Ok(resp) => {
                            session.status_raw = serde_json::to_string_pretty(&resp)
                                .unwrap_or_else(|e| e.to_string());
                            session.status_checked = search_logic::format_time(
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs(),
                            );
                            session.last_error = None;
                            connected_now = true;
                            session.index_status = status_logic::apply_status(
                                Some(&session.index_status),
                                &resp,
                                backfill,
                            );
                            // 全文索引快照：设置页全文 tab 直接读这里。
                            if let findx2_ipc::IpcResponse::StatusResult {
                                content_state,
                                content_docs,
                                content_bytes,
                                content_scanned,
                                content_extracted,
                                content_skipped,
                                content_failed,
                                content_error,
                                content_detail,
                                content_entry_total,
                                content_dir,
                                content_aux_ini_docs,
                                content_aux_ini_bytes,
                                content_aux_full_docs,
                                content_aux_full_bytes,
                                ..
                            } = &resp
                            {
                                session.content_index = crate::session::ContentIndexStatus {
                                    state: content_state.clone(),
                                    detail: content_detail.clone(),
                                    docs: *content_docs,
                                    bytes: *content_bytes,
                                    entry_total: *content_entry_total,
                                    scanned: *content_scanned,
                                    extracted: *content_extracted,
                                    skipped: *content_skipped,
                                    failed: *content_failed,
                                    error: content_error.clone(),
                                    dir: content_dir.clone(),
                                    aux_ini_docs: *content_aux_ini_docs,
                                    aux_ini_bytes: *content_aux_ini_bytes,
                                    aux_full_docs: *content_aux_full_docs,
                                    aux_full_bytes: *content_aux_full_bytes,
                                };
                            }
                        }
                        Err(err) => {
                            session.connected = false;
                            session.last_error = Some(err.clone());
                            session.index_status = status_logic::disconnected_status(&err);
                        }
                    }
                    cx.notify();
                });
                if connected_now && resume {
                    this.resume_search_when_connected = false;
                    if !this.current_query(cx).trim().is_empty() {
                        this.run_search(cx);
                    }
                }
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
        if !self.file_rows_current(true) { return; }
        self.record_single_selection(true, index);
        if self.selected != index {
            if index.is_some() {
                self.preview_file = None;
            }
            self.office_text_only = false;
            self.selected = index;
            self.preview_active_selection(cx);
        } else {
            self.selected = index;
        }
        cx.notify();
    }

    // Cancel pending work without destroying the complete published presentation.
    fn freeze_preview(&mut self) {
        self.preview_cancel.store(true, std::sync::atomic::Ordering::Release);
        self.preview_gen = self.preview_gen.wrapping_add(1);
        self._preview_task = None;
        #[cfg(windows)]
        if let Some(client) = &self.preview.native { client.surface.set_interactive(false); }
    }
    fn preview_is_current(&self) -> bool {
        self.file_scope_current() && !self.identity_pending
            && self.preview.generation == self.preview_gen
            && self.preview.meta.as_ref().is_some_and(|meta| {
                self.active_workspace_hit().map(|h| h.path).or_else(|| self.preview_file.clone())
                    .is_some_and(|path| path == meta.path)
            })
    }
    /// 是否「全文检索」上下文：模式是全文/混合，或查询串显式含 content:/utf8content:。
    /// 全文检索时命中上下文 tab 应立即出现，不等全文异步返回。
    fn is_content_query(&self, cx: &mut Context<Self>) -> bool {
        if self.effective_content_mode(cx) != ContentDefaultMode::Name {
            return true;
        }
        let q = self.current_query(cx).to_ascii_lowercase();
        q.contains("content:") || q.contains("utf8content:")
    }

    /// 命中上下文是否可用（与是否已有异步结果无关）：
    /// 全文检索 + 选中非目录文件。tab 显示与激活态都用这个口径。
    fn hit_ctx_available(&self, cx: &mut Context<Self>) -> bool {
        if !self.preview_snippet_view && !self.is_content_query(cx) {
            return false;
        }
        match self.active_workspace_hit() {
            Some(h) => !h.is_directory,
            None => false,
        }
    }

    /// 命中上下文文本列是否激活：只要该 tab 处于打开态就渲染区域
    /// （内容未到先显示占位，避免"过一会才冒出来"的跳变）。
    fn hit_text_active(&self) -> bool {
        self.preview_snippet_view
    }

    /// 命中上下文文本列内容：全文优先（spans 为字节区间），
    /// 全文未到/超大回退时用摘要窗（`<b>` 标记解析成区间）。
    fn hit_text_and_ranges(&self) -> (String, Vec<std::ops::Range<usize>>) {
        if !self.full_text.is_empty() {
            let t = &self.full_text;
            let ranges: Vec<std::ops::Range<usize>> = self
                .full_spans
                .iter()
                .filter_map(|&[a, b]| {
                    let b = b.min(t.len());
                    let a = a.min(b);
                    t.get(a..b).map(|_| a..b)
                })
                .collect();
            (t.clone(), ranges)
        } else {
            search_logic::snippet_plain_and_ranges(&self.preview_snippet)
        }
    }



    fn pause_preview(&mut self) {
        self.preview_cancel
            .store(true, std::sync::atomic::Ordering::Release);
        self.preview_gen = self.preview_gen.wrapping_add(1);
        self._preview_task = None;
        self.preview = PreviewState::empty();
    }

    /// 上一个/下一个匹配点翻页（snippet_total>1 时展示；点击即取对应窗口）。
    fn snippet_pager(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Self::tokens(cx);
        let cur = self.snippet_window;
        // docx 场景：命中数以文档侧 hits 为准（与列表/高亮同源）。
        let docx_mode = self.docx_doc.is_some() && !self.docx_hits.is_empty();
        let total = if docx_mode {
            self.docx_hits.len()
        } else {
            self.snippet_total as usize
        };
        let label = if total > 0 {
            format!("{}/{}", cur + 1, total)
        } else {
            format!("{}", cur + 1)
        };
        h_flex()
            .items_center()
            .gap_1()
            .flex_shrink_0()
            .child(
                Button::new("snippet-prev")
                    .ghost()
                    .label("上一个")
                    .disabled(cur == 0)
                    .h(px(22.))
                    .px_2()
                    .tooltip("上一个匹配处")
                    .on_click(cx.listener(|this, _, _, cx| {
                        let Some(entry_idx) =
                            this.active_workspace_hit().map(|h| h.entry_idx)
                        else {
                            return;
                        };
                        if this.snippet_window == 0 {
                            return;
                        }
                        let w = this.snippet_window - 1;
                        this.preview_snippet_lock = true;
                        if this.docx_doc.is_some() {
                            this.snippet_window = w;
                            this.scroll_to_hit(w);
                        } else {
                            this.fetch_snippet_window(entry_idx, w, true, cx);
                            this.scroll_to_hit(w);
                        }
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(tokens.text_secondary)
                    .child(label),
            )
            .child(
                Button::new("snippet-next")
                    .ghost()
                    .label("下一个")
                    .disabled(total > 0 && cur + 1 >= total)
                    .h(px(22.))
                    .px_2()
                    .tooltip("下一个匹配处")
                    .on_click(cx.listener(|this, _, _, cx| {
                        let Some(entry_idx) =
                            this.active_workspace_hit().map(|h| h.entry_idx)
                        else {
                            return;
                        };
                        let total = this.snippet_total as usize;
                        if total > 0 && this.snippet_window + 1 >= total {
                            return;
                        }
                        let w = this.snippet_window + 1;
                        this.preview_snippet_lock = true;
                        if this.docx_doc.is_some() {
                            this.snippet_window = w;
                            this.scroll_to_hit(w);
                        } else {
                            this.fetch_snippet_window(entry_idx, w, true, cx);
                            this.scroll_to_hit(w);
                        }
                        cx.notify();
                    })),
            )
            .when(total > 1, |row| {
                let list_label = if self.snippet_list_loading {
                    SharedString::from("拉取中…")
                } else {
                    SharedString::from(format!(
                        "全部{total}处{}",
                        if self.snippet_list_open { "▾" } else { "▸" }
                    ))
                };
                row.child(
                    Button::new("snippet-list")
                        .ghost()
                        .label(list_label)
                        .disabled(self.snippet_list_loading)
                        .h(px(22.))
                        .px_2()
                        .tooltip("编号命中列表（一次拉全窗，点击行号跳窗）")
                        .on_click(cx.listener(|this, _, _, cx| {
                            if this.snippet_list_open {
                                this.snippet_list_open = false;
                                cx.notify();
                                return;
                            }
                            if !this.snippet_list.is_empty() {
                                this.snippet_list_open = true;
                                cx.notify();
                                return;
                            }
                            let Some(entry_idx) =
                                this.active_workspace_hit().map(|h| h.entry_idx)
                            else {
                                return;
                            };
                            this.fetch_snippet_list(entry_idx, cx);
                        })),
                )
            })
    }

    /// AnyTXT 式编号命中列表：行号 badge + 单行摘要；点 badge 即跳窗。
    /// 当前窗行高亮；行数 = service 回传窗数（cap 50 截断，末尾明示）。
    /// docx 内容定位到第 N 个命中所在的块（点列表行 / 翻页共用）。
    /// 采用「定向渲染窗口」而非滚动：切 focus 块重绘，命中必在首屏。
    fn scroll_to_hit(&mut self, n: usize) {
        let Some(off) = self.docx_hits.get(n).map(|r| r.start) else {
            return;
        };
        let Some(block) = self
            .docx_doc
            .as_ref()
            .and_then(|d| d.block_of_offset(off))
        else {
            return;
        };
        self.docx_focus = Some(block);
        // 让句柄滚到该块（此时 track_scroll 生效）。
        self.hit_scroll.scroll_to_top_of_item(block);
    }

    /// docx 命中列表（与文档高亮/跳转同源）：
    /// 每条 = 一个命中区间 + 命中前后各 40 字上下文 + 该条内部的高亮区间。
    /// 列表条目数 == docx_hits.len()，点第 N 条就跳到第 N 个命中块。
    fn docx_hit_rows(&self) -> Vec<(String, Vec<std::ops::Range<usize>>)> {
        let Some(doc) = self.docx_doc.as_ref() else {
            return Vec::new();
        };
        const CTX: usize = 40;
        self.docx_hits
            .iter()
            .map(|r| {
                let mut a = r.start.saturating_sub(CTX);
                while a > 0 && !doc.plain.is_char_boundary(a) {
                    a -= 1;
                }
                let mut b = (r.end + CTX).min(doc.plain.len());
                while b < doc.plain.len() && !doc.plain.is_char_boundary(b) {
                    b += 1;
                }
                let ctx = doc.plain[a..b].replace(['\n', '\r', '\t'], " ");
                let inner: Vec<std::ops::Range<usize>> = self
                    .docx_hits
                    .iter()
                    .filter(|h| h.start >= a && h.end <= b)
                    .map(|h| (h.start - a)..(h.end - a))
                    .collect();
                (ctx, inner)
            })
            .collect()
    }

    fn snippet_list_view(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Self::tokens(cx);
        let cur = self.snippet_window;

        // 统一行模型：序号 / 单行文本 / 高亮区间 / 是否整串命中。
        // docx 场景用**文档侧 hits**生成（与高亮、跳转同一套坐标），
        // 非 docx 继续用 service 摘要窗口。
        let (total, rows): (
            usize,
            Vec<(usize, String, Vec<std::ops::Range<usize>>, bool)>,
        ) = if self.docx_doc.is_some() && !self.docx_hits.is_empty() {
            let q = self.effective_query(cx);
            let mut uniq = content_needles_of(&q);
            uniq.sort();
            uniq.dedup();
            let single = uniq.len() <= 1;
            let rs = self.docx_hit_rows();
            let total = rs.len();
            let rows = rs
                .into_iter()
                .enumerate()
                .map(|(i, (ctx, ranges))| (i, ctx, ranges, single))
                .collect();
            (total, rows)
        } else {
            let needle = {
                let q = self.current_query(cx);
                let lower = q.to_ascii_lowercase();
                lower
                    .split("content:")
                    .nth(1)
                    .map(|rest| {
                        let v = rest.trim_start();
                        if let Some(stripped) = v.strip_prefix('"') {
                            stripped.chars().take_while(|&c| c != '"').collect::<String>()
                        } else {
                            v.chars().take_while(|c| !c.is_whitespace()).collect::<String>()
                        }
                    })
                    .filter(|s| !s.is_empty())
            };
            let rows = self
                .snippet_list
                .clone()
                .into_iter()
                .map(|(w, html)| {
                    let (plain, ranges) = search_logic::snippet_plain_and_ranges(&html);
                    let (flat, flat_ranges) = flatten_with_ranges(&plain, &ranges);
                    let is_exact = needle.as_ref().is_some_and(|n| {
                        flat_ranges.iter().any(|r| {
                            flat.get(r.clone()).is_some_and(|s| s.eq_ignore_ascii_case(n))
                        })
                    });
                    (w, flat, flat_ranges, is_exact)
                })
                .collect();
            (self.snippet_total as usize, rows)
        };
        let shown = rows.len();

        v_flex()
            .w_full()
            .flex_shrink_0()
            .gap_px()
            .children(rows.into_iter().map(|(w, text, ranges, is_exact)| {
                let badge_text = if is_exact { "全" } else { "词" };
                let badge_color = if is_exact {
                    tokens.accent
                } else {
                    tokens.text_tertiary
                };
                let selected = w == cur;
                let entity = cx.entity();
                h_flex()
                    .id(SharedString::from(format!("snippet-row-{w}")))
                    .w_full()
                    .items_center()
                    .gap(px(6.))
                    .px_2()
                    .py(px(3.))
                    .rounded_sm()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(tokens.row_selected))
                    .hover(|d| d.bg(tokens.hover))
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |this, cx| {
                            let Some(entry_idx) =
                                this.active_workspace_hit().map(|h| h.entry_idx)
                            else {
                                return;
                            };
                            if !this.snippet_list_open {
                                return;
                            }
                            this.preview_snippet_lock = true;
                            if this.docx_doc.is_some() {
                                // docx：同源列表，直接切焦点块（不重取 service 摘要）。
                                this.snippet_window = w;
                                this.scroll_to_hit(w);
                            } else {
                                this.fetch_snippet_window(entry_idx, w, true, cx);
                            }
                            cx.notify();
                        })
                    })
                    .child(
                        div()
                            .flex_shrink_0()
                            .w(px(18.))
                            .text_xs()
                            .text_color(tokens.text_tertiary)
                            .child(format!("{}", w + 1)),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .px(px(3.))
                            .rounded_sm()
                            .text_xs()
                            .text_color(badge_color)
                            .when(is_exact, |d| d.bg(tokens.row_selected))
                            .child(badge_text),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_xs()
                            .text_color(if selected {
                                tokens.text_primary
                            } else {
                                tokens.text_secondary
                            })
                            .child(
                                StyledText::new(text).with_highlights(ranges.into_iter().map(
                                    |r| {
                                        (
                                            r,
                                            HighlightStyle {
                                                background_color: Some(tokens.hit_bg),
                                                color: Some(tokens.hit_fg),
                                                ..Default::default()
                                            },
                                        )
                                    },
                                )),
                            ),
                    )
            }))
            .when(total > shown, |v| {
                v.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .text_color(tokens.text_tertiary)
                        .child(format!("共 {total} 处，只列前 {shown}（上一个/下一个可达其余）")),
                )
            })
    }


    /// 按需取指定窗口的摘要（选中行补取 + 上一个/下一个翻页共用）。
    /// window==0 时同时回填行内摘要；总是刷新预览卡；总数回填供翻页按钮用。
    /// force=true（用户点了翻页）时无视 tab 锁；自动补取只在未锁定时抢回激活态。
    fn fetch_snippet_window(
        &mut self,
        entry_idx: u32,
        window: usize,
        force: bool,
        cx: &mut Context<Self>,
    ) {
        if self.workspace.mode != ViewMode::Search || self.preview_file.is_some() {
            return;
        }
        if !self.session.read(cx).settings.content_snippet {
            return;
        }
        let sent = self.effective_query(cx);
        if !sent.to_ascii_lowercase().contains("content:") {
            return;
        }
        let (endpoint, ipc) = self
            .session
            .read_with(cx, |s, _| (s.endpoint.clone(), s.ipc.clone()));
        let search_gen_at_req = self.search_gen;
        cx.spawn(async move |this, cx| {
            let result = ipc
                .request(
                    endpoint,
                    findx2_ipc::IpcRequest::ContentSnippets {
                        query: sent,
                        entry_indices: vec![entry_idx],
                        window,
                    },
                )
                .await
                .unwrap_or_else(|_| Err("IPC 通道已关闭".into()));
            let Ok(findx2_ipc::IpcResponse::ContentSnippets { snippets }) = result
            else {
                return;
            };
            let Some(s) = snippets.into_iter().find(|s| s.entry_idx == entry_idx) else {
                return;
            };
            if s.html.is_empty() {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                if this.search_gen != search_gen_at_req {
                    return;
                }
                if window == 0 {
                    if let Some(h) = this.hits.iter_mut().find(|h| h.entry_idx == entry_idx) {
                        h.snippet = s.html.clone();
                        h.snippet_window = s.window;
                        h.snippet_total = s.total;
                    }
                }
                if this
                    .active_workspace_hit()
                    .is_some_and(|h| h.entry_idx == entry_idx)
                {
                    this.preview_snippet = s.html;
                    this.snippet_window = s.window;
                    this.snippet_total = s.total;
                    if force || !this.preview_snippet_lock {
                        this.preview_snippet_view = true;
                    }
                    this.sync_table(false, cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// AnyTXT 式编号命中列表：一次拉全窗（cap 50，service 侧钳制），点击行即跳窗。
    /// 复用单窗缓存（service 侧拉全窗时顺手写入），翻页按钮仍走原单窗通道。
    /// 新目标/新查询时面板与缓存一起清空（见 request_preview）。
    fn fetch_snippet_list(&mut self, entry_idx: u32, cx: &mut Context<Self>) {
        if self.workspace.mode != ViewMode::Search || self.preview_file.is_some() {
            return;
        }
        if !self.session.read(cx).settings.content_snippet {
            return;
        }
        if self.snippet_list_loading {
            return;
        }
        let sent = self.effective_query(cx);
        if !sent.to_ascii_lowercase().contains("content:") {
            return;
        }
        let (endpoint, ipc) = self
            .session
            .read_with(cx, |s, _| (s.endpoint.clone(), s.ipc.clone()));
        let search_gen_at_req = self.search_gen;
        self.snippet_list_loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = ipc
                .request(
                    endpoint,
                    findx2_ipc::IpcRequest::ContentSnippetsAll {
                        query: sent,
                        entry_index: entry_idx,
                        cap: 50,
                    },
                )
                .await
                .unwrap_or_else(|_| Err("IPC 通道已关闭".into()));
            let _ = this.update(cx, |this, cx| {
                this.snippet_list_loading = false;
                if this.search_gen != search_gen_at_req {
                    return;
                }
                let Ok(findx2_ipc::IpcResponse::ContentSnippets { snippets }) = result
                else {
                    return;
                };
                // 只收本 entry 的窗（service 原样回显 entry_idx；防御性过滤）。
                let mut list: Vec<(usize, String)> = snippets
                    .into_iter()
                    .filter(|s| s.entry_idx == entry_idx)
                    .map(|s| (s.window, s.html))
                    .collect();
                list.sort_by_key(|(w, _)| *w);
                list.dedup_by_key(|(w, _)| *w);
                if list.is_empty() {
                    cx.notify();
                    return;
                }
                this.snippet_list = list;
                this.snippet_list_open = true;
                cx.notify();
            });
        })
        .detach();
    }

    /// 全文文本预览：命中上下文升级用，选中即取一次，service 侧缓存；
    /// 超大/失败回空文本（调用方回退摘要展示）。
    fn fetch_full_text(&mut self, entry_idx: u32, cx: &mut Context<Self>) {
        if self.workspace.mode != ViewMode::Search || self.preview_file.is_some() {
            return;
        }
        if !self.session.read(cx).settings.content_snippet {
            return;
        }
        let sent = self.effective_query(cx);
        if !sent.to_ascii_lowercase().contains("content:") {
            return;
        }
        let (endpoint, ipc) = self
            .session
            .read_with(cx, |s, _| (s.endpoint.clone(), s.ipc.clone()));
        let search_gen_at_req = self.search_gen;
        cx.spawn(async move |this, cx| {
            let result = ipc
                .request(
                    endpoint,
                    findx2_ipc::IpcRequest::ContentFullText {
                        query: sent,
                        entry_index: entry_idx,
                    },
                )
                .await
                .unwrap_or_else(|_| Err("IPC 通道已关闭".into()));
            let Ok(findx2_ipc::IpcResponse::ContentFullText {
                text,
                spans,
                total,
                truncated,
            }) = result
            else {
                return;
            };
            if text.is_empty() {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                if this.search_gen != search_gen_at_req {
                    return;
                }
                if this
                    .active_workspace_hit()
                    .is_some_and(|h| h.entry_idx == entry_idx)
                {
                    this.full_text = text;
                    this.full_spans = spans;
                    this.full_total = total;
                    this.full_truncated = truncated;
                    // 全文到了：用命中词在 docx.plain 内重新定位（拼音已对齐到汉字）。
                    this.recompute_docx_hits(cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// docx 富文本块解析（命中上下文自绘用）：纯 Rust、进程内，约 90ms。
    /// 后台线程解析，不卡 UI；代际校验防止旧结果覆盖新目标。
    fn fetch_docx_blocks(&mut self, path: String, cx: &mut Context<Self>) {
        self.docx_gen = self.docx_gen.wrapping_add(1);
        let req_gen = self.docx_gen;
        self.docx_doc = None;
        self.docx_hits.clear();
        self.docx_focus = None;
        cx.spawn(async move |this, cx| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            std::thread::spawn(move || {
                let _ = tx.send(crate::docx_blocks::parse(&path));
            });
            let parsed = rx.await;
            let _ = this.update(cx, |this, cx| {
                if this.docx_gen != req_gen {
                    return;
                }
                match parsed {
                    Ok(Ok(doc)) => {
                        this.docx_doc = Some(doc);
                        this.recompute_docx_hits(cx);
                        cx.notify();
                    }
                    _ => {
                        // 解析失败：保持 None，渲染回退到纯文本列。
                        this.docx_doc = None;
                    }
                }
            });
        })
        .detach();
    }

    /// 用查询词在 docx.plain 内自算命中（字面 + 拼音对齐）。
    /// 不依赖 service 的 full_spans：两套文本提取器的空白/表格顺序不同，
    /// 跨提取器按字节定位会失败（这是此前"命中列表有、文档内无高亮"的根因）。
    fn recompute_docx_hits(&mut self, cx: &mut Context<Self>) {
        self.docx_hits.clear();
        let Some(doc) = self.docx_doc.as_ref() else {
            return;
        };
        let q = self.effective_query(cx);
        let needles = content_needles_of(&q);
        if needles.is_empty() {
            return;
        }
        self.docx_hits = crate::docx_blocks::match_hits_multi(&doc.plain, &needles);
        // 命中定位：默认跳到第一处，避免长文档打开在开头看不到命中。
        self.docx_focus = self
            .docx_hits
            .first()
            .and_then(|r| doc.block_of_offset(r.start));
    }

    /// 取自渲染 PDF 当前页的命中框（高亮黄框用；失败/无命中静默无高亮）。
    fn fetch_pdf_boxes(&mut self, entry_idx: u32, page: u32, path: String, cx: &mut Context<Self>) {
        if self.workspace.mode != ViewMode::Search || self.preview_file.is_some() {
            return;
        }
        let sent = self.effective_query(cx);
        if !sent.to_ascii_lowercase().contains("content:") {
            return;
        }
        let (endpoint, ipc) = self
            .session
            .read_with(cx, |s, _| (s.endpoint.clone(), s.ipc.clone()));
        let search_gen_at_req = self.search_gen;
        cx.spawn(async move |this, cx| {
            let result = ipc
                .request(
                    endpoint,
                    findx2_ipc::IpcRequest::ContentPdfBoxes {
                        query: sent,
                        entry_index: entry_idx,
                        page,
                    },
                )
                .await
                .unwrap_or_else(|_| Err("IPC 通道已关闭".into()));
            let Ok(findx2_ipc::IpcResponse::ContentPdfBoxes {
                rects,
                page_w: _,
                page_h: _,
            }) = result
            else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.search_gen != search_gen_at_req {
                    return;
                }
                let cur = this.preview.pdf.map(|p| p.page);
                let cur_path = this.preview.meta.as_ref().map(|m| m.path.clone());
                if cur == Some(page) && cur_path.as_deref() == Some(path.as_str()) {
                    this.preview.pdf_highlights = rects;
                    this.preview.pdf_hl_page = Some(page);
                    this.preview.pdf_hl_path = Some(path);
                    this.preview.pdf_hl_gen = Some(search_gen_at_req);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn request_preview(&mut self, cx: &mut Context<Self>, reset_view: bool) {
        self.preview_cancel
            .store(true, std::sync::atomic::Ordering::Release);
        self.preview_cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel = self.preview_cancel.clone();
        let options = self.pdf_options;
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
            self.preview_snippet.clear();
            self.snippet_list.clear();
            self.snippet_list_open = false;
            self.snippet_list_loading = false;
            self.preview_snippet_view = false;
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
                snippet: String::new(), pinyin_hit: false, snippet_window: 0, snippet_total: 0,
            })
        });
        let Some(hit) = target else {
            self.pdf_count = 0;
            self.pdf_options = Default::default();
            self.office_text_only = false;
            self.preview = PreviewState::empty();
            self.preview_snippet.clear();
            self.snippet_list.clear();
            self.snippet_list_open = false;
            self.snippet_list_loading = false;
            self.preview_snippet_view = false;
            self._preview_task = None;
            return;
        };
        self.preview_snippet = hit.snippet.clone();
        self.snippet_window = hit.snippet_window;
        self.snippet_total = hit.snippet_total;
        // 全文检索：选中非目录文件即激活命中上下文 tab（不等全文/摘要异步返回）。
        // 非全文查询：沿用旧口径（有摘要才激活）。
        let content_query = self.is_content_query(cx);
        // 新目标：编号列表清空（旧文档的窗与新目标无关）。
        self.snippet_list.clear();
        // AnyTXT 式：全文检索时默认展开编号列表（内容在上、列表在下）。
        self.snippet_list_open = content_query || hit.snippet_total > 1;
        self.snippet_list_loading = false;
        self.full_text.clear();
        self.full_spans.clear();
        self.full_total = 0;
        self.full_truncated = false;
        // 任何新目标都先清空 docx 状态：非 docx 文件不会走 fetch_docx_blocks，
        // 不清就会渲染上一个 docx 的内容（预览卡住）。
        self.docx_doc = None;
        self.docx_hits.clear();
        self.docx_focus = None;
        if reset_view {
            self.preview_snippet_view =
                !hit.is_directory && (content_query || !hit.snippet.is_empty());
            self.preview_snippet_lock = false;
        }
        // 选中行无摘要时按需补第 0 窗（列表首屏只填前 N 行；失败静默）。
        if hit.snippet.is_empty() {
            let entry_idx = hit.entry_idx;
            self.fetch_snippet_window(entry_idx, 0, false, cx);
        }
        // Hit-context prefetch: full text + numbered list auto-open.
        // System layout stays native; other tabs keep existing behavior.
        let prefer_native = !self.office_text_only;
        if self.preview_snippet_view && !hit.is_directory {
            self.fetch_full_text(hit.entry_idx, cx);
            self.fetch_snippet_list(hit.entry_idx, cx);
            // docx：纯 Rust 块解析 → 命中上下文自绘真版式（零外部进程）。
            if std::path::Path::new(&hit.path)
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("docx"))
            {
                self.fetch_docx_blocks(hit.path.clone(), cx);
            }
        }
        if self.preview_memo.len() > 64 {
            self.preview_memo.clear();
        }
        self.preview_memo.insert(
            workspace::preview_stamp(&hit.path, hit.size, hit.mtime),
            self.pdf_options,
        );
        // Header, raster/text and its PDF camera are one immutable presentation.
        // Do not clear it or label it with the newly requested page while decoding.
        // First load from a blank slate still needs a visible state: header +
        // loading, so selection never sits on the "nothing selected" placeholder.
        if matches!(self.preview.kind, PreviewKind::Empty) {
            self.preview = PreviewState::loading(
                generation,
                preview::meta_from_hit(
                    &hit.name,
                    &hit.path,
                    hit.size,
                    hit.mtime,
                    hit.is_directory,
                ),
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
                    .await
                })
                .await;
            this.update(cx, |this, cx| {
                if !preview::is_current_generation(this.preview_gen, generation, loaded.generation)
                {
                    return;
                }
                this.preview_options = options;
                this.pdf_count = loaded.pdf.map(|pdf| pdf.count).unwrap_or(0);
                if let Some(pdf) = loaded.pdf {
                    this.pdf_options.page = pdf.page;
                    this.preview_options.page = pdf.page;
                }
                this.preview = loaded;
                // 自渲染 PDF 页：顺手取本页命中框（翻页重载也会经过这里）。
                // 预取（request 时并行发出）通常已命中：仅当框缺失/过期
                //（页被钳制、查询已换代）才按实际页补一次，避免重复请求。
                if let (Some(pdf), Some(meta)) =
                    (this.preview.pdf, this.preview.meta.clone())
                {
                    let hl_fresh = this.preview.pdf_hl_path.as_deref()
                        == Some(meta.path.as_str())
                        && this.preview.pdf_hl_page == Some(pdf.page)
                        && this.preview.pdf_hl_gen == Some(this.search_gen);
                    if !hl_fresh {
                        if let Some(h) = this.active_workspace_hit() {
                            let (idx, path) = (h.entry_idx, meta.path.clone());
                            this.fetch_pdf_boxes(idx, pdf.page, path, cx);
                        }
                    }
                }
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

    fn toggle_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_page.is_some() {
            return;
        }
        self.command_palette_open = !self.command_palette_open;
        if self.command_palette_open {
            self.command_palette_selected = 0;
            self.command_palette_query.update(cx, |q, cx| q.set_value(String::new(), window, cx));
            window.focus(&self.command_palette_query.read(cx).focus_handle(cx), cx);
        }
        cx.notify();
    }

    fn command_palette_commands(&self) -> Vec<crate::command_palette::PaletteCommand> {
        vec![
            crate::command_palette::PaletteCommand { id: "focus_search", label: "聚焦搜索框", keywords: &["search", "find"], group: "导航" },
            crate::command_palette::PaletteCommand { id: "open_settings", label: "打开设置", keywords: &["settings", "preferences"], group: "导航" },
            crate::command_palette::PaletteCommand { id: "open_settings_index", label: "设置：索引与服务", keywords: &["index", "service"], group: "设置" },
            crate::command_palette::PaletteCommand { id: "open_settings_diagnostics", label: "设置：诊断", keywords: &["diagnostics", "debug"], group: "设置" },
            crate::command_palette::PaletteCommand { id: "toggle_diag", label: "切换诊断浮层", keywords: &["diag", "debug"], group: "视图" },
            crate::command_palette::PaletteCommand { id: "workspace_back", label: "后退", keywords: &["back"], group: "导航" },
            crate::command_palette::PaletteCommand { id: "workspace_forward", label: "前进", keywords: &["forward"], group: "导航" },
            crate::command_palette::PaletteCommand { id: "reveal_selected", label: "在目录中定位", keywords: &["reveal", "locate"], group: "文件" },
        ]
    }

    fn run_command_palette_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.command_palette_query.read(cx).value().to_string();
        let cmds = self.command_palette_commands();
        let matches = crate::command_palette::filter(&cmds, &query);
        let idx = self.command_palette_selected.min(matches.len().saturating_sub(1));
        let Some(m) = matches.get(idx) else {
            self.command_palette_open = false;
            cx.notify();
            return;
        };
        let id = m.command.id;
        self.command_palette_open = false;
        cx.notify();
        match id {
            "focus_search" => {
                if self.workspace.mode == ViewMode::Directory {
                    self.focus_directory_filter(window, cx);
                } else {
                    self.workspace_switch(ViewMode::Search, cx);
                    window.focus(&self.search.read(cx).focus_handle(cx), cx);
                }
            }
            "open_settings" => self.open_settings_page(window, cx),
            "open_settings_index" => self.open_settings_page_at(Some("index"), window, cx),
            "open_settings_diagnostics" => self.open_settings_page_at(Some("diagnostics"), window, cx),
            "toggle_diag" => self.toggle_diag(cx),
            "workspace_back" => {
                if !self.text_edit_active(window, cx) { self.workspace_history(false, cx); }
            }
            "workspace_forward" => {
                if !self.text_edit_active(window, cx) { self.workspace_history(true, cx); }
            }
            "reveal_selected" => self.reveal_selected(cx),
            _ => {}
        }
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
        row_width: f32,
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
                    .w(px(row_width))
                    .h(px(28.))
                    .px_1()
                    .gap_2()
                    .items_center()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |this| this.bg(tokens.row_selected))
                    .when(!selected, |this| {
                        this.hover(|s| s.bg(tokens.row_selected.opacity(0.55)))
                    })
                    .child(
                        div()
                            .w(px(3.))
                            .h(px(12.))
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
            .gap_1()
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
                    (self.actual_sidebar_width - 16.).max(88.),
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
            .gap_1()
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
                    (self.actual_sidebar_width - 16.).max(88.),
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
            .w(px(self.actual_sidebar_width))
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
        if !self.preview_is_current() || self.pdf_count == 0 { return; }
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
            self.request_preview(cx, false);
        } else {
            self.preview_options = self.pdf_options;
        }
        cx.notify();
    }
    fn render_pdf_controls(&self, cx: &Context<Self>) -> impl IntoElement {
        let page = self.preview_options.page;
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
                        .disabled(disabled || !self.preview_is_current())
                        .on_click(move |_, _, cx| e.update(cx, |this, cx| this.pdf_action(id, cx)))
                }),
            )
            .child(
                div()
                    .id("pdf-page-counter")
                    .role(gpui_kit::accesskit::Role::Label)
                    .aria_label(format!(
                        "PDF 第 {} / {} 页，{}",
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
                            .disabled(!self.preview_is_current())
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
                    .disabled(!self.preview_is_current())
                    .ghost()
                    .icon(Icon::default().path("icons/move-horizontal.svg"))
                    .tooltip("适应宽度")
                    .accessibility_label("适宽")
                    .selected(self.preview_options.fit_width)
                    .w(px(22.))
                    .h(px(24.))
                    .px_0()
                    .text_xs()
                    .on_click(move |_, _, cx| e.update(cx, |this, cx| this.pdf_action("fit", cx)))
            })
            .child({
                let e = cx.entity();
                Button::new("actual")
                    .disabled(!self.preview_is_current())
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
                .h(px(56.)).overflow_hidden()
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
            None => div().h(px(56.)).flex_shrink_0().into_any_element(),
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
                let width = if self.preview_options.fit_width {
                    self.actual_preview_width - 24.
                } else {
                    pdf.width * self.preview_options.zoom
                };
                let height = width * pdf.height / pdf.width.max(1.);
                // 本页命中框（点→显示像素；仅同文件同页展示，翻页/切文件即失效）。
                let show_hl = self.preview.pdf_hl_page == Some(pdf.page)
                    && self
                        .preview
                        .pdf_hl_path
                        .as_deref()
                        .zip(self.preview.meta.as_ref().map(|m| m.path.as_str()))
                        .is_some_and(|(a, b)| a == b);
                let hl = if show_hl {
                    self.preview.pdf_highlights.clone()
                } else {
                    Vec::new()
                };
                let sx = width / pdf.width.max(1.);
                let sy = height / pdf.height.max(1.);
                let mut stack = div().relative().child(
                    img(self.preview.image.clone().unwrap())
                        .w(px(width))
                        .h(px(height))
                        .object_fit(ObjectFit::Contain),
                );
                for r in hl {
                    let (w, h) = ((r[2] - r[0]) * sx, (r[3] - r[1]) * sy);
                    if w <= 0. || h <= 0. {
                        continue;
                    }
                    stack = stack.child(
                        div()
                            .absolute()
                            .left(px(r[0] * sx))
                            .top(px(r[1] * sy))
                            .w(px(w))
                            .h(px(h))
                            .bg(rgba(0xffee58cc)),
                    );
                }
                let mut col = v_flex().p_3().gap_1();
                if !self.preview.pdf_highlights.is_empty() && show_hl {
                    col = col.child(
                        div()
                            .text_xs()
                            .text_color(tokens.text_tertiary)
                            .child(format!("本页 {} 个命中", self.preview.pdf_highlights.len())),
                    );
                }
                col.child(stack).into_any_element()
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
                        format!("{enc}，仅显示开头部分")
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
            .when(self.preview.meta.as_ref().is_some_and(|m| std::path::Path::new(&m.path)
                .extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("pdf")))
                && self.preview.pdf.is_some(), |view| {
                view.child(self.render_pdf_controls(cx))
            })
            .when(
                self.preview
                    .meta
                    .as_ref()
                    .is_some_and(|m| preview_has_mode_tabs(&m.path)),
                |view| {
                    view.child(
                        h_flex().px_2().py_1().gap_1().flex_shrink_0().children(
                            {
                                // 命中上下文排第一个：全文检索即出现（不等异步摘要），
                                // 非全文查询仍有摘要时也展示。
                                let mut tabs: Vec<(&'static str, &'static str, bool)> =
                                    vec![];
                                if self.hit_ctx_available(cx) {
                                    tabs.push((
                                        "snippet",
                                        "命中上下文",
                                        self.preview_snippet_view,
                                    ));
                                }
                                tabs.push((
                                    "office-native",
                                    "系统版式",
                                    !self.office_text_only && !self.preview_snippet_view,
                                ));
                                tabs.push((
                                    "office-text",
                                    "文字摘录",
                                    self.office_text_only && !self.preview_snippet_view,
                                ));
                                tabs
                            }
                            .into_iter()
                            .map(|(id, label, selected)| {
                                let entity = cx.entity();
                                Button::new(id)
                                    .ghost()
                                    .label(label)
                                    .selected(selected)
                                    .on_click(move |_, _, cx| {
                                        entity.update(cx, |this, cx| {
                                            if id == "snippet" {
                                                // 摘要已在内存，只切视图；
                                                // 切走 tab 时全文被清，回来补一次。
                                                this.preview_snippet_view = true;
                                                this.preview_snippet_lock = true;
                                                if this.full_text.is_empty() {
                                                    if let Some(h) = this
                                                        .active_workspace_hit()
                                                    {
                                                        if !h.is_directory {
                                                            this.fetch_full_text(h.entry_idx, cx);
                                                            if std::path::Path::new(&h.path)
                                                                .extension()
                                                                .and_then(|e| e.to_str())
                                                                .is_some_and(|e| {
                                                                    e.eq_ignore_ascii_case("docx")
                                                                })
                                                            {
                                                                this.fetch_docx_blocks(
                                                                    h.path.clone(),
                                                                    cx,
                                                                );
                                                            }
                                                        }
                                                    }
                                                }
                                                cx.notify();
                                                return;
                                            }
                                            if !this.preview_is_current() {
                                                return;
                                            }
                                            this.office_text_only = id == "office-text";
                                            // 意图先行：request_preview 按当前视图算 intent，
                                            // 必须先落到目标 tab 再请求。
                                            this.preview_snippet_view = false;
                                            this.preview_snippet_lock = true;
                                            this.request_preview(cx, false);
                                            cx.notify();
                                        })
                                    })
                            }),
                        ),
                    )
                },
            )
            // 命中上下文的操作栏：上一个/下一个翻页
            //（仅 office 预览、摘要 tab 激活且有多处匹配时展示一行；
            // PDF 可视态走专用列，这里隐藏，避免重复）。
            .when(
                !self.hit_text_active()
                    && self.preview_snippet_view
                    && self.snippet_total > 1
                    && self
                        .preview
                        .meta
                        .as_ref()
                        .is_some_and(|m| preview_has_mode_tabs(&m.path)),
                |view| {
                    view.child(
                        h_flex()
                            .w_full()
                            .flex_shrink_0()
                            .px_2()
                            .py_1()
                            .gap_1()
                            .border_b_1()
                            .border_color(tokens.border_soft)
                            .child(self.snippet_pager(cx)),
                    )
                },
            )
            // 编号命中列表（office 摘要 tab 同款，位置在操作栏与正文之间；
            // PDF 可视态走专用列这里隐藏避免重复）。
            .when(
                !self.hit_text_active()
                    && self.preview_snippet_view
                    && self.snippet_list_open
                    && self
                        .preview
                        .meta
                        .as_ref()
                        .is_some_and(|m| preview_has_mode_tabs(&m.path)),
                |view| {
                    view.child(
                        div()
                            .w_full()
                            .flex_shrink_0()
                            .px_2()
                            .py_1()
                            .border_b_1()
                            .border_color(tokens.border_soft)
                            .child(self.snippet_list_view(cx)),
                    )
                },
            )
            // PDF 可视命中上下文专用列：内容在上、导航在下。
            // 光栅（含黄框）占整块 + pager + 编号列表（capped 高度内部滚动）。
            // 光栅未就绪时不走这里（走下面的纯文本渐进态），避免转圈。
            .child(if self.hit_text_active() {
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(
                        div()
                            .id("hit-text-scroll")
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .track_scroll(&self.hit_scroll)
                            // 必须用原生 overflow_y_scroll：gpui-component 的
                            // overflow_y_scrollbar() 返回 Scrollable，它在 render 里
                            // 自建 scroll_handle，会完全忽略 track_scroll 的句柄
                            // （这是此前 set_offset/scroll_to_item 全部无效的根因）。
                            .overflow_y_scroll()
                            .px_3()
                            .py_2()
                            .text_sm()
                            // 每个块一个子元素：ScrollHandle::scroll_to_item(块号) 才能定位。
                            .children(if let Some(doc) = self.docx_doc.as_ref() {
                                // docx：纯 Rust 自绘真版式（标题/段落/粗体/表格/命中高亮）。
                                crate::ui::rich_view::render_docx(doc, &self.docx_hits, None, &tokens)
                            } else {
                                // 未解析完成或解析失败：回退纯文本列（渐进态）。
                                let (plain, ranges) = self.hit_text_and_ranges();
                                if plain.trim().is_empty() {
                                    vec![div()
                                        .text_sm()
                                        .text_color(tokens.text_tertiary)
                                        .child("正在加载命中上下文…")
                                        .into_any_element()]
                                } else {
                                    vec![StyledText::new(plain)
                                        .with_highlights(ranges.into_iter().map(|range| {
                                            (
                                                range,
                                                HighlightStyle {
                                                    color: Some(tokens.hit_fg),
                                                    background_color: Some(tokens.hit_bg),
                                                    ..Default::default()
                                                },
                                            )
                                        }))
                                        .into_any_element()]
                                }
                            })
                            .into_any_element(),
                    )
                    .child(
                        div()
                            .id("hit-split-divider")
                            .w_full()
                            .h(px(6.))
                            .flex_shrink_0()
                            .bg(tokens.border)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                    this.split_drag = Some((
                                        event.position.y.as_f32(),
                                        this.match_list_h,
                                    ));
                                    cx.notify();
                                }),
                            )
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    this.split_drag = None;
                                    cx.notify();
                                }),
                            ),
                    )
                    .child(
                        v_flex()
                            .w_full()
                            .flex_shrink_0()
                            .h(px(self.match_list_h))
                            .overflow_hidden()
                            .border_t_1()
                            .border_color(tokens.border_soft)
                            .child(self.snippet_pager(cx))
                            .child(if self.snippet_list_open {
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scrollbar()
                                    .child(self.snippet_list_view(cx))
                                    .into_any_element()
                            } else {
                                div().into_any_element()
                            }),
                    )
                    .on_mouse_move(cx.listener(
                        |this, event: &MouseMoveEvent, _, cx| {
                            if let Some((start_y, start_h)) = this.split_drag {
                                let h = (start_h + (event.position.y.as_f32() - start_y))
                                    .clamp(80.0, 600.0);
                                if (h - this.match_list_h).abs() > 0.5 {
                                    this.match_list_h = h;
                                    cx.notify();
                                }
                            }
                        },
                    ))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            if this.split_drag.take().is_some() {
                                cx.notify();
                            }
                        }),
                    )
                    .into_any_element()
            } else if self.preview.pdf.is_some() {
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
        // Each window converges its own backdrop: a cross-window update
        // cannot touch a window mid-dispatch (its state is taken out of the
        // map), but render always runs inside its own window. DWM is only
        // touched when prefs actually changed.
        let (material, dark) = self.session.read_with(cx, |s, _| {
            (
                s.prefs.window_material,
                s.effective_theme().is_dark(),
            )
        });
        if self.applied_material != Some(material) {
            self.applied_material =
                Some(crate::window_material::apply_to_window(window, material, dark));
        }
        if crate::installer_health::active() {
            let session=self.session.read(cx);
            crate::installer_health::rendered(window,session.prefs.clone(),session.settings.clone());
        }
        let tokens = Self::tokens(cx);
        let viewport = window.viewport_size();
        let content_width = viewport.width.as_f32();
        self.last_content_width = content_width;
        self.ensure_usage_scene(window, cx);
        let wanted_sidebar = self
            .session
            .read(cx)
            .prefs
            .sidebar_width
            .unwrap_or(if self.workspace.mode == ViewMode::Directory {
                workspace::DIRECTORY_SIDEBAR_WIDTH
            } else {
                SIDEBAR_WIDTH_PX
            })
            .clamp(120., 320.);
        let rail_width = wanted_sidebar;
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
            wanted_sidebar,
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
        self.actual_sidebar_width = wanted_sidebar;
        self.table_viewport_width = content_width
            - if sidebar { rail_width } else { 0. }
            - if sidebar { 2. } else { 0. }
            - if preview {
                self.actual_preview_width + 2.
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
            // 摘要 tab 激活时原生覆盖层必须让位：否则它盖住摘要正文还抢焦点
            //（这正是“切回命中上下文没反应”的根因：kind 没变，mounted 照旧）。
            let snippet_active =
                self.preview_snippet_view && !self.preview_snippet.is_empty();
            client
                .surface
                .set_interactive(self.preview_is_current() && !snippet_active);
            let mounted = preview
                && matches!(self.preview.kind, PreviewKind::NativeOffice)
                && !snippet_active;
            let allowed = crate::preview_visibility::surface_allowed(
                mounted,
                self.workspace.show_actions || self.file_actions.native_menu.is_open(),
                self.workspace.address_menu.is_some(),
                self.file_actions.keyboard_menu.is_some(),
            );
            client.surface.set_suppressed(!allowed);
        }

        // A backdrop material needs the final pixel to stay translucent: the
        // full-window root background is only painted for the solid material.
        // Inner panes keep their own backgrounds (opaque content, dimmed
        // chrome), so only the frame shows the backdrop through.
        let solid_root = self.session.read(cx).prefs.window_material
            == crate::window_material::WindowMaterial::Solid;
        v_flex()
            .id("findx-main")
            .capture_any_mouse_down(cx.listener(|this, event: &MouseDownEvent, window, cx| {
                // GPUI marks any element under a MouseDown as "active" in the
                // bubble phase, including right-clicks. That leaves a stale
                // pressed/hover highlight on rows and buttons after a context
                // menu closes. Right-click never needs that pressed state, and
                // gpui-component's context menu does not check default_prevented.
                if event.button == MouseButton::Right {
                    window.prevent_default();
                }
                if this.table_navigation_focus.pointer() {
                    cx.notify();
                }
            }))
            // No bare Space/Enter keymap actions: keymap actions run before raw
            // handlers and discard is_held. Scoped NoAction also bypasses the
            // SDK table Escape Cancel; native controls/inputs still win.
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key=="escape" && !this.text_edit_active(window,cx) && this.dismiss_usage_facts(){cx.notify();cx.stop_propagation();return;}
                use crate::ui::native_focus::{FileKey, FileKeyRoute, route_file_key};
                if FileKey::parse(&event.keystroke).is_none() {
                    return;
                }
                let allowed = file_shortcuts_allowed(&window.context_stack());
                let editing = !allowed || this.text_edit_active(window, cx);
                let file_target = this.file_navigation_focused(window, cx);
                if crate::isolation::development_var_os("FINDX_NATIVE_KEY_DIAGNOSTICS").is_some() {
                    eprintln!("[native-key] key={} held={} editing={} file_focus={} selected={:?} context={:?}",event.keystroke.key,event.is_held,editing,file_target,this.selected,window.context_stack());
                }
                match route_file_key(event, editing, file_target) {
                    FileKeyRoute::Pass => return,
                    FileKeyRoute::ConsumeRepeat => {}
                    FileKeyRoute::Dispatch(key) => {
                        let action: Box<dyn Action> = match key {
                            FileKey::Preview => Box::new(PreviewSelected),
                            FileKey::Open => Box::new(OpenSelected),
                            FileKey::Dismiss => Box::new(DismissTransient),
                        };
                        window.dispatch_action(action, cx);
                    }
                }
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &gpui_kit::component::input::MoveDown, window, cx| {
                if this.query_key_allowed(window, cx) {
                    this.request_search_key(crate::interaction_logic::SearchKeyIntent::Next, window, cx);
                    cx.stop_propagation();
                } else { cx.propagate(); }
            }))
            .capture_action(cx.listener(|this, _: &gpui_kit::component::input::MoveUp, window, cx| {
                if this.query_key_allowed(window, cx) {
                    this.request_search_key(crate::interaction_logic::SearchKeyIntent::Previous, window, cx);
                    cx.stop_propagation();
                } else { cx.propagate(); }
            }))
            .capture_action(cx.listener(|this, _: &gpui_kit::component::input::Escape, window, cx| {
                if this.query_key_allowed(window, cx) { this.search_keyboard.cancel(); }
                cx.propagate();
            }))
            .capture_action(cx.listener(|this, action: &gpui_kit::component::input::Enter, window, cx| {
                if !action.shift && !action.secondary && this.query_key_allowed(window, cx) {
                    this.request_search_key(crate::interaction_logic::SearchKeyIntent::Open, window, cx);
                    cx.stop_propagation();
                } else { cx.propagate(); }
            }))
            .key_context("FindXMain")
            .track_focus(&self.focus)
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .relative()
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                this.resize_preview_move(event, cx);
                this.resize_sidebar_move(event, cx)
            }))
            .capture_any_mouse_down(cx.listener(|this,event:&MouseDownEvent,_,cx|{
                this.workspace_native_side(event.button,cx);
            }))
            .on_mouse_up(MouseButton::Navigate(NavigationDirection::Back),cx.listener(|this,_,_,_|this.release_side_button(false)))
            .on_mouse_up_out(MouseButton::Navigate(NavigationDirection::Back),cx.listener(|this,_,_,_|this.release_side_button(false)))
            .on_mouse_up(MouseButton::Navigate(NavigationDirection::Forward),cx.listener(|this,_,_,_|this.release_side_button(true)))
            .on_mouse_up_out(MouseButton::Navigate(NavigationDirection::Forward),cx.listener(|this,_,_,_|this.release_side_button(true)))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, window, cx| {
                    this.finish_preview_resize(window, cx);
                    this.finish_sidebar_resize(window, cx)
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, window, cx| {
                    if event.button == MouseButton::Left {
                        this.finish_preview_resize(window, cx);
                        this.finish_sidebar_resize(window, cx);
                    }
                }),
            )
            .overflow_hidden()
            .when(solid_root, |v| v.bg(tokens.bg))
            .text_color(tokens.text_primary)
            .on_action(cx.listener(|this, _: &WorkspaceBack, window, cx| {
                if !this.text_edit_active(window, cx) {
                    this.workspace_history(false, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &WorkspaceForward, window, cx| {
                if !this.text_edit_active(window, cx) {
                    this.workspace_history(true, cx)
                }
            }))
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
            .on_action(
                cx.listener(|this, _: &ContextOpenInNewTab, window, cx| {
                    this.context_action(11, window, cx)
                }),
            )
            .on_action(cx.listener(|this, action: &SidebarOpenNewTab, window, cx| {
                this.open_directory_in_new_tab(PathBuf::from(action.path.to_string()), window, cx);
            }))
            .on_action(cx.listener(|this, action: &SidebarToggleFavorite, _, cx| {
                this.toggle_favorite(PathBuf::from(action.path.to_string()), cx);
            }))
            .on_action(cx.listener(|this, action: &SidebarReveal, _, cx| {
                this.reveal_sidebar_path(&action.path, cx);
            }))
            .on_action(cx.listener(|this, action: &SidebarCopyPath, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(action.path.to_string()));
                this.workspace.status = "已复制路径".into();
                cx.notify();
            }))
            .on_action(cx.listener(|this, action: &SidebarToggleExpand, _, cx| {
                this.toggle_tree_node(PathBuf::from(action.path.to_string()), cx);
            }))
            .on_action(cx.listener(|this, _: &ContextToggleFavorite, window, cx| {
                this.context_action(13, window, cx)
            }))
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
            .on_action(cx.listener(|this, _: &WorkspaceUp, window, cx| {
                if !this.text_edit_active(window, cx) {
                    this.workspace_up(cx)
                }
            }))
            .on_action(cx.listener(|this, _: &ParentSelected, window, cx| {
                if !this.text_edit_active(window, cx) {
                    this.workspace_up(cx)
                }
            }))
            .on_action(cx.listener(|this, _: &PreviewSelected, window, cx| {
                if this.text_edit_active(window, cx)
                    || this.preview_resize.is_some()
                    || this.sidebar_resize.is_some()
                    || this.file_menu_is_open()
                    || this.run_pending.is_some()
                    || this.file_actions.confirm_target.is_some()
                {
                    return;
                }
                if this.active_workspace_hit().is_none() {
                    return;
                }
                this.preview_force = Some(crate::interaction_logic::toggle_preview_force(
                    this.preview_force,
                    this.session.read(cx).prefs.preview_open,
                ));
                this.preview_active_selection(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &DismissTransient, window, cx| {
                if this.text_edit_active(window, cx) {
                    return;
                }
                this.search_keyboard.cancel();
                crate::interaction_logic::cancel_deferred_file_intents(
                    &mut this.pending_open,
                    &mut this.file_actions.pending_shell,
                );
                if this.preview_resize.is_some() {
                    this.finish_preview_resize(window, cx);
                    return;
                }
                if this.sidebar_resize.is_some() {
                    this.finish_sidebar_resize(window, cx);
                    return;
                }
                if this.file_menu_is_open() {
                    this.dismiss_file_menu(cx);
                    return;
                }
                if this.run_pending.take().is_some()
                    || this.file_actions.confirm_target.take().is_some()
                {
                    cx.notify();
                    return;
                }
                this.workspace.address_menu = None;
                this.workspace.show_actions = false;
                this.preview_force = Some(ForceState::Hide);
                cx.notify();
            }))
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
            .on_action(cx.listener(|this, _: &DirectoryLastTab, _, cx| {
                this.select_directory_tab_number(0, cx)
            }))
            .on_action(cx.listener(|this, _: &DirectoryTab1, _, cx| {
                this.select_directory_tab_number(1, cx)
            }))
            .on_action(cx.listener(|this, _: &DirectoryTab2, _, cx| {
                this.select_directory_tab_number(2, cx)
            }))
            .on_action(cx.listener(|this, _: &DirectoryTab3, _, cx| {
                this.select_directory_tab_number(3, cx)
            }))
            .on_action(cx.listener(|this, _: &DirectoryTab4, _, cx| {
                this.select_directory_tab_number(4, cx)
            }))
            .on_action(cx.listener(|this, _: &DirectoryTab5, _, cx| {
                this.select_directory_tab_number(5, cx)
            }))
            .on_action(cx.listener(|this, _: &DirectoryTab6, _, cx| {
                this.select_directory_tab_number(6, cx)
            }))
            .on_action(cx.listener(|this, _: &DirectoryTab7, _, cx| {
                this.select_directory_tab_number(7, cx)
            }))
            .on_action(cx.listener(|this, _: &DirectoryTab8, _, cx| {
                this.select_directory_tab_number(8, cx)
            }))
            .on_action(cx.listener(|this, _: &DirectoryTab9, _, cx| {
                this.select_directory_tab_number(9, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                if this.settings_page.is_some() || this.workspace.mode == ViewMode::Search {
                    return;
                }
                let pref = this.session.read(cx).prefs.sidebar_open;
                let effective = match this.sidebar_force {
                    Some(ForceState::Show) => true,
                    Some(ForceState::Hide) => false,
                    None => pref,
                };
                this.toggle_panel(search_logic::PanelKind::Sidebar, effective, cx);
            }))
            .on_action(cx.listener(|this, _: &WorkspaceFocusPath, window, cx| {
                if this.settings_page.is_some() || this.workspace.mode == ViewMode::Search {
                    return;
                }
                this.begin_path_edit(window, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectNextResult, window, cx| {
                if this.text_edit_active(window, cx) {
                    return;
                }
                this.move_sel(ResultNavKey::ArrowDown, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectPrevResult, window, cx| {
                if this.text_edit_active(window, cx) {
                    return;
                }
                this.move_sel(ResultNavKey::ArrowUp, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectFirstResult, window, cx| {
                // Home/End in the search box should stay with the input.
                if this.text_edit_active(window, cx) {
                    return;
                }
                this.move_sel(ResultNavKey::Home, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectLastResult, window, cx| {
                if this.text_edit_active(window, cx) {
                    return;
                }
                this.move_sel(ResultNavKey::End, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenSelected, window, cx| {
                if this.text_edit_active(window, cx)
                    || this.preview_resize.is_some()
                    || this.sidebar_resize.is_some()
                    || this.file_menu_is_open()
                    || this.run_pending.is_some()
                    || this.file_actions.confirm_target.is_some()
                {
                    return;
                }
                this.open_selected(cx)
            }))
            .on_action(cx.listener(|this, _: &RevealSelected, _, cx| this.reveal_selected(cx)))
            .on_action(cx.listener(|this, _: &ToggleDiag, _, cx| this.toggle_diag(cx)))
            .on_action(cx.listener(|this, _: &ToggleCommandPalette, window, cx| {
                this.toggle_command_palette(window, cx)
            }))
            .on_action(cx.listener(|this, _: &DismissCommandPalette, _, cx| {
                if this.command_palette_open {
                    this.command_palette_open = false;
                    cx.notify();
                }
            }))
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
                .when(self.command_palette_open, |view| {
                view.child(self.render_command_palette(window, cx))
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
                            .child(v_flex().when_some(self.workspace.address_menu_header.clone(),|container,header|container
                                .w(px(372.)).h(px(340.))
                                .child(div().h(px(28.)).flex_shrink_0().w_full().px_3().py_1().bg(Self::tokens(cx).surface).border_1().border_color(Self::tokens(cx).border).text_xs().overflow_hidden().whitespace_nowrap().child(header)))
                                .child(menu)),
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
    fn render_command_palette(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Self::tokens(cx);
        let query = self.command_palette_query.read(cx).value().to_string();
        let cmds = self.command_palette_commands();
        let matches = crate::command_palette::filter(&cmds, &query);
        let selected = self.command_palette_selected.min(matches.len().saturating_sub(1));
        let list: Vec<AnyElement> = matches
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let is_sel = i == selected;
                div()
                    .id(SharedString::from(format!("cp-{}", m.command.id)))
                    .w_full()
                    .h(px(32.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_md()
                    .when(is_sel, |d| d.bg(tokens.accent))
                    .child(div().text_xs().text_color(tokens.text_tertiary).child(m.command.group))
                    .child(div().text_sm().child(m.command.label))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.command_palette_selected = i;
                        this.run_command_palette_selected(window, cx);
                    }))
                    .into_any_element()
            })
            .collect();
        div()
            .absolute()
            .inset_0()
            .occlude()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "escape" => {
                        this.command_palette_open = false;
                        cx.stop_propagation();
                        cx.notify();
                    }
                    "up" => {
                        if this.command_palette_selected > 0 {
                            this.command_palette_selected -= 1;
                            cx.stop_propagation();
                            cx.notify();
                        }
                    }
                    "down" => {
                        let q = this.command_palette_query.read(cx).value().to_string();
                        let cmds = this.command_palette_commands();
                        let n = crate::command_palette::filter(&cmds, &q).len();
                        if this.command_palette_selected + 1 < n {
                            this.command_palette_selected += 1;
                            cx.stop_propagation();
                            cx.notify();
                        }
                    }
                    "enter" => {
                        this.run_command_palette_selected(window, cx);
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }))
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x00000066))
            .child(
                v_flex()
                    .w(px(420.))
                    .max_h(px(480.))
                    .bg(tokens.surface)
                    .border_1()
                    .border_color(tokens.border)
                    .rounded_lg()
                    .shadow_lg()
                    .p_3()
                    .gap_2()
                    .child(Input::new(&self.command_palette_query).w_full().h(px(34.)))
                    .child(
                        div()
                            .id("cp-list")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scrollbar()
                            .children(if list.is_empty() {
                                vec![div().text_sm().text_color(tokens.text_tertiary).child("无匹配命令").into_any_element()]
                            } else {
                                list
                            }),
                    )
                    .child(div().text_xs().text_color(tokens.text_tertiary).child("↑↓ 选择 · Enter 执行 · Esc 关闭")),
            )
            .into_any_element()
    }

    /// 悬浮卡片共用小件：标题 / 键值行 / 正文行（深浅主题靠透明度区分，不写死颜色）。
    fn tip_title(text: &str) -> Div {
        div()
            .text_sm()
            .font_weight(FontWeight::SEMIBOLD)
            .child(text.to_string())
    }
    fn tip_kv(label: &str, value: String) -> Div {
        h_flex()
            .gap_2()
            .items_start()
            .child(
                div()
                    .w(px(52.))
                    .flex_shrink_0()
                    .text_xs()
                    .opacity(0.65)
                    .child(label.to_string()),
            )
            .child(div().flex_1().min_w_0().text_xs().child(value))
    }
    fn tip_note(text: String) -> Div {
        div().flex_1().min_w_0().text_xs().child(text)
    }
    /// “已索引 N 项”悬浮卡：标题 + 键值行 + 任务/告警/WAL 段（替代裸字符串拼接）。
    /// Tooltip 容器自带 popover 底/边框/阴影，这里只管内容排版；
    /// 颜色全部继承容器，层级靠字号/透明度区分，保证深浅主题都可用。
    /// `warn` 为告警标题色（调用方从主题取）。
    fn index_tooltip_card(tip: crate::footer_status::IndexTooltip, warn: Hsla) -> impl IntoElement {
        v_flex()
            .max_w(px(340.))
            .gap_1()
            .p_1()
            .child(Self::tip_title("索引统计"))
            .child(Self::tip_kv(
                "统计范围",
                "全部已索引位置，不代表已扫描所有磁盘".into(),
            ))
            .child(Self::tip_kv("更新频率", tip.freshness.clone()))
            .child(Self::tip_kv(
                "统计口径",
                "含文件和目录，已扣删除项；尚不能拆分文件数和目录数".into(),
            ))
            .when_some(tip.scan.clone(), |v, scan| {
                v.child(Self::tip_title("后台任务"))
                    .child(Self::tip_note(scan))
            })
            .when_some(tip.alert.clone(), |v, alert| {
                v.child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(warn)
                        .child("需要注意"),
                )
                .child(Self::tip_note(alert))
            })
            .when_some(tip.wal_state.clone(), |v, state| {
                v.child(Self::tip_title("后台写入（WAL）"))
                    .child(Self::tip_kv("当前状态", state))
                    .child(
                        div()
                            .text_xs()
                            .opacity(0.8)
                            .child("心跳 5 秒 fsync（KB 级）；256MB / 2 小时 / 100 万条做一次 fold；常态写入应接近 0"),
                    )
            })
    }

    /// 占用页悬浮卡：扫描管线的多行诊断串按已知行式分类展示。
    /// 生产方字符串保持原样（测试与日志不受影响），未知行按正文直排。
    fn usage_tooltip_card(scan: String, warn: Hsla) -> impl IntoElement {
        let mut title = "占用详情".to_string();
        let mut scope: Vec<String> = Vec::new();
        let mut quality: Vec<String> = Vec::new();
        let mut monitor: Vec<String> = Vec::new();
        let mut generation: Option<String> = None;
        let mut probe: Vec<String> = Vec::new();
        let mut incomplete = false;
        let mut english: Vec<String> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        for (i, raw) in scan.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            if i == 0
                && (line == "索引逻辑"
                    || line == "索引可能滞后"
                    || line == "缓存可能过期"
                    || line.starts_with("正在")
                    || line.starts_with("目录读取失败"))
            {
                title = line.to_string();
                continue;
            }
            if line.starts_with("当前分页") || line.starts_with("当前层完整") {
                scope.push(line.to_string());
            } else if line.starts_with("元数据待验") {
                quality.push(line.to_string());
            } else if line.starts_with("监控") {
                monitor.push(line.to_string());
            } else if line.len() < 20
                && line.starts_with('g')
                && line[1..].chars().all(|c| c.is_ascii_digit())
            {
                generation = Some(line.to_string());
            } else if line.contains("显示") && line.contains("请求") {
                probe.push(line.to_string());
            } else if line.ends_with("节点") {
                probe.push(line.to_string());
            } else if line.contains("次读取") {
                probe.push(line.to_string());
            } else if line.starts_with("后代数据未完整") {
                incomplete = true;
            } else if line.is_ascii() {
                english.push(line.to_string());
            } else {
                notes.push(line.to_string());
            }
        }
        let mut v = v_flex().max_w(px(360.)).gap_1().p_1();
        v = v.child(Self::tip_title(&title));
        if !scope.is_empty() {
            v = v.child(Self::tip_note(scope.join("；")));
        }
        if !quality.is_empty() || !monitor.is_empty() {
            let mut parts = quality;
            parts.extend(monitor);
            v = v.child(Self::tip_kv("数据质量", parts.join("；")));
        }
        if let Some(g) = generation {
            v = v.child(Self::tip_kv("代数", g));
        }
        if !probe.is_empty() {
            v = v.child(Self::tip_kv("探测", probe.join(" · ")));
        }
        if incomplete {
            v = v.child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(warn)
                    .child("后代数据未完整（有界预算/错误）"),
            );
        }
        for n in notes {
            v = v.child(Self::tip_note(n));
        }
        if !english.is_empty() {
            v = v.child(Self::tip_title("原始详情（诊断）"));
            v = v.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .opacity(0.65)
                    .child(english.join("\n")),
            );
        }
        v
    }

    /// Prefetch probe for the status bar: whether entering the current
    /// directory warmed its children sizes ahead of time. Fixed width so
    /// the switch next to it never jumps.
    /// Fill-state marker for the status bar: it answers "are THIS page's
    /// folder sizes actually on screen", not whether the index cache was
    /// warmed. Green = all shown, pulsing = fill running, amber = blank
    /// cells remain (index unreachable; retry on next navigation).
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
                .child(div().w(px(200.)).flex_shrink_0().text_ellipsis().child(if selected > 0 {
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
        let footer = self.session.read(cx).engine_footer.clone();
        let left: String = if self.workspace.mode == ViewMode::Search {
            if !footer.connected {
                // The right-hand indicator already shows 服务未运行 / 未连接 / 连接已断开.
                String::new()
            } else if self.file_actions.summary.is_empty() {
                self.search_status.to_string()
            } else {
                format!("{}  {}", self.search_status, self.file_actions.summary)
            }
        } else {
            self.workspace_footer_label()
        };
        let left_detail = if self.workspace.mode == ViewMode::Usage {
            self.workspace_scan_status()
        } else {
            left.clone()
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
            .child(
                div()
                    .id("footer-operation-summary")
                    .role(gpui_kit::accesskit::Role::Status)
                    .accessibility_id("usage-read-state")
                    .aria_label(left_detail.clone())
                    .flex_1()
                    .min_w_0()
                    .text_ellipsis()
                    .tooltip({
                        let warn_color = tokens.warning;
                        move |window, cx| {
                            let tip_text = left_detail.clone();
                            // 占用页的悬浮提示是扫描管线的多行诊断串：按已知行式
                            // 分类成卡片（生产方字符串原样保留，测试与日志不受影响）。
                            let card = if tip_text.lines().count() > 1 {
                                Some(tip_text.clone())
                            } else {
                                None
                            };
                            match card.clone() {
                                Some(t) => gpui_kit::component::tooltip::Tooltip::element(
                                    move |_, _| Self::usage_tooltip_card(t.clone(), warn_color),
                                )
                                .build(window, cx),
                                None => {
                                    gpui_kit::component::tooltip::Tooltip::new(tip_text).build(window, cx)
                                }
                            }
                        }
                    })
                    .child(left),
            )
            .child(
                div()
                    .id("workspace-index-status")
                    .accessibility_id("engine-status-pill")
                    .aria_label("引擎状态和索引统计")
                    .h(px(24.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .rounded_md()
                    .whitespace_nowrap()
                    .text_xs()
                    .text_color(if footer.warning() {
                        tokens.warning
                    } else {
                        tokens.text_secondary
                    })
                    .hover(|s| s.bg(tokens.surface))
                    .child(
                        Icon::default()
                            .path(if footer.warning() {
                                "icons/circle-alert.svg"
                            } else if footer.needs_details() {
                                "icons/info.svg"
                            } else {
                                "icons/circle-check.svg"
                            })
                            .w(px(14.))
                            .h(px(14.)),
                    )
                    .child(if self.workspace.mode == ViewMode::Search {
                        // 计数与状态并入同一钮：之前是“计数 + WAL 文 + 扫描图标 + 状态钮”四个元素。
                        // WAL 文案已并入悬浮卡；扫描中时标签直接体现，点击跳占用页（原小图标按钮的行为）。
                        let counts = footer.counts_label();
                        if self.workspace.scanning {
                            format!("正在扫描 · {counts}")
                        } else {
                            format!("{counts} · {}", footer.label())
                        }
                    } else {
                        footer.label()
                    })
                    .tooltip({
                        let tip_text = footer.tooltip();
                        let mut card = footer.index_tooltip();
                        if let Some(t) = card.as_mut() {
                            if self.workspace.scanning {
                                t.scan = Some(self.workspace_scan_status());
                            }
                        }
                        let warn = tokens.warning;
                        move |window, cx| {
                            let tip_text = tip_text.clone();
                            match card.clone() {
                                Some(t) => gpui_kit::component::tooltip::Tooltip::element(
                                    move |_, _| Self::index_tooltip_card(t.clone(), warn),
                                )
                                .build(window, cx),
                                None => {
                                    gpui_kit::component::tooltip::Tooltip::new(tip_text).build(window, cx)
                                }
                            }
                        }
                    })
                    .when(self.workspace.scanning, |v| {
                        v.on_click(cx.listener(|this, _, _, cx| {
                            this.workspace_switch(ViewMode::Usage, cx)
                        }))
                    }),
            )
            .into_any_element()
    }
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

#[cfg(test)]
mod shortcut_context_tests {
    use super::{DismissTransient, FILE_SHORTCUT_CONTEXT, OpenSelected, main_key_bindings};
    use gpui_kit::{KeyBindingContextPredicate, KeyContext};
    #[test]
    fn production_raw_guard_matches_real_window_table_context_and_rejects_editors() {
        use super::file_shortcuts_allowed;
        let context = |name| KeyContext::parse(name).unwrap();
        let table = vec![context("Root"), context("FindXMain"), context("DataTable")];
        assert!(file_shortcuts_allowed(&table));
        // This reproduces the runtime defect, not just the SDK keymap helper.
        let predicate = KeyBindingContextPredicate::parse(FILE_SHORTCUT_CONTEXT).unwrap();
        assert!(!predicate.eval(&table));
        for input in ["Input", "Input mode=editor", "Input mode=textarea"] {
            let mut nested = table.clone();
            nested.push(context(input));
            assert!(!file_shortcuts_allowed(&nested));
            nested.push(context("CompletionMenu"));
            assert!(!file_shortcuts_allowed(&nested));
        }
        assert!(!file_shortcuts_allowed(&[context("OtherApp"), context("DataTable")]));
        assert!(!file_shortcuts_allowed(&[]));
    }
    #[test]
    fn actual_gpui_predicate_excludes_all_nested_text_inputs() {
        let p = KeyBindingContextPredicate::parse(FILE_SHORTCUT_CONTEXT).unwrap();
        let main = KeyContext::parse("FindXMain").unwrap();
        assert!(p.depth_of(&[main.clone()]).is_some());
        assert!(
            p.depth_of(&[main.clone(), KeyContext::parse("DataTable").unwrap()])
                .is_some()
        );
        for editor in ["Input", "Input mode=editor", "Input mode=textarea"] {
            assert!(
                p.depth_of(&[main.clone(), KeyContext::parse(editor).unwrap()])
                    .is_none()
            );
            assert!(
                p.depth_of(&[
                    main.clone(),
                    KeyContext::parse(editor).unwrap(),
                    KeyContext::parse("CompletionMenu").unwrap()
                ])
                .is_none()
            );
        }
        assert!(
            p.depth_of(&[KeyContext::parse("OtherApp").unwrap()])
                .is_none()
        );
    }
    #[test]
    fn escape_overrides_real_datatable_depth_but_never_a_nested_editor() {
        use gpui_kit::{KeyBinding, Keymap, Keystroke};
        // Same shortcut/context as the SDK's private DataTable Cancel action.
        let mut keys = vec![KeyBinding::new("escape", OpenSelected, Some("DataTable"))];
        keys.extend(main_key_bindings());
        let map = Keymap::new(keys);
        let main = KeyContext::parse("FindXMain").unwrap();
        let table = KeyContext::parse("DataTable").unwrap();
        let key = Keystroke::parse("escape").unwrap();
        let (bindings, pending) =
            map.bindings_for_input(&[key.clone()], &[main.clone(), table.clone()]);
        assert!(!pending);
        assert!(
            bindings.is_empty(),
            "SDK table Escape must reach raw repeat-aware preview dismiss"
        );
        for input in ["Input", "Input mode=editor", "Input mode=textarea"] {
            let (bindings, _) = map.bindings_for_input(
                &[key.clone()],
                &[
                    main.clone(),
                    table.clone(),
                    KeyContext::parse(input).unwrap(),
                ],
            );
            assert!(
                bindings
                    .iter()
                    .all(|b| !b.action().as_any().is::<DismissTransient>()),
                "app Escape cannot hijack nested input/IME"
            );
        }
        let (bindings, _) =
            map.bindings_for_input(&[key], &[KeyContext::parse("OtherApp").unwrap(), table]);
        assert!(
            bindings
                .iter()
                .all(|b| !b.action().as_any().is::<DismissTransient>())
        );
    }
    #[test]
    fn production_keymap_leaves_bare_activation_for_repeat_aware_raw_dispatch() {
        use gpui_kit::{Keymap, Keystroke};
        let map = Keymap::new(main_key_bindings());
        for ctx in [
            vec![KeyContext::parse("FindXMain").unwrap()],
            vec![
                KeyContext::parse("FindXMain").unwrap(),
                KeyContext::parse("DataTable").unwrap(),
            ],
        ] {
            for key in ["space", "enter", "escape"] {
                let (bindings, pending) =
                    map.bindings_for_input(&[Keystroke::parse(key).unwrap()], &ctx);
                assert!(!pending);
                assert!(
                    bindings.is_empty(),
                    "{key} must reach raw held-aware dispatch"
                );
            }
            for key in ["up", "down", "home", "end", "backspace"] {
                let (bindings, _) = map.bindings_for_input(&[Keystroke::parse(key).unwrap()], &ctx);
                assert!(
                    !bindings.is_empty(),
                    "navigation {key} must retain normal repeat"
                );
            }
        }
    }
    #[test]
    fn production_override_preserves_deeper_native_button_and_input_actions() {
        use gpui_kit::{KeyBinding, Keymap, Keystroke};
        let mut keys = vec![
            KeyBinding::new("space", OpenSelected, Some("Button")),
            KeyBinding::new("enter", OpenSelected, Some("Button")),
            KeyBinding::new("escape", DismissTransient, Some("Input")),
        ];
        keys.extend(main_key_bindings());
        let map = Keymap::new(keys);
        for (key, native) in [
            ("space", "Button"),
            ("enter", "Button"),
            ("escape", "Input mode=editor"),
        ] {
            let stack = [
                KeyContext::parse("FindXMain").unwrap(),
                KeyContext::parse("DataTable").unwrap(),
                KeyContext::parse(native).unwrap(),
            ];
            let (bindings, pending) =
                map.bindings_for_input(&[Keystroke::parse(key).unwrap()], &stack);
            assert!(!pending);
            assert_eq!(
                bindings.len(),
                1,
                "native {native} {key} action must survive"
            );
        }
    }
    #[test]
    fn actual_input_context_resolves_sdk_actions_captured_by_query_navigation() {
        use gpui_kit::component::input::{Enter, MoveDown, MoveUp};
        use gpui_kit::{KeyBinding, Keymap, Keystroke};
        let mut keys = vec![
            KeyBinding::new("down", MoveDown, Some("Input")),
            KeyBinding::new("up", MoveUp, Some("Input")),
            KeyBinding::new(
                "enter",
                Enter {
                    secondary: false,
                    shift: false,
                },
                Some("Input"),
            ),
        ];
        keys.extend(main_key_bindings());
        let map = Keymap::new(keys);
        for key in ["down", "up", "enter"] {
            let (bindings, pending) = map.bindings_for_input(
                &[Keystroke::parse(key).unwrap()],
                &[
                    KeyContext::parse("FindXMain").unwrap(),
                    KeyContext::parse("Input").unwrap(),
                ],
            );
            assert!(!pending);
            assert_eq!(bindings.len(), 1);
            let a = bindings[0].action().as_any();
            assert!(match key {
                "down" => a.is::<MoveDown>(),
                "up" => a.is::<MoveUp>(),
                _ => a.is::<Enter>(),
            });
        }
    }
}
