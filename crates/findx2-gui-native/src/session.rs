use gpui_kit::*;

use crate::cli::LaunchOptions;
use crate::ipc::IpcClient;
use crate::prefs::{self, LocalPrefs};
use crate::settings_logic::{EffectiveTheme, FindxGuiSettings, UiThemePref};
use crate::status_logic::{self, IndexStatusView};
use crate::theme;
use crate::update_logic::OfficialUpdateState;

/// 全文内容索引状态快照（2s 状态轮询时从 StatusResult.content_* 刷新，
/// 设置页全文 tab 直接读这里，不再做第二次查询）。
#[derive(Debug, Clone, Default)]
pub struct ContentIndexStatus {
    pub state: String,
    pub detail: String,
    pub docs: u64,
    pub bytes: u64,
    pub entry_total: u64,
    pub scanned: u64,
    pub extracted: u64,
    pub skipped: u64,
    pub failed: u64,
    pub error: Option<String>,
}

pub struct AppSession {
    pub ipc: IpcClient,
    pub pipe_name: String,
    pub endpoint: String,
    pub mock: bool,
    pub live: bool,
    pub official_product: bool,
    pub preview_file: Option<String>,
    pub settings: FindxGuiSettings,
    pub prefs: LocalPrefs,
    pub index_status: IndexStatusView,
    pub content_index: ContentIndexStatus,
    pub engine_footer: crate::footer_status::EngineFooter,
    pub connected: bool,
    pub status_raw: String,
    pub status_checked: String,
    pub last_error: Option<String>,
    pub available_roots: Vec<String>,
    pub system_dark: bool,
    pub preview_theme: Option<(theme::ThemeStyle, UiThemePref)>,
    pub settings_window: Option<AnyWindowHandle>,
    pub update_state: OfficialUpdateState,
}

impl AppSession {
    pub fn load(opts: &LaunchOptions, system_dark: bool) -> Self {
        let mut prefs = prefs::load_prefs();
        if opts.diag {
            prefs.diag_enabled = true;
        }
        let settings = prefs::load_settings(
            if opts.official_product && cfg!(unix) {
                "findx2"
            } else {
                "findx2-native-dev"
            },
            (if opts.official_product && cfg!(unix) {
                crate::isolation::official_data_dir().join("index.bin")
            } else {
                crate::isolation::isolated_index_path()
            })
            .to_string_lossy()
            .into_owned(),
        );
        let initial_status = if opts.live {
            status_logic::disconnected_status("尚未查询生产服务")
        } else {
            status_logic::disconnected_status("尚未查询隔离后端")
        };
        Self {
            ipc: IpcClient::start(),
            pipe_name: opts.pipe_name.clone(),
            endpoint: opts.endpoint.clone(),
            mock: opts.mock,
            live: opts.live,
            official_product: opts.official_product,
            preview_file: opts.preview_file.clone(),
            settings,
            prefs,
            index_status: initial_status,
        content_index: ContentIndexStatus::default(),
            engine_footer: Default::default(),
            connected: false,
            status_raw: "尚未收到 Status 响应".into(),
            status_checked: "尚未查询".into(),
            last_error: None,
            available_roots: Vec::new(),
            system_dark,
            preview_theme: None,
            settings_window: None,
            update_state: OfficialUpdateState::default(),
        }
    }

    pub fn effective_theme(&self) -> EffectiveTheme {
        self.preview_theme
            .map(|(_, pref)| pref)
            .unwrap_or(self.prefs.theme)
            .resolve(self.system_dark)
    }

    pub fn apply_theme(&self, cx: &mut App) {
        let style = self
            .preview_theme
            .map(|(style, _)| style)
            .unwrap_or(self.prefs.theme_style);
        theme::apply_findx_style(style, self.effective_theme(), cx);
        // Backdrop materials need translucent chrome to show through; solid
        // keeps the computed palette untouched.
        theme::apply_material_translucency(
            self.prefs.window_material,
            self.prefs.material_opacity,
            cx,
        );
        theme::apply_content_translucency(
            self.prefs.content_translucent,
            self.prefs.window_material,
            self.prefs.material_opacity,
            cx,
        );
    }

    pub fn set_theme_pref(&mut self, pref: UiThemePref, cx: &mut App) {
        self.prefs.theme = pref;
        self.apply_theme(cx);
        persist_prefs(self.prefs.clone(), cx);
    }

    pub fn set_theme_style(&mut self, style: theme::ThemeStyle, cx: &mut App) {
        self.prefs.theme_style = style;
        self.apply_theme(cx);
        persist_prefs(self.prefs.clone(), cx);
    }

    /// Apply shortcut overrides to the live keymap and persist them.
    pub fn set_key_bindings(
        &mut self,
        overrides: std::collections::BTreeMap<String, String>,
        cx: &mut App,
    ) {
        self.prefs.key_bindings = overrides.clone();
        crate::keybindings::apply(&overrides, cx);
        persist_prefs(self.prefs.clone(), cx);
    }

    pub fn set_diag(&mut self, on: bool, cx: &mut App) {
        self.prefs.diag_enabled = on;
        persist_prefs(self.prefs.clone(), cx);
    }

    pub fn set_panels(&mut self, sidebar: Option<bool>, preview: Option<bool>, cx: &mut App) {
        if let Some(v) = sidebar {
            self.prefs.sidebar_open = v;
        }
        if let Some(v) = preview {
            self.prefs.preview_open = v;
        }
        persist_prefs(self.prefs.clone(), cx);
    }

    pub fn window_title(&self) -> &'static str {
        crate::isolation::window_title(self.official_product, self.live)
    }
}

pub fn persist_prefs(prefs: LocalPrefs, cx: &mut App) {
    let _=cx; // Queue admission occurs in caller order, not background scheduling order.
    prefs::enqueue_prefs(prefs);
}

pub fn persist_settings(settings: FindxGuiSettings, cx: &mut App) {
    cx.background_spawn(async move {
        let _ = prefs::save_settings(&settings);
    })
    .detach();
}
