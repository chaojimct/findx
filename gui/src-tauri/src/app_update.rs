//! 通过 GitHub Releases API 检测新版 + 下载安装包（带进度事件）+ 触发静默安装。
//!
//! Windows 闭环：`check_app_update` 检测（含 setup.exe 资产直链）→ `download_app_update`
//! 后台线程流式下载（`findx2-update-progress` 节流事件 + 可取消）→ `install_downloaded_update`
//! 以 runas 启动 Inno 安装器 `/VERYSILENT`（马老师会看到一次 UAC 授权）。
//! 安装器自身负责：强杀旧 GUI/服务 → 覆盖安装 → 重注册并启动服务 → 静默模式下自动启动新 GUI
//! （见 installer/FindX.iss 的 PrepareToInstall / CurStepChanged / [Run] WizardSilent 项）。
//! 非 Windows 平台无对应安装器，`download_url` 为空、安装命令返回 Err，降级为打开发行页手动更新。

use serde::{Deserialize, Serialize};
use semver::Version;
use std::io::Write;
use tauri::{AppHandle, Emitter};

const GITHUB_OWNER: &str = "chaojimct";
const GITHUB_REPO: &str = "findx";

/// 下载进度事件的节流间隔。
const PROGRESS_EMIT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

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
    /// Windows 安装器（`FindX-*-setup.exe`）的直链；非 Windows 或资产缺失时为 None。
    #[serde(default)]
    pub download_url: Option<String>,
    /// 安装包字节数（来自 GitHub 资产元数据，用于下载校验与前端显示）。
    #[serde(default)]
    pub asset_size: Option<u64>,
    /// 发行说明（GitHub Release body，截断到 600 字符）。
    #[serde(default)]
    pub release_notes: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    published_at: Option<String>,
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

/// 挑选 Windows 安装器资产：`FindX-<ver>-setup.exe`（与 installer/FindX.iss 的
/// OutputBaseFilename=FindX-{version}-setup 对应）。找不到返回 None（unix 平台恒为 None）。
fn pick_windows_setup_asset(assets: &[GhAsset]) -> Option<&GhAsset> {
    if !cfg!(windows) {
        return None;
    }
    assets.iter().find(|a| {
        let n = a.name.to_ascii_lowercase();
        n.starts_with("findx-") && n.ends_with("-setup.exe")
    })
}

fn truncate_notes(body: &Option<String>) -> Option<String> {
    body.as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| {
            let mut t: String = s.chars().take(600).collect();
            if s.chars().count() > 600 {
                t.push('…');
            }
            t
        })
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
                download_url: None,
                asset_size: None,
                release_notes: None,
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
                download_url: None,
                asset_size: None,
                release_notes: None,
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
            download_url: None,
            asset_size: None,
            release_notes: None,
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
            download_url: None,
            asset_size: None,
            release_notes: None,
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
                download_url: None,
                asset_size: None,
                release_notes: None,
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
                download_url: None,
                asset_size: None,
                release_notes: None,
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
                download_url: None,
                asset_size: None,
                release_notes: truncate_notes(&gh.body),
            };
        }
    };

    let has_update = latest > current;
    let setup = pick_windows_setup_asset(&gh.assets);

    AppUpdateInfo {
        ok: true,
        error: None,
        current_version,
        latest_version: Some(tag_display),
        has_update,
        release_page_url: Some(gh.html_url),
        published_at: gh.published_at,
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
    }
}

// ── 下载 ────────────────────────────────────────────────────────────────────

/// 下载进度事件（`findx2-update-progress`）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateProgress {
    downloaded: u64,
    /// Content-Length 未知时为 0（前端只显示已下载 MB）。
    total: u64,
    percent: f64,
}

/// 下载结束事件（`findx2-update-finished`）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateFinished {
    ok: bool,
    /// 成功时为安装包本地路径（前端转交 install_downloaded_update）。
    path: Option<String>,
    error: Option<String>,
}

/// 全局「正在下载」标志：防止并发下载；取消标志由下载线程轮询。
static DL_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static DL_CANCEL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn download_dest_path(asset_name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("FindX-update");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(asset_name)
}

/// 开始下载安装包到临时目录。立即返回，进度/结果走事件：
/// - `findx2-update-progress`：`{ downloaded, total, percent }`（200ms 节流）
/// - `findx2-update-finished`：`{ ok, path, error }`
#[tauri::command]
pub fn download_app_update(
    app: AppHandle,
    url: String,
    expected_size: Option<u64>,
) -> Result<(), String> {
    let url = url.trim().to_string();
    if !url.starts_with("https://") {
        return Err("下载地址不是 https 链接，已拒绝。".into());
    }
    if DL_ACTIVE.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return Err("已有下载任务在进行中。".into());
    }
    DL_CANCEL.store(false, std::sync::atomic::Ordering::SeqCst);

    // 以 URL 尾段（如 FindX-2.4.3-setup.exe）命名临时文件，保持与发行资产同名。
    let asset_name = url.rsplit('/').next().unwrap_or("setup").to_string();
    let dest = download_dest_path(&asset_name);

    std::thread::spawn(move || {
        let result = run_download(&dest, &url, expected_size, &app);
        DL_ACTIVE.store(false, std::sync::atomic::Ordering::SeqCst);
        match result {
            Ok(()) => {
                let _ = app.emit(
                    "findx2-update-finished",
                    UpdateFinished {
                        ok: true,
                        path: Some(dest.to_string_lossy().into_owned()),
                        error: None,
                    },
                );
            }
            Err(e) => {
                let _ = app.emit(
                    "findx2-update-finished",
                    UpdateFinished {
                        ok: false,
                        path: None,
                        error: Some(e),
                    },
                );
            }
        }
    });
    Ok(())
}

/// 下载主体：ureq 流式读取 → 写临时文件 → 节流 emit 进度。
/// 返回 Err(canceled) 表示用户取消（事件里如实上报，不区别文案）。
fn run_download(
    dest: &std::path::Path,
    url: &str,
    expected_size: Option<u64>,
    app: &AppHandle,
) -> Result<(), String> {
    use std::io::Read;

    let resp = ureq::get(url)
        .set("User-Agent", "FindX2-GUI-UpdateDownload")
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
        // GitHub 资产必有 Content-Length；没有说明链路异常（如代理截断）。
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
    let mut file = std::fs::File::create(dest)
        .map_err(|e| format!("创建临时文件失败（{}）：{e}", dest.display()))?;

    let mut buf = [0u8; 64 * 1024];
    let mut downloaded: u64 = 0;
    let mut last_emit = std::time::Instant::now();
    loop {
        if DL_CANCEL.load(std::sync::atomic::Ordering::SeqCst) {
            drop(file);
            let _ = std::fs::remove_file(dest);
            return Err("已取消下载。".into());
        }
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
            let _ = app.emit(
                "findx2-update-progress",
                UpdateProgress {
                    downloaded,
                    total,
                    percent: (downloaded as f64 / total as f64) * 100.0,
                },
            );
        }
    }

    if downloaded != total {
        let _ = std::fs::remove_file(dest);
        return Err(format!("下载数据不完整（{downloaded}/{total} B），已删除临时文件。"));
    }

    let _ = app.emit(
        "findx2-update-progress",
        UpdateProgress {
            downloaded,
            total,
            percent: 100.0,
        },
    );
    Ok(())
}

/// 取消进行中的下载：下载线程在下个读取循环检查到标志后中止并清理临时文件。
#[tauri::command]
pub fn cancel_update_download() -> bool {
    DL_CANCEL.store(true, std::sync::atomic::Ordering::SeqCst);
    DL_ACTIVE.load(std::sync::atomic::Ordering::SeqCst)
}

// ── 安装 ────────────────────────────────────────────────────────────────────

/// 以管理员授权启动 Inno 安装器静默安装（Windows）。
/// 安装器会自动：强杀旧 GUI/服务 → 覆盖安装 → 重注册服务并启动 → 自动启动新 GUI。
/// UAC 被取消时返回 Err（前端提示未安装）。非 Windows 平台不支持。
#[tauri::command]
pub fn install_downloaded_update(path: String) -> Result<(), String> {
    #[cfg(windows)]
    {
        let p = std::path::PathBuf::from(path.trim().trim_matches('"'));
        if !p.exists() {
            return Err(format!("安装包不存在：{}", p.display()));
        }
        if p.extension().map(|e| e.to_ascii_lowercase()) != Some("exe".into()) {
            return Err("只允许启动 .exe 安装包。".into());
        }
        let size = std::fs::metadata(&p).map_err(|e| e.to_string())?.len();
        if size < 1024 * 1024 {
            return Err(format!("安装包大小异常（{size} B），拒绝执行。"));
        }
        let cwd = p
            .parent()
            .map(|d| d.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        crate::elevate::shell_execute_runas(
            &p,
            Some("/VERYSILENT /SUPPRESSMSGBOXES /NORESTART"),
            &cwd,
            true,
        )?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err("自动安装当前仅支持 Windows，请到发行页手动下载更新。".into())
    }
}

// ── 自动检查说明 ────────────────────────────────────────────────────────────
// 启动自动检查由前端完成（FindXSearchApp 启动 3.5s 后检查，localStorage 按 24h 节流、
// 可按版本忽略、受 autoCheckUpdate 设置开关控制），后端不再重复起检查线程。
// 后端只负责 check / download（进度事件）/ install 三个原子能力。
