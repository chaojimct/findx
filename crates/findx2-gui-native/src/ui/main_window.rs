use std::time::Duration;

use findx2_ipc::SearchHitDto;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Icon, IconName, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::result_table::{DEFAULT_WIDTHS, MIN_WIDTHS, ResultsTable};
use crate::open_path;
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
    ]
);

const PAGE_SIZE: usize = 200;
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(60);

pub struct MainWindow {
    table: Entity<TableState<ResultsTable>>,
    page_loading: bool,
    page_error: bool,
    _page_task: Option<Task<()>>,
    session: Entity<AppSession>,
    search: Entity<InputState>,
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
    preview: PreviewState,
    preview_gen: u64,
    loading: bool,
    sidebar_force: Option<ForceState>,
    preview_force: Option<ForceState>,
    last_content_width: f32,
    last_diag: String,
    key_traces: Vec<String>,
    selection_resets: Vec<String>,
    _subscriptions: Vec<Subscription>,
    _search_task: Option<Task<()>>,
    _status_task: Option<Task<()>>,
    _preview_task: Option<Task<()>>,
}

impl MainWindow {
    pub fn new(session: Entity<AppSession>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.bind_keys([
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
        let mut this = Self {
            table: table.clone(),
            page_loading: false,
            page_error: false,
            _page_task: None,
            session: session.clone(),
            search: search.clone(),
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
            preview: PreviewState::empty(),
            preview_gen: 0,
            loading: false,
            sidebar_force: None,
            preview_force: None,
            last_content_width: 1100.0,
            last_diag: String::new(),
            key_traces: Vec::new(),
            selection_resets: Vec::new(),
            _subscriptions: Vec::new(),
            _search_task: None,
            _status_task: None,
            _preview_task: None,
        };

        this._subscriptions.push(cx.subscribe_in(
            &search,
            window,
            |this, _input, event, window, cx| {
                if let InputEvent::Change = event {
                    this.schedule_search(window, cx);
                }
            },
        ));
        this._subscriptions
            .push(cx.observe(&session, |this, _, cx| {
                cx.notify();
                let _ = this;
            }));

        this._subscriptions
            .push(cx.subscribe(&table, |this, _, event: &TableEvent, cx| {
                match event {
                    TableEvent::SelectRow(ix) => this.set_selected(Some(*ix), cx),
                    TableEvent::DoubleClickedRow(ix) => {
                        this.set_selected(Some(*ix), cx);
                        this.open_selected(cx);
                    }
                    TableEvent::ColumnWidthsChanged(widths) if widths.len() == 4 => {
                        let widths =
                            std::array::from_fn(|i| widths[i].as_f32().clamp(MIN_WIDTHS[i], 1600.));
                        this.session.update(cx, |s, cx| {
                            s.prefs.column_widths = Some(widths);
                            crate::session::persist_prefs(s.prefs.clone(), cx);
                        });
                        // Keep delegate widths current so a later header refresh cannot undo a drag.
                        this.table
                            .update(cx, |t, _| t.delegate_mut().widths = widths);
                    }
                    _ => {}
                }
            }));
        this.schedule_status_poll(cx);
        this
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
                        this.hits = out.hits;
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
                            this.hits.extend(out.hits);
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
                s.settings.enable_metadata_backfill,
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
        if self.selected != index {
            self.selected = index;
            self.request_preview(cx);
        } else {
            self.selected = index;
        }
        cx.notify();
    }

    fn request_preview(&mut self, cx: &mut Context<Self>) {
        self.preview_gen = self.preview_gen.wrapping_add(1);
        let generation = self.preview_gen;
        let Some(hit) = self.selected.and_then(|i| self.hits.get(i).cloned()) else {
            self.preview = PreviewState::empty();
            self._preview_task = None;
            return;
        };
        self.preview = PreviewState::loading(
            generation,
            preview::meta_from_hit(&hit.name, &hit.path, hit.size, hit.mtime, hit.is_directory),
        );
        self._preview_task = Some(cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_spawn(async move {
                    preview::load_preview(
                        generation,
                        &hit.name,
                        &hit.path,
                        hit.size,
                        hit.mtime,
                        hit.is_directory,
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                if !preview::is_current_generation(this.preview_gen, generation, loaded.generation)
                {
                    return;
                }
                this.preview = loaded;
                cx.notify();
            })
            .ok();
        }));
    }

    fn move_sel(&mut self, key: ResultNavKey, cx: &mut Context<Self>) {
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
        let Some(i) = self.selected else {
            return;
        };
        let Some(hit) = self.hits.get(i) else {
            return;
        };
        let path = hit.path.clone();
        self.record_key("enter", "open", self.selected);
        cx.background_spawn(async move {
            if let Err(err) = open_path::open_path(&path) {
                eprintln!("findx2-gui-native: {err}");
            }
        })
        .detach();
    }

    fn reveal_selected(&mut self, cx: &mut Context<Self>) {
        let Some(i) = self.selected else {
            return;
        };
        let Some(hit) = self.hits.get(i) else {
            return;
        };
        let path = hit.path.clone();
        cx.background_spawn(async move {
            if let Err(err) = open_path::reveal_in_folder(&path) {
                eprintln!("findx2-gui-native: {err}");
            }
        })
        .detach();
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
        cx.notify();
    }

    fn tokens(cx: &App) -> FindxTokens {
        if cx.has_global::<FindxTokens>() {
            *cx.global::<FindxTokens>()
        } else {
            FindxTokens::for_theme(crate::settings_logic::EffectiveTheme::Light)
        }
    }

    fn type_icon(filter: TypeFilter) -> IconName {
        match filter {
            TypeFilter::All => IconName::Search,
            TypeFilter::Folder => IconName::Folder,
            TypeFilter::File => IconName::File,
            TypeFilter::Doc => IconName::FileText,
            TypeFilter::Image => IconName::Frame,
            TypeFilter::Video => IconName::Play,
            TypeFilter::Audio => IconName::Mic,
        }
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
        icon: IconName,
        label: &'static str,
        selected: bool,
        tokens: FindxTokens,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        h_flex()
            .id(id)
            .w_full()
            .h(px(28.))
            .px_1()
            .gap_2()
            .items_center()
            .rounded_md()
            .cursor_pointer()
            .when(selected, |this| this.bg(tokens.row_selected))
            .when(!selected, |this| this.hover(|s| s.bg(tokens.hover)))
            .on_click(on_click)
            .child(
                div()
                    .w(px(3.))
                    .h(px(14.))
                    .rounded_sm()
                    .when(selected, |this| this.bg(tokens.accent))
                    .when(!selected, |this| this.bg(hsla(0., 0., 0., 0.))),
            )
            .child(
                Icon::new(icon)
                    .with_size(gpui_kit::component::Size::Small)
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
                    IconName::Calendar,
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

    fn render_preview(&self, cx: &App) -> impl IntoElement {
        let tokens = Self::tokens(cx);
        let header = match &self.preview.meta {
            Some(meta) => v_flex()
                .w_full()
                .flex_shrink_0()
                .gap_1()
                .px_3()
                .pt_3()
                .pb_2()
                .border_b_1()
                .border_color(tokens.border)
                .child(
                    div()
                        .text_color(tokens.text_primary)
                        .text_ellipsis()
                        .child(meta.name.clone()),
                )
                .child(
                    div()
                        .text_color(tokens.text_secondary)
                        .text_xs()
                        .child(preview::format_meta_line(meta)),
                )
                .child(
                    div()
                        .text_color(tokens.text_tertiary)
                        .text_xs()
                        .child(meta.path.clone()),
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
            PreviewKind::Error => div()
                .p_3()
                .text_sm()
                .text_color(tokens.error)
                .child(
                    self.preview
                        .message
                        .clone()
                        .unwrap_or_else(|| "预览失败".into()),
                )
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
            .w(px(PREVIEW_DEFAULT_WIDTH_PX))
            .h_full()
            .min_h_0()
            .flex_shrink_0()
            .bg(tokens.surface)
            .border_l_1()
            .border_color(tokens.border)
            .child(header)
            .child(if scrollable_body {
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
        let (sidebar, preview, sidebar_reason, preview_reason) = explain_panels(
            content_width,
            PREVIEW_DEFAULT_WIDTH_PX,
            saved_sidebar,
            saved_preview,
            self.sidebar_force,
            self.preview_force,
        );
        self.last_diag = format!("{sidebar_reason}\n{preview_reason}");

        v_flex()
            .id("findx-main")
            .key_context("FindXMain")
            .track_focus(&cx.focus_handle())
            .size_full()
            .overflow_hidden()
            .bg(tokens.bg)
            .text_color(tokens.text_primary)
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
                if focus_is_search(&this.search, window, cx) {
                    return;
                }
                this.open_selected(cx)
            }))
            .on_action(cx.listener(|this, _: &RevealSelected, _, cx| this.reveal_selected(cx)))
            .on_action(cx.listener(|this, _: &ToggleDiag, _, cx| this.toggle_diag(cx)))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                window.focus(&this.search.read(cx).focus_handle(cx), cx);
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, _, cx| {
                let session = this.session.clone();
                crate::ui::open_settings_window(session, cx);
            }))
            .child(self.render_toolbar(sidebar, preview, cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .items_stretch()
                    .overflow_hidden()
                    .when(sidebar, |this| this.child(self.render_sidebar(cx)))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .overflow_hidden()
                            .child(
                                DataTable::new(&self.table)
                                    .scrollbar_visible(true, true)
                                    .bordered(false),
                            ),
                    )
                    .when(preview, |this| this.child(self.render_preview(cx))),
            )
            .child(self.render_status_bar(&index_view, cx))
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
    fn render_toolbar(
        &self,
        sidebar: bool,
        preview: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let tokens = Self::tokens(cx);
        h_flex()
            .w_full()
            .h(px(48.))
            .px_3()
            .gap_2()
            .items_center()
            .bg(tokens.surface)
            .border_b_1()
            .border_color(tokens.border)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.search).cleanable(true)),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .gap_2()
                    .items_center()
                    .child({
                        let entity = cx.entity();
                        Button::new("toolbar-sidebar")
                            .ghost()
                            .label("侧栏")
                            .selected(sidebar)
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |this, cx| {
                                    this.toggle_panel(
                                        search_logic::PanelKind::Sidebar,
                                        sidebar,
                                        cx,
                                    );
                                });
                            })
                    })
                    .child({
                        let entity = cx.entity();
                        Button::new("toolbar-preview")
                            .ghost()
                            .label("预览")
                            .selected(preview)
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |this, cx| {
                                    this.toggle_panel(
                                        search_logic::PanelKind::Preview,
                                        preview,
                                        cx,
                                    );
                                });
                            })
                    })
                    .child({
                        let session = self.session.clone();
                        Button::new("toolbar-settings")
                            .ghost()
                            .label("设置")
                            .on_click(move |_, _, cx| {
                                crate::ui::open_settings_window(session.clone(), cx);
                            })
                    }),
            )
    }

    fn render_status_bar(&self, index: &IndexStatusView, cx: &App) -> impl IntoElement {
        let tokens = Self::tokens(cx);
        let mock = self.session.read(cx).mock;
        let connected = self.session.read(cx).connected;
        let pipe = self.session.read(cx).pipe_name.clone();
        let left = self.search_status.clone();
        let mid = index.summary.clone();
        let mode = if mock {
            format!("模拟后端 · {pipe}")
        } else if self.session.read(cx).live {
            format!("生产管道 · {pipe}")
        } else if connected {
            format!("隔离管道 · {pipe}")
        } else {
            format!("未连接 · {pipe}")
        };
        h_flex()
            .w_full()
            .h(px(28.))
            .flex_shrink_0()
            .overflow_hidden()
            .px_3()
            .gap_3()
            .items_center()
            .bg(tokens.surface)
            .border_t_1()
            .border_color(tokens.border)
            .text_sm()
            .text_color(tokens.text_secondary)
            .child(div().flex_1().min_w_0().text_ellipsis().child(left))
            .child(div().min_w_0().text_ellipsis().child(mid))
            .child(div().text_color(tokens.text_tertiary).child(mode))
    }
}

fn focus_is_search(search: &Entity<InputState>, window: &Window, cx: &App) -> bool {
    let Some(focused) = window.focused(cx) else {
        return false;
    };
    focused == search.read(cx).focus_handle(cx)
}
