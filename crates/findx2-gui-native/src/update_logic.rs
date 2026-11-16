//! Official in-app updater for the shipped FindX product.
//!
//! Channel matches the 2.4.10 Tauri client (`gui/src-tauri/src/app_update.rs`):
//! `GET https://api.github.com/repos/chaojimct/findx/releases/latest`
//! (non-draft, non-prerelease), compare tag semver, pick `FindX-*-setup.exe`,
//! download with progress, then `ShellExecute` `runas` with
//! `/VERYSILENT /SUPPRESSMSGBOXES /NORESTART`.
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
pub const SILENT_SETUP_ARGS: &str = "/VERYSILENT /SUPPRESSMSGBOXES /NORESTART";
pub const AUTO_CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;
const NOTES_LIMIT: usize = 600;
const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(200);
const MIN_SETUP_BYTES: u64 = 1024 * 1024;

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

#[derive(Debug, Clone, PartialEq, Default)]
pub struct OfficialUpdateState {
    pub busy: bool,
    pub info: Option<UpdateInfo>,
    pub progress: Option<UpdateProgress>,
    pub downloaded_path: Option<String>,
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
            title: "应用更新 · 未连接安装协议".into(),
            version_line: format!("当前 native-dev 版本 {current_version}"),
            body: "此开发客户端不检查、下载或安装更新。更新请使用正式安装的 FindX；不展示虚构的新版本或下载进度。".into(),
            button_label: "检查 / 安装更新（不可操作）".into(),
            button_enabled: false,
            auto_check_label: "自动检查偏好（只保存，本客户端不执行）".into(),
            status: None,
            progress_text: None,
        };
    }
    let phase = derive_update_phase(
        state.busy,
        state.progress.as_ref(),
        state.info.as_ref(),
        state.downloaded_path.as_deref(),
    );
    let mut body = "从 GitHub Releases 检查正式版本，下载 FindX 安装包后以管理员权限静默安装。".to_string();
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
        } else if info.ok {
            if let Some(latest) = info.latest_version.as_ref() {
                status = Some(format!("已是最新（{latest}）"));
            }
        }
    }
    let progress_text = state.progress.as_ref().map(|p| {
        format!(
            "已下载 {} / {}（{:.0}%）",
            format_asset_size(p.downloaded),
            format_asset_size(p.total),
            p.percent
        )
    });
    let (button_label, button_enabled) = match phase {
        UpdatePhase::Checking => ("检查中…".into(), false),
        UpdatePhase::Downloading => ("下载中…".into(), false),
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

pub fn is_official_setup_asset_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    if n.contains("isolated") || n.contains("fixture") || n.contains("drill") {
        return false;
    }
    n.starts_with("findx-") && n.ends_with("-setup.exe")
}

pub fn pick_windows_setup_asset_name(names: &[&str]) -> Option<String> {
    names
        .iter()
        .find(|n| is_official_setup_asset_name(n))
        .map(|n| (*n).to_string())
}

fn pick_windows_setup_asset(assets: &[GhAsset]) -> Option<&GhAsset> {
    if !cfg!(windows) {
        return None;
    }
    assets.iter().find(|a| is_official_setup_asset_name(&a.name))
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
        release_notes: None,
        release_page_url: None,
    }
}

/// Evaluate a GitHub /releases/latest JSON body against the running version.
/// Used by the live client and by tests with mocked payloads.
pub fn evaluate_latest_release(current_version: &str, status: u16, body: &str) -> UpdateInfo {
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
        return error_info(
            current_version,
            "最新发行不是正式版本，已忽略。".into(),
        );
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
                release_notes: truncate_notes(&gh.body),
                release_page_url: Some(gh.html_url),
            };
        }
    };
    let has_update = latest > current;
    let setup = pick_windows_setup_asset(&gh.assets);
    UpdateInfo {
        ok: true,
        error: None,
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
    let url = url.trim();
    if !url.starts_with("https://") {
        return Err("下载地址不是 https 链接，已拒绝。".into());
    }
    let asset_name = url.rsplit('/').next().unwrap_or("").to_string();
    if !is_official_setup_asset_name(&asset_name) {
        return Err(format!("拒绝非官方安装包资产：{asset_name}"));
    }
    Ok(asset_name)
}

pub fn download_dest_path(asset_name: &str) -> PathBuf {
    std::env::temp_dir().join("FindX-update").join(asset_name)
}

pub fn download_official_setup(
    url: &str,
    expected_size: Option<u64>,
    mut on_progress: impl FnMut(UpdateProgress),
) -> Result<PathBuf, String> {
    let asset_name = validate_setup_download_url(url)?;
    let dest = download_dest_path(&asset_name);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建临时目录失败（{}）：{e}", parent.display()))?;
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
    let mut file = std::fs::File::create(&dest)
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
    drop(file);
    if downloaded != total {
        let _ = std::fs::remove_file(&dest);
        return Err(format!(
            "下载数据不完整（{downloaded}/{total} B），已删除临时文件。"
        ));
    }
    on_progress(UpdateProgress {
        downloaded,
        total,
        percent: 100.0,
    });
    Ok(dest)
}

pub fn start_silent_setup(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        start_silent_setup_windows(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err("自动安装当前仅支持 Windows，请到发行页手动下载更新。".into())
    }
}

#[cfg(windows)]
fn start_silent_setup_windows(path: &Path) -> Result<(), String> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, w};

    let p = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if !p.exists() {
        return Err(format!("安装包不存在：{}", p.display()));
    }
    if p.extension().map(|e| e.to_ascii_lowercase()) != Some("exe".into()) {
        return Err("只允许启动 .exe 安装包。".into());
    }
    let size = std::fs::metadata(&p).map_err(|e| e.to_string())?.len();
    if size < MIN_SETUP_BYTES {
        return Err(format!("安装包大小异常（{size} B），拒绝执行。"));
    }
    let cwd = p
        .parent()
        .map(|d| d.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let file = HSTRING::from(p.as_os_str());
    let params = HSTRING::from(SILENT_SETUP_ARGS);
    let dir = HSTRING::from(cwd.as_os_str());
    let rc = unsafe {
        ShellExecuteW(
            None,
            w!("runas"),
            &file,
            &params,
            &dir,
            SW_SHOWNORMAL,
        )
    };
    // ShellExecute returns a HINSTANCE value; >32 means success.
    if rc.0 as usize <= 32 {
        return Err(format!(
            "无法启动安装程序（ShellExecute 返回 {}）。若取消了 UAC，则未开始安装。",
            rc.0 as usize
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_release(tag: &str, assets: &[(&str, &str, u64)], draft: bool, pre: bool) -> String {
        let assets_json: Vec<String> = assets
            .iter()
            .map(|(name, url, size)| {
                format!(
                    r#"{{"name":"{name}","browser_download_url":"{url}","size":{size}}}"#
                )
            })
            .collect();
        format!(
            r#"{{"tag_name":"{tag}","html_url":"https://github.com/chaojimct/findx/releases/tag/{tag}","draft":{draft},"prerelease":{pre},"body":"notes for {tag}","assets":[{}]}}"#,
            assets_json.join(",")
        )
    }

    #[test]
    fn version_compare_and_asset_pick() {
        let official = "https://github.com/chaojimct/findx/releases/download/v2.5.2/FindX-2.5.2-setup.exe";
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
        let newer = evaluate_latest_release("2.5.1", 200, &body);
        assert!(newer.ok && newer.has_update);
        assert_eq!(newer.latest_version.as_deref(), Some("v2.5.2"));
        assert_eq!(newer.download_url.as_deref(), Some(official));
        assert_eq!(newer.asset_size, Some(10_940_635));
        assert!(!newer.release_notes.as_deref().unwrap_or("").contains("占位"));

        let current = evaluate_latest_release("2.5.2", 200, &body);
        assert!(current.ok && !current.has_update);
        assert_eq!(current.download_url, None);

        let older_tag = evaluate_latest_release("2.5.3", 200, &body);
        assert!(older_tag.ok && !older_tag.has_update);

        assert_eq!(
            pick_windows_setup_asset_name(&[
                "FindX-2.5.2-isolated-fixture-setup.exe",
                "FindX-2.5.2-setup.exe"
            ])
            .as_deref(),
            Some("FindX-2.5.2-setup.exe")
        );
        assert_eq!(version_from_release_tag("v2.5.2").unwrap().to_string(), "2.5.2");
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
        available.info = Some(evaluate_latest_release(
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
        assert!(model.button_label.contains("不可操作"));
        assert!(model.version_line.contains("native-dev"));
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
            validate_setup_download_url(
                "https://github.com/chaojimct/findx/releases/download/v2.5.2/FindX-2.5.2-setup.exe"
            )
            .unwrap(),
            "FindX-2.5.2-setup.exe"
        );
    }
}
