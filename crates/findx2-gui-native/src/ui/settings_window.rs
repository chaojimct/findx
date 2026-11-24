use gpui_kit::component::{Icon, IconName};
use gpui_kit::AnimationExt as _;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::switch::Switch;

use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState, SliderValue};
use gpui_kit::component::{Disableable as _, Selectable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::prefs;
use crate::session::AppSession;
use crate::settings_logic::{
    ContentDefaultMode, DraftCommit, FindxGuiSettings, UiThemePref, commit_content_commit_docs,
    commit_content_commit_secs, commit_content_max_file_mb, commit_content_mix_limit,
    commit_save_interval_secs, commit_search_limit,
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

/// Fresh per-instance id so the enter slide replays only when a settings page
/// is created, not on every tab switch or re-render.
static SETTINGS_ENTER_EPOCH: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    Appearance,
    Files,
    Preview,
    Shortcuts,
    About,
    Index,
    Search,
    Content,
    Diagnostics,
}

impl SettingsTab {
    pub fn from_canonical(tab: &str) -> Self {
        match crate::settings_search::canonical_tab(tab) {
            "appearance" => Self::Appearance,
            "files" => Self::Files,
            "preview" => Self::Preview,
            "shortcuts" => Self::Shortcuts,
            "about" => Self::About,
            "index" => Self::Index,
            "search" => Self::Search,
            "content" => Self::Content,
            "diagnostics" => Self::Diagnostics,
            _ => Self::Appearance,
        }
    }
    pub fn canonical(self) -> &'static str {
        match self {
            Self::Appearance => "appearance",
            Self::Files => "files",
            Self::Preview => "preview",
            Self::Shortcuts => "shortcuts",
            Self::About => "about",
            Self::Index => "index",
            Self::Search => "search",
            Self::Content => "content",
            Self::Diagnostics => "diagnostics",
        }
    }
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
    session: Entity<AppSession>,
    pub tab: SettingsTab,
    // Live truth: every control writes through to the session and disk
    // immediately. There is no draft/original pair, no save button.
    settings: FindxGuiSettings,
    theme: UiThemePref,
    material: crate::window_material::WindowMaterial,
    material_opacity: Entity<SliderState>,
    /// See `MainWindow::applied_material`: render converges the backdrop.
    applied_material: Option<crate::window_material::WindowMaterial>,
    search_limit: Entity<InputState>,
    save_interval: Entity<InputState>,
    content_max_file: Entity<InputState>,
    content_mix_limit: Entity<InputState>,
    content_commit_docs: Entity<InputState>,
    content_commit_secs: Entity<InputState>,
    nav_search: Entity<InputState>,
    /// 搜索结果跳转后定位到的设置项文案，用于内容区顶部反馈。
    located_label: Option<&'static str>,
    /// 页面进入动画的 id，每个实例唯一（与 workspace 的 page_epoch 同理）。
    enter_epoch: usize,
    /// 正在等待录入的快捷键 id（快捷键页点击「修改」后设置）。
    capturing: Option<String>,
    /// 让 SettingsWindow 根节点可聚焦，录制时接收原始 KeyDown。
    capture_focus: FocusHandle,
    /// 快捷键页的即时反馈（冲突 / 取消 / 已保存）。
    shortcut_hint: String,
    index_path: Entity<InputState>,
    pipe_name: Entity<InputState>,
    service_exe: Entity<InputState>,
    excluded: Entity<TextareaState>,
    hint: SharedString,
    update: UpdateCardState,
    show_syntax: bool,
    service_snapshot: Option<crate::service_control::Snapshot>,
    service_pending: Option<crate::service_control::Action>,
    service_busy: bool,
    service_advanced: bool,
    content_advanced: bool,
    service_hint: String,
    autostart_enabled: bool,
    autostart_supported: bool,
    _subscriptions: Vec<Subscription>,
}

impl SettingsWindow {
    pub fn new(session: Entity<AppSession>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_inner(session, false, window, cx, None)
    }

    pub fn new_embedded(
        session: Entity<AppSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_embedded_at(session, window, cx, None)
    }

    pub fn new_embedded_at(
        session: Entity<AppSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
        initial_tab: Option<&str>,
    ) -> Self {
        Self::new_inner(session, true, window, cx, initial_tab)
    }

    fn new_inner(
        session: Entity<AppSession>,
        embedded: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
        initial_tab: Option<&str>,
    ) -> Self {
        let style = session.read(cx).prefs.theme_style;
        let material = session.read(cx).prefs.window_material;
        let (settings, theme) = session.read_with(cx, |s, _| (s.settings.clone(), s.prefs.theme));
        let search_limit = cx
            .new(|cx| InputState::new(window, cx).default_value(settings.search_limit.to_string()));
        let save_interval = cx.new(|cx| {
            InputState::new(window, cx).default_value(settings.save_interval_secs.to_string())
        });
        let content_max_file = cx.new(|cx| {
            InputState::new(window, cx).default_value(settings.content_max_file_mb.to_string())
        });
        let content_mix_limit = cx.new(|cx| {
            InputState::new(window, cx).default_value(settings.content_mix_limit.to_string())
        });
        let content_commit_docs = cx.new(|cx| {
            InputState::new(window, cx).default_value(settings.content_commit_docs.to_string())
        });
        let content_commit_secs = cx.new(|cx| {
            InputState::new(window, cx).default_value(settings.content_commit_secs.to_string())
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

        let nav_search = cx.new(|cx| {
            InputState::new(window, cx).placeholder("搜索设置项…")
        });
        let saved_opacity =
            crate::window_material::clamp_opacity(session.read(cx).prefs.material_opacity);
        let material_opacity = cx.new(|_| {
            SliderState::new()
                .min(25.)
                .max(100.)
                .step(1.)
                .default_value(saved_opacity * 100.)
        });

        let mut this = Self {
            embedded,
            closing_app: false,
            style,
            session,
            tab: if let Some(t) = initial_tab {
                SettingsTab::from_canonical(t)
            } else if crate::isolation::development_var_os("FINDX_NATIVE_OPEN_ABOUT").is_some() {
                SettingsTab::About
            } else {
                SettingsTab::Appearance
            },
            settings,
            theme,
            material,
            material_opacity: material_opacity.clone(),
            applied_material: None,
            search_limit: search_limit.clone(),
            save_interval: save_interval.clone(),
            content_max_file: content_max_file.clone(),
            content_mix_limit: content_mix_limit.clone(),
            content_commit_docs: content_commit_docs.clone(),
            content_commit_secs: content_commit_secs.clone(),
            nav_search: nav_search.clone(),
            located_label: None,
            enter_epoch: SETTINGS_ENTER_EPOCH.fetch_add(1, Ordering::Relaxed) as usize,
            capturing: None,
            capture_focus: cx.focus_handle(),
            shortcut_hint: String::new(),
            index_path: index_path.clone(),
            pipe_name: pipe_name.clone(),
            service_exe: service_exe.clone(),
            excluded: excluded.clone(),
            hint: SharedString::from(""),
            update: UpdateCardState::new(theme_auto_check_placeholder()),
            show_syntax: false,
            service_snapshot: None,
            service_pending: None,
            service_busy: false,
            service_advanced: false,
            content_advanced: false,
            service_hint: "正在读取服务状态".into(),
            autostart_enabled: false,
            autostart_supported: true,
            _subscriptions: Vec::new(),
        };
        this.update.auto_check = this.settings.auto_check_update;

        // Every text control applies live: valid input persists immediately,
        // invalid input only raises the hint and never touches saved state.
        for input in [
            this.search_limit.clone(),
            this.save_interval.clone(),
            this.content_max_file.clone(),
            this.content_mix_limit.clone(),
            this.content_commit_docs.clone(),
            this.content_commit_secs.clone(),
        ] {
            this._subscriptions
                .push(cx.subscribe_in(&input, window, move |this, _, ev, _, cx| {
                    if let InputEvent::Change = ev {
                        this.commit_numeric_fields(cx);
                    }
                }));
        }
        for input in [
            this.index_path.clone(),
            this.pipe_name.clone(),
            this.service_exe.clone(),
        ] {
            this._subscriptions
                .push(cx.subscribe_in(&input, window, move |this, _, ev, _, cx| {
                    if let InputEvent::Change = ev {
                        this.commit_text_fields(cx);
                    }
                }));
        }

        this._subscriptions
            .push(cx.observe(&this.session, |_, _, cx| cx.notify()));
        // 搜索框改动要重绘左栏（并清掉上一次的定位反馈）：InputState 只通知自己，
        // 父视图不会自动重绘。
        this._subscriptions.push(
            cx.subscribe_in(&this.nav_search, window, |this, _, ev, _, cx| {
                if let InputEvent::Change = ev {
                    this.located_label = None;
                    cx.notify();
                }
            }),
        );
        // Opacity drags repaint live; only the release touches the disk.
        this._subscriptions.push(cx.subscribe_in(
            &this.material_opacity,
            window,
            |this, _, event: &SliderEvent, _, cx| match event {
                SliderEvent::Change(value) => this.preview_opacity(*value, cx),
                SliderEvent::Release(value) => this.commit_opacity(*value, cx),
            },
        ));
        this._subscriptions.push(
            cx.subscribe_in(&this.excluded, window, |this, _, ev, _, cx| {
                if let InputEvent::Change = ev {
                    this.sync_text_fields(cx);
                    this.apply_settings(cx);
                    cx.notify();
                }
            }),
        );
        if !embedded {
            let entity = cx.entity();
            window.on_window_should_close(cx, move |window, cx| {
                entity.update(cx, |this, cx| this.request_app_close(window, cx))
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

    fn content_max_file_text(&self, cx: &App) -> String {
        self.content_max_file.read(cx).value().to_string()
    }

    fn content_mix_limit_text(&self, cx: &App) -> String {
        self.content_mix_limit.read(cx).value().to_string()
    }

    fn content_commit_docs_text(&self, cx: &App) -> String {
        self.content_commit_docs.read(cx).value().to_string()
    }

    fn content_commit_secs_text(&self, cx: &App) -> String {
        self.content_commit_secs.read(cx).value().to_string()
    }

    /// Push the live struct into the session and onto disk right away.
    fn apply_settings(&mut self, cx: &mut App) {
        let next = self.settings.clone();
        self.session.update(cx, |session, cx| {
            session.settings = next;
            crate::session::persist_settings(session.settings.clone(), cx);
        });
    }

    /// Apply theme + style instantly: session, live UI, and disk, no draft.
    fn apply_theme_now(&mut self, style: ThemeStyle, theme: UiThemePref, cx: &mut Context<Self>) {
        self.style = style;
        self.theme = theme;
        self.session.update(cx, |s, cx| {
            s.prefs.theme = theme;
            s.prefs.theme_style = style;
            s.preview_theme = None;
            s.apply_theme(cx);
            cx.notify();
        });
        crate::session::persist_prefs(self.session.read(cx).prefs.clone(), cx);
        cx.refresh_windows();
        cx.notify();
    }

    /// Numeric fields validate on every keystroke: valid values apply
    /// instantly, invalid ones only raise the hint (saved state untouched).
    fn commit_numeric_fields(&mut self, cx: &mut Context<Self>) {
        match (
            commit_search_limit(&self.search_limit_text(cx)),
            commit_save_interval_secs(&self.save_interval_text(cx)),
            commit_content_max_file_mb(&self.content_max_file_text(cx)),
            commit_content_mix_limit(&self.content_mix_limit_text(cx)),
            commit_content_commit_docs(&self.content_commit_docs_text(cx)),
            commit_content_commit_secs(&self.content_commit_secs_text(cx)),
        ) {
            (
                DraftCommit::Ok(l),
                DraftCommit::Ok(i),
                DraftCommit::Ok(mb),
                DraftCommit::Ok(mix),
                DraftCommit::Ok(docs),
                DraftCommit::Ok(secs),
            ) => {
                let changed = self.settings.search_limit != l as u32
                    || self.settings.save_interval_secs != i
                    || self.settings.content_max_file_mb != mb
                    || self.settings.content_mix_limit != mix as u32
                    || self.settings.content_commit_docs != docs
                    || self.settings.content_commit_secs != secs;
                self.settings.search_limit = l as u32;
                self.settings.save_interval_secs = i;
                self.settings.content_max_file_mb = mb;
                self.settings.content_mix_limit = mix as u32;
                self.settings.content_commit_docs = docs;
                self.settings.content_commit_secs = secs;
                if changed {
                    self.hint = SharedString::from("");
                    self.apply_settings(cx);
                }
                cx.notify();
            }
            (DraftCommit::Err(msg), _, _, _, _, _)
            | (_, DraftCommit::Err(msg), _, _, _, _)
            | (_, _, DraftCommit::Err(msg), _, _, _)
            | (_, _, _, DraftCommit::Err(msg), _, _)
            | (_, _, _, _, DraftCommit::Err(msg), _)
            | (_, _, _, _, _, DraftCommit::Err(msg)) => {
                self.hint = msg.into();
                cx.notify();
            }
        }
    }

    /// Text fields sync on every keystroke; an illegal pipe name is refused
    /// with a hint instead of being persisted.
    fn commit_text_fields(&mut self, cx: &mut Context<Self>) {
        self.sync_text_fields(cx);
        if crate::isolation::is_production_pipe(&self.settings.pipe_name)
            && !crate::isolation::official_storage()
        {
            self.hint = "管道名称不可用，请换一个名称".into();
            cx.notify();
            return;
        }
        self.apply_settings(cx);
        cx.notify();
    }

    fn tokens(cx: &App) -> FindxTokens {
        if cx.has_global::<FindxTokens>() {
            *cx.global::<FindxTokens>()
        } else {
            FindxTokens::for_theme(crate::settings_logic::EffectiveTheme::Light)
        }
    }

    fn set_theme(&mut self, pref: UiThemePref, cx: &mut Context<Self>) {
        self.apply_theme_now(self.style, pref, cx);
    }

    /// Slider drags repaint the chrome live; only the release writes to disk.
    fn preview_opacity(&mut self, value: SliderValue, cx: &mut Context<Self>) {
        let SliderValue::Single(percent) = value else {
            return;
        };
        let opacity = crate::window_material::clamp_opacity(percent / 100.);
        if (opacity - self.session.read(cx).prefs.material_opacity).abs() < f32::EPSILON
        {
            return;
        }
        self.session.update(cx, |s, cx| {
            s.prefs.material_opacity = opacity;
            s.apply_theme(cx);
            cx.notify();
        });
        cx.refresh_windows();
        cx.notify();
    }

    fn commit_opacity(&mut self, value: SliderValue, cx: &mut Context<Self>) {
        self.preview_opacity(value, cx);
        crate::session::persist_prefs(self.session.read(cx).prefs.clone(), cx);
    }

    /// Switch the saved material: session, theme, and disk, no draft.
    /// Every open window converges itself on its next render (see
    /// `applied_material`): a cross-window update cannot touch a window
    /// mid-dispatch, so the loop over handles is gone. Mirrors
    /// `apply_theme_now`.
    fn set_material(
        &mut self,
        material: crate::window_material::WindowMaterial,
        cx: &mut Context<Self>,
    ) {
        self.material = material;
        self.session.update(cx, |s, cx| {
            s.prefs.window_material = material;
            s.apply_theme(cx);
            cx.notify();
        });
        crate::session::persist_prefs(self.session.read(cx).prefs.clone(), cx);
        if !crate::window_material::os_support().supports(material) {
            self.hint = SharedString::from(format!(
                "当前系统不支持「{}」，已回退纯色。",
                material.label()
            ));
        }
        cx.refresh_windows();
        cx.notify();
    }

    // ---- Shortcut editing -------------------------------------------------
    fn begin_capture(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.capturing = Some(id.to_string());
        self.shortcut_hint = format!(
            "正在录入「{}」：请按下新组合键，Esc 取消",
            crate::keybindings::def(id)
                .map(|d| d.label)
                .unwrap_or(id)
        );
        self.capture_focus.focus(window, cx);
        cx.notify();
    }

    fn cancel_capture(&mut self, cx: &mut Context<Self>) {
        self.capturing = None;
        self.shortcut_hint.clear();
        cx.notify();
    }

    fn save_shortcut(
        &mut self,
        id: &str,
        stroke: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(stroke) = stroke else {
            self.shortcut_hint = "这个按键不能作为快捷键，请换一个（例如 Ctrl+K）".into();
            self.capturing = None;
            cx.notify();
            return;
        };
        // Reject keys already owned by another action so a binding never
        // silently shadows its neighbour.
        let mut overrides = self.session.read(cx).prefs.key_bindings.clone();
        overrides.retain(|other, _| other != id);
        let owner = crate::keybindings::defs().iter().find(|definition| {
            definition.id != id
                && crate::keybindings::effective(definition.id, &overrides) == stroke
        });
        if let Some(owner) = owner {
            self.shortcut_hint = format!("「{}」已经在使用 {stroke}", owner.label);
            self.capturing = None;
            cx.notify();
            return;
        }
        if crate::keybindings::default_for(id) == stroke {
            overrides.remove(id);
            self.shortcut_hint = "已恢复默认快捷键".into();
        } else {
            overrides.insert(id.to_string(), stroke.clone());
            self.shortcut_hint = format!("已保存为 {}", crate::keybindings::display(&stroke));
        }
        self.capturing = None;
        let session = self.session.clone();
        session.update(cx, |session, cx| session.set_key_bindings(overrides, cx));
        cx.notify();
    }

    fn reset_shortcut(&mut self, id: &str, cx: &mut Context<Self>) {
        let mut overrides = self.session.read(cx).prefs.key_bindings.clone();
        if overrides.remove(id).is_none() {
            return;
        }
        self.shortcut_hint = "已恢复默认快捷键".into();
        let session = self.session.clone();
        session.update(cx, |session, cx| session.set_key_bindings(overrides, cx));
        cx.notify();
    }

    fn reset_all_shortcuts(&mut self, cx: &mut Context<Self>) {
        if self.session.read(cx).prefs.key_bindings.is_empty() {
            return;
        }
        self.shortcut_hint = "所有快捷键已恢复默认".into();
        let session = self.session.clone();
        session.update(cx, |session, cx| session.set_key_bindings(Default::default(), cx));
        cx.notify();
    }

    pub fn request_leave(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.closing_app = false;
        self.close_clicked(window, cx);
    }

    pub fn request_app_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.closing_app = true;
        // Nothing to confirm: every control already applied. Only an
        // in-flight service operation holds the page.
        if self.service_busy {
            self.hint = "正在执行服务操作，完成前不能离开设置".into();
            cx.notify();
            return false;
        }
        let _ = window;
        true
    }

    fn finish_leave(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        if self.service_busy {
            self.hint = "正在执行服务操作，完成前不能离开设置".into();
            cx.notify();
            return;
        }
        self.finish_leave(window, cx);
    }

    fn restore_defaults(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings = FindxGuiSettings::isolated_default(
            "findx2-native-dev",
            crate::isolation::isolated_index_path()
                .to_string_lossy()
                .into_owned(),
        );
        if crate::isolation::official_storage() {
            // Installed builds are managed by the system service: never
            // persist a dev pipe or index path over them.
            self.settings.pipe_name = "findx2".into();
            self.settings.index_path = crate::isolation::official_index_path()
                .to_string_lossy()
                .into_owned();
        }
        self.theme = UiThemePref::Light;
        self.style = ThemeStyle::VSCode;
        for (input, value) in [
            (&self.search_limit, self.settings.search_limit.to_string()),
            (
                &self.save_interval,
                self.settings.save_interval_secs.to_string(),
            ),
            (
                &self.content_max_file,
                self.settings.content_max_file_mb.to_string(),
            ),
            (
                &self.content_mix_limit,
                self.settings.content_mix_limit.to_string(),
            ),
            (
                &self.content_commit_docs,
                self.settings.content_commit_docs.to_string(),
            ),
            (
                &self.content_commit_secs,
                self.settings.content_commit_secs.to_string(),
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
        self.content_advanced = false;
        self.apply_settings(cx);
        self.apply_theme_now(self.style, self.theme, cx);
        self.hint = "已恢复默认值".into();
        cx.notify();
    }
}

fn theme_auto_check_placeholder() -> bool {
    false
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Same convergence as the main window (see
        // `MainWindow::applied_material`): render runs inside this window,
        // so it can always touch its own HWND.
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
        let tokens = Self::tokens(cx);
        let roots = self.session.read(cx).available_roots.clone();
        let nav_query = self.nav_search.read(cx).value().to_string();
        let searching = !nav_query.trim().is_empty();
        // 搜索命中设置项的内容（文案 / 分组 / 关键词 / 拼音），而不再只匹配 tab 标题。
        let matched_items: Vec<crate::settings_search::SettingsItem> = if searching {
            crate::settings_search::filter(
                &crate::settings_search::all_settings_items(),
                &nav_query,
            )
        } else {
            Vec::new()
        };
        // 搜索时左栏只列命中的设置项，用其所属 tab 去重后给标题栏定位。
        let categories: Vec<(SettingsTab, &'static str)> = if searching {
            let mut tabs: Vec<(SettingsTab, &'static str)> = Vec::new();
            for it in &matched_items {
                let tab = SettingsTab::from_canonical(it.tab);
                if !tabs.iter().any(|(t, _)| *t == tab) {
                    tabs.push((tab, crate::settings_search::tab_label(it.tab)));
                }
            }
            tabs
        } else {
            [
                (SettingsTab::Appearance, "外观"),
                (SettingsTab::Search, "搜索"),
                (SettingsTab::Files, "文件浏览"),
                (SettingsTab::Preview, "预览"),
                (SettingsTab::Index, "索引与服务"),
                (SettingsTab::Content, "全文"),
                (SettingsTab::Shortcuts, "快捷键"),
                (SettingsTab::Diagnostics, "诊断"),
                (SettingsTab::About, "关于"),
            ]
            .to_vec()
        };

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
            SettingsTab::Content => self.render_content_tab(tokens, cx).into_any_element(),
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
            SettingsTab::Shortcuts => self.render_shortcuts(tokens, cx).into_any_element(),
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
                div().px_2().pb_2().child(
                    Input::new(&self.nav_search)
                        .w_full()
                        .h(px(30.)),
                ),
            )
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
            .when(searching, |v| {
                v.child(
                    div()
                        .text_xs()
                        .px_2()
                        .py_1()
                        .text_color(tokens.text_secondary)
                        .child(format!("匹配的设置项（{}）", matched_items.len())),
                )
                .when(matched_items.is_empty(), |v| {
                    v.child(div().text_xs().px_2().py_2().text_color(tokens.text_tertiary).child("无匹配的设置项"))
                })
                .children(matched_items.iter().map(|it| {
                    let tab_id = it.tab;
                    let label = it.label;
                    Button::new(SharedString::from(format!("settings-match-{}", it.id)))
                        .ghost()
                        .w_full()
                        .h(px(32.))
                        .px_0()
                        .accessibility_label(label)
                        .child(settings_nav_content(
                            Icon::default().path(match tab_id {
                                "appearance" => "icons/palette.svg",
                                "search" => "icons/search.svg",
                                "files" => "icons/folder.svg",
                                "preview" => "icons/panel-right.svg",
                                "index" => "icons/database.svg",
                                "content" => "icons/file-text.svg",
                                "shortcuts" => "icons/keyboard.svg",
                                "diagnostics" => "icons/activity.svg",
                                "about" => "icons/info.svg",
                                _ => "icons/info.svg",
                            }),
                            label,
                        ))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.tab = SettingsTab::from_canonical(tab_id);
                            this.located_label = Some(label);
                            this.hint = SharedString::from(format!("已定位到「{}」", label));
                            cx.notify();
                        }))
                }))
            })
            .when(!searching, |v| {
                v.children(categories.into_iter().map(|(tab, label)| {
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
                                SettingsTab::Content => "icons/file-text.svg",
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
                }))
            });
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
                            .text_xs()
                            .text_color(tokens.text_secondary)
                            .child(if searching {
                                match self.located_label {
                                    Some(l) => SharedString::from(format!(
                                        "已定位到「{}」，左侧可继续选择其它结果",
                                        l
                                    )),
                                    None => SharedString::from(format!(
                                        "找到 {} 项设置，点击左侧结果跳转到所在分类",
                                        matched_items.len()
                                    )),
                                }
                            } else {
                                SharedString::from("")
                            }),
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
        // Page-enter slide, same feel as the search/files switch: the main area
        // glides in +32px -> 0 over 250ms ease-out. A per-instance epoch keeps the
        // animation id stable across re-renders but fresh for each opened page;
        // the global reduce-motion switch bypasses it entirely.
        let main_row: AnyElement = if self.session.read(cx).prefs.reduce_motion {
            h_flex()
                .w_full()
                .flex_1()
                .min_h_0()
                .items_stretch()
                .child(navigation)
                .child(content)
                .into_any_element()
        } else {
            v_flex()
                .w_full()
                .flex_1()
                .min_h_0()
                .relative()
                .child(
                    h_flex()
                        .w_full()
                        .flex_1()
                        .min_h_0()
                        .items_stretch()
                        .child(navigation)
                        .child(content),
                )
                .with_animation(
                    gpui_kit::ElementId::named_usize("settings-enter", self.enter_epoch),
                    gpui_kit::Animation::new(std::time::Duration::from_millis(
                        crate::ui::motion::PAGE_ENTER_MS,
                    ))
                    .with_easing(crate::ui::motion::ease_out_cubic),
                    move |el, delta| {
                        el.left(px(crate::ui::motion::PAGE_ENTER_PX * (1.0 - delta)))
                            .opacity(delta)
                    },
                )
                .into_any_element()
        };
        v_flex()
            .size_full()
            .relative()
            .overflow_hidden()
            .track_focus(&self.capture_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                let Some(id) = this.capturing.clone() else {
                    return;
                };
                cx.stop_propagation();
                if event.keystroke.key.eq_ignore_ascii_case("escape") {
                    this.cancel_capture(cx);
                    return;
                }
                let stroke = crate::keybindings::from_keystroke(&event.keystroke);
                this.save_shortcut(&id, stroke, cx);
            }))
            .when(
                self.material == crate::window_material::WindowMaterial::Solid,
                |v| v.bg(tokens.bg),
            )
            .when(
                self.material != crate::window_material::WindowMaterial::Solid,
                // Non-solid roots need their own translucent base: otherwise
                // gaps between cards show raw glass (alpha 0, ignores the
                // opacity slider) while cards are milky. surface follows
                // the slider via apply_material_translucency.
                |v| v.bg(tokens.surface),
            )
            .text_color(tokens.text_primary)
            .when(!self.embedded, |view| {
                view.child(crate::ui::chrome::title_bar(
                    div().px_3().text_sm().child("FindX 设置"),
                    window,
                    cx,
                ))
            })
            .child(main_row)
            .child(self.render_footer(tokens, cx))
            .when(self.service_pending.is_some(), |view| {
                view.child(self.render_service_confirm(tokens, cx))
            })
            .when(self.show_syntax, |view| {
                view.child(self.render_syntax_help(tokens, cx))
            })
    }
}

impl SettingsWindow {
    /// 快捷键页：列出全部可改快捷键，点击「修改」后按键即可重新绑定。
    fn render_shortcuts(&self, tokens: FindxTokens, cx: &mut Context<Self>) -> AnyElement {
        let overrides = self.session.read(cx).prefs.key_bindings.clone();
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut last_group = "";
        for definition in crate::keybindings::defs() {
            if definition.group != last_group {
                last_group = definition.group;
                rows.push(
                    div()
                        .w_full()
                        .pt_2()
                        .pb_1()
                        .text_xs()
                        .text_color(tokens.text_tertiary)
                        .child(definition.group.to_string())
                        .into_any_element(),
                );
            }
            let id = definition.id;
            let stroke = crate::keybindings::effective(id, &overrides);
            let custom = overrides.contains_key(id) && stroke != crate::keybindings::default_for(id);
            let capturing = self.capturing.as_deref() == Some(id);
            rows.push(
                h_flex()
                    .id(SharedString::from(format!("shortcut-row-{id}")))
                    .w_full()
                    .h(px(34.))
                    .items_center()
                    .gap_3()
                    .px_2()
                    .rounded_md()
                    .bg(if capturing {
                        tokens.surface_raised
                    } else {
                        tokens.bg
                    })
                    .child(
                        div()
                            .w(px(184.))
                            .flex_shrink_0()
                            .text_sm()
                            .child(definition.label.to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_md()
                                    .bg(tokens.surface_raised)
                                    .text_xs()
                                    .text_color(if custom {
                                        tokens.accent
                                    } else {
                                        tokens.text_secondary
                                    })
                                    .child(if capturing {
                                        "请按键…".to_string()
                                    } else {
                                        crate::keybindings::display(&stroke)
                                    }),
                            ),
                    )
                    .child(
                        Button::new(SharedString::from(format!("shortcut-edit-{id}")))
                            .ghost()
                            .h(px(26.))
                            .px_2()
                            .child(if capturing { "取消" } else { "修改" })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if this.capturing.as_deref() == Some(id) {
                                    this.cancel_capture(cx);
                                } else {
                                    this.begin_capture(id, window, cx);
                                }
                            })),
                    )
                    .when(custom, |row| {
                        row.child(
                            Button::new(SharedString::from(format!("shortcut-reset-{id}")))
                                .ghost()
                                .h(px(26.))
                                .px_2()
                                .child("重置")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.reset_shortcut(id, cx)
                                })),
                        )
                    })
                    .into_any_element(),
            );
        }
        settings_page()
            .child(
                settings_card(tokens)
                    .child(section_title("快捷键", tokens))
                    .child(settings_note(
                        "点击「修改」后按下新组合键即可生效；修改立即保存。",
                        tokens,
                    ))
                    .child(
                        div()
                            .id("shortcut-hint")
                            .min_h(px(20.))
                            .text_sm()
                            .text_color(tokens.accent)
                            .child(self.shortcut_hint.clone()),
                    )
                    .child(
                        Button::new("shortcut-reset-all")
                            .ghost()
                            .h(px(28.))
                            .px_2()
                            .child("全部恢复默认")
                            .on_click(cx.listener(|this, _, _, cx| this.reset_all_shortcuts(cx))),
                    ),
            )
            .child(
                settings_card(tokens)
                    .w_full()
                    .overflow_y_scrollbar()
                    .children(rows),
            )
            .into_any_element()
    }

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
                        "选择后立即生效。",
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
                                    this.apply_theme_now(style, this.theme, cx);
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
            // Backdrop materials are a Windows-only DWM feature; other
            // platforms keep the solid theme background.
            .when(cfg!(windows), |v| {
                let support = crate::window_material::os_support();
                v.child(
                    settings_card(tokens)
                        .child(section_title("窗口材质", tokens))
                        .child(h_flex().gap_2().children(
                            crate::window_material::WindowMaterial::ALL
                                .into_iter()
                                .map(|(material, label)| {
                                    Button::new(SharedString::from(format!(
                                        "window-material-{label}"
                                    )))
                                    .flex_1()
                                    .min_w_0()
                                    .h(px(32.))
                                    .label(label)
                                    .selected(self.material == material)
                                    .disabled(!support.supports(material))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.set_material(material, cx)
                                    }))
                                }),
                        ))
                        .child(
                            h_flex().gap_2().items_center().child(
                                div().text_sm().text_color(tokens.text_secondary).child(
                                    "背景不透明度",
                                ),
                            )
                            .child(Slider::new(&self.material_opacity).flex_1().disabled(
                                self.material
                                    == crate::window_material::WindowMaterial::Solid,
                            ))
                            .child(
                                div().w(px(44.)).text_sm().text_color(tokens.text_primary).child(
                                    format!(
                                        "{}%",
                                        match self.material_opacity.read(cx).value() {
                                            SliderValue::Single(v) => v as i32,
                                            SliderValue::Range(_, end) => end as i32,
                                        }
                                    ),
                                ),
                            ),
                        )
                        .child(settings_note(self.material.note(), tokens))
                        .child(self.service_switch_row(
                            "content-translucent",
                            "内容区透明",
                            "列表、预览、详情等内容底色跟随材质透明（纯色材质下不可用）。",
                            self.session.read(cx).prefs.content_translucent,
                            self.material
                                == crate::window_material::WindowMaterial::Solid,
                            tokens,
                            cx,
                            |this, on, cx| {
                                let on = *on;
                                this.session.update(cx, |s, cx| {
                                    s.prefs.content_translucent = on;
                                    s.apply_theme(cx);
                                    crate::session::persist_prefs(s.prefs.clone(), cx);
                                });
                                cx.refresh_windows();
                                cx.notify();
                            },
                        ))
                        .child(settings_note(
                            if support.supports(
                                crate::window_material::WindowMaterial::Mica,
                            ) {
                                "切换立即生效；亚克力在壁纸上最明显。"
                            } else {
                                "当前系统仅支持亚克力与纯色；云母需要 Windows 11。"
                            },
                            tokens,
                        )),
                )
            })
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
                        "勾选要扫描的磁盘。都不勾选时扫描全部磁盘。重建索引后生效。",
                        tokens,
                    ))
                    .child(div()
                        .text_sm()
                        .text_color(tokens.text_secondary)
                        .child("勾选要扫描的盘。一个都不勾 = 全盘，与命令行默认一致。这些范围草稿只写入 native-dev；要改变正式索引范围，请先保存后到「服务模式」确认重建（重建使用正式排除规则，不使用这里的草稿）。"))
                    .child(v_flex().id("settings-drives-viewport").h(px(128.)).flex_shrink_0().gap_2().overflow_y_scrollbar()
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
                        this.apply_settings(cx);
                        cx.notify();
                        });
                    }
                })
            })))
            )
            .child(section_title("排除目录", tokens))
            .child(
                div()
                    .text_sm()
                    .text_color(tokens.text_secondary)
                    .child("每行一个完整路径。规则只写入隔离设置；正式重建以服务目录中的 index.bin.exclude.json 为准。"),
            )
            .child(
                settings_card(tokens)
                    .child(section_title("已索引位置", tokens))
                    .child(settings_note(
                        "状态条持续更新，此处的位置信息来自 service 的最新状态快照；已勾选的磁盘可能尚未完成扫描。",
                        tokens,
                    ))
                    .child({
                        // The live label, not the configured drives: this is a
                        // report of what the index actually covers.
                        let raw = self.session.read(cx).status_raw.clone();
                        let label =
                            crate::status_logic::indexed_locations_label(&raw);
                        div()
                            .id("indexed-locations-row")
                            .w_full()
                            .h(px(24.))
                            .text_sm()
                            .text_color(tokens.text_secondary)
                            .child(match label {
                                Some(l) => l,
                                None => "暂无已索引位置".into(),
                            })
                    }),
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
                        this.apply_settings(cx);
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
                        this.apply_settings(cx);
                        cx.notify();
                        });
                    }
                }),
            )
            .child(
                Checkbox::new("everything-ipc")
                    .checked(self.settings.enable_everything_ipc)
                    .label("启用 Everything IPC（仅 Windows；Unix 不提供此接口）")
                    .on_change({
                    let entity = cx.entity();
                    move |checked, _, cx| {
                        entity.update(cx, |this, cx| {
                        this.settings.enable_everything_ipc = *checked;
                        this.apply_settings(cx);
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
            .child(
                div()
                    .text_sm()
                    .text_color(tokens.text_secondary)
                    .child("服务选项需保存后在「服务模式」确认应用。Unix 写入当前用户服务配置并恢复原运行状态；维护会保留完整备份。Windows 使用 SCM。扫描范围/索引路径草稿不会改写正式服务。"),
            )
            .child(settings_note(
                "修改即时保存，可在高级维护中应用到已安装的服务。",
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
                                        this.apply_settings(cx);
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
                    "须为 1 到 4294967295 的整数，默认 5000。输入即时生效，无效输入会被忽略并提示。",
                    tokens,
                ))
        )
    }

    /// 全文内容索引页签：状态与维护 / 查询行为 / 范围与限额 / 高级。
    ///
    /// 状态读取与构建执行接线后续（service StatusResult/content IPC），本期只落配置项：
    /// 数字框输入即时校验保存，开关即时生效，维护按钮置灰并明示后续。
    fn render_content_tab(&self, tokens: FindxTokens, cx: &mut Context<Self>) -> impl IntoElement {
        settings_page()
            .child(self.render_content_status(tokens, cx))
            .child(self.render_content_query(tokens, cx))
            .child(self.render_content_scope(tokens, cx))
            .child(self.render_content_advanced(tokens, cx))
    }

    fn content_dir_label(&self) -> String {
        format!("{}.content", self.settings.index_path)
    }

    fn render_content_status(&self, tokens: FindxTokens, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = cx;
        settings_card(tokens)
            .child(section_title("全文索引状态", tokens))
            .child(settings_note(
                "内容倒排与文件名索引独立存放；schema v1，多卷 key 隔离。未建时 content: 查询走慢速逐文件。",
                tokens,
            ))
            .child(settings_kv("索引目录", &self.content_dir_label(), tokens))
            .child(settings_kv(
                "状态",
                "读取接线后续（service Status 透出 docs/体积后此处显示就绪与否）",
                tokens,
            ))
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(Button::new("content-build").label("构建索引").disabled(true))
                    .child(Button::new("content-rebuild").label("重建").disabled(true))
                    .child(Button::new("content-clear").label("清空").disabled(true)),
            )
            .child(settings_note(
                "构建/重建/清空执行接线后续，本期只保存配置项。",
                tokens,
            ))
    }

    fn render_content_query(&self, tokens: FindxTokens, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = self.settings.content_default_mode;
        settings_card(tokens)
            .child(section_title("查询行为", tokens))
            .child(self.service_switch_row(
                "content-enabled",
                "启用全文搜索",
                "关闭后搜索页隐藏全文入口；已建索引与增量跟随不受影响",
                self.settings.content_enabled,
                false,
                tokens,
                cx,
                |this, on, cx| {
                    this.settings.content_enabled = *on;
                    this.apply_settings(cx);
                    cx.notify();
                },
            ))
            .child(
                h_flex()
                    .h(px(36.))
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(160.))
                            .flex_shrink_0()
                            .text_sm()
                            .child("默认搜索模式"),
                    )
                    .children(
                        [
                            ContentDefaultMode::Name,
                            ContentDefaultMode::Content,
                            ContentDefaultMode::Mix,
                        ]
                        .into_iter()
                        .map(|m| {
                            let entity = cx.entity();
                            Button::new(SharedString::from(format!(
                                "content-mode-{}",
                                m.as_str()
                            )))
                            .w(px(80.))
                            .flex_shrink_0()
                            .label(m.label())
                            .selected(mode == m)
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |this, cx| {
                                    this.settings.content_default_mode = m;
                                    this.apply_settings(cx);
                                    cx.notify();
                                });
                            })
                        }),
                    ),
            )
            .child(settings_note(
                "文件名：只搜文件名；全文：只搜内容，保留类型/时间/大小过滤；混合：两者并集。",
                tokens,
            ))
            .child(self.service_switch_row(
                "content-snippet",
                "命中摘要",
                "结果表第二行与预览窗显示命中上下文",
                self.settings.content_snippet,
                false,
                tokens,
                cx,
                |this, on, cx| {
                    this.settings.content_snippet = *on;
                    this.apply_settings(cx);
                    cx.notify();
                },
            ))
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
                            .child("混合模式内容上限"),
                    )
                    .child(
                        div()
                            .w(px(140.))
                            .flex_shrink_0()
                            .child(Input::new(&self.content_mix_limit)),
                    ),
            )
            .child(settings_note(
                "1 到 100000 的整数，默认 200。输入即时生效，无效输入会被忽略并提示。",
                tokens,
            ))
    }

    fn render_content_scope(&self, tokens: FindxTokens, cx: &mut Context<Self>) -> impl IntoElement {
        settings_card(tokens)
            .child(section_title("索引范围与限额", tokens))
            .child(settings_note(
                "范围跟随「索引与服务」页的磁盘勾选与排除目录；内容库从同一文件名索引全量构建。",
                tokens,
            ))
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
                            .child("单文件上限（MB）"),
                    )
                    .child(
                        div()
                            .w(px(140.))
                            .flex_shrink_0()
                            .child(Input::new(&self.content_max_file)),
                    ),
            )
            .child(settings_note(
                "1 到 512 的整数 MB，默认 20；超限与空文件跳过。",
                tokens,
            ))
            .child(self.service_switch_row(
                "content-office",
                "抽取 Office 文档",
                "docx / xlsx / pptx（纯 Rust 解析）",
                self.settings.content_index_office,
                false,
                tokens,
                cx,
                |this, on, cx| {
                    this.settings.content_index_office = *on;
                    this.apply_settings(cx);
                    cx.notify();
                },
            ))
            .child(self.service_switch_row(
                "content-pdf",
                "抽取 PDF",
                "扫描版与损坏文件自动跳过",
                self.settings.content_index_pdf,
                false,
                tokens,
                cx,
                |this, on, cx| {
                    this.settings.content_index_pdf = *on;
                    this.apply_settings(cx);
                    cx.notify();
                },
            ))
            .child(self.service_switch_row(
                "content-code",
                "抽取代码文件",
                "80+ 代码/配置文件扩展名",
                self.settings.content_index_code,
                false,
                tokens,
                cx,
                |this, on, cx| {
                    this.settings.content_index_code = *on;
                    this.apply_settings(cx);
                    cx.notify();
                },
            ))
            .child(settings_note(
                "另支持 txt/md/csv/log 等纯文本；老 doc/xls、压缩包内文件、图片 OCR 跳过。",
                tokens,
            ))
    }

    fn render_content_advanced(
        &self,
        tokens: FindxTokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        settings_card(tokens)
            .child(
                h_flex().h(px(36.)).items_center().justify_between().child(section_title(
                    "高级",
                    tokens,
                )).child({
                    let entity = cx.entity();
                    Button::new("content-advanced")
                        .ghost()
                        .w(px(112.))
                        .flex_shrink_0()
                        .selected(self.content_advanced)
                        .label(if self.content_advanced { "收起" } else { "展开" })
                        .on_click(move |_, _, cx| {
                            entity.update(cx, |this, cx| {
                                this.content_advanced = !this.content_advanced;
                                cx.notify();
                            });
                        })
                }),
            )
            .when(self.content_advanced, |card| {
                card.child(self.service_switch_row(
                    "content-follow",
                    "增量跟随",
                    "service 常驻时文件变更自动更新内容索引；关闭则只保留已建快照",
                    self.settings.content_follow_incremental,
                    false,
                    tokens,
                    cx,
                    |this, on, cx| {
                        this.settings.content_follow_incremental = *on;
                        this.apply_settings(cx);
                        cx.notify();
                    },
                ))
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
                                .child("攒批文档数"),
                        )
                        .child(
                            div()
                                .w(px(140.))
                                .flex_shrink_0()
                                .child(Input::new(&self.content_commit_docs)),
                        ),
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
                                .child("攒批间隔（秒）"),
                        )
                        .child(
                            div()
                                .w(px(140.))
                                .flex_shrink_0()
                                .child(Input::new(&self.content_commit_secs)),
                        ),
                )
                .child(settings_note(
                    "增量线程按文档数或间隔 commit；改动需应用到服务后生效。",
                    tokens,
                ))
            })
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
        v_flex().gap_3().w_full()
            .child(settings_card(tokens)
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
            .child(settings_async_line("service-hint".into(), self.service_hint.clone())))
            .child(section_title("管理已安装的正式 FindX 服务",tokens))
            .child(div().text_sm().child("只管理身份核对通过的正式平台服务；不会按进程名称批量终止，也不会注册另一套开发服务。操作前确认，执行后回读。"))
            .child({
                let path=self.service_snapshot.as_ref().map(|s|s.path_name.clone()).unwrap_or_default();
                settings_async_line("service-path".into(), path)
            })
            .child({
                let options=self.service_snapshot.as_ref()
                    .and_then(|s|crate::service_control::official_runtime_options(s).ok())
                    .map(|(interval,backfill,everything)|format!("正式服务当前选项（注册命令行）：落盘 {interval} 秒，元数据回填 {}，Everything {}",if backfill{"开启"}else{"关闭"},if everything{"开启"}else{"关闭"}))
                    .unwrap_or_default();
                settings_async_line("service-options".into(), options)
            })
            .child({let entity=cx.entity();Button::new("refresh-service").label("刷新服务状态").disabled(self.service_busy).on_click(move|_,_,cx|entity.update(cx,|this,cx|this.refresh_service(cx)))})
            .child(h_flex().gap_2().flex_wrap().children([Action::Start,Action::Stop,Action::Auto,Action::Manual,Action::GuiAutoOn,Action::GuiAutoOff,Action::Compact,Action::Rebuild,Action::ApplyPreferences].into_iter().map(|action|{let entity=cx.entity();Button::new(SharedString::from(action.label())).label(action.label()).disabled(self.service_busy||self.service_snapshot.is_none()||(cfg!(unix)&&!self.session.read(cx).official_product)).on_click(move|_,_,cx|entity.update(cx,|this,cx|{this.service_pending=Some(action);cx.notify();}))})))
            .child(div().text_sm().text_color(tokens.warning).child("维护先停止已核对的服务并完整备份，在副本上运行安装版 CLI，校验成功后替换，保留旧库并恢复原运行状态。缺少明确扫描范围或排除规则未知时拒绝重建。Unix 使用当前用户服务权限；Windows 需要 SCM/管理员权限。"))
            .child(div().text_sm().child("此页只操作身份核对通过的平台服务/登录启动。Unix 隔离入口禁用正式服务操作。范围/路径草稿不会覆盖正式服务；注册命令行是服务参数真源。"))
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
        let target = self
            .service_snapshot
            .as_ref()
            .map(|s| {
                format!(
                    "目标：{} · PID {} · {}\n{}",
                    s.name, s.process_id, s.state, s.path_name
                )
            })
            .unwrap_or_default();
        div().absolute().inset_0().occlude().bg(rgba(0x00000088)).flex().items_center().justify_center().child(v_flex().w(px(520.)).max_w_full().m_4().p_4().gap_3().rounded_lg().bg(tokens.surface_raised)
            .child(div().child(format!("确认{}？",action.label())))
            .child(div().text_sm().child(target))
            .when(action==crate::service_control::Action::ApplyPreferences,|v|{let saved=&self.session.read(cx).settings;let official=self.service_snapshot.as_ref().and_then(|s|crate::service_control::official_runtime_options(s).ok()).map(|(i,b,e)|format!("正式服务当前：落盘 {i} 秒，回填 {}，Everything {}",if b{"开启"}else{"关闭"},if e{"开启"}else{"关闭"})).unwrap_or_else(||"无法解析正式服务当前选项".into());v.child(div().text_sm().child(official)).child(div().text_sm().child(format!("将写入已保存选项：落盘 {} 秒，元数据回填 {}，Everything {}。请先在设置中保存再应用。不迁移开发索引/管道/路径。Unix 应用时恢复原运行状态；Windows 下次启动生效。",saved.save_interval_secs,if saved.enable_metadata_backfill{"开启"}else{"关闭"},if saved.enable_everything_ipc{"开启"}else{"关闭"})))})
            .child(div().text_sm().child("此操作真正影响正式 FindX。停止/维护期间搜索将不可用。备份可能占用大量磁盘空间；过程失败会明确报告。没有自动删除备份。"))
            .child(h_flex().gap_2().child({let e=cx.entity();Button::new("confirm-service").label("确认执行").on_click(move|_,_,cx|e.update(cx,|this,cx|{let Some(expected)=this.service_snapshot.clone()else{return;};this.service_pending=None;this.service_busy=true;this.service_hint="正在执行正式服务操作，请勿关闭窗口…".into();cx.notify();let saved_preferences=this.session.read(cx).settings.clone();cx.spawn(async move|this,cx|{let result=cx.background_spawn(async move{crate::service_control::execute(action,expected,saved_preferences)}).await;this.update(cx,|this,cx|{this.service_busy=false;this.hint=match result{Ok(m)=>m.into(),Err(e)=>format!("操作失败：{e}").into()};this.refresh_service(cx);cx.notify();}).ok();}).detach();}))})
            .child({let e=cx.entity();Button::new("cancel-service").label("取消操作").on_click(move|_,_,cx|e.update(cx,|this,cx|{this.service_pending=None;cx.notify();}))})))
    }

    fn persist_auto_check(&mut self, checked: bool, cx: &mut Context<Self>) {
        self.update.auto_check = checked;
        self.settings.auto_check_update = checked;
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
        let checksum_url = self
            .session
            .read(cx)
            .update_state
            .info
            .as_ref()
            .and_then(|i| i.checksum_url.clone());
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
                crate::update_logic::download_official_setup(
                    &url,
                    expected,
                    checksum_url.as_deref(),
                    |progress| {
                        let _ = tx.send(progress);
                    },
                )
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
        v_flex()
            .w_full()
            .gap_3()
            .child(section_title("只读运行诊断", tokens))
            .children(rows.into_iter().enumerate().map(|(i,r)| settings_async_line(format!("runtime-diagnostic-{i}").into(), r)))
            .child({
                let session = self.session.clone();
                Checkbox::new("main-diagnostics")
                    .checked(s.prefs.diag_enabled)
                    .label("显示主窗诊断浮层（立即生效，Ctrl+Shift+D）")
                    .on_change(move |on, _, cx| {
                        session.update(cx, |s, cx| {
                            s.set_diag(*on, cx);
                            cx.notify();
                        })
                    })
            })
            .child(
                        h_flex()
                            .h(px(36.))
                            .items_center()
                            .gap_2()
                            .child(settings_note(
                                "复制的内容与上方一致，只是隐去用户名与邮箱；数字和路径保持原样。",
                                tokens,
                            ))
                            .child(
                                Button::new("copy-diagnostics")
                                    .ghost()
                                    .w(px(120.))
                                    .flex_shrink_0()
                                    .label("复制诊断")
                                    .accessibility_label("复制已脱敏的诊断信息")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.copy_diagnostics(cx);
                                    })),
                            )
            )
            .child(section_title("原始 Status 响应（每 2 秒更新）", tokens))
            .child(
                div()
                    .id("raw-status-viewport").h(px(192.)).flex_shrink_0().overflow_y_scrollbar()
                    .text_xs()
                    .p_3()
                    .bg(tokens.surface)
                    .child(s.status_raw.clone()),
            )
            .child(self.render_storage_fields(tokens))
    }

    /// Support report: same facts as the rows above, with usernames and e-mail
    /// addresses removed. The clipboard is the delivery channel so nothing is
    /// written to disk without the user asking for a file.
    fn copy_diagnostics(&mut self, cx: &mut Context<Self>) {
        let s = self.session.read(cx);
        let mode = if s.mock {
            "模拟数据"
        } else if s.official_product {
            "已安装的索引服务"
        } else if s.live {
            "连接到已有服务"
        } else {
            "独立配置"
        };
        let mut header = crate::diagnostics_bundle::environment(env!("CARGO_PKG_VERSION"));
        header.push(format!("运行方式: {mode}"));
        header.push(format!("连接: {}", if s.connected { "已连接" } else { "未连接" }));
        header.push(format!("连接地址: {}", s.endpoint));
        header.push(format!("管道名称: {}", self.settings.pipe_name));
        header.push(format!(
            "设置文件: {}",
            prefs::settings_path().display()
        ));
        let mut index_lines = vec![
            crate::status_logic::diagnostic_index_line(&s.index_status.summary),
            format!("最近状态: {}", s.status_checked),
            format!("错误: {}", s.last_error.as_deref().unwrap_or("无")),
        ];
        if let Some(detail) = s.index_status.detail.clone() {
            index_lines.push(detail);
        }
        if let Some(wal) = s.engine_footer.wal_label() {
            index_lines.push(format!("WAL: {wal}"));
        }
        let raw = s
            .status_raw
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let now_unix_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let generated_at = crate::search_logic::format_time(now_unix_secs);
        let doc = crate::diagnostics_bundle::render(
            &generated_at,
            &header,
            &[
                crate::diagnostics_bundle::Section { title: "索引状态".into(), lines: index_lines },
                crate::diagnostics_bundle::Section { title: "原始状态".into(), lines: raw },
            ],
        );
        let home = crate::diagnostics_bundle::home_dir();
        let doc = crate::diagnostics_bundle::redact(&doc, home.as_deref());
        cx.write_to_clipboard(ClipboardItem::new_string(doc));
        self.hint = "已复制诊断信息（已隐去用户名与邮箱）".into();
        cx.notify();
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
                    .disabled(self.service_busy)
                    .on_click(cx.listener(|this, _, window, cx| this.restore_defaults(window, cx))),
            )
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
