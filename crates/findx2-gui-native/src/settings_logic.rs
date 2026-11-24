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

/// 全文搜索默认模式：搜索页全文开关的初始值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ContentDefaultMode {
    /// 只搜文件名（现有行为）。
    #[default]
    Name,
    /// 只搜文件内容（`content:` 替换文件名谓词，不过滤名）。
    Content,
    /// 两次查询 union（文件名命中 + 内容命中并集）。
    Mix,
}

impl ContentDefaultMode {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "name" => Some(Self::Name),
            "content" => Some(Self::Content),
            "mix" => Some(Self::Mix),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Content => "content",
            Self::Mix => "mix",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "文件名",
            Self::Content => "全文检索",
            Self::Mix => "混合检索",
        }
    }

    /// 三段 switch 下标（文件名 0 / 全文检索 1 / 混合检索 2），供滑块定位。
    pub fn tab_index(self) -> usize {
        match self {
            Self::Name => 0,
            Self::Content => 1,
            Self::Mix => 2,
        }
    }
}

/// 搜索框三段 switch 单段宽度（px）：三段等宽，滑块按下标定位。
pub const CONTENT_MODE_SEG_W: f32 = 68.0;
/// 滑块滑动时长（ms，与 motion::ease_in_out_cubic 搭配）。
pub const CONTENT_MODE_GLIDE_MS: u64 = 180;

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
    // ---- 全文内容索引（独立 ContentSearch 倒排，见 docs/CONTENT-SEARCH-M1.md）----
    /// GUI 总开关：关 = 搜索页隐藏全文入口（service 侧增量跟随不受影响）。
    #[serde(default = "default_true")]
    pub content_enabled: bool,
    /// 搜索页全文开关的初始模式。
    #[serde(default)]
    pub content_default_mode: ContentDefaultMode,
    /// 结果表第二行 + 预览窗显示命中摘要。
    #[serde(default = "default_true")]
    pub content_snippet: bool,
    /// 全文拼音辅库：首字母查询（`qw` 命中“全文”）。缺库时自动降级。
    #[serde(default = "default_true")]
    pub content_pinyin_initials: bool,
    /// 全文拼音辅库：全拼查询（`quan wen` 命中“全文”）。缺库时自动降级。
    #[serde(default = "default_true")]
    pub content_pinyin_full: bool,
    /// 混合模式下内容侧取回上限（防 TopDocs 爆炸）。
    #[serde(default = "default_content_mix_limit")]
    pub content_mix_limit: u32,
    /// 单文件抽取上限（MB），映射 tantivy 建库侧 MAX_EXTRACT_BYTES。
    #[serde(default = "default_content_max_file_mb")]
    pub content_max_file_mb: u64,
    /// 建库是否抽取 Office（docx/xlsx/pptx）。
    #[serde(default = "default_true")]
    pub content_index_office: bool,
    /// 建库是否抽取 PDF。
    #[serde(default = "default_true")]
    pub content_index_pdf: bool,
    /// 建库是否抽取代码扩展名（80+ 白名单）。
    #[serde(default = "default_true")]
    pub content_index_code: bool,
    /// service 是否挂载内容索引增量跟随。
    #[serde(default = "default_true")]
    pub content_follow_incremental: bool,
    /// 增量线程攒批 commit 的文档数阈值。
    #[serde(default = "default_content_commit_docs")]
    pub content_commit_docs: u64,
    /// 增量线程攒批 commit 的时间阈值（秒）。
    #[serde(default = "default_content_commit_secs")]
    pub content_commit_secs: u64,
}

fn default_true() -> bool {
    true
}

fn default_save_interval() -> u64 {
    30
}

fn default_content_mix_limit() -> u32 {
    200
}

fn default_content_max_file_mb() -> u64 {
    20
}

fn default_content_commit_docs() -> u64 {
    500
}

fn default_content_commit_secs() -> u64 {
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
            content_enabled: true,
            content_default_mode: ContentDefaultMode::Name,
            content_snippet: true,
            content_pinyin_initials: true,
            content_pinyin_full: true,
            content_mix_limit: default_content_mix_limit(),
            content_max_file_mb: default_content_max_file_mb(),
            content_index_office: true,
            content_index_pdf: true,
            content_index_code: true,
            content_follow_incremental: true,
            content_commit_docs: default_content_commit_docs(),
            content_commit_secs: default_content_commit_secs(),
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

fn commit_u64_in(text: &str, lo: u64, hi: u64, empty_msg: &'static str, range_msg: &'static str) -> DraftCommit {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return DraftCommit::Err(empty_msg);
    }
    if !trimmed.chars().all(|c| c.is_ascii_digit()) {
        return DraftCommit::Err(range_msg);
    }
    match trimmed.parse::<u64>() {
        Ok(value) if (lo..=hi).contains(&value) => DraftCommit::Ok(value),
        _ => DraftCommit::Err(range_msg),
    }
}

/// 全文单文件上限（MB）：1..=512，映射建库侧 MAX_EXTRACT_BYTES。
pub fn commit_content_max_file_mb(text: &str) -> DraftCommit {
    commit_u64_in(
        text,
        1,
        512,
        "单文件上限还是空的。保存前请填写 1 到 512 的整数 MB。",
        "单文件上限须在 1 到 512 MB 之间。",
    )
}

/// 混合模式内容侧取回上限：1..=100000。
pub fn commit_content_mix_limit(text: &str) -> DraftCommit {
    commit_u64_in(
        text,
        1,
        100_000,
        "混合上限还是空的。保存前请填写 1 到 100000 的整数。",
        "混合上限须在 1 到 100000 之间。",
    )
}

/// 增量攒批 commit 文档数：1..=1000000。
pub fn commit_content_commit_docs(text: &str) -> DraftCommit {
    commit_u64_in(
        text,
        1,
        1_000_000,
        "提交文档数还是空的。保存前请填写不小于 1 的整数。",
        "提交文档数须在 1 到 1000000 之间。",
    )
}

/// 增量攒批 commit 时间（秒）：不小于 1。
pub fn commit_content_commit_secs(text: &str) -> DraftCommit {
    commit_u64_in(
        text,
        1,
        3_600,
        "提交间隔还是空的。保存前请填写不小于 1 的整数秒。",
        "提交间隔须在 1 到 3600 秒之间。",
    )
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
    fn content_numeric_bounds() {
        assert_eq!(commit_content_max_file_mb("20"), DraftCommit::Ok(20));
        assert!(matches!(commit_content_max_file_mb(""), DraftCommit::Err(_)));
        assert!(matches!(commit_content_max_file_mb("0"), DraftCommit::Err(_)));
        assert!(matches!(commit_content_max_file_mb("513"), DraftCommit::Err(_)));
        assert_eq!(commit_content_mix_limit("200"), DraftCommit::Ok(200));
        assert!(matches!(commit_content_mix_limit("0"), DraftCommit::Err(_)));
        assert_eq!(commit_content_commit_docs("500"), DraftCommit::Ok(500));
        assert!(matches!(commit_content_commit_docs("0"), DraftCommit::Err(_)));
        assert_eq!(commit_content_commit_secs("30"), DraftCommit::Ok(30));
        assert!(matches!(commit_content_commit_secs("0"), DraftCommit::Err(_)));
        assert!(matches!(commit_content_commit_secs("3601"), DraftCommit::Err(_)));
    }

    #[test]
    fn content_settings_default_and_back_compat() {
        let s = sample();
        assert!(s.content_enabled);
        assert_eq!(s.content_default_mode, ContentDefaultMode::Name);
        assert!(s.content_snippet);
        assert!(s.content_pinyin_initials);
        assert!(s.content_pinyin_full);
        assert_eq!(s.content_mix_limit, 200);
        assert_eq!(s.content_max_file_mb, 20);
        assert!(s.content_index_office && s.content_index_pdf && s.content_index_code);
        assert!(s.content_follow_incremental);
        assert_eq!(s.content_commit_docs, 500);
        assert_eq!(s.content_commit_secs, 30);
        // 老配置（无全文键）照常解析，全文走默认值。
        let old = serde_json::json!({
            "indexPath": "index.bin",
            "volume": "",
            "pipeName": "findx2-native-dev",
            "serviceExePath": "",
            "searchLimit": 5000u32,
            "pinyinDefault": true
        });
        let s: FindxGuiSettings = serde_json::from_value(old).unwrap();
        assert!(s.content_enabled);
        assert!(s.content_pinyin_initials);
        assert!(s.content_pinyin_full);
        assert_eq!(s.content_default_mode, ContentDefaultMode::Name);
        assert_eq!(s.content_max_file_mb, 20);
        // 模式解析。
        assert_eq!(ContentDefaultMode::parse("mix"), Some(ContentDefaultMode::Mix));
        assert_eq!(ContentDefaultMode::parse("nope"), None);
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
