//! 隔离目录里的本地偏好与设置。只允许在后台任务里调用这些读写函数。

use crate::isolation::prefs_path;
pub use crate::isolation::settings_path;
use crate::settings_logic::{FindxGuiSettings, UiThemePref};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalPrefs {
    #[serde(default)]
    pub theme: UiThemePref,
    #[serde(default)]
    pub diag_enabled: bool,
    #[serde(default = "default_true")]
    pub sidebar_open: bool,
    #[serde(default = "default_true")]
    pub preview_open: bool,
    #[serde(default)]
    pub preview_width: Option<f32>,
    #[serde(default)]
    pub column_widths: Option<[f32; 4]>,
}

fn default_true() -> bool {
    true
}

impl Default for LocalPrefs {
    fn default() -> Self {
        Self {
            theme: UiThemePref::Light,
            diag_enabled: false,
            sidebar_open: true,
            preview_open: true,
            preview_width: None,
            column_widths: None,
        }
    }
}

pub fn load_prefs() -> LocalPrefs {
    let path = prefs_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return LocalPrefs::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn save_prefs(prefs: &LocalPrefs) -> Result<(), String> {
    let path = prefs_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(prefs).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| e.to_string())
}

pub fn load_settings(pipe_name: &str, default_index: String) -> FindxGuiSettings {
    let path = settings_path();
    let default = FindxGuiSettings::isolated_default(pipe_name, default_index);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return default;
    };
    match serde_json::from_str::<FindxGuiSettings>(&text) {
        Ok(mut s) => {
            // 绝不把隔离客户端的管道名偷偷改回生产默认。
            if crate::isolation::is_production_pipe(&s.pipe_name) {
                s.pipe_name = pipe_name.to_string();
            }
            s
        }
        Err(_) => default,
    }
}

pub fn save_settings(settings: &FindxGuiSettings) -> Result<(), String> {
    if crate::isolation::is_production_pipe(&settings.pipe_name) {
        return Err("拒绝把管道名保存为生产默认 findx2。请使用隔离管道名。".into());
    }
    let path = settings_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| e.to_string())
}

/// 在后台线程枚举可能存在的盘符/根路径，供设置页勾选。失败时返回空列表。
pub fn list_available_roots() -> Vec<String> {
    #[cfg(windows)]
    {
        let mut out = Vec::new();
        for letter in b'A'..=b'Z' {
            let root = format!("{}:\\", letter as char);
            if std::path::Path::new(&root).exists() {
                out.push(root);
            }
        }
        out
    }
    #[cfg(not(windows))]
    {
        let mut out = Vec::new();
        if std::path::Path::new("/").exists() {
            out.push("/".into());
        }
        if let Ok(home) = std::env::var("HOME") {
            if std::path::Path::new(&home).exists() {
                out.push(home);
            }
        }
        out
    }
}
