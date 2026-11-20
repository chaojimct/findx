//! 隔离目录里的本地偏好与设置。只允许在后台任务里调用这些读写函数。

use crate::isolation::prefs_path;
pub use crate::isolation::settings_path;
use crate::settings_logic::{FindxGuiSettings, UiThemePref};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalPrefs {
    #[serde(default)]
    pub workspace_resume: crate::legacy_compat::Resume,
    #[serde(flatten)]
    pub compatibility_extra: std::collections::BTreeMap<String,serde_json::Value>,
    #[serde(default)]
    pub usage_reduce_motion: bool,
    #[serde(default = "default_usage_depth")]
    pub usage_preferred_depth: usize,
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
    #[serde(default)]
    pub last_official_update_check_unix: i64,
}

fn default_usage_depth()->usize{3}

fn default_true() -> bool {
    true
}

impl Default for LocalPrefs {
    fn default() -> Self {
        Self {
            workspace_resume: Default::default(),
            compatibility_extra: Default::default(),
            usage_reduce_motion: false,
            usage_preferred_depth: 3,
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
            last_official_update_check_unix: 0,
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
    let mut value=if path.exists(){let bytes=std::fs::read(&path).map_err(|e|e.to_string())?;let _:LocalPrefs=serde_json::from_slice(&bytes).map_err(|e|format!("existing preferences unsupported; preserved: {e}"))?;serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|e|e.to_string())?}else{serde_json::json!({})};
    let object=value.as_object_mut().ok_or("existing preferences are not an object; preserved")?;
    object.extend(serde_json::to_value(prefs).map_err(|e|e.to_string())?.as_object().ok_or("preferences serialization")?.clone());
    let text=serde_json::to_string_pretty(&value).map_err(|e|e.to_string())?;
    write_atomic(&path, text.as_bytes())
}

pub fn load_settings(pipe_name: &str, default_index: String) -> FindxGuiSettings {
    let path = settings_path();
    let default = FindxGuiSettings::isolated_default(pipe_name, default_index.clone());
    let Ok(text) = std::fs::read_to_string(&path) else {
        return default;
    };
    match serde_json::from_str::<FindxGuiSettings>(&text) {
        Ok(mut s) => {
            // 绝不把隔离客户端的管道名偷偷改回生产默认。
            if crate::isolation::official_storage() {
                // Persisted GUI drafts never redirect an official GUI to dev storage.
                s.pipe_name = "findx2".into();
                s.index_path = default_index;
            } else if crate::isolation::is_production_pipe(&s.pipe_name) {
                s.pipe_name = pipe_name.to_string();
            }
            s
        }
        Err(_) => default,
    }
}

pub fn save_settings(settings: &FindxGuiSettings) -> Result<(), String> {
    if crate::isolation::is_production_pipe(&settings.pipe_name)
        && !crate::isolation::official_storage()
    {
        return Err("管道名称不可用，请换一个名称后再保存。".into());
    }
    if crate::isolation::official_storage() && settings.pipe_name != "findx2" {
        return Err("已安装版本的连接由系统管理，不能在这里修改。".into());
    }
    let path = settings_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut value=if path.exists(){let bytes=std::fs::read(&path).map_err(|e|e.to_string())?;let _:FindxGuiSettings=serde_json::from_slice(&bytes).map_err(|e|format!("existing settings unsupported; preserved: {e}"))?;serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|e|e.to_string())?}else{serde_json::json!({})};
    value.as_object_mut().ok_or("existing settings are not an object; preserved")?.extend(serde_json::to_value(settings).map_err(|e|e.to_string())?.as_object().ok_or("settings serialization")?.clone());
    let text=serde_json::to_string_pretty(&value).map_err(|e|e.to_string())?;
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

#[cfg(test)]mod usage_depth_preference_tests{#[test]fn default_is_three_and_manual_two_roundtrips_without_available_depth(){let p=super::LocalPrefs::default();assert_eq!(p.usage_preferred_depth,3);let mut p=p;p.usage_preferred_depth=2;let q:super::LocalPrefs=serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();assert_eq!(q.usage_preferred_depth,2);let q:super::LocalPrefs=serde_json::from_str("{}").unwrap();assert_eq!(q.usage_preferred_depth,3);}}

/// Runs before platform/window creation, never in drawing/input callbacks.
pub fn migrate_legacy()->Result<(),String>{
 if !crate::isolation::official_storage(){return Ok(());}
 #[cfg(windows)]{
  let config=std::path::PathBuf::from(std::env::var_os("APPDATA").ok_or("APPDATA unavailable")?).join("tools.findx.gui").join("findx2-gui-settings.json");
  if !settings_path().exists()&&config.exists(){
   // Validate typed settings BEFORE installing, but retain the complete raw JSON
   // (including unknown keys) and leave the source immutable.
   let _:FindxGuiSettings=serde_json::from_slice(&std::fs::read(&config).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
   crate::legacy_compat::import_settings(&config,&settings_path(),true,"findx2",&crate::isolation::official_index_path().to_string_lossy())?;
  }
  if !prefs_path().exists(){
   let profile=std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA unavailable")?).join("tools.findx.gui/EBWebView/Default/Local Storage/leveldb");
   let values=crate::legacy_compat::webview_values(&profile)?;
   if !values.is_empty(){let value=crate::legacy_compat::map_prefs(&values)?;let _:LocalPrefs=serde_json::from_value(value.clone()).map_err(|e|e.to_string())?;crate::legacy_compat::install_new(&prefs_path(),&value)?;}
  }
 }
 Ok(())
}
enum PrefWrite{Save(LocalPrefs),Barrier(std::sync::mpsc::SyncSender<()>)}
fn writer()->&'static std::sync::mpsc::Sender<PrefWrite>{
 static Q:std::sync::OnceLock<std::sync::mpsc::Sender<PrefWrite>>=std::sync::OnceLock::new();Q.get_or_init(||{let(tx,rx)=std::sync::mpsc::channel();std::thread::spawn(move||{for item in rx{match item{PrefWrite::Save(p)=>{if let Err(e)=save_prefs(&p){eprintln!("FindX preferences save failed: {e}");}},PrefWrite::Barrier(tx)=>{let _=tx.send(());}}}});tx})
}
pub fn enqueue_prefs(prefs:LocalPrefs){let _=writer().send(PrefWrite::Save(prefs));}
/// Called only AFTER the application event loop returns; no render-thread disk IO.
pub fn flush_queued_prefs(){let(tx,rx)=std::sync::mpsc::sync_channel(1);if writer().send(PrefWrite::Barrier(tx)).is_ok(){let _=rx.recv_timeout(std::time::Duration::from_secs(5));}}
