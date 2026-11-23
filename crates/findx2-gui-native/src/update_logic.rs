//! Official in-app updater for the shipped FindX product.
//!
//! Channel matches the retired 2.4.10 client: `GET https://api.github.com/repos/chaojimct/findx/releases/latest`
//! (non-draft, non-prerelease), compare tag semver, then pick the official
//! asset for the running platform:
//! - Windows: `FindX-*-setup.exe`, launched with `ShellExecuteEx` `runas`
//!   `/SILENT /SUPPRESSMSGBOXES /NORESTART` so Inno shows its own file-copy
//!   progress window; the GUI additionally waits on the installer process and
//!   reports launch/running/verify stages on the About card.
//! - Linux: `FindX-*-linux-x64.deb` (preferred when the current install is a
//!   `.deb`) or `FindX-*-linux-x64.tar.gz`, installed with `pkexec`/`sudo`
//! - macOS: `FindX-*-macos-aarch64.dmg` or `FindX-*-macos-x64.dmg`, copied
//!   into `/Applications` with an administrator prompt
//!
//! Isolated / native-dev / fixture clients must not check, download, or install.

use serde::Deserialize;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const GITHUB_OWNER: &str = "chaojimct";
pub const GITHUB_REPO: &str = "findx";
pub const LATEST_API_URL: &str = "https://api.github.com/repos/chaojimct/findx/releases/latest";
pub const USER_AGENT_CHECK: &str = "FindX2-GUI-UpdateCheck";
pub const USER_AGENT_DOWNLOAD: &str = "FindX2-GUI-UpdateDownload";
/// Inno flags for an in-app update. `/SILENT` keeps the
/// installer's own progress window visible so the file-copy phase is no
/// longer a black box; message boxes stay suppressed and no reboot happens.
pub const SILENT_SETUP_ARGS: &str = "/SILENT /SUPPRESSMSGBOXES /NORESTART";
/// Exit code the FindX installer reports when the upgrade transaction was
/// started but not committed (rolled back, nothing changed). Mirrors
/// `GetCustomSetupExitCode` in `installer/FindX.iss`; never treat it as success.
pub const SETUP_ROLLBACK_EXIT_CODE: u32 = 80;
/// Registry value name holding the installed version under the uninstall key.
pub const UNINSTALL_DISPLAY_VERSION: &str = "DisplayVersion";
/// Uninstall registry key (without the `HKLM\...\Uninstall\` prefix) of the
/// official FindX product. Used to confirm a version flip after setup exits.
pub const FINDX_UNINSTALL_KEY: &str = "{7E8A9B0C-1D2E-3F4A-5B6C-7D8E9F0A1B2C}_is1";
pub const AUTO_CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;
const NOTES_LIMIT: usize = 600;
const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(200);
const MIN_SETUP_BYTES: u64 = 1024 * 1024;
/// Release asset carrying sha256sum lines for the other assets.
const CHECKSUM_ASSET_NAME: &str = "SHA256SUMS";

/// Official update asset platform. Tests pass this explicitly so Linux CI can
/// still assert Windows asset picking, and vice versa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdatePlatform {
    Windows,
    LinuxX64,
    MacosX64,
    MacosAarch64,
}

impl UpdatePlatform {
    pub fn current() -> Self {
        #[cfg(windows)]
        {
            Self::Windows
        }
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            Self::LinuxX64
        }
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        {
            Self::MacosX64
        }
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            Self::MacosAarch64
        }
        #[cfg(not(any(
            windows,
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        )))]
        {
            // Unsupported official pack targets (e.g. Linux aarch64) still
            // need a stable value so checks can report a missing asset.
            #[cfg(target_os = "linux")]
            {
                return Self::LinuxX64;
            }
            #[cfg(target_os = "macos")]
            {
                return Self::MacosAarch64;
            }
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            {
                Self::Windows
            }
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Windows => "Windows",
            Self::LinuxX64 => "Linux x64",
            Self::MacosX64 => "macOS x64",
            Self::MacosAarch64 => "macOS aarch64",
        }
    }
}

/// How the current official Linux install was laid down. Unknown prefers the
/// `.deb` (the primary official Linux package) and falls back to the tarball.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxPackageFormat {
    Deb,
    Tar,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdatePhase {
    Idle,
    Checking,
    Available,
    Downloading,
    Downloaded,
    Installing,
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
            Self::Installing => Some("正在安装"),
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
    /// Browser URL of the release's `SHA256SUMS` asset, when present. Download
    /// verifies the installer against this before handing it to the installer.
    pub checksum_url: Option<String>,
    pub release_notes: Option<String>,
    pub release_page_url: Option<String>,
}

impl UpdateInfo {
    pub fn current_only(current_version: &str) -> Self {
        Self {
            ok: true,
            error: None,
            current_version: current_version.to_string(),
            latest_version: None,
            has_update: false,
            download_url: None,
            asset_size: None,
            checksum_url: None,
            release_notes: None,
            release_page_url: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UpdateProgress {
    pub downloaded: u64,
    pub total: u64,
    pub percent: f32,
}

/// Observable stage of the install phase. The elevated transaction root is
/// intentionally unreadable from this medium-integrity process, so stages
/// come from the installer process itself (launched / still running /
/// exited and being verified), never from polling private transaction files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallStage {
    Launching,
    Running,
    Verifying,
}

impl InstallStage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Launching => "已启动安装程序等待系统提权确认…",
            Self::Running => "正在安装请不要关闭安装程序窗口…",
            Self::Verifying => "安装程序已退出正在确认安装结果…",
        }
    }

    /// Determinate progress is impossible here (Inno owns the copy loop),
    /// so the card shows an indeterminate bar for every install stage.
    pub fn bar_percent(self) -> Option<f32> {
        let _ = self;
        None
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct OfficialUpdateState {
    pub busy: bool,
    pub info: Option<UpdateInfo>,
    pub progress: Option<UpdateProgress>,
    pub downloaded_path: Option<String>,
    pub install_stage: Option<InstallStage>,
}

pub fn updater_allowed(official_product: bool) -> bool {
    official_product
}

pub fn should_auto_check(official_product: bool, auto_check_enabled: bool, last_unix: i64) -> bool {
    if !official_product || !auto_check_enabled {
        return false;
    }
    let now = now_unix();
    last_unix <= 0 || now.saturating_sub(last_unix) >= AUTO_CHECK_INTERVAL_SECS as i64
}

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn derive_update_phase(
    busy: bool,
    progress: Option<&UpdateProgress>,
    info: Option<&UpdateInfo>,
    downloaded_path: Option<&str>,
) -> UpdatePhase {
    if busy && downloaded_path.is_some() {
        return UpdatePhase::Installing;
    }
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

/// Progress bar for the About update card. `percent` is `None` for an
/// indeterminate (loading) bar, `Some(0..=100)` for a determinate one.
#[derive(Debug, Clone, PartialEq)]
pub struct CardProgressBar {
    pub percent: Option<f32>,
}

/// View-model for the About update card. Official and isolated copy are
/// decided here so tests can assert enablement without driving GPUI.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateCardModel {
    pub official: bool,
    pub title: String,
    pub version_line: String,
    pub body: String,
    pub button_label: String,
    pub button_enabled: bool,
    pub auto_check_label: String,
    pub status: Option<String>,
    pub progress_text: Option<String>,
    pub progress_bar: Option<CardProgressBar>,
}

pub fn update_card_model(
    official_product: bool,
    current_version: &str,
    state: &OfficialUpdateState,
    auto_check: bool,
) -> UpdateCardModel {
    let _ = auto_check;
    if !updater_allowed(official_product) {
        return UpdateCardModel {
            official: false,
            title: "应用更新".into(),
            version_line: format!("当前版本 {current_version}"),
            body: "当前程序不能在这里检查或安装更新。请使用已安装的 FindX。".into(),
            button_label: "检查更新".into(),
            button_enabled: false,
            auto_check_label: "自动检查更新".into(),
            status: None,
            progress_text: None,
            progress_bar: None,
        };
    }
    let phase = derive_update_phase(
        state.busy,
        state.progress.as_ref(),
        state.info.as_ref(),
        state.downloaded_path.as_deref(),
    );
    let mut body =
        "从 GitHub Releases 检查正式版本，下载 FindX 安装包后以管理员权限安装（会显示安装程序进度窗口）。".to_string();
    let mut status = phase.label().map(|s| s.to_string());
    if let Some(info) = state.info.as_ref() {
        if let Some(err) = info.error.as_ref() {
            status = Some(format!("检查失败：{err}"));
        } else if info.has_update {
            if let Some(latest) = info.latest_version.as_ref() {
                let mut line = format!("发现新版本 {latest}");
                if let Some(size) = info.asset_size {
                    line.push_str(&format!("（{}）", format_asset_size(size)));
                }
                status = Some(line);
            }
            if let Some(notes) = info.release_notes.as_ref() {
                body = notes.clone();
            }
            if info.download_url.is_none() {
                if let Some(err) = info.error.as_ref() {
                    status = Some(err.clone());
                } else {
                    status = Some("发现新版本，但此发行没有当前平台的官方安装包".into());
                }
            }
        } else if info.ok {
            if let Some(latest) = info.latest_version.as_ref() {
                status = Some(format!("已是最新（{latest}）"));
            }
        }
    }
    // During install the stale 100% download text would be misleading; the
    // install stage owns the progress line instead.
    let (progress_text, progress_bar) = if matches!(phase, UpdatePhase::Installing) {
        let text = state
            .install_stage
            .map(|s| s.label().to_string())
            .unwrap_or_else(|| "正在安装…".into());
        (Some(text), Some(CardProgressBar { percent: None }))
    } else {
        let text = state.progress.as_ref().map(|p| {
            format!(
                "已下载 {} / {}（{:.0}%）",
                format_asset_size(p.downloaded),
                format_asset_size(p.total),
                p.percent
            )
        });
        let bar = state.progress.as_ref().map(|p| CardProgressBar {
            percent: Some(p.percent.clamp(0.0, 100.0)),
        });
        (text, bar)
    };
    let missing_platform_asset = matches!(phase, UpdatePhase::Available)
        && state
            .info
            .as_ref()
            .is_some_and(|info| info.download_url.is_none());
    let (button_label, button_enabled) = match phase {
        UpdatePhase::Checking => ("检查中…".into(), false),
        UpdatePhase::Installing => ("安装中…".into(), false),
        UpdatePhase::Downloading => ("下载中…".into(), false),
        UpdatePhase::Available if missing_platform_asset => ("暂无本平台安装包".into(), false),
        UpdatePhase::Available => ("下载并安装".into(), true),
        UpdatePhase::Downloaded => ("安装更新".into(), true),
        UpdatePhase::Idle | UpdatePhase::Current | UpdatePhase::Error => ("检查更新".into(), true),
    };
    UpdateCardModel {
        official: true,
        title: "应用更新".into(),
        version_line: format!("当前版本 {current_version}"),
        body,
        button_label,
        button_enabled,
        auto_check_label: "自动检查更新".into(),
        status,
        progress_text,
        progress_bar,
    }
}

pub fn official_card_uses_placeholder(model: &UpdateCardModel) -> bool {
    let blob = format!(
        "{} {} {} {}",
        model.title, model.version_line, model.body, model.button_label
    );
    !model.official
        || blob.contains("native-dev")
        || blob.contains("不可操作")
        || blob.contains("占位")
        || blob.contains("虚构")
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Debug, Deserialize)]
struct GhAsset {
    name: String,
    #[serde(rename = "browser_download_url")]
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

pub fn is_forbidden_asset_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("isolated")
        || n.contains("fixture")
        || n.contains("drill")
        || n.contains("tauri")
        || n.contains("webview")
}

/// Windows official Inno setup. Kept as a named helper for existing tests.
pub fn is_official_setup_asset_name(name: &str) -> bool {
    is_official_asset_name(name, UpdatePlatform::Windows)
}

pub fn is_official_asset_name(name: &str, platform: UpdatePlatform) -> bool {
    let n = name.to_ascii_lowercase();
    if is_forbidden_asset_name(&n) {
        return false;
    }
    if !n.starts_with("findx-") {
        return false;
    }
    // CLI-only sidecars are never the official GUI installer.
    if n.contains("-cli.") || n.ends_with("-cli.tar.gz") {
        return false;
    }
    let suffixes: &[&str] = match platform {
        UpdatePlatform::Windows => &["-setup.exe"],
        UpdatePlatform::LinuxX64 => &["-linux-x64.deb", "-linux-x64.tar.gz"],
        UpdatePlatform::MacosX64 => &["-macos-x64.dmg"],
        UpdatePlatform::MacosAarch64 => &["-macos-aarch64.dmg"],
    };
    suffixes.iter().any(|suffix| {
        n.strip_prefix("findx-")
            .and_then(|s| s.strip_suffix(suffix))
            .is_some_and(|v| {
                semver::Version::parse(v).is_ok_and(|v| v.pre.is_empty() && v.build.is_empty())
            })
    })
}

pub fn pick_windows_setup_asset_name(names: &[&str]) -> Option<String> {
    pick_official_asset_name(names, UpdatePlatform::Windows, LinuxPackageFormat::Unknown)
}

pub fn pick_official_asset_name(
    names: &[&str],
    platform: UpdatePlatform,
    linux_format: LinuxPackageFormat,
) -> Option<String> {
    let matches: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| is_official_asset_name(n, platform))
        .collect();
    if matches.is_empty() {
        return None;
    }
    if platform == UpdatePlatform::LinuxX64 {
        let deb = matches
            .iter()
            .find(|n| n.to_ascii_lowercase().ends_with(".deb"))
            .copied();
        let tar = matches
            .iter()
            .find(|n| n.to_ascii_lowercase().ends_with(".tar.gz"))
            .copied();
        return match linux_format {
            LinuxPackageFormat::Deb => deb,
            LinuxPackageFormat::Tar => tar,
            LinuxPackageFormat::Unknown => deb.or(tar),
        }
        .map(|s| s.to_string());
    }
    matches.first().map(|s| (*s).to_string())
}

fn pick_official_asset<'a>(
    assets: &'a [GhAsset],
    platform: UpdatePlatform,
    linux_format: LinuxPackageFormat,
    version: &semver::Version,
) -> Option<&'a GhAsset> {
    let prefix = format!("findx-{version}-");
    let names: Vec<&str> = assets
        .iter()
        .filter(|a| {
            a.name.to_ascii_lowercase().starts_with(&prefix)
                && validate_official_asset_url(&a.browser_download_url, platform)
                    .ok()
                    .as_deref()
                    == Some(a.name.as_str())
        })
        .map(|a| a.name.as_str())
        .collect();
    let chosen = pick_official_asset_name(&names, platform, linux_format)?;
    assets.iter().find(|a| a.name == chosen)
}

/// Read a sibling `FindX.pkgformat` (`deb` or `tar`) next to the running exe.
pub fn detect_linux_package_format(exe: &Path) -> LinuxPackageFormat {
    let path = exe.with_file_name("FindX.pkgformat");
    match std::fs::read_to_string(path) {
        Ok(s) => match s.trim().to_ascii_lowercase().as_str() {
            "deb" => LinuxPackageFormat::Deb,
            "tar" | "tar.gz" | "tarball" => LinuxPackageFormat::Tar,
            _ => LinuxPackageFormat::Unknown,
        },
        Err(_) => LinuxPackageFormat::Unknown,
    }
}

pub fn current_linux_package_format() -> LinuxPackageFormat {
    std::env::current_exe()
        .ok()
        .map(|exe| detect_linux_package_format(&exe))
        .unwrap_or(LinuxPackageFormat::Unknown)
}

pub fn version_from_release_tag(tag: &str) -> Option<semver::Version> {
    let tag = tag.trim();
    let core = tag.strip_prefix('v').unwrap_or(tag);
    if let Ok(v) = semver::Version::parse(core) {
        return Some(v);
    }
    let chars: Vec<char> = tag.chars().collect();
    let mut best: Option<semver::Version> = None;
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            let slice: String = chars[start..i].iter().collect();
            if slice.matches('.').count() >= 2 {
                if let Ok(v) = semver::Version::parse(&slice) {
                    best = match best {
                        Some(ref b) if *b >= v => best,
                        _ => Some(v),
                    };
                }
            }
            continue;
        }
        i += 1;
    }
    best
}

fn truncate_notes(body: &Option<String>) -> Option<String> {
    body.as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| {
            let mut t: String = s.chars().take(NOTES_LIMIT).collect();
            if s.chars().count() > NOTES_LIMIT {
                t.push('…');
            }
            t
        })
}

fn error_info(current_version: &str, error: String) -> UpdateInfo {
    UpdateInfo {
        ok: false,
        error: Some(error),
        current_version: current_version.to_string(),
        latest_version: None,
        has_update: false,
        download_url: None,
        asset_size: None,
        checksum_url: None,
        release_notes: None,
        release_page_url: None,
    }
}

/// Evaluate a GitHub /releases/latest JSON body against the running version
/// on the current platform.
pub fn evaluate_latest_release(current_version: &str, status: u16, body: &str) -> UpdateInfo {
    evaluate_latest_release_for(
        current_version,
        status,
        body,
        UpdatePlatform::current(),
        current_linux_package_format(),
    )
}

/// Evaluate a GitHub /releases/latest JSON body for an explicit platform.
/// Used by the live client (via [`evaluate_latest_release`]) and by tests.
pub fn evaluate_latest_release_for(
    current_version: &str,
    status: u16,
    body: &str,
    platform: UpdatePlatform,
    linux_format: LinuxPackageFormat,
) -> UpdateInfo {
    let current = match semver::Version::parse(current_version.trim().trim_start_matches('v')) {
        Ok(v) => v,
        Err(e) => {
            return error_info(
                current_version,
                format!("当前版本号无法解析为语义化版本：{e}"),
            );
        }
    };
    if status == 404 {
        return UpdateInfo {
            ok: true,
            error: None,
            current_version: current_version.to_string(),
            latest_version: None,
            has_update: false,
            download_url: None,
            asset_size: None,
            checksum_url: None,
            release_notes: None,
            release_page_url: Some(format!(
                "https://github.com/{GITHUB_OWNER}/{GITHUB_REPO}/releases"
            )),
        };
    }
    if !(200..300).contains(&status) {
        let tail: String = body.chars().take(200).collect();
        return error_info(
            current_version,
            format!(
                "GitHub API 返回 HTTP {status}{}",
                if tail.is_empty() {
                    String::new()
                } else {
                    format!("：{tail}")
                }
            ),
        );
    }
    let gh: GhRelease = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return error_info(current_version, format!("解析 GitHub 响应失败：{e}")),
    };
    if gh.draft || gh.prerelease {
        return error_info(current_version, "最新发行不是正式版本，已忽略。".into());
    }
    let tag_display = gh.tag_name.clone();
    let latest = match version_from_release_tag(&gh.tag_name) {
        Some(v) => v,
        None => {
            return UpdateInfo {
                ok: true,
                error: Some(format!(
                    "最新发行标签「{tag_display}」无法解析为语义化版本，请手动对照"
                )),
                current_version: current_version.to_string(),
                latest_version: Some(tag_display),
                has_update: false,
                download_url: None,
                asset_size: None,
                checksum_url: find_checksum_asset_url(&gh.assets),
                release_notes: truncate_notes(&gh.body),
                release_page_url: Some(gh.html_url),
            };
        }
    };
    let has_update = latest > current;
    let setup = pick_official_asset(&gh.assets, platform, linux_format, &latest);
    let missing = has_update && setup.is_none();
    let error = if missing {
        Some(format!(
            "发现新版本 {tag_display}，但此发行没有当前平台（{}）的官方安装包。",
            platform.label()
        ))
    } else {
        None
    };
    UpdateInfo {
        ok: true,
        error,
        current_version: current_version.to_string(),
        latest_version: Some(tag_display),
        has_update,
        download_url: if has_update {
            setup.map(|a| a.browser_download_url.clone())
        } else {
            None
        },
        asset_size: if has_update {
            setup.map(|a| a.size)
        } else {
            None
        },
        checksum_url: if has_update {
            find_checksum_asset_url(&gh.assets)
        } else {
            None
        },
        release_notes: truncate_notes(&gh.body),
        release_page_url: Some(gh.html_url),
    }
}

pub fn check_latest_release(current_version: &str) -> UpdateInfo {
    match fetch_latest_release() {
        Ok((status, body)) => evaluate_latest_release(current_version, status, &body),
        Err(e) => error_info(current_version, format!("请求 GitHub 失败：{e}")),
    }
}

fn fetch_latest_release() -> Result<(u16, String), String> {
    let resp = ureq::get(LATEST_API_URL)
        .set("User-Agent", USER_AGENT_CHECK)
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let body = resp.into_string().map_err(|e| e.to_string())?;
    Ok((status, body))
}

pub fn validate_setup_download_url(url: &str) -> Result<String, String> {
    validate_official_asset_url(url, UpdatePlatform::current())
}

pub fn validate_official_asset_url(url: &str, platform: UpdatePlatform) -> Result<String, String> {
    let url = url.trim();
    if !url.starts_with("https://") {
        return Err("下载地址不是 https 链接，已拒绝。".into());
    }
    let release_path = url
        .strip_prefix("https://github.com/chaojimct/findx/releases/download/")
        .ok_or("下载地址不属于官方 FindX 发行仓库")?;
    let (tag, file) = release_path.split_once('/').ok_or("发行资产路径无效")?;
    if tag.is_empty()
        || file.contains('/')
        || semver::Version::parse(tag.trim_start_matches('v')).is_err()
    {
        return Err("发行标签/资产路径无效".into());
    }
    let asset_name = file.to_string();
    if !is_official_asset_name(&asset_name, platform) {
        return Err(format!("拒绝非官方安装包资产：{asset_name}"));
    }
    Ok(asset_name)
}

/// Whether an asset is the release's checksum manifest. Accepts the plain
/// name and a `.txt` suffix, case-insensitively.
pub fn is_checksum_asset_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == CHECKSUM_ASSET_NAME.to_ascii_lowercase()
        || n == format!("{}.txt", CHECKSUM_ASSET_NAME.to_ascii_lowercase())
}

/// Locate the checksum manifest among release assets.
pub fn find_checksum_asset_url(assets: &[GhAsset]) -> Option<String> {
    assets
        .iter()
        .find(|a| is_checksum_asset_name(&a.name))
        .map(|a| a.browser_download_url.clone())
}

/// Only accept a checksum manifest served from the official release path.
/// Reusing the installer's URL rules would reject it (it is not an installer),
/// so the path/tag checks here mirror those rules for this one fixed filename.
pub fn validate_checksums_url(url: &str) -> Result<String, String> {
    let url = url.trim();
    if !url.starts_with("https://") {
        return Err("校验和下载地址不是 https 链接，已拒绝。".into());
    }
    let release_path = url
        .strip_prefix("https://github.com/chaojimct/findx/releases/download/")
        .ok_or("校验和下载地址不属于官方 FindX 发行仓库")?;
    let (tag, file) = release_path.split_once('/').ok_or("校验和路径无效")?;
    if tag.is_empty()
        || file.contains('/')
        || semver::Version::parse(tag.trim_start_matches('v')).is_err()
        || !is_checksum_asset_name(file)
    {
        return Err("校验和标签/文件名无效".into());
    }
    Ok(file.to_string())
}

/// Extract the expected hex digest for `asset_name` from a sha256sum manifest.
///
/// Standard `sha256sum` output is `<64 hex>  <name>` (two spaces) or
/// `<64 hex> *<name>` (binary marker); the name may be prefixed with `./`.
/// Matching is on the basename, case-insensitively. Anything that is not a
/// well-formed 64-hex digest is ignored, so a malformed manifest is treated
/// as "no checksum available" rather than as a match.
pub fn parse_sha256sums(text: &str, asset_name: &str) -> Option<String> {
    let wanted = asset_name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(asset_name)
        .to_ascii_lowercase();
    for line in text.lines() {
        let line = line.trim().trim_start_matches('\u{feff}');
        // Split on the first run of whitespace after the digest.
        let Some(split) = line.find(char::is_whitespace) else {
            continue;
        };
        let (hash, rest) = line.split_at(split);
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let name = rest
            .trim_start_matches(char::is_whitespace)
            .trim_start_matches('*')
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if name == wanted {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

/// SHA-256 over a byte slice, returned as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let out = hasher.finalize();
    let mut s = String::with_capacity(out.len() * 2);
    for byte in out.iter() {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

/// Streamed SHA-256 over a file, so a large installer is never held in memory.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let out = hasher.finalize();
    let mut s = String::with_capacity(out.len() * 2);
    for byte in out.iter() {
        s.push_str(&format!("{byte:02x}"));
    }
    Ok(s)
}

/// Fetch the manifest and return the expected digest for one asset.
fn fetch_expected_sha256(checksum_url: &str, asset_name: &str) -> Result<String, String> {
    validate_checksums_url(checksum_url)?;
    let resp = ureq::get(checksum_url)
        .set("User-Agent", USER_AGENT_CHECK)
        .call()
        .map_err(|e| format!("请求校验和清单失败：{e}"))?;
    if !(200..300).contains(&resp.status()) {
        return Err(format!("校验和清单返回 HTTP {}", resp.status()));
    }
    let text = resp.into_string().map_err(|e| e.to_string())?;
    parse_sha256sums(&text, asset_name).ok_or_else(|| {
        format!("校验和清单里没有安装包 {asset_name} 的记录，拒绝安装。")
    })
}

pub fn download_dest_path(asset_name: &str) -> PathBuf {
    std::env::temp_dir()
        .join(format!(
            "FindX-download-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
        .join(asset_name)
}

pub fn download_official_setup(
    url: &str,
    expected_size: Option<u64>,
    checksum_url: Option<&str>,
    mut on_progress: impl FnMut(UpdateProgress),
) -> Result<PathBuf, String> {
    let asset_name = validate_setup_download_url(url)?;
    let dest = download_dest_path(&asset_name);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir(parent)
            .map_err(|e| format!("创建独占临时目录失败（{}）：{e}", parent.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
    }
    let resp = ureq::get(url)
        .set("User-Agent", USER_AGENT_DOWNLOAD)
        .call()
        .map_err(|e| format!("请求下载失败：{e}"))?;
    let status = resp.status();
    if !(200..300).contains(&status) {
        return Err(format!("下载返回 HTTP {status}"));
    }
    let total: u64 = resp
        .header("Content-Length")
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    if total == 0 {
        return Err("响应缺少 Content-Length，无法校验完整性，已中止。".into());
    }
    if let Some(expected) = expected_size {
        if expected != 0 && total != expected {
            return Err(format!(
                "安装包大小与发行信息不一致（元数据 {expected} B，实际 {total} B），已中止。"
            ));
        }
    }
    let mut reader = resp.into_reader();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&dest)
        .map_err(|e| format!("创建临时文件失败（{}）：{e}", dest.display()))?;
    let mut buf = [0u8; 64 * 1024];
    let mut downloaded: u64 = 0;
    let mut last_emit = std::time::Instant::now();
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("读取下载流失败：{e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| format!("写入临时文件失败：{e}"))?;
        downloaded += n as u64;
        if last_emit.elapsed() >= PROGRESS_EMIT_INTERVAL {
            last_emit = std::time::Instant::now();
            on_progress(UpdateProgress {
                downloaded,
                total,
                percent: (downloaded as f32 / total as f32) * 100.0,
            });
        }
    }
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    if downloaded != total {
        let _ = std::fs::remove_file(&dest);
        return Err(format!(
            "下载数据不完整（{downloaded}/{total} B），已删除临时文件。"
        ));
    }
    // Size alone is not integrity. When the release published a checksum
    // manifest, verify the bytes we actually wrote before the installer is
    // ever allowed to run. A missing/malformed manifest or a mismatch is a
    // hard failure and the temp file is removed.
    if let Some(url) = checksum_url {
        let expected = match fetch_expected_sha256(url, &asset_name) {
            Ok(v) => v,
            Err(e) => {
                let _ = std::fs::remove_file(&dest);
                return Err(e);
            }
        };
        let actual = sha256_file(&dest)?;
        if !actual.eq_ignore_ascii_case(&expected) {
            let _ = std::fs::remove_file(&dest);
            return Err(format!(
                "安装包校验和不匹配（期望 {expected}，实际 {actual}），已删除临时文件，拒绝安装。"
            ));
        }
    }
    on_progress(UpdateProgress {
        downloaded,
        total,
        percent: 100.0,
    });
    Ok(dest)
}

/// Privilege helper and argument vector used to apply a downloaded official
/// package. Tests assert this plan without launching pkexec/osascript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallPlan {
    pub program: String,
    pub args: Vec<String>,
}

pub fn privileged_wrapper() -> Option<String> {
    // Do not run a PATH-supplied privilege helper. sudo is non-interactive:
    // a GUI without a terminal must never hang waiting for a password on stdin.
    for candidate in ["/usr/bin/pkexec", "/usr/bin/sudo"] {
        if Path::new(candidate).is_file() {
            return Some(candidate.into());
        }
    }
    None
}

fn which(name: &str) -> Option<PathBuf> {
    if let Ok(paths) = std::env::var("PATH") {
        let sep = if cfg!(windows) { ';' } else { ':' };
        for dir in paths.split(sep) {
            if dir.is_empty() {
                continue;
            }
            let p = PathBuf::from(dir).join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// Extract as the unprivileged user. Reject links, traversal, duplicate paths,
/// unexpected layout and decompression bombs before invoking any root helper.
pub fn unpack_linux_update(path: &Path) -> Result<PathBuf, String> {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("安装包名称无效")?;
    if !is_official_asset_name(name, UpdatePlatform::LinuxX64) || !name.ends_with(".tar.gz") {
        return Err("非官方 tar 包".into());
    }
    let root = name.trim_end_matches(".tar.gz");
    let dir = std::env::temp_dir().join(format!(
        "FindX-update-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    let result = (|| {
        let input = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(input));
        let mut seen = std::collections::HashSet::new();
        let mut total = 0u64;
        for entry in archive.entries().map_err(|e| e.to_string())? {
            let mut entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path().map_err(|e| e.to_string())?.into_owned();
            if path
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                return Err("tar 路径越界".into());
            }
            let relative = path.strip_prefix(root).map_err(|_| "tar 根目录不匹配")?;
            let kind = entry.header().entry_type();
            if kind.is_dir() && (relative.as_os_str().is_empty() || relative == Path::new("icons"))
            {
                continue;
            }
            let allowed = [
                "FindX",
                "findx2",
                "fx",
                "findx2-service",
                "FindX.installed",
                "FindX.pkgformat",
                "version",
                "install.sh",
                "uninstall.sh",
                "findx.desktop",
                "findx.desktop.in",
                "findx2.service",
                "findx2.service.in",
                "findx.png",
                "icons/findx.png",
            ];
            if !kind.is_file()
                || !allowed.iter().any(|v| relative == Path::new(v))
                || !seen.insert(relative.to_path_buf())
            {
                return Err("tar 含链接/重复/未知文件".into());
            }
            total = total.checked_add(entry.size()).ok_or("tar 大小溢出")?;
            if total > 1024 * 1024 * 1024 {
                return Err("tar 解压大小超过 1GiB".into());
            }
            entry.unpack_in(&dir).map_err(|e| e.to_string())?;
        }
        let stage = dir.join(root);
        for file in [
            "FindX",
            "findx2",
            "fx",
            "findx2-service",
            "FindX.installed",
            "install.sh",
        ] {
            if !stage.join(file).is_file() {
                return Err(format!("tar 缺少 {file}"));
            }
        }
        Ok(stage)
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    result
}

pub fn linux_install_plan(path: &Path) -> Result<InstallPlan, String> {
    let p = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if !p.exists() {
        return Err(format!("安装包不存在：{}", p.display()));
    }
    let size = std::fs::metadata(&p).map_err(|e| e.to_string())?.len();
    if size < MIN_SETUP_BYTES {
        return Err(format!("安装包大小异常（{size} B），拒绝执行。"));
    }
    let name = p
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !is_official_asset_name(&name, UpdatePlatform::LinuxX64) {
        return Err(format!("拒绝非官方 Linux 安装包：{name}"));
    }
    let helper = privileged_wrapper().ok_or_else(|| {
        "未找到 pkexec 或 sudo，无法提权安装。请使用官方 .deb/.tar.gz 手动安装。".to_string()
    })?;
    let mut args = if helper.ends_with("/sudo") {
        vec!["-n".to_string()]
    } else {
        vec![]
    };
    let lower = name.to_ascii_lowercase();
    match current_linux_package_format() {
        LinuxPackageFormat::Deb if !lower.ends_with(".deb") => {
            return Err("deb 安装不得通过 tar 更新绕过包管理器".into());
        }
        LinuxPackageFormat::Tar if !lower.ends_with(".tar.gz") => {
            return Err("tar 安装不得静默切换到 deb 包管理器".into());
        }
        _ => {}
    }
    if lower.ends_with(".deb") {
        args.extend([
            "/usr/bin/env".into(),
            "DEBIAN_FRONTEND=noninteractive".into(),
            "/usr/bin/apt-get".into(),
            "install".into(),
            "-y".into(),
            p.to_string_lossy().into_owned(),
        ]);
        return Ok(InstallPlan {
            program: helper,
            args,
        });
    }
    let stage = unpack_linux_update(&p)?;
    args.extend([
        "/bin/bash".into(),
        stage.join("install.sh").to_string_lossy().into_owned(),
        "--stage".into(),
        stage.to_string_lossy().into_owned(),
        "--prefix".into(),
        "/opt/FindX".into(),
    ]);
    Ok(InstallPlan {
        program: helper,
        args,
    })
}

pub fn macos_install_plan(path: &Path) -> Result<InstallPlan, String> {
    let p = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if !p.exists() {
        return Err(format!("安装包不存在：{}", p.display()));
    }
    let size = std::fs::metadata(&p).map_err(|e| e.to_string())?.len();
    if size < MIN_SETUP_BYTES {
        return Err(format!("安装包大小异常（{size} B），拒绝执行。"));
    }
    let name = p
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !(is_official_asset_name(&name, UpdatePlatform::MacosAarch64)
        || is_official_asset_name(&name, UpdatePlatform::MacosX64))
    {
        return Err(format!("拒绝非官方 macOS 安装包：{name}"));
    }
    let dmg = p.to_string_lossy().replace('\'', r"'\''");
    // Explicit mountpoint: never parse hdiutil's whitespace-delimited output.
    // Stage first, keep the previous app, and rollback on a failed rename.
    let inner = format!(
        "set -eu; mp=$(mktemp -d /tmp/FindX-mount-XXXXXX); stage=$(mktemp -d /Applications/.FindX-stage-XXXXXX); trap 'hdiutil detach \"$mp\" >/dev/null 2>&1 || true; rmdir \"$mp\" 2>/dev/null || true; rmdir \"$stage\" 2>/dev/null || true' EXIT; hdiutil attach -nobrowse -readonly -mountpoint \"$mp\" '{dmg}' >/dev/null; test -x \"$mp/FindX.app/Contents/MacOS/FindX\"; test -f \"$mp/FindX.app/Contents/MacOS/FindX.installed\"; ditto \"$mp/FindX.app\" \"$stage/FindX.app\"; backup=/Applications/FindX.backup-$(date +%s)-$$.app; if test -e /Applications/FindX.app; then mv /Applications/FindX.app \"$backup\"; fi; if ! mv \"$stage/FindX.app\" /Applications/FindX.app; then test ! -d \"$backup\" || mv \"$backup\" /Applications/FindX.app; exit 1; fi"
    );
    let escaped = inner.replace('\\', "\\\\").replace('"', "\\\"");
    Ok(InstallPlan {
        program: "osascript".into(),
        args: vec![
            "-e".into(),
            format!("do shell script \"{escaped}\" with administrator privileges"),
        ],
    })
}

/// Outcome of waiting on the elevated installer process (Windows only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupOutcome {
    /// Installer exited 0 and the uninstall entry reports the new version.
    Committed,
    /// Installer exited with [`SETUP_ROLLBACK_EXIT_CODE`]: rolled back,
    /// nothing was changed.
    RolledBack,
    /// Launch failed, the installer exited non-zero, or the installed
    /// version did not flip to the expected one.
    Failed(String),
    /// Still running past the wait budget. The installer is never killed;
    /// the user finishes in the installer's own progress window.
    Timeout,
}

/// How long the GUI waits on the installer process before it stops watching.
/// Copy + Verify/Commit of a ~100MB payload takes a few minutes; 15 minutes
/// covers slow disks while UAC left unconfirmed still resolves as Timeout.
#[cfg(windows)]
pub const SETUP_WAIT_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Whether the registered uninstall entry already reports `expected`.
/// Read-only (`KEY_READ`); never touches transaction state.
#[cfg(windows)]
pub fn installed_version_matches(expected: &str) -> bool {
    read_installed_display_version().is_some_and(|v| v.trim() == expected.trim())
}

#[cfg(windows)]
fn read_installed_display_version() -> Option<String> {
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        HKEY_LOCAL_MACHINE, KEY_READ, REG_SZ, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
    };
    use windows::core::PCWSTR;
    let sub: Vec<u16> = format!(
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{FINDX_UNINSTALL_KEY}"
    )
    .encode_utf16()
    .chain(Some(0))
    .collect();
    let mut hkey = Default::default();
    let rc = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(sub.as_ptr()),
            None,
            KEY_READ,
            &mut hkey,
        )
    };
    if rc != ERROR_SUCCESS {
        return None;
    }
    let name: Vec<u16> = UNINSTALL_DISPLAY_VERSION
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut ty = REG_SZ;
    let mut size: u32 = 0;
    let rc = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut ty),
            None,
            Some(&mut size),
        )
    };
    if rc != ERROR_SUCCESS || size == 0 || size > 256 {
        let _ = unsafe { RegCloseKey(hkey) };
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    let rc = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut ty),
            Some(buf.as_mut_ptr()),
            Some(&mut size),
        )
    };
    let _ = unsafe { RegCloseKey(hkey) };
    if rc != ERROR_SUCCESS {
        return None;
    }
    buf.truncate(size as usize);
    let wide: Vec<u16> = buf
        .chunks_exact(2)
        .map(|c| u16::from_ne_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16(&wide)
        .ok()
        .map(|s| s.trim_end_matches('\0').to_string())
}

/// Launch the setup with `runas`, wait on its process, and confirm the
/// version flip. All blocking work runs on the caller's (worker) thread;
/// `report` is invoked for stage transitions. The installer window itself
/// (`/SILENT`) shows Inno's file-copy progress in parallel.
#[cfg(windows)]
pub fn run_setup_and_wait(
    path: &Path,
    expected_version: &str,
    mut report: impl FnMut(InstallStage),
    timeout: Duration,
) -> SetupOutcome {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows::Win32::UI::Shell::{
        SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
        ShellExecuteExW,
    };
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::PCWSTR;

    report(InstallStage::Launching);
    let p = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if !p.exists() {
        return SetupOutcome::Failed(format!("安装包不存在：{}", p.display()));
    }
    if p.extension().map(|e| e.to_ascii_lowercase()) != Some("exe".into()) {
        return SetupOutcome::Failed("只允许启动 .exe 安装包。".into());
    }
    let size = match std::fs::metadata(&p).map(|m| m.len()) {
        Ok(size) => size,
        Err(e) => return SetupOutcome::Failed(e.to_string()),
    };
    if size < MIN_SETUP_BYTES {
        return SetupOutcome::Failed(format!("安装包大小异常（{size} B），拒绝执行。"));
    }
    if !expected_version.is_empty() && installed_version_matches(expected_version) {
        return SetupOutcome::Failed(format!(
            "已安装版本已是 {expected_version}无需重复安装。"
        ));
    }
    let file: Vec<u16> = p.as_os_str().encode_wide().chain(Some(0)).collect();
    let verb: Vec<u16> = "runas".encode_utf16().chain(Some(0)).collect();
    // Inno setup.exe takes its flags as a single lpParameters string.
    let params: Vec<u16> = SILENT_SETUP_ARGS.encode_utf16().chain(Some(0)).collect();
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    if let Err(error) = unsafe { ShellExecuteExW(&mut info) } {
        // Includes UAC declined: the shell reports the failure, nothing ran.
        return SetupOutcome::Failed(format!(
            "无法启动安装程序（Win32 错误 {}）若取消了提权确认则未开始安装。",
            error.code().0
        ));
    }
    let handle = info.hProcess;
    if handle.is_invalid() {
        return SetupOutcome::Failed("安装程序未能返回进程句柄。".into());
    }
    report(InstallStage::Running);
    // Wait in slices so an over-long install ends our watch, never the install.
    let deadline = std::time::Instant::now() + timeout;
    let exit_code: Option<u32> = loop {
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .unwrap_or_default();
        if remaining.is_zero() {
            break None;
        }
        let slice_ms = remaining.as_millis().min(1000) as u32;
        if unsafe { WaitForSingleObject(handle, slice_ms) } == WAIT_OBJECT_0 {
            let mut code = 1u32;
            let _ = unsafe { GetExitCodeProcess(handle, &mut code) };
            break Some(code);
        }
    };
    let _ = unsafe { CloseHandle(handle) };
    let Some(code) = exit_code else {
        return SetupOutcome::Timeout;
    };
    if code == SETUP_ROLLBACK_EXIT_CODE {
        return SetupOutcome::RolledBack;
    }
    if code != 0 {
        return SetupOutcome::Failed(format!("安装程序退出码 {code}本次未改动。"));
    }
    report(InstallStage::Verifying);
    if expected_version.is_empty() || installed_version_matches(expected_version) {
        SetupOutcome::Committed
    } else {
        SetupOutcome::Failed("安装程序已退出但已安装版本号未变化请检查安装程序窗口。".into())
    }
}

pub fn start_silent_setup(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        // Windows updates go through run_setup_and_wait so the About card can
        // report launch/running/verify stages; this entry stays for Unix.
        let _ = path;
        Err("Windows 更新请走 run_setup_and_wait（带安装进度等待）。".into())
    }
    #[cfg(target_os = "linux")]
    {
        start_silent_setup_linux(path)
    }
    #[cfg(target_os = "macos")]
    {
        start_silent_setup_macos(path)
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = path;
        Err("自动安装不支持当前平台，请到发行页手动下载更新。".into())
    }
}

#[cfg(unix)]
fn run_unix_install_plan(plan: &InstallPlan) -> Result<std::process::ExitStatus, String> {
    // Refuse an unknown or redirected service before any privileged replacement.
    // A missing user manager is acceptable for manual-launch installs; an installed
    // but invalid unit is not. Query only when the official unit/plist is installed.
    let registered = if cfg!(target_os = "linux") {
        Path::new("/usr/lib/systemd/user/findx2.service").exists()
    } else {
        std::env::var_os("HOME").is_some_and(|h| {
            PathBuf::from(h)
                .join("Library/LaunchAgents/com.findx.findx2.plist")
                .exists()
        })
    };
    let service = if registered {
        Some(crate::service_control::snapshot()?)
    } else {
        None
    };
    let running = service.as_ref().is_some_and(|s| s.state == "Running");
    if running {
        crate::service_control::set_running_for_update(false)?;
    }
    let result = std::process::Command::new(&plan.program)
        .args(&plan.args)
        .status()
        .map_err(|e| e.to_string());
    let mut reload_failed = false;
    #[cfg(target_os = "linux")]
    if registered {
        let reload = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
        reload_failed = !reload.is_ok_and(|s| s.success());
    }
    let restore = if running {
        crate::service_control::set_running_for_update(true)
    } else {
        Ok(())
    };
    if let Some(i) = plan.args.iter().position(|a| a == "--stage") {
        if let Some(stage) = plan.args.get(i + 1).map(PathBuf::from) {
            if let Some(parent) = stage.parent().filter(|p| {
                p.parent() == Some(std::env::temp_dir().as_path())
                    && p.file_name()
                        .is_some_and(|s| s.to_string_lossy().starts_with("FindX-update-"))
            }) {
                let _ = std::fs::remove_dir_all(parent);
            }
        }
    }
    restore.map_err(|e| {
        format!("安装结果 {result:?}；用户服务恢复失败: {e}；daemon-reload 失败={reload_failed}")
    })?;
    if reload_failed {
        return Err(format!(
            "安装结果 {result:?}；已尝试恢复原用户服务，但 daemon-reload 失败，请检查用户会话"
        ));
    }
    result
}

#[cfg(target_os = "linux")]
fn start_silent_setup_linux(path: &Path) -> Result<(), String> {
    let plan = linux_install_plan(path)?;
    let status = run_unix_install_plan(&plan)?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "安装程序退出码 {:?}。若取消了提权提示，则未开始安装。",
            status.code()
        ))
    }
}

#[cfg(target_os = "macos")]
fn start_silent_setup_macos(path: &Path) -> Result<(), String> {
    let plan = macos_install_plan(path)?;
    let status = run_unix_install_plan(&plan)?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "安装程序退出码 {:?}。若取消了管理员授权，则未开始安装。",
            status.code()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_matches_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn parse_sha256sums_handles_real_formats_and_skips_garbage() {
        let sums = concat!(
            "# a comment line with spaces\n",
            "not-a-hash  FindX-2.5.10-setup.exe\n",
            " BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD  ",
            "./FindX-2.5.10-setup.exe\n",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 *FindX-2.5.10-linux-x64.deb\n",
        );
        assert_eq!(
            parse_sha256sums(sums, "FindX-2.5.10-setup.exe").as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        assert_eq!(
            parse_sha256sums(sums, "other/FindX-2.5.10-linux-x64.deb").as_deref(),
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
        // Basename matching is case-insensitive; missing names yield None.
        assert!(parse_sha256sums(sums, "FINDX-2.5.10-SETUP.EXE").is_some());
        assert!(parse_sha256sums(sums, "FindX-2.5.10-mac.dmg").is_none());
        assert!(parse_sha256sums("", "anything").is_none());
    }

    #[test]
    fn checksum_asset_is_recognised_and_url_is_validated() {
        assert!(is_checksum_asset_name("SHA256SUMS"));
        assert!(is_checksum_asset_name("sha256sums.txt"));
        assert!(!is_checksum_asset_name("FindX-2.5.10-setup.exe"));

        let assets = [
            GhAsset {
                name: "FindX-2.5.10-setup.exe".into(),
                browser_download_url: "https://github.com/chaojimct/findx/releases/download/v2.5.10/FindX-2.5.10-setup.exe".into(),
                size: 1,
            },
            GhAsset {
                name: "SHA256SUMS".into(),
                browser_download_url: "https://github.com/chaojimct/findx/releases/download/v2.5.10/SHA256SUMS".into(),
                size: 1,
            },
        ];
        assert_eq!(
            find_checksum_asset_url(&assets).as_deref(),
            Some("https://github.com/chaojimct/findx/releases/download/v2.5.10/SHA256SUMS")
        );
        assert!(find_checksum_asset_url(&assets[..1]).is_none());

        assert!(validate_checksums_url(
            "https://github.com/chaojimct/findx/releases/download/v2.5.10/SHA256SUMS"
        )
        .is_ok());
        assert!(validate_checksums_url(
            "http://github.com/chaojimct/findx/releases/download/v2.5.10/SHA256SUMS"
        )
        .is_err());
        assert!(validate_checksums_url("https://evil.example/SHA256SUMS").is_err());
        assert!(validate_checksums_url(
            "https://github.com/chaojimct/findx/releases/download/v2.5.10/FindX-2.5.10-setup.exe"
        )
        .is_err());
    }

    #[test]
    fn release_evaluation_carries_checksum_url_for_updates_only() {
        let sums = "https://github.com/chaojimct/findx/releases/download/v2.5.10/SHA256SUMS";
        let setup = "https://github.com/chaojimct/findx/releases/download/v2.5.10/FindX-2.5.10-setup.exe";
        let body = sample_release(
            "v2.5.10",
            &[
                ("FindX-2.5.10-setup.exe", setup, 42),
                ("SHA256SUMS", sums, 100),
            ],
            false,
            false,
        );
        let info = evaluate_latest_release_for(
            "2.5.2",
            200,
            &body,
            UpdatePlatform::Windows,
            LinuxPackageFormat::Unknown,
        );
        assert!(info.has_update);
        assert_eq!(info.checksum_url.as_deref(), Some(sums));

        // No update → no checksum URL (nothing will be downloaded).
        let current = evaluate_latest_release_for(
            "2.5.10",
            200,
            &body,
            UpdatePlatform::Windows,
            LinuxPackageFormat::Unknown,
        );
        assert!(!current.has_update);
        assert!(current.checksum_url.is_none());
    }

    fn sample_release(tag: &str, assets: &[(&str, &str, u64)], draft: bool, pre: bool) -> String {
        let assets_json: Vec<String> = assets
            .iter()
            .map(|(name, url, size)| {
                format!(r#"{{"name":"{name}","browser_download_url":"{url}","size":{size}}}"#)
            })
            .collect();
        format!(
            r#"{{"tag_name":"{tag}","html_url":"https://github.com/chaojimct/findx/releases/tag/{tag}","draft":{draft},"prerelease":{pre},"body":"notes for {tag}","assets":[{}]}}"#,
            assets_json.join(",")
        )
    }

    #[test]
    fn version_compare_and_asset_pick() {
        let official =
            "https://github.com/chaojimct/findx/releases/download/v2.5.2/FindX-2.5.2-setup.exe";
        let fixture = "https://example.com/FindX-2.5.2-isolated-fixture-setup.exe";
        let body = sample_release(
            "v2.5.2",
            &[
                ("FindX-2.5.2-isolated-fixture-setup.exe", fixture, 100),
                ("FindX-2.5.2-setup.exe", official, 10_940_635),
            ],
            false,
            false,
        );
        let ev = |cur: &str| {
            evaluate_latest_release_for(
                cur,
                200,
                &body,
                UpdatePlatform::Windows,
                LinuxPackageFormat::Unknown,
            )
        };
        let newer = ev("2.5.1");
        assert!(newer.ok && newer.has_update);
        assert_eq!(newer.latest_version.as_deref(), Some("v2.5.2"));
        assert_eq!(newer.download_url.as_deref(), Some(official));
        assert_eq!(newer.asset_size, Some(10_940_635));
        assert!(
            !newer
                .release_notes
                .as_deref()
                .unwrap_or("")
                .contains("占位")
        );

        let current = ev("2.5.2");
        assert!(current.ok && !current.has_update);
        assert_eq!(current.download_url, None);

        let older_tag = ev("2.5.3");
        assert!(older_tag.ok && !older_tag.has_update);

        assert_eq!(
            pick_windows_setup_asset_name(&[
                "FindX-2.5.2-isolated-fixture-setup.exe",
                "FindX-2.5.2-setup.exe"
            ])
            .as_deref(),
            Some("FindX-2.5.2-setup.exe")
        );
        assert_eq!(
            version_from_release_tag("v2.5.2").unwrap().to_string(),
            "2.5.2"
        );
        assert_eq!(
            version_from_release_tag("gui-2.0.2").unwrap().to_string(),
            "2.0.2"
        );
    }

    #[test]
    fn ignores_draft_and_fixture_only_assets() {
        let body = sample_release(
            "v2.5.9",
            &[(
                "FindX-2.5.9-setup.exe",
                "https://github.com/chaojimct/findx/releases/download/v2.5.9/FindX-2.5.9-setup.exe",
                12,
            )],
            true,
            false,
        );
        let info = evaluate_latest_release("2.5.2", 200, &body);
        assert!(!info.ok);
        assert!(info.error.as_deref().unwrap_or("").contains("不是正式版本"));

        let only_fixture = sample_release(
            "v2.5.3",
            &[(
                "FindX-2.5.3-isolated-fixture-setup.exe",
                "https://example.invalid/FindX-2.5.3-isolated-fixture-setup.exe",
                12,
            )],
            false,
            false,
        );
        let info = evaluate_latest_release("2.5.2", 200, &only_fixture);
        assert!(info.ok && info.has_update);
        assert_eq!(info.download_url, None);
    }

    #[test]
    fn official_card_is_enabled_and_has_no_placeholder() {
        let official = update_card_model(true, "2.5.2", &OfficialUpdateState::default(), false);
        assert!(official.official);
        assert!(official.button_enabled);
        assert_eq!(official.button_label, "检查更新");
        assert_eq!(official.version_line, "当前版本 2.5.2");
        assert!(!official_card_uses_placeholder(&official));
        assert!(!official.version_line.contains("native-dev"));

        let mut available = OfficialUpdateState::default();
        available.info = Some(evaluate_latest_release_for(
            "2.5.2",
            200,
            &sample_release(
                "v2.5.3",
                &[(
                    "FindX-2.5.3-setup.exe",
                    "https://github.com/chaojimct/findx/releases/download/v2.5.3/FindX-2.5.3-setup.exe",
                    11_000_000,
                )],
                false,
                false,
            ),
            UpdatePlatform::Windows,
            LinuxPackageFormat::Unknown,
        ));
        let model = update_card_model(true, "2.5.2", &available, true);
        assert_eq!(model.button_label, "下载并安装");
        assert!(model.button_enabled);
        assert!(model.status.as_deref().unwrap_or("").contains("2.5.3"));
        assert!(!official_card_uses_placeholder(&model));
    }

    #[test]
    fn isolated_card_stays_disabled_without_fiction() {
        let model = update_card_model(false, "2.5.2", &OfficialUpdateState::default(), true);
        assert!(!model.official);
        assert!(!model.button_enabled);
        assert_eq!(model.button_label, "检查更新");
        assert_eq!(model.version_line, "当前版本 2.5.2");
        assert!(!model.version_line.contains("native-dev"));
        assert!(!model.body.contains("native-dev"));
        assert!(!model.title.contains('·'));
        assert!(!model.body.contains("2.4.11"));
        assert!(!model.body.contains("占位"));
        assert!(should_auto_check(false, true, 0) == false);
        assert!(should_auto_check(true, false, 0) == false);
        assert!(should_auto_check(true, true, 0));
        assert!(!should_auto_check(true, true, now_unix()));
    }

    #[test]
    fn phase_order_matches_webview() {
        let progress = UpdateProgress {
            downloaded: 10,
            total: 100,
            percent: 10.0,
        };
        assert_eq!(
            derive_update_phase(true, None, None, None),
            UpdatePhase::Checking
        );
        assert_eq!(
            derive_update_phase(true, Some(&progress), None, None),
            UpdatePhase::Downloading
        );
        assert_eq!(
            derive_update_phase(false, None, None, Some(r"C:\tmp\setup.exe")),
            UpdatePhase::Downloaded
        );
        let err = error_info("2.5.2", "network".into());
        assert_eq!(
            derive_update_phase(false, None, Some(&err), None),
            UpdatePhase::Error
        );
        let mut available = UpdateInfo::current_only("2.5.2");
        available.has_update = true;
        available.latest_version = Some("2.5.3".into());
        assert_eq!(
            derive_update_phase(false, None, Some(&available), None),
            UpdatePhase::Available
        );
        assert_eq!(
            derive_update_phase(false, None, Some(&UpdateInfo::current_only("2.5.2")), None),
            UpdatePhase::Current
        );
        assert_eq!(
            derive_update_phase(false, None, None, None),
            UpdatePhase::Idle
        );
    }

    #[test]
    fn download_url_must_be_https_official_setup() {
        assert!(validate_setup_download_url("http://github.com/FindX-2.5.2-setup.exe").is_err());
        assert!(
            validate_setup_download_url(
                "https://github.com/chaojimct/findx/releases/download/v2.5.2/FindX-2.5.2-isolated-fixture-setup.exe"
            )
            .is_err()
        );
        assert_eq!(
            validate_official_asset_url(
                "https://github.com/chaojimct/findx/releases/download/v2.5.2/FindX-2.5.2-setup.exe",
                UpdatePlatform::Windows,
            )
            .unwrap(),
            "FindX-2.5.2-setup.exe"
        );
        assert_eq!(
            validate_official_asset_url(
                "https://github.com/chaojimct/findx/releases/download/v2.5.3/FindX-2.5.3-linux-x64.deb",
                UpdatePlatform::LinuxX64,
            )
            .unwrap(),
            "FindX-2.5.3-linux-x64.deb"
        );
        assert!(
            validate_official_asset_url(
                "https://github.com/chaojimct/findx/releases/download/v2.5.3/FindX-2.5.3-setup.exe",
                UpdatePlatform::LinuxX64,
            )
            .is_err()
        );
        assert!(
            validate_official_asset_url(
                "https://github.com/chaojimct/findx/releases/download/v2.5.3/FindX-2.5.3-linux-x64-cli.tar.gz",
                UpdatePlatform::LinuxX64,
            )
            .is_err()
        );
    }

    #[test]
    fn unix_assets_are_picked_per_platform_and_package_format() {
        let win =
            "https://github.com/chaojimct/findx/releases/download/v2.5.3/FindX-2.5.3-setup.exe";
        let deb =
            "https://github.com/chaojimct/findx/releases/download/v2.5.3/FindX-2.5.3-linux-x64.deb";
        let tar = "https://github.com/chaojimct/findx/releases/download/v2.5.3/FindX-2.5.3-linux-x64.tar.gz";
        let dmg_arm = "https://github.com/chaojimct/findx/releases/download/v2.5.3/FindX-2.5.3-macos-aarch64.dmg";
        let dmg_x64 =
            "https://github.com/chaojimct/findx/releases/download/v2.5.3/FindX-2.5.3-macos-x64.dmg";
        let body = sample_release(
            "v2.5.3",
            &[
                ("FindX-2.5.3-setup.exe", win, 11_000_000),
                ("FindX-2.5.3-linux-x64.tar.gz", tar, 12_000_000),
                ("FindX-2.5.3-linux-x64.deb", deb, 12_100_000),
                (
                    "FindX-2.5.3-linux-x64-cli.tar.gz",
                    "https://example.invalid/cli.tar.gz",
                    1_000_000,
                ),
                ("FindX-2.5.3-macos-aarch64.dmg", dmg_arm, 13_000_000),
                ("FindX-2.5.3-macos-x64.dmg", dmg_x64, 13_100_000),
                (
                    "FindX-2.5.3-isolated-fixture-setup.exe",
                    "https://example.invalid/fx.exe",
                    100,
                ),
            ],
            false,
            false,
        );
        let linux_deb = evaluate_latest_release_for(
            "2.5.2",
            200,
            &body,
            UpdatePlatform::LinuxX64,
            LinuxPackageFormat::Deb,
        );
        assert!(linux_deb.ok && linux_deb.has_update);
        assert_eq!(linux_deb.download_url.as_deref(), Some(deb));
        let linux_tar = evaluate_latest_release_for(
            "2.5.2",
            200,
            &body,
            UpdatePlatform::LinuxX64,
            LinuxPackageFormat::Tar,
        );
        assert_eq!(linux_tar.download_url.as_deref(), Some(tar));
        let linux_default = evaluate_latest_release_for(
            "2.5.2",
            200,
            &body,
            UpdatePlatform::LinuxX64,
            LinuxPackageFormat::Unknown,
        );
        assert_eq!(linux_default.download_url.as_deref(), Some(deb));
        let mac = evaluate_latest_release_for(
            "2.5.2",
            200,
            &body,
            UpdatePlatform::MacosAarch64,
            LinuxPackageFormat::Unknown,
        );
        assert_eq!(mac.download_url.as_deref(), Some(dmg_arm));
        let mac_x64 = evaluate_latest_release_for(
            "2.5.2",
            200,
            &body,
            UpdatePlatform::MacosX64,
            LinuxPackageFormat::Unknown,
        );
        assert_eq!(mac_x64.download_url.as_deref(), Some(dmg_x64));

        // Windows-only release on Linux: update exists but cannot be installed here.
        let windows_only = sample_release(
            "v2.5.3",
            &[("FindX-2.5.3-setup.exe", win, 11_000_000)],
            false,
            false,
        );
        let missing = evaluate_latest_release_for(
            "2.5.2",
            200,
            &windows_only,
            UpdatePlatform::LinuxX64,
            LinuxPackageFormat::Unknown,
        );
        assert!(missing.ok && missing.has_update);
        assert_eq!(missing.download_url, None);
        assert!(
            missing
                .error
                .as_deref()
                .unwrap_or("")
                .contains("没有当前平台")
        );
        let mut state = OfficialUpdateState::default();
        state.info = Some(missing);
        let model = update_card_model(true, "2.5.2", &state, true);
        assert!(!model.button_enabled);
        assert_eq!(model.button_label, "暂无本平台安装包");
        assert!(!official_card_uses_placeholder(&model));

        assert_eq!(
            pick_official_asset_name(
                &[
                    "FindX-2.5.3-linux-x64-cli.tar.gz",
                    "FindX-2.5.3-linux-x64.tar.gz",
                    "FindX-2.5.3-linux-x64.deb"
                ],
                UpdatePlatform::LinuxX64,
                LinuxPackageFormat::Unknown
            )
            .as_deref(),
            Some("FindX-2.5.3-linux-x64.deb")
        );
    }

    #[test]
    fn linux_pkgformat_file_is_read_from_sibling_marker() {
        let dir = std::env::temp_dir().join(format!("findx-pkgformat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("FindX");
        std::fs::write(&exe, b"exe").unwrap();
        assert_eq!(
            detect_linux_package_format(&exe),
            LinuxPackageFormat::Unknown
        );
        std::fs::write(dir.join("FindX.pkgformat"), b"deb\n").unwrap();
        assert_eq!(detect_linux_package_format(&exe), LinuxPackageFormat::Deb);
        std::fs::write(dir.join("FindX.pkgformat"), b"tar\n").unwrap();
        assert_eq!(detect_linux_package_format(&exe), LinuxPackageFormat::Tar);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
mod unix_install_tests {
    use super::*;
    fn archive(path: &Path, symlink: bool) {
        let gz = flate2::write::GzEncoder::new(
            std::fs::File::create(path).unwrap(),
            flate2::Compression::fast(),
        );
        let mut tar = tar::Builder::new(gz);
        let root = "FindX-2.5.2-linux-x64";
        for file in [
            "FindX",
            "findx2",
            "fx",
            "findx2-service",
            "FindX.installed",
            "install.sh",
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(1);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, format!("{root}/{file}"), &b"x"[..])
                .unwrap();
        }
        if symlink {
            let mut header = tar::Header::new_gnu();
            header.set_size(0);
            header.set_mode(0o777);
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_link_name("/etc/passwd").unwrap();
            header.set_cksum();
            tar.append_data(&mut header, format!("{root}/version"), &b""[..])
                .unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();
    }
    #[test]
    fn tar_update_rejects_links_and_requires_exact_layout() {
        let dir = std::env::temp_dir().join(format!(
            "findx-tar-test-{}-{}",
            std::process::id(),
            now_unix()
        ));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("FindX-2.5.2-linux-x64.tar.gz");
        archive(&path, false);
        let stage = unpack_linux_update(&path).unwrap();
        assert!(stage.join("FindX").is_file());
        std::fs::remove_dir_all(stage.parent().unwrap()).unwrap();
        archive(&path, true);
        assert!(unpack_linux_update(&path).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
    #[test]
    fn macos_mount_and_replacement_plan_is_fail_closed() {
        let dir = std::env::temp_dir().join(format!("findx-dmg-test-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("FindX-2.5.2-macos-aarch64.dmg");
        std::fs::File::create(&path)
            .unwrap()
            .set_len(MIN_SETUP_BYTES)
            .unwrap();
        let plan = macos_install_plan(&path).unwrap();
        assert_eq!(plan.program, "osascript");
        let script = plan.args.join(" ");
        assert!(script.contains("-mountpoint"));
        assert!(script.contains("FindX.backup-"));
        assert!(script.contains("trap "));
        assert!(!script.contains("awk"));
        assert!(!script.contains("rm -rf /Applications/FindX.app"));
        assert!(!script.contains("pipefail")); // do shell script uses /bin/sh
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod followup_safety_tests {
    use super::*;
    #[test]
    fn linux_update_never_switches_package_ownership() {
        assert_eq!(
            pick_official_asset_name(
                &["FindX-2.5.3-linux-x64.tar.gz"],
                UpdatePlatform::LinuxX64,
                LinuxPackageFormat::Deb
            ),
            None
        );
        assert_eq!(
            pick_official_asset_name(
                &["FindX-2.5.3-linux-x64.deb"],
                UpdatePlatform::LinuxX64,
                LinuxPackageFormat::Tar
            ),
            None
        );
    }
    #[test]
    fn official_downloads_reject_foreign_hosts_and_busy_install_is_disabled() {
        assert!(
            validate_official_asset_url(
                "https://evil.example/FindX-2.5.3-linux-x64.deb",
                UpdatePlatform::LinuxX64
            )
            .is_err()
        );
        assert!(
            validate_official_asset_url(
                "https://github.com/other/findx/releases/download/v2.5.3/FindX-2.5.3-linux-x64.deb",
                UpdatePlatform::LinuxX64
            )
            .is_err()
        );
        assert_ne!(
            download_dest_path("FindX-2.5.3-linux-x64.deb"),
            download_dest_path("FindX-2.5.3-linux-x64.deb")
        );
        let state = OfficialUpdateState {
            busy: true,
            downloaded_path: Some("/fixture/package".into()),
            ..Default::default()
        };
        assert_eq!(
            derive_update_phase(true, None, None, state.downloaded_path.as_deref()),
            UpdatePhase::Installing
        );
        assert!(!update_card_model(true, "2.5.2", &state, true).button_enabled);
    }

    #[test]
    fn install_stage_labels_are_human_readable_and_indeterminate() {
        for stage in [
            InstallStage::Launching,
            InstallStage::Running,
            InstallStage::Verifying,
        ] {
            assert!(!stage.label().is_empty());
            assert_eq!(stage.bar_percent(), None);
        }
        // Launching must mention elevation: that is the only user action.
        assert!(InstallStage::Launching.label().contains("提权"));
        assert!(InstallStage::Running.label().contains("安装程序窗口"));
    }

    #[test]
    fn installing_card_shows_stage_not_stale_download_text() {
        let state = OfficialUpdateState {
            busy: true,
            downloaded_path: Some("/fixture/package".into()),
            progress: Some(UpdateProgress {
                downloaded: 10,
                total: 10,
                percent: 100.0,
            }),
            install_stage: Some(InstallStage::Running),
            ..Default::default()
        };
        let model = update_card_model(true, "2.5.2", &state, true);
        assert_eq!(
            model.progress_text.as_deref(),
            Some(InstallStage::Running.label())
        );
        // Indeterminate: Inno owns the copy loop, no honest percent exists.
        assert_eq!(model.progress_bar, Some(CardProgressBar { percent: None }));
        // Without a stage yet, Installing still shows a generic line + bar.
        let mut bare = state.clone();
        bare.install_stage = None;
        let bare_model = update_card_model(true, "2.5.2", &bare, true);
        assert!(bare_model.progress_text.is_some());
        assert_eq!(
            bare_model.progress_bar,
            Some(CardProgressBar { percent: None })
        );
    }

    #[test]
    fn downloading_card_shows_determinate_bar() {
        let state = OfficialUpdateState {
            busy: true,
            progress: Some(UpdateProgress {
                downloaded: 5,
                total: 10,
                percent: 50.0,
            }),
            ..Default::default()
        };
        let model = update_card_model(true, "2.5.2", &state, true);
        assert_eq!(
            model.progress_bar,
            Some(CardProgressBar {
                percent: Some(50.0)
            })
        );
        assert!(model.progress_text.is_some_and(|t| t.contains("50%")));
    }

    #[test]
    fn idle_and_current_cards_have_no_progress_bar() {
        let idle = OfficialUpdateState::default();
        assert_eq!(
            update_card_model(true, "2.5.2", &idle, true).progress_bar,
            None
        );
        let current = OfficialUpdateState {
            info: Some(UpdateInfo::current_only("2.5.2")),
            ..Default::default()
        };
        assert_eq!(
            update_card_model(true, "2.5.2", &current, true).progress_bar,
            None
        );
    }

    #[test]
    fn silent_setup_args_show_progress_window_and_stay_noninteractive() {
        // Exact match: any fully-silent flag would hide the installer's own
        // progress window, and the source-scan regression pins the absence.
        assert_eq!(
            SILENT_SETUP_ARGS,
            "/SILENT /SUPPRESSMSGBOXES /NORESTART"
        );
        assert_eq!(SETUP_ROLLBACK_EXIT_CODE, 80);
    }
}
