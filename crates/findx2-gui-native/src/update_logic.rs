//! 与 `gui/src/findx/UpdateCard.tsx` 的状态机对齐；本批只做 UI 占位，不访问 GitHub。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdatePhase {
    Idle,
    Checking,
    Available,
    Downloading,
    Downloaded,
    Current,
    Error,
}

impl UpdatePhase {
    pub fn label(self) -> Option<&'static str> {
        match self {
            Self::Idle => None,
            Self::Checking => Some("检查中"),
            Self::Downloading => Some("下载中"),
            Self::Downloaded => Some("已下载，可安装"),
            Self::Current => Some("已是最新"),
            Self::Available => Some("有新版本"),
            Self::Error => Some("检查失败"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UpdateInfo {
    pub ok: bool,
    pub error: Option<String>,
    pub current_version: String,
    pub latest_version: Option<String>,
    pub has_update: bool,
    pub download_url: Option<String>,
    pub asset_size: Option<u64>,
    pub release_notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UpdateProgress {
    pub downloaded: u64,
    pub total: u64,
    pub percent: f32,
}

pub fn derive_update_phase(
    busy: bool,
    progress: Option<&UpdateProgress>,
    info: Option<&UpdateInfo>,
    downloaded_path: Option<&str>,
) -> UpdatePhase {
    if busy && progress.is_some() {
        return UpdatePhase::Downloading;
    }
    if busy {
        return UpdatePhase::Checking;
    }
    if downloaded_path.is_some() {
        return UpdatePhase::Downloaded;
    }
    if let Some(info) = info {
        if !info.ok {
            return UpdatePhase::Error;
        }
        if info.has_update {
            return UpdatePhase::Available;
        }
        return UpdatePhase::Current;
    }
    UpdatePhase::Idle
}

pub fn format_asset_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / 1024.0 / 1024.0)
    } else {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    }
}

/// 占位演示数据，不代表真实 GitHub 检查结果。
pub fn placeholder_available() -> UpdateInfo {
    UpdateInfo {
        ok: true,
        error: None,
        current_version: "2.4.10".into(),
        latest_version: Some("2.4.11".into()),
        has_update: true,
        download_url: Some("https://github.com/chaojimct/findx/releases".into()),
        asset_size: Some(41_943_040),
        release_notes: Some("这是占位发行说明，不会执行脚本。本批不连接 GitHub。".into()),
    }
}

pub fn placeholder_current() -> UpdateInfo {
    UpdateInfo {
        ok: true,
        error: None,
        current_version: "2.4.10".into(),
        latest_version: Some("2.4.10".into()),
        has_update: false,
        download_url: None,
        asset_size: None,
        release_notes: None,
    }
}

pub fn placeholder_error() -> UpdateInfo {
    UpdateInfo {
        ok: false,
        error: Some("（占位）未连接 GitHub，这是演示用的失败状态。".into()),
        current_version: "2.4.10".into(),
        latest_version: None,
        has_update: false,
        download_url: None,
        asset_size: None,
        release_notes: None,
    }
}

pub fn placeholder_progress(percent: f32) -> UpdateProgress {
    let total = 41_943_040u64;
    let downloaded = ((percent.clamp(0.0, 100.0) / 100.0) * total as f32) as u64;
    UpdateProgress {
        downloaded,
        total,
        percent: percent.clamp(0.0, 100.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_order_matches_webview() {
        assert_eq!(
            derive_update_phase(true, None, None, None),
            UpdatePhase::Checking
        );
        assert_eq!(
            derive_update_phase(true, Some(&placeholder_progress(41.7)), None, None),
            UpdatePhase::Downloading
        );
        assert_eq!(
            derive_update_phase(false, None, None, Some("C:\\\\tmp\\\\setup.exe")),
            UpdatePhase::Downloaded
        );
        assert_eq!(
            derive_update_phase(false, None, Some(&placeholder_error()), None),
            UpdatePhase::Error
        );
        assert_eq!(
            derive_update_phase(false, None, Some(&placeholder_available()), None),
            UpdatePhase::Available
        );
        assert_eq!(
            derive_update_phase(false, None, Some(&placeholder_current()), None),
            UpdatePhase::Current
        );
        assert_eq!(
            derive_update_phase(false, None, None, None),
            UpdatePhase::Idle
        );
    }
}
