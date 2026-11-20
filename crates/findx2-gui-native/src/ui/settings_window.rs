use gpui_kit::component::{Icon, IconName};
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::switch::Switch;

use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{Disableable as _, Selectable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::prefs;
use crate::session::AppSession;
use crate::settings_logic::{
    CLOSE_DIRTY_MESSAGE, DraftCommit, FindxGuiSettings, UiThemePref, commit_save_interval_secs,
    commit_search_limit, form_is_dirty,
};
use crate::theme::{FindxTokens, ThemeStyle};
use crate::ui::update_card::{UpdateCardState, render_update_card};
use crate::update_logic::{
    UpdatePhase, derive_update_phase, should_auto_check, start_silent_setup,
};

// A changing hint uses one reserved line; the complete text remains accessible
// by tooltip instead of wrapping and moving the controls beneath it.
fn settings_async_line(id: SharedString, text: String) -> impl IntoElement {
    let detail = text.clone();
    div().id(id).h(px(20.)).flex_shrink_0().min_w_0().text_sm()
        .whitespace_nowrap().text_ellipsis()
        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(detail.clone()).build(window, cx))
        .child(text)
}

// Fixed-width content avoids Button's internal centered label changing each row's icon origin.
fn settings_nav_content(icon: Icon, label: &'static str) -> impl IntoElement {
    h_flex()
        .w(px(152.))
        .items_center()
        .child(div().w(px(8.)).flex_shrink_0())
        .child(
            h_flex()
                .w(px(20.))
                .flex_shrink_0()
                .justify_center()
                .child(icon.size_4()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .pl_1()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(label),
        )
        .child(div().w(px(8.)).flex_shrink_0())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    Appearance,
    Files,
    Preview,
    Shortcuts,
    About,
    Index,
    Search,
    Diagnostics,
}

#[derive(Clone, Copy)]
pub enum SettingsEvent {
    Return,
    CloseApp,
    Stay,
    /// A service action finished. The workspace should poll immediately.
    ServiceChanged,
    /// Start succeeded. Reconnect and refresh an in-progress search.
    ServiceReady,
}
impl EventEmitter<SettingsEvent> for SettingsWindow {}

pub struct SettingsWindow {
    embedded: bool,
    closing_app: bool,
    style: ThemeStyle,
    original_style: ThemeStyle,
    session: Entity<AppSession>,
    tab: SettingsTab,
    settings: FindxGuiSettings,
    original: FindxGuiSettings,
    theme: UiThemePref,
    original_theme: UiThemePref,
    search_limit: Entity<InputState>,
    save_interval: Entity<InputState>,
    index_path: Entity<InputState>,
    pipe_name: Entity<InputState>,
    service_exe: Entity<InputState>,
    excluded: Entity<TextareaState>,
    hint: SharedString,
    dirty_tick: u64,
    update: UpdateCardState,
    show_syntax: bool,
    close_guarded: bool,
    saving: bool,
    service_snapshot: Option<crate::service_control::Snapshot>,
    service_pending: Option<crate::service_control::Action>,
    service_busy: bool,
    service_advanced: bool,
    service_hint: String,
    autostart_enabled: bool,
    autostart_supported: bool,
    _subscriptions: Vec<Subscription>,
}

impl SettingsWindow {
    pub fn new(session: Entity<AppSession>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_inner(session, false, window, cx)
    }

    pub fn new_embedded(
        session: Entity<AppSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_inner(session, true, window, cx)
    }

    fn new_inner(
        session: Entity<AppSession>,
        embedded: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let style = session.read(cx).prefs.theme_style;
        let (settings, theme) = session.read_with(cx, |s, _| (s.settings.clone(), s.prefs.theme));
        let search_limit = cx
            .new(|cx| InputState::new(window, cx).default_value(settings.search_limit.to_string()));
        let save_interval = cx.new(|cx| {
            InputState::new(window, cx).default_value(settings.save_interval_secs.to_string())
        });
        let index_path =
            cx.new(|cx| InputState::new(window, cx).default_value(settings.index_path.clone()));
        let pipe_name =
            cx.new(|cx| InputState::new(window, cx).default_value(settings.pipe_name.clone()));
        let service_exe = cx
            .new(|cx| InputState::new(window, cx).default_value(settings.service_exe_path.clone()));
        let excluded = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(4)
                .default_value(settings.excluded_dirs.join("\n"))
        });

        let mut this = Self {
            embedded,
            closing_app: false,
            style,
            original_style: style,
            session,
            tab: if crate::isolation::development_var_os("FINDX_NATIVE_OPEN_ABOUT").is_some() {
                SettingsTab::About
            } else {
                SettingsTab::Appearance
            },
            original: settings.clone(),
            settings,
            theme,
            original_theme: theme,
            search_limit: search_limit.clone(),
            save_interval: save_interval.clone(),
            index_path: index_path.clone(),
            pipe_name: pipe_name.clone(),
            service_exe: service_exe.clone(),
            excluded: excluded.clone(),
            hint: SharedString::from(""),
            dirty_tick: 0,
            update: UpdateCardState::new(theme_auto_check_placeholder()),
            show_syntax: false,
            close_guarded: false,
            saving: false,
            service_snapshot: None,
            service_pending: None,
            service_busy: false,
            service_advanced: false,
            service_hint: "正在读取服务状态".into(),
            autostart_enabled: false,
            autostart_supported: true,
            _subscriptions: Vec::new(),
        };
        this.update.auto_check = this.settings.auto_check_update;

        let mark_dirty = |this: &mut Self, cx: &mut Context<Self>| {
            this.dirty_tick = this.dirty_tick.wrapping_add(1);
            this.sync_text_fields(cx);
            cx.notify();
        };
        for input in [
            this.search_limit.clone(),
            this.save_interval.clone(),
            this.index_path.clone(),
            this.pipe_name.clone(),
            this.service_exe.clone(),
        ] {
            this._subscriptions
                .push(cx.subscribe_in(&input, window, move |this, _, ev, _, cx| {
                    if let InputEvent::Change = ev {
                        mark_dirty(this, cx);
                    }
                }));
        }

        this._subscriptions
            .push(cx.observe(&this.session, |_, _, cx| cx.notify()));
        this._subscriptions.push(
            cx.subscribe_in(&this.excluded, window, |this, _, ev, _, cx| {
                if let InputEvent::Change = ev {
                    this.sync_text_fields(cx);
                    cx.notify();
                }
            }),
        );
        this._subscriptions.push(
            cx.subscribe_in(&this.excluded, window, |this, _, ev, _, cx| {
                if let InputEvent::Change = ev {
                    this.sync_text_fields(cx);
                    cx.notify();
                }
            }),
        );
        if !embedded {
            let entity = cx.entity();
            window.on_window_should_close(cx, move |window, cx| {
                entity.update(cx, |this, cx| this.confirm_close(window, cx))
            });
        }

        this.refresh_service(cx);
        this
    }

    fn sync_text_fields(&mut self, cx: &App) {
        self.settings.index_path = self.index_path.read(cx).value().to_string();
        self.settings.pipe_name = self.pipe_name.read(cx).value().to_string();
        self.settings.service_exe_path = self.service_exe.read(cx).value().to_string();
        self.settings.excluded_dirs = self
            .excluded
            .read(cx)
            .value()
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }

    fn search_limit_text(&self, cx: &App) -> String {
        self.search_limit.read(cx).value().to_string()
    }

    fn save_interval_text(&self, cx: &App) -> String {
        self.save_interval.read(cx).value().to_string()
    }

    fn is_dirty(&self, cx: &App) -> bool {
        self.style != self.original_style
            || form_is_dirty(
                &self.settings,
                Some(&self.original),
                self.theme,
                self.original_theme,
                &self.search_limit_text(cx),
                &self.save_interval_text(cx),
            )
    }

    fn confirm_close(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.saving || self.service_busy {
            self.hint = "正在保存或执行服务操作，完成前不能离开设置".into();
            cx.notify();
            return false;
        }
        if !self.is_dirty(cx) {
            return true;
        }
        self.close_guarded = true;
        cx.notify();
        false
    }

    fn revert_unsaved(&mut self, cx: &mut Context<Self>) {
        self.settings = self.original.clone();
        self.theme = self.original_theme;
        self.style = self.original_style;
        self.session.update(cx, |s, cx| {
            s.preview_theme = None;
            s.apply_theme(cx);
            cx.notify();
        });
        cx.refresh_windows();
        self.hint = "已丢弃未保存的更改".into();
        cx.notify();
    }

    fn save(&mut self, close_after: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        self.sync_text_fields(cx);
        let (limit, interval) = match (
            commit_search_limit(&self.search_limit_text(cx)),
            commit_save_interval_secs(&self.save_interval_text(cx)),
        ) {
            (DraftCommit::Ok(l), DraftCommit::Ok(i)) => (l, i),
            (DraftCommit::Err(msg), _) | (_, DraftCommit::Err(msg)) => {
                self.hint = msg.into();
                self.close_guarded = false;
                cx.notify();
                return;
            }
        };
        if crate::isolation::is_production_pipe(&self.settings.pipe_name)
            && !crate::isolation::official_storage()
        {
            self.hint = "管道名称不可用，请换一个名称后再保存".into();
            self.close_guarded = false;
            cx.notify();
            return;
        }
        self.settings.search_limit = limit as u32;
        self.settings.save_interval_secs = interval;
        self.search_limit
            .update(cx, |i, cx| i.set_value(limit.to_string(), window, cx));
        self.save_interval
            .update(cx, |i, cx| i.set_value(interval.to_string(), window, cx));
        let next = self.settings.clone();
        let theme = self.theme;
        let mut prefs = self.session.read(cx).prefs.clone();
        prefs.theme = theme;
        let style = self.style;
        prefs.theme_style = style;
        let handle = window.window_handle();
        self.saving = true;
        self.hint = "正在保存…".into();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let saved = next.clone();
            let local_prefs = prefs.clone();
            let result = cx
                .background_spawn(async move {
                    crate::prefs::save_settings(&saved)?;
                    crate::prefs::save_prefs(&local_prefs)?;
                    let text = std::fs::read_to_string(crate::prefs::settings_path())
                        .map_err(|e| e.to_string())?;
                    let readback: FindxGuiSettings =
                        serde_json::from_str(&text).map_err(|e| e.to_string())?;
                    if readback != saved {
                        return Err("设置回读与保存内容不一致".into());
                    }
                    if crate::prefs::load_prefs().theme != local_prefs.theme
                        || crate::prefs::load_prefs().theme_style != local_prefs.theme_style
                    {
                        return Err("主题回读失败".into());
                    }
                    Ok::<(), String>(())
                })
                .await;
            let ok = result.is_ok();
            this.update(cx, |this, cx| {
                this.saving = false;
                this.close_guarded = false;
                match result {
                    Ok(()) => {
                        this.original = next.clone();

                        this.original_theme = theme;
                        this.original_style = style;
                        this.session.update(cx, |s, cx| {
                            s.settings = next;
                            s.prefs.theme = theme;
                            s.prefs.theme_style = style;
                            s.preview_theme = None;
                            s.apply_theme(cx);
                            cx.notify();
                        });
                        this.hint = "已保存".into();
                    }
                    Err(e) => this.hint = format!("保存失败（草稿保留）：{e}").into(),
                }
                cx.notify();
            })
            .ok();
            if ok && close_after {
                let _ = handle.update(cx, |_, w, cx| {
                    let _ = this.update(cx, |this, cx| this.finish_leave(w, cx));
                });
            }
        })
        .detach();
    }

    fn set_theme(&mut self, pref: UiThemePref, cx: &mut Context<Self>) {
        self.theme = pref;
        self.preview_theme(cx);
        cx.notify();
    }

    fn tokens(cx: &App) -> FindxTokens {
        if cx.has_global::<FindxTokens>() {
            *cx.global::<FindxTokens>()
        } else {
            FindxTokens::for_theme(crate::settings_logic::EffectiveTheme::Light)
        }
    }

    fn preview_theme(&self, cx: &mut Context<Self>) {
        self.session.update(cx, |s, cx| {
            s.preview_theme = Some((self.style, self.theme));
            s.apply_theme(cx);
            cx.notify();
        });
        cx.refresh_windows();
    }

    pub fn request_leave(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.closing_app = false;
        self.close_clicked(window, cx);
    }

    pub fn request_app_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.closing_app = true;
        self.confirm_close(window, cx)
    }

    fn finish_leave(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.session.update(cx, |session, cx| {
            session.preview_theme = None;
            session.apply_theme(cx);
            cx.notify();
        });
        if self.embedded {
            cx.emit(if self.closing_app {
                SettingsEvent::CloseApp
            } else {
                SettingsEvent::Return
            });
        } else {
            window.remove_window();
        }
    }

    fn close_clicked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm_close(window, cx) {
            self.finish_leave(window, cx);
        }
    }

    fn restore_defaults(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings = FindxGuiSettings::isolated_default(
            "findx2-native-dev",
            crate::isolation::isolated_index_path()
                .to_string_lossy()
                .into_owned(),
        );
        self.theme = UiThemePref::Light;
        self.style = ThemeStyle::Warm;
        for (input, value) in [
            (&self.search_limit, self.settings.search_limit.to_string()),
            (
                &self.save_interval,
                self.settings.save_interval_secs.to_string(),
            ),
            (&self.index_path, self.settings.index_path.clone()),
            (&self.pipe_name, self.settings.pipe_name.clone()),
            (&self.service_exe, self.settings.service_exe_path.clone()),
        ] {
            input.update(cx, |i, cx| i.set_value(value, window, cx));
        }
        self.excluded.update(cx, |i, cx| {
            i.set_value(self.settings.excluded_dirs.join("\n"), window, cx)
        });
        self.preview_theme(cx);
        self.hint = "已恢复默认值，保存后生效".into();
        cx.notify();
    }
}

fn theme_auto_check_placeholder() -> bool {
    false
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Self::tokens(cx);
        let roots = self.session.read(cx).available_roots.clone();
        let dirty = self.is_dirty(cx);
        let categories = [
            (SettingsTab::Appearance, "外观"),
            (SettingsTab::Search, "搜索"),
            (SettingsTab::Files, "文件浏览"),
            (SettingsTab::Preview, "预览"),
            (SettingsTab::Index, "索引与服务"),
            (SettingsTab::Shortcuts, "快捷键"),
            (SettingsTab::Diagnostics, "诊断"),
            (SettingsTab::About, "关于"),
        ];
        let title = categories
            .iter()
            .find(|(tab, _)| *tab == self.tab)
            .map(|(_, label)| *label)
            .unwrap_or("设置");
        let body = match self.tab {
            SettingsTab::Appearance => self.render_appearance(tokens, cx).into_any_element(),
            SettingsTab::Search => self.render_search_tab(tokens, cx).into_any_element(),
            SettingsTab::Index => settings_page()
                .child(self.render_service_tab(tokens, cx))
                .child(self.render_index_tab(&roots, tokens, cx))
                .child(self.render_maintenance(tokens, cx))
                .into_any_element(),
            SettingsTab::About => self.render_about(tokens, cx).into_any_element(),
            SettingsTab::Diagnostics => self.render_diagnostics(tokens, cx).into_any_element(),
            SettingsTab::Files => self
                .render_policy(
                    "目录",
                    &[
                        ("标签", "每个目录标签记住自己的位置、历史和选中项"),
                        ("过滤", "只匹配当前这一层的名称"),
                        ("子目录", "要查找下层文件，请到搜索页发起搜索"),
                        ("操作", "复制、移动和重命名前会确认，默认不覆盖"),
                    ],
                    tokens,
                )
                .into_any_element(),
            SettingsTab::Preview => self
                .render_policy(
                    "预览",
                    &[
                        ("打开", "在搜索或文件页按需展开预览"),
                        ("宽度", "拖动分隔线调整，关闭后会记住"),
                        ("文档", "PDF 可以翻页；Office 使用本机预览"),
                        ("主题", "预览外壳跟随界面主题"),
                    ],
                    tokens,
                )
                .into_any_element(),
            SettingsTab::Shortcuts => self
                .render_policy(
                    "快捷键",
                    &[
                        ("Ctrl+F", "搜索，或过滤当前目录"),
                        ("Ctrl+L", "编辑目录地址"),
                        ("Alt+左 / 右", "当前标签后退或前进"),
                        ("Alt+上", "上一级目录"),
                        ("F5", "刷新当前目录"),
                        ("方向键", "移动选择，Enter 打开"),
                        ("Ctrl+Enter", "在文件中定位"),
                        ("Ctrl+,", "打开设置"),
                    ],
                    tokens,
                )
                .into_any_element(),
        };
        let navigation = v_flex()
            .id("settings-navigation")
            .min_h_0()
            .overflow_y_scrollbar()
            .w(px(176.))
            .h_full()
            .flex_shrink_0()
            .p_3()
            .gap_1()
            .bg(tokens.surface)
            .border_r_1()
            .border_color(tokens.border)
            .child(div().text_lg().px_2().py_3().child("设置"))
            .child(
                Button::new("settings-back")
                    .ghost()
                    .w_full()
                    .h(px(32.))
                    .px_0()
                    .accessibility_label("返回工作区")
                    .child(settings_nav_content(
                        Icon::new(IconName::ArrowLeft),
                        "返回工作区",
                    ))
                    .on_click(cx.listener(|this, _, window, cx| this.request_leave(window, cx))),
            )
            .child(div().h(px(12.)))
            .children(categories.into_iter().map(|(tab, label)| {
                Button::new(SharedString::from(format!("settings-category-{label}")))
                    .ghost()
                    .w_full()
                    .h(px(32.))
                    .px_0()
                    .accessibility_label(label)
                    .child(settings_nav_content(
                        Icon::default().path(match tab {
                            SettingsTab::Appearance => "icons/palette.svg",
                            SettingsTab::Search => "icons/search.svg",
                            SettingsTab::Files => "icons/folder.svg",
                            SettingsTab::Preview => "icons/panel-right.svg",
                            SettingsTab::Index => "icons/database.svg",
                            SettingsTab::Shortcuts => "icons/keyboard.svg",
                            SettingsTab::Diagnostics => "icons/activity.svg",
                            SettingsTab::About => "icons/info.svg",
                        }),
                        label,
                    ))
                    .selected(self.tab == tab)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.tab = tab;
                        cx.notify();
                    }))
            }));
        let content = v_flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(
                h_flex()
                    .px_6()
                    .py_4()
                    .gap_3()
                    .items_center()
                    .border_b_1()
                    .border_color(tokens.border)
                    .child(div().text_lg().child(title))
                    .child(
                        div()
                            .w(px(72.))
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(tokens.accent)
                            .child(if dirty { "未保存" } else { "" }),
                    ),
            )
            .child(
                div()
                    .id("settings-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .p_6()
                    .child(h_flex().w_full().justify_center().child(body)),
            );
        v_flex()
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(tokens.bg)
            .text_color(tokens.text_primary)
            .when(!self.embedded, |view| {
                view.child(crate::ui::chrome::title_bar(
                    div().px_3().text_sm().child("FindX 设置"),
                    window,
                    cx,
                ))
            })
            .child(
                h_flex()
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .child(navigation)
                    .child(content),
            )
            .child(self.render_footer(tokens, cx))
            .when(self.service_pending.is_some(), |view| {
                view.child(self.render_service_confirm(tokens, cx))
            })
            .when(self.close_guarded, |view| {
                view.child(self.render_close_confirm(tokens, cx))
            })
            .when(self.show_syntax, |view| {
                view.child(self.render_syntax_help(tokens, cx))
            })
    }
}

impl SettingsWindow {
    fn render_policy(&self, title: &str, rows: &[(&str, &str)], tokens: FindxTokens) -> AnyElement {
        settings_page()
            .child(
                settings_card(tokens)
                    .child(section_title(title, tokens))
                    .children(
                        rows.iter()
                            .map(|(label, detail)| settings_kv(*label, *detail, tokens)),
                    ),
            )
            .into_any_element()
    }

    fn render_appearance(&self, tokens: FindxTokens, cx: &mut Context<Self>) -> impl IntoElement {
        let dark = self.theme.resolve(self.session.read(cx).system_dark);
        settings_page()
            .child(
                settings_card(tokens)
                    .child(section_title("主题风格", tokens))
                    .child(settings_note(
                        "选择后立即预览，取消或丢弃可还原。",
                        tokens,
                    ))
                    .child(h_flex().w_full().gap_3().items_stretch().children(ThemeStyle::ALL.into_iter().map(
                        |(style, label)| {
                            let p = FindxTokens::for_style(style, dark);
                            let selected = self.style == style;
                            div()
                                .id(SharedString::from(format!("theme-style-{label}")))
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .h(px(156.))
                                .overflow_hidden()
                                .p_3()
                                .gap_2()
                                .rounded_lg()
                                .border_2()
                                .border_color(if selected { tokens.accent } else { tokens.border })
                                .bg(p.bg)
                                .cursor_pointer()
                                .child(
                                    h_flex()
                                        .w_full()
                                        .h(px(88.))
                                        .flex_shrink_0()
                                        .rounded_md()
                                        .overflow_hidden()
                                        .border_1()
                                        .border_color(p.border)
                                        .child(div().w(px(34.)).h_full().flex_shrink_0().bg(p.surface))
                                        .child(
                                            v_flex()
                                                .flex_1()
                                                .min_w_0()
                                                .p_2()
                                                .gap_2()
                                                .child(div().h(px(8.)).w_full().bg(p.surface))
                                                .child(div().h(px(8.)).w(px(72.)).bg(p.row_selected))
                                                .child(div().h(px(8.)).w(px(92.)).bg(p.border_soft)),
                                        ),
                                )
                                .child(
                                    h_flex()
                                        .h(px(24.))
                                        .w_full()
                                        .flex_shrink_0()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div()
                                                .id(SharedString::from(format!("theme-check-{label}")))
                                                .w(px(16.))
                                                .h(px(16.))
                                                .flex_shrink_0()
                                                .when(selected, |slot| {
                                                    slot.child(Icon::new(IconName::Check).size_3())
                                                }),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .text_sm()
                                                .whitespace_nowrap()
                                                .text_ellipsis()
                                                .text_color(p.text_primary)
                                                .child(label.to_string()),
                                        ),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.style = style;
                                    this.preview_theme(cx);
                                    cx.notify();
                                }))
                        },
                    ))),
            )
            .child(
                settings_card(tokens)
                    .child(section_title("明暗模式", tokens))
                    .child(h_flex().gap_2().children(
                        [
                            (UiThemePref::Light, "浅色"),
                            (UiThemePref::Dark, "深色"),
                            (UiThemePref::System, "跟随系统"),
                        ]
                        .into_iter()
                        .map(|(pref, label)| {
                            Button::new(SharedString::from(format!("theme-mode-{label}")))
                                .w(px(112.))
                                .h(px(32.))
                                .flex_shrink_0()
                                .label(label)
                                .selected(self.theme == pref)
                                .on_click(cx.listener(move |this, _, _, cx| this.set_theme(pref, cx)))
                        }),
                    ))
                    .child(settings_note("跟随系统时，外观会随系统切换。", tokens))
            )
    }

    fn render_index_tab(
        &self,
        roots: &[String],
        tokens: FindxTokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.settings.drives.clone();
        let drive_rows = roots.len().clamp(1, 4) as f32;
        v_flex()
            .w_full()
            .gap_4()
            .child(
                settings_card(tokens)
                    .child(section_title("索引范围", tokens))
                    .child(settings_note(
                        "勾选要扫描的磁盘。都不勾选时扫描全部磁盘。保存后，在高级维护中重建才会生效。",
                        tokens,
                    ))
                    .child(v_flex().id("settings-drives-viewport").h(px(drive_rows * 40.)).flex_shrink_0().gap_2().overflow_y_scrollbar()
            .when(roots.is_empty(), |this| {
                this.child(div().text_sm().text_color(tokens.text_tertiary).child("（加载磁盘列表中…）"))
            })
            .children(roots.iter().cloned().map(|root| {
                let checked = selected.is_empty() || selected.iter().any(|d| d == &root);
                let root_key = root.clone();
                Checkbox::new(SharedString::from(format!("drive-{root}")))
                    .label(root.clone())
                    .checked(checked && !selected.is_empty() || selected.iter().any(|d| d == &root))
                    .on_change({
                    let entity = cx.entity();
                    move |checked, _, cx| {
                        entity.update(cx, |this, cx| {
                        let mut drives = this.settings.drives.clone();
                        if *checked {
                            if !drives.iter().any(|d| d == &root_key) {
                                drives.push(root_key.clone());
                            }
                        } else {
                            drives.retain(|d| d != &root_key);
                        }
                        this.settings.drives = drives;
                        cx.notify();
                        });
                    }
                })
            })))
            )
            .child(
                settings_card(tokens)
                    .child(section_title("排除目录", tokens))
                    .child(settings_note("每行一个完整路径。", tokens))
                    .child(Textarea::new(&self.excluded).h(px(96.))),
            )
            .child(
                settings_card(tokens).child(section_title("元数据", tokens))
            .child(
                Checkbox::new("backfill")
                    .checked(self.settings.enable_metadata_backfill)
                    .label("回填文件的时间和大小")
                    .on_change({
                    let entity = cx.entity();
                    move |checked, _, cx| {
                        entity.update(cx, |this, cx| {
                        this.settings.enable_metadata_backfill = *checked;
                        cx.notify();
                        });
                    }
                }),
            )
            .child(
                Checkbox::new("full-stat")
                    .checked(self.settings.first_index_full_metadata)
                    .label("首次建库时读取完整信息")
                    .on_change({
                    let entity = cx.entity();
                    move |checked, _, cx| {
                        entity.update(cx, |this, cx| {
                        this.settings.first_index_full_metadata = *checked;
                        cx.notify();
                        });
                    }
                }),
            )
            .child(
                Checkbox::new("everything-ipc")
                    .checked(self.settings.enable_everything_ipc)
                    .label("兼容 Everything 搜索")
                    .on_change({
                    let entity = cx.entity();
                    move |checked, _, cx| {
                        entity.update(cx, |this, cx| {
                        this.settings.enable_everything_ipc = *checked;
                        cx.notify();
                        });
                    }
                }),
            )
            .child(
                h_flex()
                    .h(px(36.))
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .w(px(160.))
                            .flex_shrink_0()
                            .text_sm()
                            .child("落盘间隔（秒）"),
                    )
                    .child(
                        div()
                            .w(px(140.))
                            .flex_shrink_0()
                            .child(Input::new(&self.save_interval)),
                    ),
            )
            .child(settings_note(
                "保存后，可在高级维护中应用到已安装的服务。",
                tokens,
            ))
        )
    }

    fn render_search_tab(&self, tokens: FindxTokens, cx: &mut Context<Self>) -> impl IntoElement {
        settings_page().child(
            settings_card(tokens)
                .child(section_title("搜索", tokens))
                .child(
                    h_flex().h(px(36.)).items_center().child(
                        Checkbox::new("pinyin")
                            .checked(self.settings.pinyin_default)
                            .label("默认拼音匹配")
                            .on_change({
                                let entity = cx.entity();
                                move |checked, _, cx| {
                                    entity.update(cx, |this, cx| {
                                        this.settings.pinyin_default = *checked;
                                        cx.notify();
                                    });
                                }
                            }),
                    ),
                )
                .child(settings_note("可用全拼或简拼。更多写法见语法帮助。", tokens))
                .child(
                    h_flex()
                        .h(px(36.))
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .w(px(160.))
                                .flex_shrink_0()
                                .text_sm()
                                .child("结果上限"),
                        )
                        .child(
                            div()
                                .w(px(140.))
                                .flex_shrink_0()
                                .child(Input::new(&self.search_limit)),
                        )
                        .child({
                            let entity = cx.entity();
                            Button::new("syntax")
                                .w(px(96.))
                                .flex_shrink_0()
                                .label("语法帮助")
                                .on_click(move |_, _, cx| {
                                    entity.update(cx, |this, cx| {
                                        this.show_syntax = true;
                                        cx.notify();
                                    });
                                })
                        }),
                )
                .child(settings_note(
                    "保存时须为 1 到 4294967295 的整数，默认 5000。",
                    tokens,
                ))
        )
    }

    fn refresh_service(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let (snapshot, auto) = cx
                .background_spawn(async {
                    let s = crate::service_control::snapshot();
                    let a = crate::autostart::read();
                    (s, a)
                })
                .await;
            this.update(cx, |this, cx| {
                this.autostart_enabled = auto.enabled;
                this.autostart_supported = auto.supported && auto.unsupported_reason.is_none();
                match snapshot {
                    Ok(s) => {
                        this.service_hint.clear();
                        if this.service_snapshot.is_none()
                            && this.session.read(cx).live
                            && let Ok((interval, backfill, everything)) =
                                crate::service_control::official_runtime_options(&s)
                        {
                            this.settings.save_interval_secs = interval;
                            this.settings.enable_metadata_backfill = backfill;
                            this.settings.enable_everything_ipc = everything;
                            this.original.save_interval_secs = interval;
                            this.original.enable_metadata_backfill = backfill;
                            this.original.enable_everything_ipc = everything;
                            this.session.update(cx, |session, _| {
                                session.settings.save_interval_secs = interval;
                                session.settings.enable_metadata_backfill = backfill;
                                session.settings.enable_everything_ipc = everything;
                            });
                        }
                        this.service_snapshot = Some(s);
                    }
                    Err(e) => {
                        this.service_snapshot = None;
                        this.service_hint = e;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
    fn service_locked(&self, cx: &App) -> bool {
        self.service_busy
            || self.service_snapshot.is_none()
            || (cfg!(unix) && !self.session.read(cx).official_product)
    }

    fn queue_service(&mut self, action: crate::service_control::Action, cx: &mut Context<Self>) {
        if self.service_locked(cx) {
            return;
        }
        if action.needs_confirmation() {
            self.service_pending = Some(action);
            cx.notify();
            return;
        }
        self.begin_service_action(action, cx);
    }

    fn begin_service_action(&mut self, action: crate::service_control::Action, cx: &mut Context<Self>) {
        let Some(expected) = self.service_snapshot.clone() else {
            return;
        };
        self.service_pending = None;
        self.service_busy = true;
        self.service_hint = "正在执行".into();
        cx.notify();
        let saved_preferences = self.session.read(cx).settings.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    crate::service_control::execute(action, expected, saved_preferences)
                })
                .await;
            this.update(cx, |this, cx| {
                this.service_busy = false;
                this.hint = match &result {
                    Ok(message) => message.clone().into(),
                    Err(error) => format!("操作失败：{error}").into(),
                };
                if matches!(result, Ok(_)) && action == crate::service_control::Action::Start {
                    cx.emit(SettingsEvent::ServiceReady);
                } else {
                    cx.emit(SettingsEvent::ServiceChanged);
                }
                this.refresh_service(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_service_tab(&self, tokens: FindxTokens, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::service_control::Action;
        let running = self
            .service_snapshot
            .as_ref()
            .is_some_and(|s| s.state.eq_ignore_ascii_case("Running"));
        let boot = self
            .service_snapshot
            .as_ref()
            .is_some_and(|s| s.start_mode.eq_ignore_ascii_case("Auto"));
        let locked = self.service_locked(cx);
        let state = self
            .service_snapshot
            .as_ref()
            .map(|s| service_state_label(&s.state))
            .unwrap_or("未检测到服务");
        settings_card(tokens)
            .child(section_title("索引服务", tokens))
            .child(
                h_flex()
                    .id("service-status")
                    .h(px(52.))
                    .px_3()
                    .items_center()
                    .gap_3()
                    .rounded_md()
                    .border_1()
                    .border_color(tokens.border)
                    .bg(tokens.surface)
                    .child(
                        div()
                            .w(px(8.))
                            .h(px(8.))
                            .flex_shrink_0()
                            .rounded_full()
                            .bg(if running { tokens.success } else { tokens.text_tertiary }),
                    )
                    .child(
                        div()
                            .w(px(112.))
                            .flex_shrink_0()
                            .text_sm()
                            .child(state),
                    )
                    .child(div().flex_1())
                    .child({
                        let entity = cx.entity();
                        Button::new("service-start-stop")
                            .primary()
                            .w(px(112.))
                            .label(if running { "停止服务" } else { "启动服务" })
                            .disabled(locked)
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |this, cx| {
                                    let running = this
                                        .service_snapshot
                                        .as_ref()
                                        .is_some_and(|s| s.state.eq_ignore_ascii_case("Running"));
                                    this.queue_service(
                                        if running { Action::Stop } else { Action::Start },
                                        cx,
                                    );
                                });
                            })
                    })
                    .child({
                        let entity = cx.entity();
                        Button::new("service-refresh")
                            .w(px(72.))
                            .label("刷新")
                            .disabled(self.service_busy)
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |this, cx| this.refresh_service(cx));
                            })
                    }),
            )
            .child(self.service_switch_row(
                "service-boot",
                "服务开机自启",
                "开机后自动启动索引服务",
                boot,
                locked,
                tokens,
                cx,
                |this, on, cx| {
                    this.queue_service(if *on { Action::Auto } else { Action::Manual }, cx);
                },
            ))
            .child(self.service_switch_row(
                "service-login",
                "登录时自动启动 FindX",
                "登录后打开 FindX",
                self.autostart_enabled,
                locked || !self.autostart_supported,
                tokens,
                cx,
                |this, on, cx| {
                    this.queue_service(
                        if *on {
                            Action::GuiAutoOn
                        } else {
                            Action::GuiAutoOff
                        },
                        cx,
                    );
                },
            ))
            .child(settings_async_line("service-hint".into(), self.service_hint.clone()))
    }

    fn render_maintenance(&self, tokens: FindxTokens, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::service_control::Action;
        let locked = self.service_locked(cx);
        let name = self
            .service_snapshot
            .as_ref()
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "不可用".into());
        let path = self
            .service_snapshot
            .as_ref()
            .map(|s| s.path_name.clone())
            .unwrap_or_else(|| "不可用".into());
        let options = self
            .service_snapshot
            .as_ref()
            .and_then(|s| crate::service_control::official_runtime_options(s).ok())
            .map(|(interval, backfill, everything)| {
                format!(
                    "落盘 {interval} 秒，元数据回填 {}，Everything {}",
                    if backfill { "开启" } else { "关闭" },
                    if everything { "开启" } else { "关闭" }
                )
            })
            .unwrap_or_else(|| "不可用".into());
        settings_card(tokens)
            .child(
                h_flex().h(px(36.)).items_center().justify_between().child(section_title(
                    "高级维护",
                    tokens,
                )).child({
                    let entity = cx.entity();
                    Button::new("service-advanced")
                        .ghost()
                        .w(px(112.))
                        .flex_shrink_0()
                        .selected(self.service_advanced)
                        .label(if self.service_advanced { "收起" } else { "展开" })
                        .on_click(move |_, _, cx| {
                            entity.update(cx, |this, cx| {
                                this.service_advanced = !this.service_advanced;
                                cx.notify();
                            });
                        })
                }),
            )
            .child(settings_note(
                "压缩或重建会先完整备份。失败时保留原索引和备份。",
                tokens,
            ))
            .child(
                div()
                    .id("service-advanced-body")
                    .w_full()
                    .h(if self.service_advanced { px(264.) } else { px(0.) })
                    .flex_shrink_0()
                    .overflow_hidden()
                    .child(
                        v_flex()
                            .gap_3()
                            .child(self.maintenance_button(Action::Compact, locked, cx))
                            .child(self.maintenance_button(Action::Rebuild, locked, cx))
                            .child(self.maintenance_button(Action::ApplyPreferences, locked, cx))
                            .child(settings_async_line(
                                "service-name".into(),
                                format!("服务名称  {name}"),
                            ))
                            .child(settings_async_line("service-path".into(), path))
                            .child(settings_async_line("service-options".into(), options)),
                    ),
            )
    }

    fn service_switch_row(
        &self,
        id: &'static str,
        title: &'static str,
        detail: &'static str,
        checked: bool,
        disabled: bool,
        tokens: FindxTokens,
        cx: &mut Context<Self>,
        on_toggle: impl Fn(&mut Self, &bool, &mut Context<Self>) + 'static,
    ) -> impl IntoElement {
        h_flex()
            .id(id)
            .h(px(48.))
            .px_3()
            .items_center()
            .gap_3()
            .rounded_md()
            .border_1()
            .border_color(tokens.border)
            .bg(tokens.surface)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_sm().child(title))
                    .child(
                        div()
                            .text_xs()
                            .text_color(tokens.text_secondary)
                            .child(detail),
                    ),
            )
            .child(
                div().w(px(48.)).flex_shrink_0().child(
                    Switch::new(SharedString::from(format!("{id}-switch")))
                        .checked(checked)
                        .disabled(disabled)
                        .on_click(cx.listener(move |this, on, _, cx| on_toggle(this, on, cx))),
                ),
            )
    }

    fn maintenance_button(
        &self,
        action: crate::service_control::Action,
        locked: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let entity = cx.entity();
        Button::new(SharedString::from(action.key()))
            .w(px(220.))
            .label(action.label())
            .disabled(locked)
            .on_click(move |_, _, cx| {
                entity.update(cx, |this, cx| this.queue_service(action, cx));
            })
    }

    fn render_service_confirm(
        &self,
        tokens: FindxTokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let action = self.service_pending.unwrap();
        let saved = &self.session.read(cx).settings;
        let detail = if action == crate::service_control::Action::ApplyPreferences {
            format!(
                "将写入已保存选项：落盘 {} 秒，元数据回填 {}，Everything {}。请先保存再应用。",
                saved.save_interval_secs,
                if saved.enable_metadata_backfill { "开启" } else { "关闭" },
                if saved.enable_everything_ipc { "开启" } else { "关闭" }
            )
        } else {
            self.service_snapshot
                .as_ref()
                .map(|s| format!("服务 {}，状态 {}", s.name, service_state_label(&s.state)))
                .unwrap_or_default()
        };
        div().absolute().inset_0().occlude().bg(rgba(0x00000088)).flex().items_center().justify_center().child(v_flex().w(px(440.)).max_w_full().m_4().p_4().gap_3().rounded_lg().bg(tokens.surface_raised)
            .child(div().h(px(24.)).flex_shrink_0().child(format!("确认{}？", action.label())))
            .child(div().id("service-confirm-body").h(px(72.)).flex_shrink_0().overflow_y_scrollbar().text_sm().child(detail))
            .child(div().h(px(20.)).flex_shrink_0().text_sm().text_ellipsis().text_color(tokens.text_secondary).child("执行期间搜索会暂时不可用。备份会保留在索引旁边。"))
            .child(h_flex().h(px(32.)).gap_2().child({
                let entity = cx.entity();
                Button::new("confirm-service").primary().w(px(112.)).label("确认执行").on_click(move |_, _, cx| {
                    entity.update(cx, |this, cx| this.begin_service_action(action, cx));
                })
            }).child({
                let entity = cx.entity();
                Button::new("cancel-service").w(px(88.)).label("取消").on_click(move |_, _, cx| {
                    entity.update(cx, |this, cx| {
                        this.service_pending = None;
                        cx.notify();
                    });
                })
            })))
    }

    fn persist_auto_check(&mut self, checked: bool, cx: &mut Context<Self>) {
        self.update.auto_check = checked;
        self.settings.auto_check_update = checked;
        self.original.auto_check_update = checked;
        self.session.update(cx, |session, cx| {
            session.settings.auto_check_update = checked;
            crate::session::persist_settings(session.settings.clone(), cx);
        });
        if checked {
            let official = self.session.read(cx).official_product;
            let last = self.session.read(cx).prefs.last_official_update_check_unix;
            if should_auto_check(official, true, last) {
                self.start_check(cx);
            }
        }
        cx.notify();
    }

    fn start_update_action(&mut self, cx: &mut Context<Self>) {
        let official = self.session.read(cx).official_product;
        if !official {
            return;
        }
        let state = self.session.read(cx).update_state.clone();
        let phase = derive_update_phase(
            state.busy,
            state.progress.as_ref(),
            state.info.as_ref(),
            state.downloaded_path.as_deref(),
        );
        match phase {
            UpdatePhase::Available => self.start_download_install(cx),
            UpdatePhase::Downloaded => self.start_install(cx),
            UpdatePhase::Checking | UpdatePhase::Downloading | UpdatePhase::Installing => {}
            UpdatePhase::Idle | UpdatePhase::Current | UpdatePhase::Error => self.start_check(cx),
        }
    }

    fn start_check(&mut self, cx: &mut Context<Self>) {
        if !self.session.read(cx).official_product {
            return;
        }
        if self.session.read(cx).update_state.busy {
            return;
        }
        self.session.update(cx, |session, cx| {
            session.update_state.busy = true;
            session.update_state.progress = None;
            session.update_state.downloaded_path = None;
            cx.notify();
        });
        let current = env!("CARGO_PKG_VERSION").to_string();
        cx.spawn(async move |this, cx| {
            let info = cx
                .background_spawn(
                    async move { crate::update_logic::check_latest_release(&current) },
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                this.session.update(cx, |session, cx| {
                    session.update_state.busy = false;
                    session.update_state.info = Some(info);
                    session.prefs.last_official_update_check_unix = crate::update_logic::now_unix();
                    crate::session::persist_prefs(session.prefs.clone(), cx);
                    cx.notify();
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn start_download_install(&mut self, cx: &mut Context<Self>) {
        if !self.session.read(cx).official_product {
            return;
        }
        if self.session.read(cx).update_state.busy {
            return;
        }
        if self.session.read(cx).update_state.downloaded_path.is_some() {
            self.start_install(cx);
            return;
        }
        let Some(url) = self
            .session
            .read(cx)
            .update_state
            .info
            .as_ref()
            .and_then(|i| i.download_url.clone())
        else {
            self.session.update(cx, |session, cx| {
                if let Some(info) = session.update_state.info.as_mut() {
                    info.ok = false;
                    info.error = Some("没有可用的官方安装包下载地址。".into());
                }
                cx.notify();
            });
            return;
        };
        let expected = self
            .session
            .read(cx)
            .update_state
            .info
            .as_ref()
            .and_then(|i| i.asset_size);
        self.session.update(cx, |session, cx| {
            session.update_state.busy = true;
            session.update_state.progress = Some(crate::update_logic::UpdateProgress {
                downloaded: 0,
                total: expected.unwrap_or(0),
                percent: 0.0,
            });
            cx.notify();
        });
        let (tx, rx) = std::sync::mpsc::channel();
        cx.spawn(async move |this, cx| {
            let worker = std::thread::spawn(move || {
                crate::update_logic::download_official_setup(&url, expected, |progress| {
                    let _ = tx.send(progress);
                })
            });
            loop {
                while let Ok(progress) = rx.try_recv() {
                    let _ = this.update(cx, |this, cx| {
                        this.session.update(cx, |session, cx| {
                            session.update_state.progress = Some(progress);
                            cx.notify();
                        });
                    });
                }
                if worker.is_finished() {
                    break;
                }
                cx.background_spawn(async {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                })
                .await;
            }
            let result = worker
                .join()
                .unwrap_or_else(|_| Err("下载线程异常退出。".into()));
            let _ = this.update(cx, |this, cx| {
                this.session.update(cx, |session, cx| {
                    session.update_state.busy = false;
                    match &result {
                        Ok(path) => {
                            session.update_state.downloaded_path =
                                Some(path.to_string_lossy().into_owned());
                            session.update_state.progress =
                                session.update_state.progress.take().map(|mut p| {
                                    p.percent = 100.0;
                                    p
                                });
                        }
                        Err(e) => {
                            if let Some(info) = session.update_state.info.as_mut() {
                                info.ok = false;
                                info.error = Some(e.clone());
                            }
                            session.update_state.progress = None;
                        }
                    }
                    cx.notify();
                });
                if result.is_ok() {
                    this.start_install(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_install(&mut self, cx: &mut Context<Self>) {
        if !self.session.read(cx).official_product || self.session.read(cx).update_state.busy {
            return;
        }
        let Some(path) = self.session.read(cx).update_state.downloaded_path.clone() else {
            return;
        };
        self.session.update(cx, |s, _| {
            s.update_state.busy = true;
        });
        self.hint = "正在安装更新；请确认系统提权提示。取消会保留当前安装。".into();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { start_silent_setup(std::path::Path::new(&path)) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.session.update(cx, |s, _| {
                    s.update_state.busy = false;
                });
                match result {
                    Ok(()) => {
                        #[cfg(unix)]
                        {
                            let launch = if cfg!(target_os = "macos") {
                                std::process::Command::new("/usr/bin/open")
                                    .args(["-n", "/Applications/FindX.app"])
                                    .spawn()
                            } else {
                                std::process::Command::new("/opt/FindX/FindX").spawn()
                            };
                            match launch {
                                Ok(_) => cx.quit(),
                                Err(e) => {
                                    this.hint = format!(
                                        "更新已安装，但重新启动失败：{e}。请手动重新打开 FindX。"
                                    )
                                    .into();
                                }
                            }
                        }
                        #[cfg(windows)]
                        {
                            this.hint =
                                "已启动安装程序，请确认 UAC；安装器完成后重新打开应用。".into();
                        }
                    }
                    Err(e) => {
                        this.hint = format!("安装未完成：{e}").into();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_about(&self, tokens: FindxTokens, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let auto = self.update.auto_check;
        settings_page().child({
            let official = self.session.read(cx).official_product;
            let runtime = self.session.read(cx).update_state.clone();
            render_update_card(
                official,
                env!("CARGO_PKG_VERSION"),
                runtime,
                auto,
                tokens,
                {
                    let entity = entity.clone();
                    Rc::new(move |_, cx: &mut App, checked| {
                        entity.update(cx, |this, cx| {
                            this.persist_auto_check(checked, cx);
                        });
                    })
                },
                {
                    let entity = entity.clone();
                    Rc::new(move |_, cx: &mut App| {
                        entity.update(cx, |this, cx| {
                            this.start_update_action(cx);
                        });
                    })
                },
            )
        })
    }

    fn render_storage_fields(&self, tokens: FindxTokens) -> impl IntoElement {
        settings_card(tokens)
            .child(section_title("连接与存储", tokens))
            .child(settings_note("已安装版本由系统服务管理这些值。", tokens))
            .child(settings_field("索引文件", Input::new(&self.index_path)))
            .child(settings_field("管道名称", Input::new(&self.pipe_name)))
            .child(settings_field("服务程序", Input::new(&self.service_exe)))
    }

    fn render_close_confirm(
        &self,
        tokens: FindxTokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .absolute()
            .inset_0()
            .occlude()
            .bg(rgba(0x00000066))
            .flex()
            .items_center()
            .justify_center()
            .child(
                v_flex()
                    .w(px(420.))
                    .max_w_full()
                    .m_4()
                    .p_4()
                    .gap_3()
                    .rounded_lg()
                    .bg(tokens.surface_raised)
                    .border_1()
                    .border_color(tokens.border)
                    .child(div().child("设置尚未保存"))
                    .child(div().text_sm().child(CLOSE_DIRTY_MESSAGE))
                    .child(
                        h_flex().h(px(32.)).gap_2().children(
                            [
                                ("save-close", "保存并关闭", 112.),
                                ("discard-close", "丢弃并关闭", 112.),
                                ("cancel-close", "取消", 88.),
                            ]
                            .into_iter()
                            .map(|(id, label, width)| {
                                let entity = cx.entity();
                                Button::new(id).w(px(width)).flex_shrink_0().label(label).disabled(self.saving).on_click(
                                    move |_, window, cx| {
                                        entity.update(cx, |this, cx| match id {
                                            "save-close" => this.save(true, window, cx),
                                            "discard-close" => {
                                                this.revert_unsaved(cx);
                                                this.finish_leave(window, cx);
                                            }
                                            _ => {
                                                this.close_guarded = false;
                                                this.closing_app = false;
                                                cx.emit(SettingsEvent::Stay);
                                                cx.notify();
                                            }
                                        })
                                    },
                                )
                            }),
                        ),
                    ),
            )
    }

    fn render_diagnostics(&self, tokens: FindxTokens, cx: &mut Context<Self>) -> impl IntoElement {
        let s = self.session.read(cx);
        let rows = vec![
            format!(
                "客户端版本：{} {}",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::ARCH
            ),
            format!("连接：{}", if s.connected { "已连接" } else { "未连接" }),
            format!("连接地址：{}", s.endpoint),
            format!(
                "运行方式：{}",
                if s.mock {
                    "模拟数据"
                } else if s.official_product {
                    "已安装的索引服务"
                } else if s.live {
                    "连接到已有服务"
                } else {
                    "独立配置"
                }
            ),
            format!("最近状态：{}", s.status_checked),
            "服务版本：接口未提供".into(),
            crate::status_logic::diagnostic_index_line(&s.index_status.summary),
            format!("错误：{}", s.last_error.as_deref().unwrap_or("无")),
            format!("设置文件：{}", prefs::settings_path().display()),
            format!("管道名称：{}", self.settings.pipe_name),
            "窗口较窄时，预览和侧栏会自动收起，也可以手动展开。".into(),
            "这里只读取状态，不会改动索引。".into(),
        ];
        settings_page()
            .child(
                settings_card(tokens)
                    .child(section_title("运行状态", tokens))
                    .children(rows.into_iter().enumerate().map(|(i, r)| {
                        settings_async_line(format!("runtime-diagnostic-{i}").into(), r)
                    }))
                    .child(
                        h_flex().h(px(36.)).items_center().child({
                            let session = self.session.clone();
                            Checkbox::new("main-diagnostics")
                                .checked(s.prefs.diag_enabled)
                                .label("在主窗口显示诊断浮层")
                                .on_change(move |on, _, cx| {
                                    session.update(cx, |s, cx| {
                                        s.set_diag(*on, cx);
                                        cx.notify();
                                    })
                                })
                        }),
                    ),
            )
            .child(self.render_storage_fields(tokens))
            .child(
                settings_card(tokens)
                    .child(section_title("原始状态", tokens))
                    .child(settings_note("每 2 秒刷新一次。", tokens))
                    .child(
                        div()
                            .id("raw-status-viewport").h(px(192.))
                            .flex_shrink_0()
                            .overflow_y_scrollbar()
                            .text_xs()
                            .child(s.status_raw.clone()),
                    ),
            )
    }

    fn render_footer(&self, tokens: FindxTokens, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .h(px(56.))
            .flex_shrink_0()
            .px_4()
            .gap_2()
            .items_center()
            .bg(tokens.surface)
            .border_t_1()
            .border_color(tokens.border)
            .child({
                let hint = self.hint.clone();
                div()
                    .id("settings-footer-hint")
                    .flex_1()
                    .min_w_0()
                    .h(px(20.))
                    .text_sm()
                    .text_color(tokens.text_secondary)
                    .text_ellipsis()
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(hint.clone()).build(window, cx)
                    })
                    .child(self.hint.clone())
            })
            .child(
                Button::new("settings-defaults")
                    .ghost()
                    .w(px(96.))
                    .flex_shrink_0()
                    .label("恢复默认")
                    .disabled(self.saving || self.service_busy)
                    .on_click(cx.listener(|this, _, window, cx| this.restore_defaults(window, cx))),
            )
            .child({
                let entity = cx.entity();
                Button::new("save")
                    .primary()
                    .w(px(88.))
                    .flex_shrink_0()
                    .label("保存")
                    .disabled(self.saving)
                    .on_click(move |_, window, cx| {
                        entity.update(cx, |this, cx| this.save(false, window, cx));
                    })
            })
            .child({
                let entity = cx.entity();
                Button::new("close")
                    .w(px(112.))
                    .flex_shrink_0()
                    .label("取消并返回")
                    .disabled(self.saving || self.service_busy)
                    .on_click(move |_, window, cx| {
                        entity.update(cx, |this, cx| this.close_clicked(window, cx));
                    })
            })
    }

    fn render_syntax_help(&self, tokens: FindxTokens, cx: &mut Context<Self>) -> impl IntoElement {
        {
            let entity = cx.entity();
            let entity2 = entity.clone();
            div()
                .id("syntax-overlay")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x00000066))
                .on_click(move |_, _, cx| {
                    entity.update(cx, |this, cx| {
                        this.show_syntax = false;
                        cx.notify();
                    });
                })
                .child(
                    v_flex()
                        .id("syntax-dialog")
                        .w(px(520.))
                        .max_h(px(560.))
                        .p_4()
                        .gap_2()
                        .rounded_lg()
                        .bg(tokens.surface_raised)
                        .border_1()
                        .border_color(tokens.border)
                        .on_click(|_, _, _| {})
                        .child(
                            h_flex()
                                .justify_between()
                                .child(div().child("搜索语法帮助"))
                                .child(Button::new("close-syntax").ghost().label("关闭").on_click(
                                    move |_, _, cx| {
                                        entity2.update(cx, |this, cx| {
                                            this.show_syntax = false;
                                            cx.notify();
                                        });
                                    },
                                )),
                        )
                        .child(
                            div()
                                .id("syntax-body")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scrollbar()
                                .text_sm()
                                .text_color(tokens.text_secondary)
                                .child(
                                    "基础：beijing / bj 拼音，直接英文名。\n\
修饰：;py 强制拼音，;en 或 ;np 关拼音。\n\
类型：ext:pdf、file:report、folder:docs、startwith:2024、endwith:backup、path:、parent:。\n\
组合：report | document 为或，!temp 排除。\n\
路径：Windows 用 C:\\Users\\...，Unix 用 /home/user 或 ~/Documents，不要写盘符。",
                                ),
                        ),
                )
        }
    }
}

fn service_state_label(state: &str) -> &'static str {
    if state.eq_ignore_ascii_case("Running") {
        "运行中"
    } else if state.eq_ignore_ascii_case("Stopped") {
        "已停止"
    } else if state.eq_ignore_ascii_case("Start Pending")
        || state.eq_ignore_ascii_case("Stop Pending")
    {
        "正在切换"
    } else {
        "状态未知"
    }
}

fn section_title(text: &str, tokens: FindxTokens) -> impl IntoElement {
    div()
        .h(px(24.))
        .flex_shrink_0()
        .text_sm()
        .text_color(tokens.text_primary)
        .child(text.to_string())
}

fn settings_page() -> Div {
    v_flex().w(px(960.)).max_w_full().gap_4()
}

fn settings_card(tokens: FindxTokens) -> Div {
    v_flex()
        .w_full()
        .gap_3()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(tokens.border)
        .bg(tokens.surface)
}

fn settings_note(text: &str, tokens: FindxTokens) -> impl IntoElement {
    let detail = text.to_string();
    div()
        .id(SharedString::from(format!("settings-note-{text}")))
        .h(px(20.))
        .flex_shrink_0()
        .min_w_0()
        .text_sm()
        .text_color(tokens.text_secondary)
        .whitespace_nowrap()
        .text_ellipsis()
        .tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(detail.clone()).build(window, cx)
        })
        .child(text.to_string())
}

fn settings_kv(label: &str, detail: &str, tokens: FindxTokens) -> impl IntoElement {
    let text = detail.to_string();
    let tip = text.clone();
    h_flex()
        .h(px(36.))
        .w_full()
        .flex_shrink_0()
        .items_center()
        .gap_4()
        .child(
            div()
                .w(px(132.))
                .flex_shrink_0()
                .text_sm()
                .child(label.to_string()),
        )
        .child(
            div()
                .id(SharedString::from(format!("settings-kv-{label}")))
                .flex_1()
                .min_w_0()
                .text_sm()
                .text_color(tokens.text_secondary)
                .text_ellipsis()
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                })
                .child(text),
        )
}

fn settings_field(label: &str, control: impl IntoElement) -> impl IntoElement {
    v_flex()
        .w_full()
        .gap_1()
        .flex_shrink_0()
        .child(div().h(px(20.)).text_sm().child(label.to_string()))
        .child(control)
}
