//! 与 `gui/src/findx/settingsDraft.ts` 对齐的脏检查 / 数字草稿 / 关闭确认。

use serde::{Deserialize, Serialize};

/// 与 `FindxGuiSettings.search_limit: u32` 相同的上界。
pub const SEARCH_LIMIT_U32_MAX: u64 = 4_294_967_295;

pub const CLOSE_DIRTY_MESSAGE: &str = "设置已修改但未保存,确定要关闭吗？未保存的更改将丢失。";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunMode {
    Service,
    Standalone,
}

impl Default for RunMode {
    fn default() -> Self {
        Self::Service
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiThemePref {
    Light,
    Dark,
    System,
}

impl Default for UiThemePref {
    fn default() -> Self {
        Self::Light
    }
}

impl UiThemePref {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            "system" => Some(Self::System),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::System => "system",
        }
    }

    pub fn resolve(self, system_dark: bool) -> EffectiveTheme {
        match self {
            Self::Dark => EffectiveTheme::Dark,
            Self::Light => EffectiveTheme::Light,
            Self::System => {
                if system_dark {
                    EffectiveTheme::Dark
                } else {
                    EffectiveTheme::Light
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveTheme {
    Light,
    Dark,
}

impl EffectiveTheme {
    pub fn is_dark(self) -> bool {
        matches!(self, Self::Dark)
    }
}

/// 与现有 GUI 设置字段对齐，但只落在隔离目录，不写正式 GUI 配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindxGuiSettings {
    pub index_path: String,
    pub volume: String,
    pub pipe_name: String,
    pub pinyin_default: bool,
    pub service_exe_path: String,
    pub search_limit: u32,
    #[serde(default = "default_true")]
    pub auto_start_service: bool,
    #[serde(default)]
    pub first_index_full_metadata: bool,
    #[serde(default)]
    pub run_mode: RunMode,
    #[serde(default)]
    pub drives: Vec<String>,
    #[serde(default)]
    pub excluded_dirs: Vec<String>,
    #[serde(default = "default_true")]
    pub enable_metadata_backfill: bool,
    #[serde(default = "default_true")]
    pub enable_everything_ipc: bool,
    #[serde(default = "default_save_interval")]
    pub save_interval_secs: u64,
    #[serde(default)]
    pub auto_start_app: bool,
    #[serde(default = "default_true")]
    pub auto_check_update: bool,
}

fn default_true() -> bool {
    true
}

fn default_save_interval() -> u64 {
    30
}

impl FindxGuiSettings {
    pub fn isolated_default(pipe_name: &str, index_path: String) -> Self {
        Self {
            index_path,
            volume: String::new(),
            pipe_name: pipe_name.to_string(),
            pinyin_default: true,
            service_exe_path: String::new(),
            search_limit: 5000,
            auto_start_service: false,
            first_index_full_metadata: false,
            run_mode: RunMode::Standalone,
            drives: Vec::new(),
            excluded_dirs: Vec::new(),
            enable_metadata_backfill: true,
            enable_everything_ipc: true,
            save_interval_secs: 30,
            auto_start_app: false,
            auto_check_update: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftCommit {
    Ok(u64),
    Err(&'static str),
}

/// 编辑过程允许空串和未完成的数字。保存时才提交。
/// 接受 1..=u32::MAX。0 不接受。
pub fn commit_search_limit(text: &str) -> DraftCommit {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return DraftCommit::Err("结果条数上限还是空的。编辑时可以先清空，保存前需要填一个整数。");
    }
    if !trimmed.chars().all(|c| c.is_ascii_digit()) {
        return DraftCommit::Err("结果条数上限必须是十进制整数。");
    }
    match trimmed.parse::<u64>() {
        Ok(value) if (1..=SEARCH_LIMIT_U32_MAX).contains(&value) => DraftCommit::Ok(value),
        _ => DraftCommit::Err("结果条数上限须在 1 到 4294967295 之间。"),
    }
}

/// `save_interval_secs` 在后端是 u64，界面只拒绝空值、非整数和小于 1。
pub fn commit_save_interval_secs(text: &str) -> DraftCommit {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return DraftCommit::Err("落盘间隔还是空的。保存前请填写不小于 1 的整数秒。");
    }
    if !trimmed.chars().all(|c| c.is_ascii_digit()) {
        return DraftCommit::Err("落盘间隔必须是整数秒。");
    }
    match trimmed.parse::<u64>() {
        Ok(value) if value >= 1 => DraftCommit::Ok(value),
        _ => DraftCommit::Err("落盘间隔必须是不小于 1 的整数秒。"),
    }
}

/// 数字框在保存前只改草稿。主题即时预览，所以脏标记必须把草稿和主题算进去。
pub fn form_is_dirty(
    settings: &FindxGuiSettings,
    original: Option<&FindxGuiSettings>,
    theme: UiThemePref,
    original_theme: UiThemePref,
    search_limit_text: &str,
    save_interval_text: &str,
) -> bool {
    let Some(original) = original else {
        return false;
    };
    if theme != original_theme {
        return true;
    }
    if settings != original {
        return true;
    }
    if search_limit_text != original.search_limit.to_string() {
        return true;
    }
    if save_interval_text != original.save_interval_secs.to_string() {
        return true;
    }
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseDecision {
    Hide,
    Stay,
}

pub fn close_decision(is_dirty: bool, confirmed: bool) -> CloseDecision {
    if !is_dirty || confirmed {
        CloseDecision::Hide
    } else {
        CloseDecision::Stay
    }
}

pub fn discard_unsaved(
    settings: FindxGuiSettings,
    original: Option<FindxGuiSettings>,
    theme: UiThemePref,
    original_theme: UiThemePref,
) -> (FindxGuiSettings, UiThemePref) {
    (original.unwrap_or(settings), original_theme.or_keep(theme))
}

trait OrKeep {
    fn or_keep(self, current: UiThemePref) -> UiThemePref;
}

impl OrKeep for UiThemePref {
    fn or_keep(self, _current: UiThemePref) -> UiThemePref {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> FindxGuiSettings {
        FindxGuiSettings::isolated_default("findx2-native-dev", "index.bin".into())
    }

    #[test]
    fn search_limit_accepts_valid_and_rejects_zero_or_empty() {
        assert_eq!(commit_search_limit("5001"), DraftCommit::Ok(5001));
        assert!(matches!(commit_search_limit(""), DraftCommit::Err(_)));
        assert!(matches!(commit_search_limit("0"), DraftCommit::Err(_)));
        assert!(matches!(commit_search_limit("12a"), DraftCommit::Err(_)));
        assert_eq!(
            commit_search_limit("4294967295"),
            DraftCommit::Ok(SEARCH_LIMIT_U32_MAX)
        );
        assert!(matches!(
            commit_search_limit("4294967296"),
            DraftCommit::Err(_)
        ));
    }

    #[test]
    fn save_interval_rejects_empty_and_zero() {
        assert_eq!(commit_save_interval_secs("30"), DraftCommit::Ok(30));
        assert!(matches!(commit_save_interval_secs(""), DraftCommit::Err(_)));
        assert!(matches!(
            commit_save_interval_secs("0"),
            DraftCommit::Err(_)
        ));
    }

    #[test]
    fn dirty_includes_numeric_drafts_even_if_settings_unchanged() {
        let s = sample();
        assert!(!form_is_dirty(
            &s,
            Some(&s),
            UiThemePref::Light,
            UiThemePref::Light,
            "5000",
            "30",
        ));
        assert!(form_is_dirty(
            &s,
            Some(&s),
            UiThemePref::Light,
            UiThemePref::Light,
            "5001",
            "30",
        ));
        assert!(form_is_dirty(
            &s,
            Some(&s),
            UiThemePref::Dark,
            UiThemePref::Light,
            "5000",
            "30",
        ));
    }

    #[test]
    fn close_requires_confirm_when_dirty() {
        assert_eq!(close_decision(false, false), CloseDecision::Hide);
        assert_eq!(close_decision(true, false), CloseDecision::Stay);
        assert_eq!(close_decision(true, true), CloseDecision::Hide);
    }
}
