use gpui_kit::*;

use crate::cli::LaunchOptions;
use crate::ipc::IpcClient;
use crate::prefs::{self, LocalPrefs};
use crate::settings_logic::{EffectiveTheme, FindxGuiSettings, UiThemePref};
use crate::status_logic::{self, IndexStatusView};
use crate::theme;

pub struct AppSession {
    pub ipc: IpcClient,
    pub pipe_name: String,
    pub endpoint: String,
    pub mock: bool,
    pub live: bool,
    pub preview_file: Option<String>,
    pub settings: FindxGuiSettings,
    pub prefs: LocalPrefs,
    pub index_status: IndexStatusView,
    pub engine_footer: crate::footer_status::EngineFooter,
    pub connected: bool,
    pub status_raw: String,
    pub status_checked: String,
    pub last_error: Option<String>,
    pub available_roots: Vec<String>,
    pub system_dark: bool,
    pub preview_theme: Option<(theme::ThemeStyle, UiThemePref)>,
    pub settings_window: Option<AnyWindowHandle>,
}

impl AppSession {
    pub fn load(opts: &LaunchOptions, system_dark: bool) -> Self {
        let mut prefs = prefs::load_prefs();
        if opts.diag {
            prefs.diag_enabled = true;
        }
        let settings = prefs::load_settings(
            "findx2-native-dev",
            crate::isolation::isolated_index_path()
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
            preview_file: opts.preview_file.clone(),
            settings,
            prefs,
            index_status: initial_status,
            engine_footer: Default::default(),
            connected: false,
            status_raw: "尚未收到 Status 响应".into(),
            status_checked: "尚未查询".into(),
            last_error: None,
            available_roots: Vec::new(),
            system_dark,
            preview_theme: None,
            settings_window: None,
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
    }

    pub fn set_theme_pref(&mut self, pref: UiThemePref, cx: &mut App) {
        self.prefs.theme = pref;
        self.apply_theme(cx);
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
        crate::isolation::window_title(self.live)
    }
}

pub fn persist_prefs(prefs: LocalPrefs, cx: &mut App) {
    cx.background_spawn(async move {
        let _ = prefs::save_prefs(&prefs);
    })
    .detach();
}

pub fn persist_settings(settings: FindxGuiSettings, cx: &mut App) {
    cx.background_spawn(async move {
        let _ = prefs::save_settings(&settings);
    })
    .detach();
}
