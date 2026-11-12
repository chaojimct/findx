//! 隔离目录里的本地偏好与设置。只允许在后台任务里调用这些读写函数。

use crate::isolation::prefs_path;
pub use crate::isolation::settings_path;
use crate::settings_logic::{FindxGuiSettings, UiThemePref};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalPrefs {
    #[serde(default)]
    pub browser_favorites: Vec<String>,
    #[serde(default)]
    pub browser_recent: Vec<String>,
    #[serde(default)]
    pub browser_show_hidden: bool,
    #[serde(default)]
    pub browser_column_schema: u8,
    #[serde(default)]
    pub theme: UiThemePref,
    #[serde(default)]
    pub theme_style: crate::theme::ThemeStyle,
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
    #[serde(default = "default_true")]
    pub search_fit_columns: bool,
    #[serde(default = "default_true")]
    pub browser_fit_columns: bool,
    #[serde(default)]
    pub browser_column_widths: Option<[f32; 4]>,
}

fn default_true() -> bool {
    true
}

impl Default for LocalPrefs {
    fn default() -> Self {
        Self {
            browser_favorites: Vec::new(),
            browser_recent: Vec::new(),
            browser_show_hidden: false,
            browser_column_schema: 0,
            theme: UiThemePref::Light,
            theme_style: crate::theme::ThemeStyle::default(),
            diag_enabled: false,
            sidebar_open: true,
            preview_open: true,
            preview_width: None,
            column_widths: None,
            search_fit_columns: true,
            browser_fit_columns: true,
            browser_column_widths: None,
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
    write_atomic(&path, text.as_bytes())
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
    write_atomic(&path, text.as_bytes())
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

/// Same-directory atomic replacement keeps the last good JSON intact on interrupted writes.
/// Call only from background I/O. Windows rename does not replace by default.
pub(crate) fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let name = format!(
        ".findx-save-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let temp = path.parent().ok_or("配置文件没有父目录")?.join(name);
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        file.write_all(bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            use windows::Win32::Storage::FileSystem::{
                MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
            };
            let src: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
            let dst: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            unsafe {
                MoveFileExW(
                    windows::core::PCWSTR(src.as_ptr()),
                    windows::core::PCWSTR(dst.as_ptr()),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
                .map_err(|e| e.to_string())?;
            }
        }
        #[cfg(not(windows))]
        {
            std::fs::rename(&temp, path).map_err(|e| e.to_string())?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}
#[cfg(test)]
mod storage_tests {
    use super::*;
    #[test]
    fn atomic_replace_roundtrips_and_failure_keeps_other_files() {
        let root = std::env::temp_dir().join(format!("findx-prefs-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("prefs.json");
        write_atomic(&file, b"old").unwrap();
        write_atomic(&file, b"new").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"new");
        assert!(write_atomic(&root, b"bad").is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"new");
        std::fs::remove_dir_all(root).unwrap();
    }
}
