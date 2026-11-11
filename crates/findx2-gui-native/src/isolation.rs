//! 隔离运行约束：默认拒绝生产管道，设置目录始终使用 FindX-native-dev。
//! 只有显式 `--live` 才允许作为只读客户端连接生产管道 `findx2`。
pub const DEFAULT_ISOLATED_PIPE: &str = "findx2-native-dev";
pub const PRODUCTION_PIPE: &str = "findx2";
pub const APP_DIR_NAME: &str = "FindX-native-dev";

/// 生产默认管道（以及带路径前缀的等价形式）。
pub fn is_production_pipe(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return true;
    }
    let bare = pipe_bare_name(trimmed);
    bare.eq_ignore_ascii_case(PRODUCTION_PIPE)
}

pub fn pipe_bare_name(name: &str) -> String {
    let t = name.trim();
    #[cfg(windows)]
    {
        let lower = t.replace('/', r"\");
        if let Some(rest) = lower
            .strip_prefix(r"\\.\pipe\")
            .or_else(|| lower.strip_prefix(r"\\?\pipe\"))
        {
            return rest.to_string();
        }
    }
    t.trim_end_matches(".sock").to_string()
}

pub fn normalize_pipe(pipe_name: &str) -> Result<String, String> {
    normalize_pipe_ex(pipe_name, false)
}

/// `allow_production` 为真时，允许规范化生产管道名（只读客户端，不启动 service）。
pub fn normalize_pipe_ex(pipe_name: &str, allow_production: bool) -> Result<String, String> {
    if is_production_pipe(pipe_name) && !allow_production {
        return Err(format!(
            "拒绝连接生产管道 `{pipe_name}`。原生客户端默认只允许隔离管道（默认 `{DEFAULT_ISOLATED_PIPE}`）。若要只读连接现有 findx2-service，请使用 --live。"
        ));
    }
    let p = pipe_name.trim();
    #[cfg(windows)]
    {
        if p.starts_with(r"\\") {
            return Ok(p.to_string());
        }
        Ok(format!(r"\\.\pipe\{p}"))
    }
    #[cfg(unix)]
    {
        Ok(findx2_ipc::unix_socket_path(p)
            .to_string_lossy()
            .into_owned())
    }
    #[cfg(not(any(windows, unix)))]
    {
        Ok(p.to_string())
    }
}

/// 隔离数据目录：Windows 为 `%LOCALAPPDATA%\FindX-native-dev`。
pub fn isolated_data_dir() -> std::path::PathBuf {
    #[cfg(windows)]
    {
        if let Ok(base) = std::env::var("LOCALAPPDATA") {
            if !base.is_empty() {
                return std::path::PathBuf::from(base).join(APP_DIR_NAME);
            }
        }
        std::env::temp_dir().join(APP_DIR_NAME)
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        std::path::PathBuf::from(home)
            .join("Library/Application Support")
            .join(APP_DIR_NAME)
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            if !xdg.is_empty() {
                return std::path::PathBuf::from(xdg).join(APP_DIR_NAME);
            }
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        std::path::PathBuf::from(home)
            .join(".local/share")
            .join(APP_DIR_NAME)
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        std::env::temp_dir().join(APP_DIR_NAME)
    }
}

pub fn settings_path() -> std::path::PathBuf {
    isolated_data_dir().join("settings.json")
}

pub fn prefs_path() -> std::path::PathBuf {
    isolated_data_dir().join("prefs.json")
}

pub fn isolated_index_path() -> std::path::PathBuf {
    isolated_data_dir().join("index.bin")
}

pub fn window_title(live: bool) -> &'static str {
    if live {
        "FindX Native (live)"
    } else {
        "FindX Native (isolated)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_production_pipe_names() {
        assert!(is_production_pipe("findx2"));
        assert!(is_production_pipe("FINDX2"));
        assert!(is_production_pipe("  findx2  "));
        assert!(is_production_pipe(r"\\.\pipe\findx2"));
        assert!(is_production_pipe(""));
        assert!(!is_production_pipe(DEFAULT_ISOLATED_PIPE));
        assert!(!is_production_pipe(r"\\.\pipe\findx2-native-dev"));
        assert!(normalize_pipe("findx2").is_err());
        assert!(normalize_pipe(DEFAULT_ISOLATED_PIPE).is_ok());
    }

    #[test]
    fn live_mode_normalizes_production_pipe() {
        let endpoint = normalize_pipe_ex("findx2", true).expect("live allows findx2");
        assert!(is_production_pipe("findx2"));
        #[cfg(windows)]
        assert_eq!(endpoint, r"\\.\pipe\findx2");
        assert!(normalize_pipe_ex(r"\\.\pipe\findx2", true).is_ok());
        assert!(normalize_pipe_ex("findx2", false).is_err());
    }
}
