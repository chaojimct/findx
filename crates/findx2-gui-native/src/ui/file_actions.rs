//! One action model for search rows, directory rows and allocation nodes.
use super::*;
use crate::{
    browser::FileAction,
    file_transfer::{self, Clipboard, Source},
};
use gpui_kit::component::menu::PopupMenu;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
type SelectionKey = (String, u64, u64);
fn capture_selection_sources(
    hits: Vec<SearchHitDto>,
    keys: Vec<SelectionKey>,
    known: std::collections::HashMap<SelectionKey, crate::file_identity::FileStamp>,
    cancel: Arc<AtomicBool>,
) -> Result<Vec<(SelectionKey, Source)>, String> {
    let mut sources = Vec::with_capacity(hits.len());
    for (hit, key) in hits.into_iter().zip(keys.iter()) {
        if cancel.load(Ordering::Acquire) {
            return Err("选择已更改；已取消身份核对".to_string());
        }
        let path = PathBuf::from(&hit.path);
        let stamp = if let Some(stamp) = known.get(key) {
            crate::file_identity::revalidate(&path, *stamp)?;
            *stamp
        } else {
            crate::file_identity::capture(&path)
                .map_err(|_| "条目不可访问；请检查文件、权限或磁盘连接".to_string())?
        };
        if !hit.is_directory
            && (hit.size != stamp.size
                || (hit.mtime != 0
                    && search_logic::unix_secs_from_mtime(hit.mtime)
                        != search_logic::unix_secs_from_mtime(stamp.modified)))
        {
            return Err("条目已变化或索引尚未更新；请刷新后重新选择".to_string());
        }
        sources.push((key.clone(), Source { path, stamp }));
    }
    Ok::<_, String>(sources)
}

#[derive(Default)]
pub(super) struct FileActions {
    pub(super) keyboard_menu: Option<Entity<PopupMenu>>,
    menu_subscription: Option<Subscription>,
    context_blank: bool,
    pub(super) pending_shell: Option<crate::open_path::ShellDialog>,
    clipboard: Clipboard,
    clipboard_generation: u64,
    pointer_event: Option<(bool, usize)>,
    anchor: Option<(String, usize)>,
    pub(super) confirm_target: Option<PathBuf>,
    pub(super) busy: bool,
    cancel: Arc<AtomicBool>,
    report: Vec<String>,
    pub(super) summary: String,
    show_report: bool,
    activity_dismissed: bool,
    pub(super) native_menu_open: bool,
    native_menu_focus_subscription: Option<Subscription>,
    snapshot_key: Vec<(String, u64, u64)>,
    snapshot_generation: u64,
    snapshot_cancel: Arc<AtomicBool>,
    snapshot_pending: bool,
    snapshot_error: Option<String>,
    snapshot_sources: Vec<Source>,
    identity_baselines:
        std::collections::HashMap<(String, u64, u64), crate::file_identity::FileStamp>,
    pending_clipboard: Option<bool>,
}
impl MainWindow {
    pub(crate) fn context_row_selected(&self, search: bool, row: usize, _: &App) -> bool {
        if search {
            self.search_selection.contains(&row)
        } else {
            self.workspace.selected_rows.contains(&row)
        }
    }
    pub(crate) fn selected_file_hits(&self) -> Vec<SearchHitDto> {
        if self.workspace.mode == ViewMode::Usage {
            return self.active_workspace_hit().into_iter().collect();
        }
        let (set, rows, index) = if self.workspace.mode == ViewMode::Search {
            (&self.search_selection, &self.hits, self.selected)
        } else {
            (
                &self.workspace.selected_rows,
                &self.workspace.rows,
                self.workspace.selected,
            )
        };
        if set.is_empty() {
            index
                .and_then(|i| rows.get(i).cloned())
                .into_iter()
                .collect()
        } else {
            set.iter().filter_map(|&i| rows.get(i).cloned()).collect()
        }
    }
    fn selection_table_key(&self, search: bool) -> String {
        format!(
            "{:?}",
            if search {
                self.table.entity_id()
            } else {
                self.workspace.table.entity_id()
            }
        )
    }
    pub(crate) fn selection_pointer(
        &mut self,
        search: bool,
        row: usize,
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) {
        let key = self.selection_table_key(search);
        let previous = if search {
            self.selected
        } else {
            self.workspace.selected
        };
        let anchor = self
            .file_actions
            .anchor
            .as_ref()
            .filter(|(k, _)| *k == key)
            .map(|(_, i)| *i)
            .or(previous)
            .unwrap_or(row);
        let set = if search {
            &mut self.search_selection
        } else {
            &mut self.workspace.selected_rows
        };
        update_selection(set, row, anchor, modifiers.control, modifiers.shift);
        let focus = if set.contains(&row) {
            Some(row)
        } else {
            set.iter().next_back().copied()
        };
        if !modifiers.shift {
            self.file_actions.anchor = Some((key, row));
        }
        self.file_actions.pointer_event = Some((search, row));
        if search {
            self.selected = focus;
            self.preview_file = None;
        } else {
            self.workspace.selected = focus;
        }
        self.office_text_only = false;
        self.preview_active_selection(cx);
        if focus != Some(row) {
            let owner = cx.weak_entity();
            let table_search = self.table.clone();
            let table_directory = self.workspace.table.clone();
            cx.defer(move |cx| {
                if let Some(index) = focus {
                    let _ = owner.update(cx, |view, _| {
                        view.file_actions.pointer_event = Some((search, index))
                    });
                    if search {
                        table_search.update(cx, |t, cx| t.set_selected_row(index, cx));
                    } else {
                        table_directory.update(cx, |t, cx| t.set_selected_row(index, cx));
                    }
                } else if search {
                    table_search.update(cx, |t, cx| t.clear_selection(cx));
                } else {
                    table_directory.update(cx, |t, cx| t.clear_selection(cx));
                }
            });
        }
        cx.notify();
    }
    pub(crate) fn record_single_selection(&mut self, search: bool, index: Option<usize>) {
        let set = if search {
            &mut self.search_selection
        } else {
            &mut self.workspace.selected_rows
        };
        set.clear();
        set.extend(index);
    }
    pub(crate) fn table_selected_event(
        &mut self,
        search: bool,
        row: usize,
        cx: &mut Context<Self>,
    ) {
        if self.file_actions.pointer_event.take() == Some((search, row)) {
            cx.notify();
            return;
        }
        if search {
            self.set_selected(Some(row), cx);
        } else {
            self.workspace_select(row, cx);
        }
    }
    pub(crate) fn prepare_context_row(&mut self, search: bool, row: usize, cx: &mut Context<Self>) {
        let set = if search {
            &mut self.search_selection
        } else {
            &mut self.workspace.selected_rows
        };
        if !set.contains(&row) {
            set.clear();
            set.insert(row);
        }
        if search {
            self.selected = Some(row);
        } else {
            self.workspace.selected = Some(row);
        }
        self.file_actions.pointer_event = Some((search, row));
        self.preview_active_selection(cx);
        cx.notify();
    }
    fn paste_directory(&self) -> Option<PathBuf> {
        if self.workspace.mode == ViewMode::Directory {
            Some(self.workspace.location.clone())
        } else {
            self.active_workspace_hit()
                .filter(|h| h.is_directory)
                .map(|h| h.path.into())
        }
    }
    fn track_native_menu_focus(
        &mut self,
        focus: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.file_actions.native_menu_open = true;
        #[cfg(windows)]
        if let Some(client) = &self.preview.native {
            client.surface.hide();
        }
        self.file_actions.native_menu_focus_subscription =
            Some(cx.on_blur(&focus, window, |this, _, cx| {
                this.file_actions.native_menu_open = false;
                this.file_actions.native_menu_focus_subscription = None;
                cx.notify();
            }));
        cx.notify();
    }
    pub(crate) fn file_context_menu(
        &mut self,
        menu: PopupMenu,
        blank: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        self.track_native_menu_focus(menu.focus_handle(cx), window, cx);
        self.build_file_context_menu(menu, blank, window, cx)
    }
    fn build_file_context_menu(
        &mut self,
        menu: PopupMenu,
        blank: bool,
        _: &mut Window,
        cx: &mut App,
    ) -> PopupMenu {
        self.file_actions.context_blank = blank;
        let hits = if blank {
            Vec::new()
        } else {
            self.selected_file_hits()
        };
        let single = hits.len() == 1;
        let any = !hits.is_empty();
        let current_root = self.workspace.mode == ViewMode::Directory && (blank || hits.is_empty());
        let directory = current_root || (single && hits[0].is_directory);
        let busy = self.file_actions.busy || self.workspace.op_busy;
        let paste = !self.file_actions.clipboard.sources.is_empty()
            && self.paste_directory().is_some()
            && !busy;
        menu.menu_with_icon_and_disabled(
            "打开",
            IconName::ExternalLink,
            Box::new(ContextOpen),
            !single,
        )
        .menu_with_icon_and_disabled(
            "打开方式…",
            IconName::File,
            Box::new(ContextOpenWith),
            !single || directory,
        )
        .menu_with_icon_and_disabled(
            "定位所在目录",
            IconName::FolderOpen,
            Box::new(ContextLocate),
            !single,
        )
        .separator()
        .menu_with_icon_and_disabled("复制路径", IconName::Copy, Box::new(ContextCopyPath), !any)
        .menu_with_icon_and_disabled(
            "复制文件",
            IconName::Copy,
            Box::new(ContextCopy),
            !any || busy,
        )
        .menu_with_icon_and_disabled(
            "剪切文件",
            Icon::default().path("icons/scissors.svg"),
            Box::new(ContextCut),
            !any || busy,
        )
        .menu_with_icon_and_disabled(
            "粘贴文件…",
            Icon::default().path("icons/clipboard-paste.svg"),
            Box::new(ContextPaste),
            !paste,
        )
        .menu_with_icon_and_disabled(
            "重命名…",
            Icon::default().path("icons/square-pen.svg"),
            Box::new(ContextRename),
            !single || busy,
        )
        .separator()
        .menu_with_icon_and_disabled(
            "分析此目录占用",
            IconName::ChartPie,
            Box::new(ContextAnalyze),
            !directory,
        )
        .menu_with_icon_and_disabled(
            "在此目录范围搜索",
            IconName::Search,
            Box::new(ContextScope),
            !directory,
        )
        .menu_with_icon_and_disabled("属性", IconName::Info, Box::new(ContextProperties), !single)
        .separator()
        .menu_with_icon("刷新", IconName::RefreshCw, Box::new(WorkspaceRefresh))
        .menu_with_icon_and_disabled(
            "全选（当前已加载）",
            IconName::Check,
            Box::new(ContextSelectAll),
            self.workspace.mode == ViewMode::Usage,
        )
        .menu_with_icon_and_disabled(
            if if self.workspace.mode == ViewMode::Search {
                self.session.read(cx).prefs.search_fit_columns
            } else {
                self.session.read(cx).prefs.browser_fit_columns
            } {
                "停用自动适宽"
            } else {
                "自动适应列宽"
            },
            Icon::default().path("icons/columns-3.svg"),
            Box::new(ContextAutoFit),
            self.workspace.mode == ViewMode::Usage,
        )
    }
    pub(crate) fn show_file_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_page.is_some() {
            return;
        }
        let menu = PopupMenu::build(window, cx, |menu, window, cx| {
            self.build_file_context_menu(menu, false, window, cx)
                .action_context(self.focus.clone())
        });
        self.track_native_menu_focus(menu.read(cx).focus_handle(cx), window, cx);
        self.file_actions.menu_subscription =
            Some(cx.subscribe(&menu, |this, _, _: &DismissEvent, cx| {
                this.file_actions.native_menu_open = false;
                this.file_actions.native_menu_focus_subscription = None;
                this.file_actions.keyboard_menu = None;
                this.file_actions.menu_subscription = None;
                cx.notify();
            }));
        menu.read(cx).focus_handle(cx).focus(window, cx);
        self.file_actions.keyboard_menu = Some(menu);
        cx.notify();
    }
    pub(super) fn cancel_active_batch_before_close(&mut self, cx: &mut Context<Self>) -> bool {
        if self.file_actions.busy {
            self.file_actions.cancel.store(true, Ordering::Release);
            self.file_actions.summary =
                "正在取消文件操作；安全收尾后请再次关闭窗口，已完成项不会自动撤销".into();
            cx.notify();
            true
        } else {
            false
        }
    }
    pub(crate) fn context_action(
        &mut self,
        kind: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_page.is_some() {
            return;
        }
        let blank = std::mem::take(&mut self.file_actions.context_blank);
        let hits = self.selected_file_hits();
        let single = hits.len() == 1;
        let current_root = self.workspace.mode == ViewMode::Directory && (blank || hits.is_empty());
        match kind {
            0 if single => self.workspace_enter(cx),
            1 if single && !hits[0].is_directory => {
                self.open_shell_dialog(crate::open_path::ShellDialog::OpenWith, cx)
            }
            2 if single => self.workspace_reveal(cx),
            3 if !hits.is_empty() => {
                cx.write_to_clipboard(ClipboardItem::new_string(
                    hits.iter()
                        .map(|h| h.path.clone())
                        .collect::<Vec<_>>()
                        .join("\r\n"),
                ));
                self.workspace.status = format!("已复制 {} 条路径", hits.len());
                self.file_actions.summary = self.workspace.status.clone();
            }
            4 | 5 if !hits.is_empty() && !self.file_actions.busy => {
                self.capture_file_clipboard(hits, kind == 5, cx)
            }
            6 if !self.file_actions.busy && !self.file_actions.clipboard.sources.is_empty() => {
                self.file_actions.confirm_target = self.paste_directory();
            }
            7 if single && !self.file_actions.busy => {
                self.begin_workspace_operation(FileAction::Rename, window, cx)
            }
            8 if single => self.open_shell_dialog(crate::open_path::ShellDialog::Properties, cx),
            9 if current_root => self.analyze_directory(self.workspace.location.clone(), cx),
            9 if single && hits[0].is_directory => self.workspace_analyze_selected(cx),
            10 if current_root => {
                self.begin_scoped_search(self.workspace.location.clone(), window, cx)
            }
            10 if single && hits[0].is_directory => {
                self.begin_scoped_search(hits[0].path.clone().into(), window, cx)
            }
            11 => {
                if self.workspace.mode == ViewMode::Search {
                    self.search_selection = (0..self.hits.len()).collect();
                } else if self.workspace.mode == ViewMode::Directory {
                    self.workspace.selected_rows = (0..self.workspace.rows.len()).collect();
                }
                self.preview_active_selection(cx);
            }
            12 if self.workspace.mode != ViewMode::Usage => {
                let search = self.workspace.mode == ViewMode::Search;
                self.session.update(cx, |s, cx| {
                    if search {
                        s.prefs.search_fit_columns = !s.prefs.search_fit_columns;
                    } else {
                        s.prefs.browser_fit_columns = !s.prefs.browser_fit_columns;
                    }
                    crate::session::persist_prefs(s.prefs.clone(), cx);
                });
            }
            _ => {}
        }
        cx.notify();
    }
    pub(super) fn cancel_file_background(&self) {
        self.file_actions
            .snapshot_cancel
            .store(true, Ordering::Release);
        self.file_actions.cancel.store(true, Ordering::Release);
    }
    pub(super) fn reset_selected_identity_baselines(&mut self) {
        let keys = self
            .selected_file_hits()
            .iter()
            .map(|h| workspace::preview_stamp(&h.path, h.size, h.mtime))
            .collect::<Vec<_>>();
        for key in keys {
            self.file_actions.identity_baselines.remove(&key);
        }
        self.file_actions
            .snapshot_cancel
            .store(true, Ordering::Release);
        self.file_actions.snapshot_generation =
            self.file_actions.snapshot_generation.wrapping_add(1);
        self.file_actions.snapshot_key.clear();
        self.file_actions.snapshot_sources.clear();
        self.file_actions.snapshot_error = None;
        self.file_actions.snapshot_pending = false;
        self.file_actions.pending_clipboard = None;
    }
    pub(super) fn refresh_selection_snapshot(&mut self, cx: &mut Context<Self>) {
        let hits = self.selected_file_hits();
        let keys = hits
            .iter()
            .map(|h| workspace::preview_stamp(&h.path, h.size, h.mtime))
            .collect::<Vec<_>>();
        if self.file_actions.snapshot_key == keys {
            return;
        }
        let mut known = std::collections::HashMap::new();
        {
            let wanted = keys.iter().collect::<std::collections::HashSet<_>>();
            for (key, source) in self
                .file_actions
                .snapshot_key
                .iter()
                .zip(self.file_actions.snapshot_sources.iter())
            {
                if wanted.contains(key) {
                    known.insert(key.clone(), source.stamp);
                }
            }
        }
        for key in &keys {
            if let Some(stamp) = self.file_actions.identity_baselines.get(key) {
                known.entry(key.clone()).or_insert(*stamp);
            }
        }
        self.file_actions
            .snapshot_cancel
            .store(true, Ordering::Release);
        let cancel = Arc::new(AtomicBool::new(false));
        self.file_actions.snapshot_cancel = cancel.clone();
        self.file_actions.snapshot_generation =
            self.file_actions.snapshot_generation.wrapping_add(1);
        let generation = self.file_actions.snapshot_generation;
        self.file_actions.snapshot_key = keys.clone();
        self.file_actions.snapshot_sources.clear();
        self.file_actions.snapshot_error = None;
        self.file_actions.snapshot_pending = !keys.is_empty();
        if self.file_actions.pending_clipboard.take().is_some() {
            self.file_actions.summary = "选中项已更改；未记录文件剪贴板，请重新复制或剪切".into();
        }
        if keys.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(
                    async move { capture_selection_sources(hits, keys, known, cancel) },
                )
                .await;
            this.update(cx, |this, cx| {
                if this.file_actions.snapshot_generation != generation {
                    return;
                }
                this.file_actions.snapshot_pending = false;
                match result {
                    Ok(sources) => {
                        if this.file_actions.identity_baselines.len() + sources.len() > 4096 {
                            this.file_actions.identity_baselines.clear();
                        }
                        for (key, source) in sources.iter().take(4096) {
                            this.file_actions
                                .identity_baselines
                                .insert(key.clone(), source.stamp);
                        }
                        this.file_actions.snapshot_sources =
                            sources.into_iter().map(|(_, source)| source).collect();
                        if let Some(cut) = this.file_actions.pending_clipboard.take() {
                            this.finish_file_clipboard(cut, cx);
                        }
                    }
                    Err(error) => {
                        this.file_actions.snapshot_error = Some(error.clone());
                        if this.file_actions.pending_clipboard.take().is_some() {
                            this.file_actions.summary = error.clone();
                        }
                        this.file_actions.summary = error.clone();
                        this.workspace.status = error;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
    fn capture_file_clipboard(
        &mut self,
        _hits: Vec<SearchHitDto>,
        cut: bool,
        cx: &mut Context<Self>,
    ) {
        self.file_actions.clipboard_generation =
            self.file_actions.clipboard_generation.wrapping_add(1);
        // A failed new copy must not leave an older unrelated file list ready to paste.
        self.file_actions.clipboard = Clipboard::default();
        self.refresh_selection_snapshot(cx);
        if self.file_actions.snapshot_pending {
            self.file_actions.pending_clipboard = Some(cut);
            self.file_actions.summary = "正在核对所选文件身份；尚未记录文件剪贴板".into();
        } else {
            self.finish_file_clipboard(cut, cx);
        }
        cx.notify();
    }
    fn finish_file_clipboard(&mut self, cut: bool, cx: &mut Context<Self>) {
        if let Some(error) = self.file_actions.snapshot_error.clone() {
            self.file_actions.summary = error;
            cx.notify();
            return;
        }
        let sources = self.file_actions.snapshot_sources.clone();
        if sources.is_empty() {
            self.file_actions.summary = "没有可复制的有效文件；请重新选择".into();
            cx.notify();
            return;
        }
        let generation = self.file_actions.clipboard_generation;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    for source in &sources {
                        crate::file_identity::revalidate(&source.path, source.stamp)?;
                    }
                    Ok::<_, String>(sources)
                })
                .await;
            this.update(cx, |this, cx| {
                if this.file_actions.clipboard_generation != generation {
                    return;
                }
                match result {
                    Ok(sources) => {
                        let count = sources.len();
                        this.file_actions.clipboard = Clipboard { sources, cut };
                        this.file_actions.summary = format!(
                            "应用内{} {count} 项 · 到目标目录粘贴；尚未修改源文件",
                            if cut { "剪切" } else { "复制" }
                        );
                    }
                    Err(error) => this.file_actions.summary = error,
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
    pub(super) fn open_shell_dialog(
        &mut self,
        dialog: crate::open_path::ShellDialog,
        cx: &mut Context<Self>,
    ) {
        let Some(hit) = self.active_workspace_hit() else {
            return;
        };
        if !self.ensure_selection_identity(cx) {
            if self.identity_pending {
                self.file_actions.pending_shell = Some(dialog);
            } else {
                self.workspace.status = "位置不可用或身份已变化；请刷新后重选".into();
                return;
            }
            self.workspace.status = "正在核对文件身份…".into();
            return;
        }
        let Some(stamp) = self.selected_identity else {
            return;
        };
        self.pause_preview();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    crate::file_identity::revalidate(Path::new(&hit.path), stamp)?;
                    crate::open_path::shell_dialog(&hit.path, dialog)
                })
                .await;
            this.update(cx, |this, cx| {
                this.file_actions.summary = match result {
                    Ok(()) => "已请求系统对话框；不会绕过权限或修改默认关联".into(),
                    Err(_) => "系统对话框不可用；请检查文件关联或访问权限".into(),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
    fn execute_paste(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.file_actions.confirm_target.take() else {
            return;
        };
        if self.file_actions.busy {
            return;
        }
        self.pause_preview();
        self.file_actions.busy = true;
        self.file_actions.activity_dismissed = false;
        self.file_actions.show_report = false;
        self.file_actions.report.clear();
        self.file_actions.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.file_actions.cancel.clone();
        let clipboard = self.file_actions.clipboard.clone();
        let generation = self.file_actions.clipboard_generation;
        self.file_actions.summary = "正在处理文件 · 可导航或取消；完成项不会自动撤销".into();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(
                    async move { file_transfer::transfer(&clipboard, &target, &cancel) },
                )
                .await;
            this.update(cx, |this, cx| {
                this.file_actions.busy = false;
                this.file_actions.summary = format!(
                    "{} · 成功 {} / 失败或冲突 {} · 不覆盖；索引稍后同步",
                    if result.cancelled {
                        "操作已取消"
                    } else {
                        "操作完成"
                    },
                    result.completed.len(),
                    result.failed.len()
                );
                this.file_actions.report = result
                    .failed
                    .into_iter()
                    .map(|(p, e)| format!("{}：{}", workspace::display_path(&p), e))
                    .collect();
                if this.file_actions.clipboard.cut
                    && this.file_actions.clipboard_generation == generation
                {
                    this.file_actions
                        .clipboard
                        .sources
                        .retain(|s| !result.completed.contains(&s.path));
                }
                this.workspace.cache.clear();
                this.workspace.scan_stale = true;
                if this.workspace.mode == ViewMode::Directory {
                    this.workspace_refresh(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
    pub(super) fn render_paste_confirmation(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Self::tokens(cx);
        let target = self
            .file_actions
            .confirm_target
            .as_ref()
            .map(|p| workspace::display_path(p))
            .unwrap_or_default();
        div().absolute().inset_0().occlude().bg(rgba(0x00000066)).flex().items_center().justify_center().child(v_flex().w(px(480.)).max_w_full().m_4().p_5().gap_4().rounded_lg().border_1().border_color(t.border).bg(t.surface_raised).child(format!("确认{} {} 项",if self.file_actions.clipboard.cut{"移动"}else{"复制"},self.file_actions.clipboard.sources.len())).child(div().id("paste-target-path").text_sm().text_ellipsis().aria_description(target.clone()).child(target)).child(div().text_sm().text_color(t.text_secondary).child("同名项跳过，不覆盖或合并。跨卷目录移动拒绝；取消只停止后续工作，不保证撤销已完成项。操作前释放预览并核对物理身份。")).child(h_flex().justify_end().gap_2().child(Button::new("cancel-file-paste").label("取消").on_click(cx.listener(|this,_,_,cx|{this.file_actions.confirm_target=None;cx.notify();}))).child(Button::new("confirm-file-paste").primary().label("确认粘贴").on_click(cx.listener(|this,_,_,cx|this.execute_paste(cx))))))
    }
    pub(super) fn render_file_activity(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Self::tokens(cx);
        v_flex()
            .w_full()
            .flex_shrink_0()
            .bg(t.surface)
            .border_t_1()
            .border_color(t.border)
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_1()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_ellipsis()
                            .child(self.file_actions.summary.clone()),
                    )
                    .when(self.file_actions.busy, |v| {
                        v.child(
                            Button::new("cancel-file-batch")
                                .ghost()
                                .icon(IconName::Close)
                                .label("取消操作")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.file_actions.cancel.store(true, Ordering::Release);
                                    cx.notify();
                                })),
                        )
                    })
                    .when(!self.file_actions.report.is_empty(), |v| {
                        v.child(
                            Button::new("file-batch-report")
                                .ghost()
                                .icon(IconName::Info)
                                .label("操作详情")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.file_actions.show_report = !this.file_actions.show_report;
                                    cx.notify();
                                })),
                        )
                    })
                    .when(!self.file_actions.busy, |v| {
                        v.child(
                            Button::new("dismiss-file-notice")
                                .ghost()
                                .icon(IconName::Close)
                                .w(px(24.))
                                .h(px(24.))
                                .px_0()
                                .tooltip("收起操作通知")
                                .accessibility_label("收起操作通知")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.file_actions.activity_dismissed = true;
                                    this.file_actions.show_report = false;
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .when(self.file_actions.show_report, |v| {
                v.child(
                    v_flex()
                        .max_h(px(160.))
                        .overflow_y_scrollbar()
                        .px_3()
                        .py_2()
                        .text_xs()
                        .children(
                            self.file_actions
                                .report
                                .iter()
                                .map(|line| div().child(line.clone())),
                        ),
                )
            })
    }
    pub(super) fn has_file_activity(&self) -> bool {
        (!self.file_actions.report.is_empty() && !self.file_actions.activity_dismissed)
            || self.file_actions.busy
    }
    pub(super) fn render_preview(&self, cx: &mut Context<Self>) -> AnyElement {
        let hits = self.selected_file_hits();
        if hits.len() < 2 {
            return self.render_single_preview(cx).into_any_element();
        }
        let t = Self::tokens(cx);
        v_flex().w(px(self.actual_preview_width)).h_full().flex_shrink_0().p_4().gap_3().bg(t.surface).child(Icon::new(Icon::default().path("icons/files.svg")).size_6()).child(format!("已选择 {} 项",hits.len())).child(div().text_sm().text_color(t.text_secondary).child("多选仅显示摘要，不加载任意一份文档。复制/剪切按选区处理；重命名、打开和属性仅支持单项。")).child(div().text_sm().child(format!("普通文件逻辑大小：{}",search_logic::format_size(hits.iter().filter(|h|!h.is_directory).map(|h|h.size).sum(),false)))).child(div().text_xs().text_color(t.text_secondary).child("全选仅针对当前已加载行；目录大小不在此处递归计算，硬链接逻辑大小不代表可回收空间。")).into_any_element()
    }
}
fn update_selection(
    set: &mut BTreeSet<usize>,
    row: usize,
    anchor: usize,
    control: bool,
    shift: bool,
) {
    if shift {
        if !control {
            set.clear();
        }
        set.extend(anchor.min(row)..=anchor.max(row));
    } else if control {
        if !set.remove(&row) {
            set.insert(row);
        }
    } else {
        set.clear();
        set.insert(row);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    #[test]
    fn snapshot_rejects_same_metadata_replacement_and_keeps_alias_paths() {
        let root = std::env::temp_dir().join(format!(
            "findx-selection-id-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let first = root.join("first.txt");
        let alias = root.join("alias.txt");
        std::fs::write(&first, b"fixture").unwrap();
        std::fs::hard_link(&first, &alias).unwrap();
        let make_hit = |path: &Path| {
            let m = std::fs::metadata(path).unwrap();
            SearchHitDto {
                entry_idx: 0,
                name: path.file_name().unwrap().to_string_lossy().into_owned(),
                path: path.to_string_lossy().into_owned(),
                size: m.len(),
                mtime: m
                    .modified()
                    .unwrap()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                is_directory: false,
                name_highlight: vec![],
            }
        };
        let hits = vec![make_hit(&first), make_hit(&alias)];
        let keys = hits
            .iter()
            .map(|h| workspace::preview_stamp(&h.path, h.size, h.mtime))
            .collect::<Vec<_>>();
        let initial = capture_selection_sources(
            hits.clone(),
            keys.clone(),
            std::collections::HashMap::new(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert_eq!(initial.len(), 2);
        assert_ne!(initial[0].1.path, initial[1].1.path);
        assert_eq!(initial[0].1.stamp.id, initial[1].1.stamp.id);
        let modified = std::fs::metadata(&first).unwrap().modified().unwrap();
        std::fs::rename(&first, root.join("old.txt")).unwrap();
        std::fs::write(&first, b"fixture").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&first)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let known = initial.iter().map(|(k, s)| (k.clone(), s.stamp)).collect();
        assert!(
            capture_selection_sources(
                hits.clone(),
                keys.clone(),
                known,
                Arc::new(AtomicBool::new(false))
            )
            .is_err()
        );
        assert!(
            capture_selection_sources(
                hits.clone(),
                keys.clone(),
                std::collections::HashMap::new(),
                Arc::new(AtomicBool::new(false))
            )
            .is_ok()
        );
        assert!(
            capture_selection_sources(
                hits,
                keys,
                std::collections::HashMap::new(),
                Arc::new(AtomicBool::new(true))
            )
            .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn modifiers_keep_range_and_toggle() {
        let mut s = BTreeSet::new();
        update_selection(&mut s, 2, 0, false, false);
        update_selection(&mut s, 4, 2, true, false);
        assert_eq!(s, [2, 4].into_iter().collect());
        update_selection(&mut s, 2, 4, true, false);
        assert_eq!(s, [4].into_iter().collect());
        update_selection(&mut s, 1, 4, false, true);
        assert_eq!(s, [1, 2, 3, 4].into_iter().collect());
    }
}
