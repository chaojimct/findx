use std::time::Duration;

use findx2_ipc::SearchHitDto;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Selectable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::open_path;
use crate::search_logic::{
    self, ColId, DEFAULT_COL_FRAC, ForceState, PREVIEW_DEFAULT_WIDTH_PX, RESULT_ROW_HEIGHT_PX,
    ResultNavKey, SIDEBAR_WIDTH_PX, TimeFilter, TypeFilter, column_pixel_widths, explain_panels,
    format_size, format_time, highlight_segments, move_result_selection, visible_column_ids,
};
use crate::session::AppSession;
use crate::status_logic::{self, IndexStatusView};
use crate::theme::FindxTokens;

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

        let mut this = Self {
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
            cx.notify();
            return;
        }
        self.record_reset("重新查询");
        self.selected = None;
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
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
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

    fn move_sel(&mut self, key: ResultNavKey, cx: &mut Context<Self>) {
        if let Some(index) = move_result_selection(self.selected, key, self.hits.len()) {
            let label = match key {
                ResultNavKey::ArrowDown => "down",
                ResultNavKey::ArrowUp => "up",
                ResultNavKey::Home => "home",
                ResultNavKey::End => "end",
            };
            self.record_key(label, "move", Some(index));
            self.selected = Some(index);
            cx.notify();
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

    fn render_highlighted_name(hit: &SearchHitDto, cx: &App) -> impl IntoElement + 'static {
        let tokens = Self::tokens(cx);
        h_flex().gap_0().min_w_0().children(
            highlight_segments(&hit.name, &hit.name_highlight)
                .into_iter()
                .map(|(text, on)| {
                    div()
                        .when(on, |this| this.bg(tokens.hit_bg).text_color(tokens.hit_fg))
                        .when(!on, |this| this.text_color(tokens.text_primary))
                        .child(text.to_string())
                }),
        )
    }

    fn render_row(
        ix: usize,
        hit: SearchHitDto,
        selected: bool,
        widths: &[f32; 4],
        visible: &[ColId],
        entity: Entity<MainWindow>,
        cx: &App,
    ) -> impl IntoElement + 'static {
        let tokens = Self::tokens(cx);
        let dir_mark = if hit.is_directory { "📁 " } else { "" };
        h_flex()
            .id(("result-row", ix))
            .w_full()
            .h(px(RESULT_ROW_HEIGHT_PX))
            .px_2()
            .gap_2()
            .items_center()
            .when(selected, |this| this.bg(tokens.row_selected))
            .when(!selected, |this| this.hover(|s| s.bg(tokens.hover)))
            .cursor_pointer()
            .on_click(move |event: &ClickEvent, _window, cx| {
                let open = event.click_count() >= 2;
                entity.update(cx, |this, cx| {
                    this.selected = Some(ix);
                    if open {
                        this.open_selected(cx);
                    }
                    cx.notify();
                });
            })
            .children(visible.iter().map(|col| {
                let w = px(widths[col.index()].max(24.0));
                let child: AnyElement = match col {
                    ColId::Name => h_flex()
                        .w(w)
                        .min_w_0()
                        .gap_1()
                        .child(dir_mark.to_string())
                        .child(Self::render_highlighted_name(&hit, cx))
                        .into_any_element(),
                    ColId::Path => div()
                        .w(w)
                        .min_w_0()
                        .text_color(tokens.text_secondary)
                        .text_ellipsis()
                        .child(hit.path.clone())
                        .into_any_element(),
                    ColId::Size => div()
                        .w(w)
                        .text_color(tokens.text_secondary)
                        .child(format_size(hit.size, hit.is_directory))
                        .into_any_element(),
                    ColId::Mtime => div()
                        .w(w)
                        .text_color(tokens.text_secondary)
                        .child(format_time(hit.mtime))
                        .into_any_element(),
                };
                child
            }))
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Self::tokens(cx);
        v_flex()
            .w(px(SIDEBAR_WIDTH_PX))
            .h_full()
            .flex_shrink_0()
            .bg(tokens.surface)
            .border_r_1()
            .border_color(tokens.border)
            .p_3()
            .gap_3()
            .child(
                div()
                    .text_color(tokens.text_tertiary)
                    .text_sm()
                    .child("类型"),
            )
            .children(TypeFilter::ALL.iter().map(|filter| {
                let selected = self.type_filter == *filter;
                let f = *filter;
                let entity = cx.entity();
                Button::new(SharedString::from(format!("type-{}", f.label())))
                    .label(f.label())
                    .ghost()
                    .selected(selected)
                    .on_click(move |_, window, cx| {
                        entity.update(cx, |this, cx| {
                            this.type_filter = f;
                            this.schedule_search(window, cx);
                            cx.notify();
                        });
                    })
            }))
            .child(
                div()
                    .mt_2()
                    .text_color(tokens.text_tertiary)
                    .text_sm()
                    .child("时间"),
            )
            .children(TimeFilter::ALL.iter().map(|filter| {
                let selected = self.time_filter == *filter;
                let f = *filter;
                let entity = cx.entity();
                Button::new(SharedString::from(format!("time-{}", f.label())))
                    .label(f.label())
                    .ghost()
                    .selected(selected)
                    .on_click(move |_, window, cx| {
                        entity.update(cx, |this, cx| {
                            this.time_filter = f;
                            this.schedule_search(window, cx);
                            cx.notify();
                        });
                    })
            }))
    }

    fn render_preview(&self, cx: &App) -> impl IntoElement {
        let tokens = Self::tokens(cx);
        let body = if let Some(i) = self.selected.and_then(|i| self.hits.get(i)) {
            format!(
                "{}\n{}\n{}\n{}",
                i.name,
                i.path,
                format_size(i.size, i.is_directory),
                format_time(i.mtime)
            )
        } else {
            "选择一条结果后，这里显示摘要。完整预览和浏览操作属于后续阶段。".into()
        };
        v_flex()
            .w(px(PREVIEW_DEFAULT_WIDTH_PX))
            .h_full()
            .flex_shrink_0()
            .bg(tokens.surface)
            .border_l_1()
            .border_color(tokens.border)
            .p_3()
            .gap_2()
            .child(div().text_color(tokens.text_primary).child("预览（占位）"))
            .child(
                div()
                    .text_color(tokens.text_secondary)
                    .text_sm()
                    .child(body),
            )
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

        let table_width = (content_width
            - if sidebar { SIDEBAR_WIDTH_PX } else { 0.0 }
            - if preview {
                PREVIEW_DEFAULT_WIDTH_PX + 4.0
            } else {
                0.0
            })
        .max(160.0);
        let visible = visible_column_ids(table_width);
        let widths = column_pixel_widths(DEFAULT_COL_FRAC, table_width, &visible);
        let count = self.hits.len();

        let entity = cx.entity();
        let list = uniform_list("results", count, {
            let widths = widths;
            let visible = visible.clone();
            let entity = entity.clone();
            move |range, _window, cx| {
                range
                    .map(|ix| {
                        let (hit, selected) = {
                            let view = entity.read(cx);
                            (view.hits.get(ix).cloned(), view.selected == Some(ix))
                        };
                        match hit {
                            Some(hit) => MainWindow::render_row(
                                ix,
                                hit,
                                selected,
                                &widths,
                                &visible,
                                entity.clone(),
                                cx,
                            )
                            .into_any_element(),
                            None => div().into_any_element(),
                        }
                    })
                    .collect::<Vec<_>>()
            }
        })
        .flex_1()
        .h_full();

        v_flex()
            .id("findx-main")
            .key_context("FindXMain")
            .track_focus(&cx.focus_handle())
            .size_full()
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
                    .when(sidebar, |this| this.child(self.render_sidebar(cx)))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(self.render_header(&visible, &widths, cx))
                            .child(div().id("result-list").flex_1().min_h_0().child(list)),
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
            .child({
                let entity = cx.entity();
                Button::new("toggle-sidebar")
                    .ghost()
                    .label("侧栏")
                    .selected(sidebar)
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |this, cx| {
                            this.toggle_panel(search_logic::PanelKind::Sidebar, sidebar, cx);
                        });
                    })
            })
            .child({
                let entity = cx.entity();
                Button::new("toggle-preview")
                    .ghost()
                    .label("预览")
                    .selected(preview)
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |this, cx| {
                            this.toggle_panel(search_logic::PanelKind::Preview, preview, cx);
                        });
                    })
            })
            .child({
                let session = self.session.clone();
                Button::new("open-settings")
                    .ghost()
                    .label("设置")
                    .on_click(move |_, _, cx| {
                        crate::ui::open_settings_window(session.clone(), cx);
                    })
            })
    }

    fn render_header(&self, visible: &[ColId], widths: &[f32; 4], cx: &App) -> impl IntoElement {
        let tokens = Self::tokens(cx);
        h_flex()
            .w_full()
            .h(px(28.))
            .px_2()
            .gap_2()
            .items_center()
            .bg(tokens.surface)
            .border_b_1()
            .border_color(tokens.border)
            .text_color(tokens.text_secondary)
            .text_sm()
            .children(visible.iter().map(|col| {
                div()
                    .w(px(widths[col.index()].max(24.0)))
                    .child(col.label())
            }))
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
        } else if connected {
            format!("隔离管道 · {pipe}")
        } else {
            format!("未连接 · {pipe}")
        };
        h_flex()
            .w_full()
            .h(px(28.))
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
