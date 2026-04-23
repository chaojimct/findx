use serde::{Deserialize, Serialize};
use std::{
    ffi::c_void,
    io::ErrorKind,
    mem::size_of,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    plugin::TauriPlugin,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    App, AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, PhysicalSize, Position,
    Runtime, Size, WebviewWindow, Window, WindowEvent,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use tauri_plugin_window_state::{
    AppHandleExt as WindowStateAppHandleExt, StateFlags, WindowExt as WindowStateWindowExt,
};
#[cfg(windows)]
use windows::Win32::{
    Foundation::COLORREF,
    Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR},
    System::{ProcessStatus::K32EmptyWorkingSet, Threading::GetCurrentProcess},
};

const MAIN_WINDOW_LABEL: &str = "main";
/// 独立设置对话框（与 tauri.conf `label` 一致）。关闭时必须 hide 而非 destroy，否则无法再次 show。
const SETTINGS_WINDOW_LABEL: &str = "settings";
const TRAY_ICON_ID: &str = "findx2-tray";
const TRAY_MENU_SEARCH_ID: &str = "findx-search";
const TRAY_MENU_SETTINGS_ID: &str = "findx-settings";
/// 根据当前管道是否可连，在「启动索引服务」与「停止索引服务」之间切换文案（同一条目 id）。
const TRAY_MENU_SERVICE_ID: &str = "findx-service-toggle";
const TRAY_MENU_HIDE_ID: &str = "hide-window";
const TRAY_MENU_QUIT_ID: &str = "quit-app";
const DESKTOP_SETTINGS_FILE_NAME: &str = "desktop-settings.json";
const LEGACY_APP_SHORTCUT: &str = "Alt+Space";
const DEFAULT_APP_SHORTCUT: &str = "Alt+Shift+S";
// 与 findx SearchWindow 主区域比例一致（Tauri 窗体含标题栏）。
const FULL_WINDOW_WIDTH: f64 = 960.0;
const FULL_WINDOW_HEIGHT: f64 = 620.0;
const FULL_WINDOW_MIN_WIDTH: f64 = 720.0;
const FULL_WINDOW_MIN_HEIGHT: f64 = 480.0;
const QUICK_WINDOW_WIDTH: f64 = 1140.0;
const QUICK_WINDOW_HEIGHT: f64 = 730.0;
const QUICK_WINDOW_MIN_WIDTH: f64 = 960.0;
const QUICK_WINDOW_MIN_HEIGHT: f64 = 620.0;
pub const WINDOW_MODE_EVENT: &str = "findx2://window-mode";

static WINDOW_STATE_SAVE_ENABLED: OnceLock<Arc<AtomicBool>> = OnceLock::new();

#[derive(Clone, Copy)]
enum WindowMode {
    Full,
    Quick,
}

impl WindowMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Quick => "quick",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum PersistedWindowMode {
    Full,
    Quick,
}

impl From<WindowMode> for PersistedWindowMode {
    fn from(value: WindowMode) -> Self {
        match value {
            WindowMode::Full => Self::Full,
            WindowMode::Quick => Self::Quick,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct DesktopSettings {
    pub background_mode_enabled: bool,
    pub shortcut_enabled: bool,
    pub shortcut: String,
    pub remember_window_bounds: bool,
}

impl Default for DesktopSettings {
    fn default() -> Self {
        Self {
            background_mode_enabled: true,
            shortcut_enabled: true,
            shortcut: DEFAULT_APP_SHORTCUT.to_string(),
            remember_window_bounds: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistedDesktopState {
    /// 桌面状态文件格式；<1 时丢弃 `last_full_window_layout`，避免旧「物理像素」存档被误读为逻辑尺寸。
    #[serde(default)]
    desktop_state_version: u32,
    #[serde(default)]
    settings: DesktopSettings,
    #[serde(default)]
    last_full_window_layout: Option<FullWindowLayoutSnapshot>,
    #[serde(default)]
    last_window_mode: Option<PersistedWindowMode>,
}

impl Default for PersistedDesktopState {
    fn default() -> Self {
        Self {
            desktop_state_version: 1,
            settings: DesktopSettings::default(),
            last_full_window_layout: None,
            last_window_mode: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct FullWindowLayoutSnapshot {
    x: i32,
    y: i32,
    /// 与 `layout_version` 联动：v1 为逻辑像素（与显示器 DPI 无关的「视觉尺寸」），v0 为历史 inner 物理像素。
    width: f64,
    height: f64,
    maximized: bool,
    /// 0：旧版（`width`/`height` 按物理像素 `set_size`）；1：按逻辑像素恢复，修复混合 DPI 多显示器下「尺寸减半」。
    #[serde(default)]
    layout_version: u32,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WindowModePayload {
    mode: &'static str,
}

pub struct DesktopRuntimeState {
    window_mode: Mutex<WindowMode>,
    settings: Mutex<DesktopSettings>,
    last_full_window_layout: Mutex<Option<FullWindowLayoutSnapshot>>,
    active_shortcut: Mutex<Option<String>>,
    window_state_save_enabled: Arc<AtomicBool>,
    pending_full_restore_from_quick: AtomicBool,
    is_quitting: AtomicBool,
}

impl Default for DesktopRuntimeState {
    fn default() -> Self {
        Self {
            window_mode: Mutex::new(WindowMode::Full),
            settings: Mutex::new(DesktopSettings::default()),
            last_full_window_layout: Mutex::new(None),
            active_shortcut: Mutex::new(None),
            window_state_save_enabled: shared_window_state_save_enabled(),
            pending_full_restore_from_quick: AtomicBool::new(false),
            is_quitting: AtomicBool::new(false),
        }
    }
}

fn shared_window_state_save_enabled() -> Arc<AtomicBool> {
    WINDOW_STATE_SAVE_ENABLED
        .get_or_init(|| Arc::new(AtomicBool::new(true)))
        .clone()
}

fn full_window_state_flags() -> StateFlags {
    // 不保存 SIZE：插件按物理像素存宽高，在 100% 扩展屏与 200% 主屏之间切换会「尺寸减半」。
    // 完整尺寸由 `FullWindowLayoutSnapshot`（逻辑像素）+ desktop-settings.json 负责。
    StateFlags::POSITION | StateFlags::MAXIMIZED
}

pub fn window_state_plugin<R: Runtime>() -> TauriPlugin<R> {
    let save_enabled = shared_window_state_save_enabled();
    tauri_plugin_window_state::Builder::default()
        .skip_initial_state(MAIN_WINDOW_LABEL)
        .with_state_flags(full_window_state_flags())
        .with_filter(move |label| {
            label == MAIN_WINDOW_LABEL && save_enabled.load(Ordering::SeqCst)
        })
        .build()
}

fn desktop_state<R: Runtime>(app: &AppHandle<R>) -> &DesktopRuntimeState {
    app.state::<DesktopRuntimeState>().inner()
}

fn main_window<R: Runtime>(app: &AppHandle<R>) -> Result<WebviewWindow<R>, String> {
    app.get_webview_window(MAIN_WINDOW_LABEL)
        .ok_or_else(|| "Main window is not available".to_string())
}

fn emit_window_mode<R: Runtime>(app: &AppHandle<R>, mode: WindowMode) -> Result<(), String> {
    app.emit(
        WINDOW_MODE_EVENT,
        WindowModePayload {
            mode: mode.as_str(),
        },
    )
    .map_err(|err| err.to_string())
}

fn normalize_shortcut(shortcut: &str) -> Result<String, String> {
    let trimmed = shortcut.trim();
    if trimmed.is_empty() {
        return Err("Shortcut cannot be empty.".to_string());
    }

    let normalized_shortcut = trimmed
        .parse::<Shortcut>()
        .map(|shortcut| shortcut.to_string())
        .map_err(|err| format!("Invalid shortcut '{trimmed}': {err}"))?;

    if normalized_shortcut.eq_ignore_ascii_case(LEGACY_APP_SHORTCUT) {
        return Err(
            "Alt+Space conflicts with the Windows system menu. Choose another shortcut."
                .to_string(),
        );
    }

    Ok(normalized_shortcut)
}

fn sanitized_shortcut_or_default(shortcut: &str) -> String {
    let trimmed = shortcut.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case(LEGACY_APP_SHORTCUT) {
        return DEFAULT_APP_SHORTCUT.to_string();
    }

    normalize_shortcut(trimmed).unwrap_or_else(|_| DEFAULT_APP_SHORTCUT.to_string())
}

fn sanitize_desktop_settings(settings: DesktopSettings) -> DesktopSettings {
    if !settings.background_mode_enabled
        && !settings.shortcut_enabled
        && settings
            .shortcut
            .trim()
            .eq_ignore_ascii_case(LEGACY_APP_SHORTCUT)
    {
        return DesktopSettings::default();
    }

    DesktopSettings {
        background_mode_enabled: settings.background_mode_enabled,
        shortcut_enabled: settings.shortcut_enabled,
        shortcut: sanitized_shortcut_or_default(&settings.shortcut),
        remember_window_bounds: settings.remember_window_bounds,
    }
}

fn sanitize_persisted_desktop_state(mut state: PersistedDesktopState) -> PersistedDesktopState {
    if state.desktop_state_version < 1 {
        state.desktop_state_version = 1;
        state.last_full_window_layout = None;
    }
    PersistedDesktopState {
        desktop_state_version: state.desktop_state_version,
        settings: sanitize_desktop_settings(state.settings),
        last_full_window_layout: state
            .last_full_window_layout
            .and_then(sanitize_full_window_layout_snapshot),
        last_window_mode: state.last_window_mode,
    }
}

fn sanitize_full_window_layout_snapshot(
    snapshot: FullWindowLayoutSnapshot,
) -> Option<FullWindowLayoutSnapshot> {
    if snapshot.width <= 0.0 || snapshot.height <= 0.0 || !snapshot.width.is_finite() || !snapshot.height.is_finite()
    {
        return None;
    }

    Some(FullWindowLayoutSnapshot {
        width: snapshot.width.max(FULL_WINDOW_MIN_WIDTH),
        height: snapshot.height.max(FULL_WINDOW_MIN_HEIGHT),
        ..snapshot
    })
}

fn desktop_settings_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_config_dir()
        .map_err(|err| err.to_string())?
        .join(DESKTOP_SETTINGS_FILE_NAME))
}

fn load_desktop_state<R: Runtime>(app: &AppHandle<R>) -> PersistedDesktopState {
    let path = match desktop_settings_path(app) {
        Ok(path) => path,
        Err(err) => {
            log_desktop_error("resolve the desktop settings file", &err);
            return PersistedDesktopState::default();
        }
    };

    match std::fs::read_to_string(&path) {
        Ok(contents) => match serde_json::from_str::<PersistedDesktopState>(&contents) {
            Ok(state) => sanitize_persisted_desktop_state(state),
            Err(state_err) => match serde_json::from_str::<DesktopSettings>(&contents) {
                Ok(settings) => PersistedDesktopState {
                    desktop_state_version: 1,
                    settings: sanitize_desktop_settings(settings),
                    last_full_window_layout: None,
                    last_window_mode: None,
                },
                Err(_) => {
                    log_desktop_error("parse the desktop settings file", &state_err.to_string());
                    PersistedDesktopState::default()
                }
            },
        },
        Err(err) if err.kind() == ErrorKind::NotFound => PersistedDesktopState::default(),
        Err(err) => {
            log_desktop_error("read the desktop settings file", &err.to_string());
            PersistedDesktopState::default()
        }
    }
}

fn save_desktop_state_file<R: Runtime>(
    app: &AppHandle<R>,
    state: &PersistedDesktopState,
) -> Result<(), String> {
    let path = desktop_settings_path(app)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }

    let payload = serde_json::to_vec_pretty(state).map_err(|err| err.to_string())?;
    std::fs::write(path, payload).map_err(|err| err.to_string())
}

fn current_desktop_settings<R: Runtime>(app: &AppHandle<R>) -> Result<DesktopSettings, String> {
    desktop_state(app)
        .settings
        .lock()
        .map_err(|_| "Failed to lock desktop settings".to_string())
        .map(|settings| settings.clone())
}

fn set_desktop_settings<R: Runtime>(
    app: &AppHandle<R>,
    settings: &DesktopSettings,
) -> Result<(), String> {
    *desktop_state(app)
        .settings
        .lock()
        .map_err(|_| "Failed to lock desktop settings".to_string())? = settings.clone();
    Ok(())
}

fn set_window_state_save_enabled<R: Runtime>(app: &AppHandle<R>, enabled: bool) {
    desktop_state(app)
        .window_state_save_enabled
        .store(enabled, Ordering::SeqCst);
}

fn set_pending_full_restore_from_quick<R: Runtime>(app: &AppHandle<R>, pending: bool) {
    desktop_state(app)
        .pending_full_restore_from_quick
        .store(pending, Ordering::SeqCst);
}

fn has_pending_full_restore_from_quick<R: Runtime>(app: &AppHandle<R>) -> bool {
    desktop_state(app)
        .pending_full_restore_from_quick
        .load(Ordering::SeqCst)
}

fn sync_window_state_save_behavior<R: Runtime>(app: &AppHandle<R>) {
    set_window_state_save_enabled(
        app,
        remember_window_bounds_enabled(app) && matches!(current_window_mode(app), WindowMode::Full),
    );
}

fn is_background_mode_enabled<R: Runtime>(app: &AppHandle<R>) -> bool {
    desktop_state(app)
        .settings
        .lock()
        .map(|settings| settings.background_mode_enabled)
        .unwrap_or(false)
}

fn remember_window_bounds_enabled<R: Runtime>(app: &AppHandle<R>) -> bool {
    desktop_state(app)
        .settings
        .lock()
        .map(|settings| settings.remember_window_bounds)
        .unwrap_or(true)
}

fn current_window_mode<R: Runtime>(app: &AppHandle<R>) -> WindowMode {
    desktop_state(app)
        .window_mode
        .lock()
        .map(|mode| *mode)
        .unwrap_or(WindowMode::Full)
}

fn set_window_mode<R: Runtime>(app: &AppHandle<R>, mode: WindowMode) -> Result<(), String> {
    *desktop_state(app)
        .window_mode
        .lock()
        .map_err(|_| "Failed to lock the window mode".to_string())? = mode;
    Ok(())
}

fn set_window_mode_and_sync<R: Runtime>(app: &AppHandle<R>, mode: WindowMode) -> Result<(), String> {
    set_window_mode(app, mode)?;
    sync_window_state_save_behavior(app);
    Ok(())
}

fn persist_desktop_state<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let last_full_window_layout = desktop_state(app)
        .last_full_window_layout
        .lock()
        .map_err(|_| "Failed to lock the live full window layout".to_string())?
        .clone();
    save_desktop_state_file(
        app,
        &PersistedDesktopState {
            desktop_state_version: 1,
            settings: current_desktop_settings(app)?,
            last_full_window_layout,
            last_window_mode: Some(PersistedWindowMode::from(current_window_mode(app))),
        },
    )
}

fn snapshot_from_window<R: Runtime>(
    window: &WebviewWindow<R>,
) -> Result<FullWindowLayoutSnapshot, String> {
    let position = window.outer_position().map_err(|err| err.to_string())?;
    let size = window.inner_size().map_err(|err| err.to_string())?;
    let scale = window.scale_factor().map_err(|err| err.to_string())?;
    if !scale.is_finite() || scale <= 0.0 {
        return Err("Invalid window scale factor".to_string());
    }

    Ok(FullWindowLayoutSnapshot {
        x: position.x,
        y: position.y,
        width: size.width as f64 / scale,
        height: size.height as f64 / scale,
        maximized: window.is_maximized().map_err(|err| err.to_string())?,
        layout_version: 1,
    })
}

fn apply_full_window_layout_snapshot<R: Runtime>(
    window: &WebviewWindow<R>,
    snapshot: &FullWindowLayoutSnapshot,
) -> Result<(), String> {
    window
        .set_min_size(Some(LogicalSize::new(
            FULL_WINDOW_MIN_WIDTH,
            FULL_WINDOW_MIN_HEIGHT,
        )))
        .map_err(|err| err.to_string())?;
    if window.is_maximized().map_err(|err| err.to_string())? {
        window.unmaximize().map_err(|err| err.to_string())?;
    }
    if snapshot.layout_version >= 1 {
        window
            .set_size(Size::Logical(LogicalSize::new(snapshot.width, snapshot.height)))
            .map_err(|err| err.to_string())?;
    } else {
        let w = snapshot
            .width
            .round()
            .clamp(1.0, u32::MAX as f64) as u32;
        let h = snapshot
            .height
            .round()
            .clamp(1.0, u32::MAX as f64) as u32;
        window
            .set_size(Size::Physical(PhysicalSize::new(w, h)))
            .map_err(|err| err.to_string())?;
    }
    window
        .set_position(Position::Physical(PhysicalPosition::new(
            snapshot.x,
            snapshot.y,
        )))
        .map_err(|err| err.to_string())?;
    if snapshot.maximized {
        window.maximize().map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn snapshot_looks_like_quick_layout(snapshot: &FullWindowLayoutSnapshot) -> bool {
    if snapshot.maximized {
        return false;
    }

    let width_scale = snapshot.width as f64 / QUICK_WINDOW_WIDTH;
    let height_scale = snapshot.height as f64 / QUICK_WINDOW_HEIGHT;
    width_scale >= 0.9
        && height_scale >= 0.9
        && (width_scale - height_scale).abs() <= 0.03
}

fn persist_full_window_state_snapshot<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    if matches!(current_window_mode(app), WindowMode::Quick) {
        // Quick mode reuses the same native window, but it should never overwrite the
        // saved full-workspace bounds on hide/quit.
        set_window_state_save_enabled(app, false);
        return persist_desktop_state(app);
    }

    if remember_window_bounds_enabled(app) && matches!(current_window_mode(app), WindowMode::Full) {
        let window = main_window(app)?;
        capture_live_full_window_layout(app, &window)?;
        set_window_state_save_enabled(app, true);
        app.save_window_state(full_window_state_flags())
            .map_err(|err| err.to_string())?;
        sync_window_state_save_behavior(app);
    }
    persist_desktop_state(app)
}

fn capture_live_full_window_layout<R: Runtime>(
    app: &AppHandle<R>,
    window: &WebviewWindow<R>,
) -> Result<(), String> {
    let snapshot = snapshot_from_window(window)?;

    *desktop_state(app)
        .last_full_window_layout
        .lock()
        .map_err(|_| "Failed to lock the live full window layout".to_string())? = Some(snapshot);
    Ok(())
}

fn restore_live_full_window_layout<R: Runtime>(
    app: &AppHandle<R>,
    window: &WebviewWindow<R>,
) -> Result<bool, String> {
    if !has_pending_full_restore_from_quick(app) {
        return Ok(false);
    }

    let snapshot = desktop_state(app)
        .last_full_window_layout
        .lock()
        .map_err(|_| "Failed to lock the live full window layout".to_string())?
        .clone();

    let Some(snapshot) = snapshot else {
        set_pending_full_restore_from_quick(app, false);
        return Ok(false);
    };

    apply_full_window_layout_snapshot(window, &snapshot)?;

    set_pending_full_restore_from_quick(app, false);
    Ok(true)
}

fn apply_default_main_window_layout<R: Runtime>(
    window: &WebviewWindow<R>,
    mode: WindowMode,
) -> Result<(), String> {
    let (width, height, min_width, min_height) = match mode {
        WindowMode::Full => (
            FULL_WINDOW_WIDTH,
            FULL_WINDOW_HEIGHT,
            FULL_WINDOW_MIN_WIDTH,
            FULL_WINDOW_MIN_HEIGHT,
        ),
        WindowMode::Quick => (
            QUICK_WINDOW_WIDTH,
            QUICK_WINDOW_HEIGHT,
            QUICK_WINDOW_MIN_WIDTH,
            QUICK_WINDOW_MIN_HEIGHT,
        ),
    };

    window
        .set_min_size(Some(LogicalSize::new(min_width, min_height)))
        .map_err(|err| err.to_string())?;
    if window.is_maximized().map_err(|err| err.to_string())? {
        window.unmaximize().map_err(|err| err.to_string())?;
    }
    window
        .set_size(LogicalSize::new(width, height))
        .map_err(|err| err.to_string())?;
    window.center().map_err(|err| err.to_string())
}

/// 校验窗口当前外框是否与任一可见监视器有足够交集；若几乎完全脱屏（包括位置落在已断开的扩展屏旧坐标），返回 false。
/// 这是 `tauri-plugin-window-state` 在多显示器 / DPI 变化场景下的兜底——插件保存的是物理像素，
/// 当显示器拓扑变化时直接 `restore_state` 会把窗口放到不可见区域。
fn window_visible_on_any_monitor<R: Runtime>(window: &WebviewWindow<R>) -> bool {
    let Ok(pos) = window.outer_position() else {
        return false;
    };
    let Ok(size) = window.outer_size() else {
        return false;
    };
    let Ok(monitors) = window.available_monitors() else {
        return false;
    };
    let win_left = pos.x;
    let win_top = pos.y;
    let win_right = pos.x + size.width as i32;
    let win_bottom = pos.y + size.height as i32;
    // 至少 100x100 的可见交集才算"在屏幕上"。
    const MIN_VISIBLE: i32 = 100;
    for m in monitors {
        let mp = m.position();
        let ms = m.size();
        let m_left = mp.x;
        let m_top = mp.y;
        let m_right = mp.x + ms.width as i32;
        let m_bottom = mp.y + ms.height as i32;
        let ix = (win_right.min(m_right) - win_left.max(m_left)).max(0);
        let iy = (win_bottom.min(m_bottom) - win_top.max(m_top)).max(0);
        if ix >= MIN_VISIBLE && iy >= MIN_VISIBLE {
            return true;
        }
    }
    false
}

fn restore_saved_full_window_layout<R: Runtime>(
    app: &AppHandle<R>,
    window: &WebviewWindow<R>,
) -> Result<bool, String> {
    if !remember_window_bounds_enabled(app) {
        return Ok(false);
    }

    let saved_snapshot = desktop_state(app)
        .last_full_window_layout
        .lock()
        .map_err(|_| "Failed to lock the live full window layout".to_string())?
        .clone();

    if let Some(snapshot) = saved_snapshot {
        apply_full_window_layout_snapshot(window, &snapshot)?;
        return Ok(true);
    }

    let loaded_state = load_desktop_state(app);
    if matches!(loaded_state.last_window_mode, Some(PersistedWindowMode::Quick)) {
        return Ok(false);
    }

    window
        .set_min_size(Some(LogicalSize::new(
            FULL_WINDOW_MIN_WIDTH,
            FULL_WINDOW_MIN_HEIGHT,
        )))
        .map_err(|err| err.to_string())?;

    match window.restore_state(full_window_state_flags()) {
        Ok(()) => {
            // 兜底：保存的位置可能在已断开 / 当前不可见的扩展屏上。
            // 若窗口外框与所有当前监视器交集都不足，直接返回 false → 上层会调用
            // `apply_default_main_window_layout` 居中到主屏，避免"窗口隐形启动"。
            if !window_visible_on_any_monitor(window) {
                return Ok(false);
            }

            let snapshot = snapshot_from_window(window)?;
            if loaded_state.last_window_mode.is_none() && snapshot_looks_like_quick_layout(&snapshot) {
                return Ok(false);
            }

            *desktop_state(app)
                .last_full_window_layout
                .lock()
                .map_err(|_| "Failed to lock the live full window layout".to_string())? =
                Some(snapshot);

            Ok(true)
        }
        Err(_) => Ok(false),
    }
}

fn apply_main_window_layout<R: Runtime>(
    app: &AppHandle<R>,
    window: &WebviewWindow<R>,
    mode: WindowMode,
) -> Result<(), String> {
    if matches!(mode, WindowMode::Full) && restore_live_full_window_layout(app, window)? {
        return Ok(());
    }

    if matches!(mode, WindowMode::Full) && restore_saved_full_window_layout(app, window)? {
        return Ok(());
    }

    apply_default_main_window_layout(window, mode)
}

fn show_window_in_mode<R: Runtime>(app: &AppHandle<R>, mode: WindowMode) -> Result<(), String> {
    let window = main_window(app)?;
    let is_visible = window.is_visible().map_err(|err| err.to_string())?;
    let was_minimized = window.is_minimized().map_err(|err| err.to_string())?;
    let previous_mode = current_window_mode(app);

    if matches!(previous_mode, WindowMode::Full) && matches!(mode, WindowMode::Quick) {
        capture_live_full_window_layout(app, &window)?;
        set_pending_full_restore_from_quick(app, true);
        persist_full_window_state_snapshot(app)?;
    }

    if matches!(mode, WindowMode::Quick) {
        set_window_mode_and_sync(app, mode)?;
    } else {
        set_window_state_save_enabled(app, false);
        set_window_mode(app, mode)?;
    }

    if is_visible && !was_minimized {
        window.hide().map_err(|err| err.to_string())?;
    }

    if was_minimized {
        window.unminimize().map_err(|err| err.to_string())?;
    }

    emit_window_mode(app, mode)?;
    apply_main_window_layout(app, &window, mode)?;

    window
        .set_always_on_top(false)
        .map_err(|err| err.to_string())?;
    window.show().map_err(|err| err.to_string())?;

    if was_minimized {
        apply_main_window_layout(app, &window, mode)?;
    }

    sync_window_state_save_behavior(app);
    window.set_focus().map_err(|err| err.to_string())
}

fn show_main_window<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let window = main_window(app)?;
    apply_main_window_layout(app, &window, current_window_mode(app))?;

    if window.is_minimized().map_err(|err| err.to_string())? {
        window.unminimize().map_err(|err| err.to_string())?;
    }

    window
        .set_always_on_top(false)
        .map_err(|err| err.to_string())?;
    window.show().map_err(|err| err.to_string())?;
    window.set_focus().map_err(|err| err.to_string())
}

fn hide_main_window<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    // 原生系统预览（Office/doc 等）使用独立顶层 popup HWND，叠在 WebView 之上；
    // 仅 hide 主窗时前端不一定运行 cleanup，必须在 Rust 侧先卸掉预览，否则会「窗没了预览框还在」。
    #[cfg(windows)]
    {
        let _ = crate::win_preview::unload_preview();
    }
    let window = main_window(app)?;
    persist_full_window_state_snapshot(app)?;
    if matches!(current_window_mode(app), WindowMode::Full) {
        set_pending_full_restore_from_quick(app, false);
    }
    window.hide().map_err(|err| err.to_string())?;
    trim_process_working_set();
    Ok(())
}

#[cfg(windows)]
fn trim_process_working_set() {
    unsafe {
        let _ = K32EmptyWorkingSet(GetCurrentProcess());
    }
}

#[cfg(not(windows))]
fn trim_process_working_set() {}

fn toggle_main_window<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let window = main_window(app)?;
    let is_visible = window.is_visible().map_err(|err| err.to_string())?;
    let is_minimized = window.is_minimized().map_err(|err| err.to_string())?;

    if is_visible && !is_minimized {
        hide_main_window(app)
    } else {
        show_main_window(app)
    }
}

fn open_full_window<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    show_window_in_mode(app, WindowMode::Full)
}

/// 显示独立「设置」Webview 窗口（tauri.conf 中 label=`settings`），并通知其重新拉取表单。
fn show_settings_window_impl<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let w = app
        .get_webview_window(SETTINGS_WINDOW_LABEL)
        .ok_or_else(|| "设置窗口未初始化".to_string())?;
    w.show().map_err(|e| e.to_string())?;
    w.unminimize().map_err(|e| e.to_string())?;
    w.set_focus().map_err(|e| e.to_string())?;
    let _ = app.emit("findx2-settings-reload", ());
    Ok(())
}

fn open_quick_window<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    // FindX2 前端统一为 findx 风格主窗口，不再使用 quick 尺寸。
    open_full_window(app)
}

fn toggle_quick_window<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    // FindX2：与托盘一致，全局快捷键仅切换主搜索窗口（不再区分 quick/full 布局）。
    toggle_main_window(app)
}

fn apply_native_window_theme<R: Runtime>(
    app: &AppHandle<R>,
    theme_mode: &str,
    background_color: Option<&str>,
    title_bar_color: Option<&str>,
    title_bar_text_color: Option<&str>,
) -> Result<(), String> {
    let native_theme = match theme_mode.trim().to_ascii_lowercase().as_str() {
        "dark" => tauri::Theme::Dark,
        "light" => tauri::Theme::Light,
        other => return Err(format!("Unsupported theme mode '{other}'.")),
    };

    let bg_color = background_color
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|color_value| {
            color_value
                .parse::<tauri::window::Color>()
                .map_err(|err| format!("Invalid background color '{color_value}': {err}"))
        })
        .transpose()?;

    // 主窗口与「设置」对话框共用同一套 DWM 标题栏色，避免仅主窗同步、设置窗仍为默认灰条。
    for label in [MAIN_WINDOW_LABEL, SETTINGS_WINDOW_LABEL] {
        let Some(window) = app.get_webview_window(label) else {
            continue;
        };
        window
            .set_theme(Some(native_theme))
            .map_err(|err| err.to_string())?;
        if let Some(c) = bg_color {
            window
                .set_background_color(Some(c))
                .map_err(|err| err.to_string())?;
        }
        apply_native_title_bar_colors(&window, title_bar_color, title_bar_text_color);
    }

    Ok(())
}

#[cfg(windows)]
fn apply_native_title_bar_colors<R: Runtime>(
    window: &WebviewWindow<R>,
    title_bar_color: Option<&str>,
    title_bar_text_color: Option<&str>,
) {
    let Ok(hwnd) = window.hwnd() else {
        return;
    };

    if let Some(caption_color) = title_bar_color.and_then(parse_css_colorref) {
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_CAPTION_COLOR,
                &caption_color as *const COLORREF as *const c_void,
                size_of::<COLORREF>() as u32,
            );
        }
    }

    if let Some(text_color) = title_bar_text_color.and_then(parse_css_colorref) {
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_TEXT_COLOR,
                &text_color as *const COLORREF as *const c_void,
                size_of::<COLORREF>() as u32,
            );
        }
    }
}

#[cfg(not(windows))]
fn apply_native_title_bar_colors<R: Runtime>(
    _window: &WebviewWindow<R>,
    _title_bar_color: Option<&str>,
    _title_bar_text_color: Option<&str>,
) {
}

#[cfg(windows)]
fn parse_css_colorref(value: &str) -> Option<COLORREF> {
    let tauri::window::Color(red, green, blue, _) = value.trim().parse().ok()?;
    Some(COLORREF(
        u32::from(red) | (u32::from(green) << 8) | (u32::from(blue) << 16),
    ))
}

fn unregister_active_shortcut<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let state = desktop_state(app);
    let mut active_shortcut = state
        .active_shortcut
        .lock()
        .map_err(|_| "Failed to lock the active shortcut".to_string())?;

    if let Some(previous_shortcut) = active_shortcut.clone() {
        if let Ok(previous_parsed_shortcut) = previous_shortcut.parse::<Shortcut>() {
            if app
                .global_shortcut()
                .is_registered(previous_parsed_shortcut.clone())
            {
                app.global_shortcut()
                    .unregister(previous_parsed_shortcut)
                    .map_err(|err| err.to_string())?;
            }
        }
    }

    *active_shortcut = None;
    Ok(())
}

fn register_app_shortcut<R: Runtime>(
    app: &AppHandle<R>,
    settings: &DesktopSettings,
) -> Result<String, String> {
    let normalized_shortcut = normalize_shortcut(&settings.shortcut)?;

    if !settings.shortcut_enabled {
        unregister_active_shortcut(app)?;
        return Ok(normalized_shortcut);
    }

    let parsed_shortcut = normalized_shortcut
        .parse::<Shortcut>()
        .map_err(|err| err.to_string())?;
    let state = desktop_state(app);
    let mut active_shortcut = state
        .active_shortcut
        .lock()
        .map_err(|_| "Failed to lock the active shortcut".to_string())?;

    if active_shortcut
        .as_deref()
        .map(|registered| registered.eq_ignore_ascii_case(&normalized_shortcut))
        .unwrap_or(false)
        && app.global_shortcut().is_registered(parsed_shortcut.clone())
    {
        return Ok(normalized_shortcut);
    }

    if let Some(previous_shortcut) = active_shortcut.clone() {
        if !previous_shortcut.eq_ignore_ascii_case(&normalized_shortcut) {
            if let Ok(previous_parsed_shortcut) = previous_shortcut.parse::<Shortcut>() {
                if app
                    .global_shortcut()
                    .is_registered(previous_parsed_shortcut.clone())
                {
                    app.global_shortcut()
                        .unregister(previous_parsed_shortcut)
                        .map_err(|err| err.to_string())?;
                }
            }
        }
    }

    if !app.global_shortcut().is_registered(parsed_shortcut.clone()) {
        let shortcut_for_logs = normalized_shortcut.clone();
        app.global_shortcut()
            .on_shortcut(parsed_shortcut, move |app, _, event| {
                if event.state == ShortcutState::Pressed {
                    if let Err(err) = toggle_quick_window(app) {
                        log_desktop_error(
                            &format!("toggle the quick window with {shortcut_for_logs}"),
                            &err,
                        );
                    }
                }
            })
            .map_err(|err| err.to_string())?;
    }

    *active_shortcut = Some(normalized_shortcut.clone());
    Ok(normalized_shortcut)
}

fn quit_background_app<R: Runtime>(app: &AppHandle<R>) {
    #[cfg(windows)]
    {
        crate::findx_settings::stop_findx_service_detached();
    }
    if let Err(err) = persist_full_window_state_snapshot(app) {
        log_desktop_error("persist the desktop window layout before quitting", &err);
    }
    desktop_state(app).is_quitting.store(true, Ordering::SeqCst);
    app.exit(0);
}

fn log_desktop_error(action: &str, err: &str) {
    eprintln!("FindX2 desktop action failed while trying to {action}: {err}");
}

/// Windows：读设置并同步探测管道（可能阻塞数百毫秒，勿在弹出菜单期间于主线程调用）。
#[cfg(windows)]
fn probe_tray_service_is_up(app: &AppHandle<tauri::Wry>) -> bool {
    crate::findx_settings::load_findx_settings(app.clone())
        .map(|s| crate::pipe::probe_service_pipe_sync(&s.pipe_name))
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn probe_tray_service_is_up(_app: &AppHandle<tauri::Wry>) -> bool {
    false
}

/// 构建托盘菜单。`service_up` 由调用方提供，避免在启动路径上阻塞做管道探测。
fn build_tray_menu(app: &AppHandle<tauri::Wry>, service_up: bool) -> tauri::Result<Menu<tauri::Wry>> {
    let svc_label = if service_up {
        "停止索引服务"
    } else {
        "启动索引服务"
    };

    let search_item =
        MenuItem::with_id(app, TRAY_MENU_SEARCH_ID, "打开搜索", true, None::<&str>)?;
    let settings_item = MenuItem::with_id(app, TRAY_MENU_SETTINGS_ID, "设置…", true, None::<&str>)?;
    let svc_item = MenuItem::with_id(app, TRAY_MENU_SERVICE_ID, svc_label, true, None::<&str>)?;
    let hide_item = MenuItem::with_id(app, TRAY_MENU_HIDE_ID, "隐藏窗口", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, TRAY_MENU_QUIT_ID, "退出 FindX", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let sep3 = PredefinedMenuItem::separator(app)?;
    Menu::with_items(
        app,
        &[
            &search_item,
            &settings_item,
            &sep1,
            &svc_item,
            &sep2,
            &hide_item,
            &sep3,
            &quit_item,
        ],
    )
}

fn sync_tray_menu_from_state(app: &AppHandle<tauri::Wry>, service_up: bool) {
    let Ok(menu) = build_tray_menu(app, service_up) else {
        return;
    };
    if let Some(tray) = app.tray_by_id(TRAY_ICON_ID) {
        let _ = tray.set_menu(Some(menu));
    }
}

/// 在主线程同步探测并刷新托盘（仅适用于菜单已关闭等安全时机）。
fn sync_tray_menu(app: &AppHandle<tauri::Wry>) {
    let up = probe_tray_service_is_up(app);
    sync_tray_menu_from_state(app, up);
}

/// 在后台探测管道，再回到主线程 `set_menu`，避免卡住启动与托盘消息泵。
/// 注意：不要在系统正在显示托盘右键菜单时调用（会替换 HMENU 导致菜单闪退）。
fn refresh_tray_menu_async(app: AppHandle<tauri::Wry>) {
    tauri::async_runtime::spawn(async move {
        let h_probe = app.clone();
        let up = match tokio::task::spawn_blocking(move || probe_tray_service_is_up(&h_probe)).await {
            Ok(v) => v,
            Err(_) => false,
        };
        let h = app.clone();
        let _ = app.run_on_main_thread(move || {
            sync_tray_menu_from_state(&h, up);
        });
    });
}

static TRAY_MENU_LAST_ENTER_REFRESH: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();

fn tray_menu_enter_refresh_should_run() -> bool {
    let gate = TRAY_MENU_LAST_ENTER_REFRESH.get_or_init(|| Mutex::new(None));
    let Ok(mut last) = gate.lock() else {
        return true;
    };
    let now = Instant::now();
    if let Some(t) = *last {
        if now.duration_since(t) < Duration::from_millis(400) {
            return false;
        }
    }
    *last = Some(now);
    true
}

fn setup_system_tray(app: &mut App<tauri::Wry>) -> tauri::Result<()> {
    let handle = app.handle().clone();
    // 首帧菜单不探测管道，避免启动阶段阻塞；随后 `refresh_tray_menu_async` 会异步校正文案。
    let tray_menu = build_tray_menu(&handle, false)?;

    let mut tray_builder = TrayIconBuilder::with_id(TRAY_ICON_ID)
        .menu(&tray_menu)
        .tooltip("FindX2")
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            TRAY_MENU_SEARCH_ID => {
                if let Err(err) = open_full_window(app) {
                    log_desktop_error("open search window from the tray", &err);
                }
            }
            TRAY_MENU_SETTINGS_ID => {
                if let Err(err) = show_settings_window_impl(app) {
                    log_desktop_error("open settings from the tray", &err);
                }
            }
            TRAY_MENU_SERVICE_ID => {
                #[cfg(windows)]
                {
                    let was_up = probe_tray_service_is_up(app);
                    if was_up {
                        if let Err(err) = crate::findx_settings::stop_findx_service() {
                            log_desktop_error("stop findx2-service from the tray", &err);
                        }
                    } else {
                        let handle = app.clone();
                        tauri::async_runtime::spawn(async move {
                            if let Err(err) = crate::ensure_service_running(handle.clone()).await {
                                log_desktop_error("start findx2-service from the tray", &err);
                            }
                            refresh_tray_menu_async(handle);
                        });
                        // 启动为异步：后台刷新菜单，避免阻塞托盘消息线程
                        refresh_tray_menu_async(app.clone());
                        return;
                    }
                    sync_tray_menu(app);
                }
                #[cfg(not(windows))]
                {
                    let handle = app.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(err) = crate::ensure_service_running(handle).await {
                            log_desktop_error("start findx2-service from the tray", &err);
                        }
                    });
                }
            }
            TRAY_MENU_HIDE_ID => {
                if let Err(err) = hide_main_window(app) {
                    log_desktop_error("hide the app from the tray", &err);
                }
            }
            TRAY_MENU_QUIT_ID => quit_background_app(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            match &event {
                // 鼠标移入托盘图标时异步刷新「启动/停止」文案；勿在右键菜单已弹出时 set_menu。
                TrayIconEvent::Enter { .. } => {
                    if tray_menu_enter_refresh_should_run() {
                        refresh_tray_menu_async(tray.app_handle().clone());
                    }
                }
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } => {
                    let app = tray.app_handle();
                    if let Err(err) = toggle_main_window(app) {
                        log_desktop_error("toggle the app from the tray icon", &err);
                    }
                }
                _ => {}
            }
        });

    if let Some(icon) = app.default_window_icon().cloned() {
        tray_builder = tray_builder.icon(icon);
    }

    let _tray = tray_builder.build(app)?;
    refresh_tray_menu_async(app.handle().clone());
    Ok(())
}

#[tauri::command]
pub fn get_desktop_settings(app: AppHandle<tauri::Wry>) -> Result<DesktopSettings, String> {
    current_desktop_settings(&app)
}

#[tauri::command]
pub fn open_full_window_command(app: AppHandle<tauri::Wry>) -> Result<(), String> {
    open_full_window(&app)
}

#[tauri::command]
pub fn open_quick_window_command(app: AppHandle<tauri::Wry>) -> Result<(), String> {
    open_quick_window(&app)
}

#[tauri::command]
pub fn reset_window_layout_command(app: AppHandle<tauri::Wry>) -> Result<(), String> {
    if matches!(current_window_mode(&app), WindowMode::Full) {
        let window = main_window(&app)?;
        apply_default_main_window_layout(&window, WindowMode::Full)?;
        capture_live_full_window_layout(&app, &window)?;
        if remember_window_bounds_enabled(&app) {
            set_window_state_save_enabled(&app, true);
            app.save_window_state(full_window_state_flags())
                .map_err(|err| err.to_string())?;
            sync_window_state_save_behavior(&app);
        }
    }

    persist_desktop_state(&app)
}

#[tauri::command]
pub fn show_settings_window(app: AppHandle<tauri::Wry>) -> Result<(), String> {
    show_settings_window_impl(&app)
}

#[tauri::command]
pub fn hide_settings_window(app: AppHandle<tauri::Wry>) -> Result<(), String> {
    let w = app
        .get_webview_window(SETTINGS_WINDOW_LABEL)
        .ok_or_else(|| "设置窗口未初始化".to_string())?;
    w.hide().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn sync_window_theme_command(
    app: AppHandle<tauri::Wry>,
    theme_mode: Option<String>,
    #[allow(non_snake_case)] themeMode: Option<String>,
    background_color: Option<String>,
    #[allow(non_snake_case)] backgroundColor: Option<String>,
    title_bar_color: Option<String>,
    #[allow(non_snake_case)] titleBarColor: Option<String>,
    title_bar_text_color: Option<String>,
    #[allow(non_snake_case)] titleBarTextColor: Option<String>,
) -> Result<(), String> {
    let resolved_mode = theme_mode
        .or(themeMode)
        .unwrap_or_else(|| "dark".to_string());
    let resolved_background_color = background_color.or(backgroundColor);
    let resolved_title_bar_color = title_bar_color.or(titleBarColor);
    let resolved_title_bar_text_color = title_bar_text_color.or(titleBarTextColor);

    apply_native_window_theme(
        &app,
        &resolved_mode,
        resolved_background_color.as_deref(),
        resolved_title_bar_color.as_deref(),
        resolved_title_bar_text_color.as_deref(),
    )
}

#[tauri::command]
pub fn update_desktop_settings(
    app: AppHandle<tauri::Wry>,
    background_mode_enabled: Option<bool>,
    #[allow(non_snake_case)] backgroundModeEnabled: Option<bool>,
    shortcut_enabled: Option<bool>,
    #[allow(non_snake_case)] shortcutEnabled: Option<bool>,
    remember_window_bounds: Option<bool>,
    #[allow(non_snake_case)] rememberWindowBounds: Option<bool>,
    shortcut: String,
) -> Result<DesktopSettings, String> {
    let defaults = DesktopSettings::default();
    let settings = DesktopSettings {
        background_mode_enabled: background_mode_enabled
            .or(backgroundModeEnabled)
            .unwrap_or(defaults.background_mode_enabled),
        shortcut_enabled: shortcut_enabled
            .or(shortcutEnabled)
            .unwrap_or(defaults.shortcut_enabled),
        remember_window_bounds: remember_window_bounds
            .or(rememberWindowBounds)
            .unwrap_or(defaults.remember_window_bounds),
        shortcut,
    };

    let normalized_shortcut = register_app_shortcut(&app, &settings)?;
    let next_settings = DesktopSettings {
        shortcut: normalized_shortcut,
        ..settings
    };

    set_desktop_settings(&app, &next_settings)?;
    sync_window_state_save_behavior(&app);
    if next_settings.remember_window_bounds && matches!(current_window_mode(&app), WindowMode::Full)
    {
        persist_full_window_state_snapshot(&app)?;
    } else {
        persist_desktop_state(&app)?;
    }
    Ok(next_settings)
}

pub fn setup(app: &mut App<tauri::Wry>) -> tauri::Result<()> {
    setup_system_tray(app)?;

    let loaded_state = load_desktop_state(app.handle());
    if let Ok(mut last_full_window_layout) = desktop_state(app.handle()).last_full_window_layout.lock() {
        *last_full_window_layout = loaded_state.last_full_window_layout.clone();
    }
    let loaded_settings = loaded_state.settings.clone();
    let resolved_shortcut = match register_app_shortcut(app.handle(), &loaded_settings) {
        Ok(shortcut) => shortcut,
        Err(err) => {
            log_desktop_error("register the global shortcut", &err);
            loaded_settings.shortcut.clone()
        }
    };
    let resolved_settings = DesktopSettings {
        shortcut: resolved_shortcut,
        ..loaded_settings
    };

    if let Err(err) = set_desktop_settings(app.handle(), &resolved_settings) {
        log_desktop_error("sync desktop settings in memory", &err);
    }
    if let Err(err) = set_window_mode_and_sync(app.handle(), WindowMode::Full) {
        log_desktop_error("sync the initial window mode", &err);
    }
    if let Ok(window) = main_window(app.handle()) {
        if let Err(err) = apply_main_window_layout(app.handle(), &window, WindowMode::Full) {
            log_desktop_error("apply the initial full window layout", &err);
        }
        if let Err(err) = window.show() {
            log_desktop_error("show the main window after restoring layout", &err.to_string());
        } else if let Err(err) = window.set_focus() {
            log_desktop_error("focus the main window after restoring layout", &err.to_string());
        }
        if let Err(err) = capture_live_full_window_layout(app.handle(), &window) {
            log_desktop_error("capture the initial full window layout", &err);
        }
    }
    if let Err(err) = persist_desktop_state(app.handle()) {
        log_desktop_error("persist desktop settings", &err);
    }
    if let Err(err) = emit_window_mode(app.handle(), WindowMode::Full) {
        log_desktop_error("emit the initial window mode", &err);
    }

    #[cfg(windows)]
    {
        // PowerShell 改用户 PATH 可能耗时数秒，勿阻塞首屏与托盘初始化。
        std::thread::spawn(|| {
            if let Err(err) = crate::findx_settings::ensure_cli_on_user_path() {
                log_desktop_error("append findx2 CLI directory to user PATH", &err);
            }
        });
        if let Ok(settings) = crate::findx_settings::load_findx_settings(app.handle().clone()) {
            let base = crate::findx_settings::exe_resource_dir();
            let index = crate::findx_settings::resolve_index_path(&base, &settings);
            if settings.auto_start_service && !index.exists() {
                crate::mark_pending_auto_index_build(true);
            }
        }
        let handle = app.handle().clone();
        tauri::async_runtime::spawn(async move {
            if let Err(err) = crate::auto_start_flow(handle).await {
                log_desktop_error("auto_start_flow", &err);
                crate::mark_pending_auto_index_build(false);
            }
        });
    }

    Ok(())
}

pub fn focus_existing_instance<R: Runtime>(app: &AppHandle<R>) {
    if let Err(err) = open_full_window(app) {
        log_desktop_error("focus the existing app instance", &err);
    }
}

pub fn handle_window_event<R: Runtime>(window: &Window<R>, event: &WindowEvent) {
    let app = window.app_handle();

    // 设置窗：用户点「关闭」或标题栏 X 时若直接 destroy，下次 invoke show 将找不到窗口。
    // 拦截为隐藏，与主窗托盘「隐藏」语义一致，可反复打开。
    if window.label() == SETTINGS_WINDOW_LABEL {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            if let Some(w) = app.get_webview_window(SETTINGS_WINDOW_LABEL) {
                if let Err(e) = w.hide() {
                    log_desktop_error("hide settings window instead of closing", &e.to_string());
                }
            }
        }
        return;
    }

    if window.label() != MAIN_WINDOW_LABEL {
        return;
    }

    match event {
        WindowEvent::CloseRequested { api, .. } => {
            // 标题栏关闭 / Alt+F4：在窗口真正销毁前卸掉 prevhost popup，避免残留（与前端 unmount 时序无关）。
            #[cfg(windows)]
            {
                let _ = crate::win_preview::unload_preview();
            }
            if !desktop_state(&app).is_quitting.load(Ordering::SeqCst)
                && is_background_mode_enabled(&app)
            {
                api.prevent_close();
                if let Err(err) = hide_main_window(&app) {
                    log_desktop_error("hide the app instead of closing", &err);
                }
            } else if let Err(err) = persist_full_window_state_snapshot(&app) {
                log_desktop_error("persist the desktop window layout before closing", &err);
            }
        }
        _ => {}
    }
}

pub fn desktop_state_for_builder() -> DesktopRuntimeState {
    DesktopRuntimeState::default()
}
