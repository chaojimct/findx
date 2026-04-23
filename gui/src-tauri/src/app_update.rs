//! 通过 GitHub Releases API 检测新版（不自动下载安装包）。

use serde::{Deserialize, Serialize};
use semver::Version;
use tauri::AppHandle;

const GITHUB_OWNER: &str = "chaojimct";
const GITHUB_REPO: &str = "findx";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdateInfo {
    pub ok: bool,
    /// 请求失败或解析异常时的说明
    pub error: Option<String>,
    pub current_version: String,
    pub latest_version: Option<String>,
    pub has_update: bool,
    pub release_page_url: Option<String>,
    pub published_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    published_at: Option<String>,
}

fn version_from_release_tag(tag: &str) -> Option<Version> {
    let tag = tag.trim();
    let core = tag.strip_prefix('v').unwrap_or(tag);
    if let Ok(v) = Version::parse(core) {
        return Some(v);
    }
    // 例如 tag 为 "gui-2.0.2"：在字符串中扫描 x.y.z 片段，取其中最大的合法版本
    let chars: Vec<char> = tag.chars().collect();
    let mut best: Option<Version> = None;
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            let slice: String = chars[start..i].iter().collect();
            if slice.matches('.').count() >= 2 {
                if let Ok(v) = Version::parse(&slice) {
                    best = match best {
                        Some(ref b) if *b >= v => best.clone(),
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

/// 查询 `https://github.com/chaojimct/findx/releases/latest` 对应 API，与当前包版本比较。
#[tauri::command]
pub fn check_app_update(app: AppHandle) -> AppUpdateInfo {
    let current_version = app.package_info().version.to_string();
    let current = match Version::parse(&current_version) {
        Ok(v) => v,
        Err(e) => {
            return AppUpdateInfo {
                ok: false,
                error: Some(format!("当前版本号无法解析为语义化版本：{e}")),
                current_version,
                latest_version: None,
                has_update: false,
                release_page_url: None,
                published_at: None,
            };
        }
    };

    let url = format!(
        "https://api.github.com/repos/{GITHUB_OWNER}/{GITHUB_REPO}/releases/latest"
    );

    let resp = match ureq::get(&url)
        .set("User-Agent", "FindX2-GUI-UpdateCheck")
        .set("Accept", "application/vnd.github+json")
        .call()
    {
        Ok(r) => r,
        Err(e) => {
            return AppUpdateInfo {
                ok: false,
                error: Some(format!("请求 GitHub 失败：{e}")),
                current_version,
                latest_version: None,
                has_update: false,
                release_page_url: None,
                published_at: None,
            };
        }
    };

    let status = resp.status();
    if status == 404 {
        return AppUpdateInfo {
            ok: true,
            error: None,
            current_version,
            latest_version: None,
            has_update: false,
            release_page_url: Some(format!(
                "https://github.com/{GITHUB_OWNER}/{GITHUB_REPO}/releases"
            )),
            published_at: None,
        };
    }

    if !(200..300).contains(&status) {
        let body = resp.into_string().unwrap_or_default();
        let tail = body.chars().take(200).collect::<String>();
        return AppUpdateInfo {
            ok: false,
            error: Some(format!(
                "GitHub API 返回 HTTP {status}{}",
                if tail.is_empty() {
                    String::new()
                } else {
                    format!("：{tail}")
                }
            )),
            current_version,
            latest_version: None,
            has_update: false,
            release_page_url: None,
            published_at: None,
        };
    }

    let body = match resp.into_string() {
        Ok(s) => s,
        Err(e) => {
            return AppUpdateInfo {
                ok: false,
                error: Some(format!("读取响应失败：{e}")),
                current_version,
                latest_version: None,
                has_update: false,
                release_page_url: None,
                published_at: None,
            };
        }
    };

    let gh: GhRelease = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return AppUpdateInfo {
                ok: false,
                error: Some(format!("解析 GitHub 响应失败：{e}")),
                current_version,
                latest_version: None,
                has_update: false,
                release_page_url: None,
                published_at: None,
            };
        }
    };

    let tag_display = gh.tag_name.clone();
    let latest = match version_from_release_tag(&gh.tag_name) {
        Some(v) => v,
        None => {
            return AppUpdateInfo {
                ok: true,
                error: Some(format!(
                    "最新发行标签「{tag_display}」无法解析为语义化版本，请手动对照"
                )),
                current_version,
                latest_version: Some(tag_display),
                has_update: false,
                release_page_url: Some(gh.html_url),
                published_at: gh.published_at,
            };
        }
    };

    let has_update = latest > current;

    AppUpdateInfo {
        ok: true,
        error: None,
        current_version,
        latest_version: Some(tag_display),
        has_update,
        release_page_url: Some(gh.html_url),
        published_at: gh.published_at,
    }
}
