use base64::Engine;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::webview::PageLoadEvent;
use tauri::Manager;
use tauri::Runtime;
use tauri_plugin_opener::OpenerExt;

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use windows::{
    core::{implement, PCWSTR},
    Win32::{
        Foundation::{
            DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, S_OK,
        },
        System::{
            Com::IDataObject,
            Ole::{IDropSource, IDropSource_Impl, DROPEFFECT, DROPEFFECT_COPY},
            SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS},
        },
        UI::Shell::{
            Common::ITEMIDLIST, ILClone, ILCreateFromPathW, ILFindLastID, ILFree, ILRemoveLastID,
            SHCreateDataObject, SHDoDragDrop,
        },
    },
};

#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod desktop;

mod app_update;
mod findx_settings;
#[cfg(windows)]
mod elevate;
#[cfg(windows)]
mod win_file_context_menu;
#[cfg(windows)]
mod win_preview;

#[cfg(target_os = "windows")]
mod pipe;

/// 主窗口在首帧 WebView 页面加载完成后再 `show`，避免启动时出现空白壳窗口闪烁。
static MAIN_WINDOW_SHOWN_AFTER_LOAD: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct IndexStatus {
    indexing: bool,
    ready: bool,
    indexed_count: u64,
    /// 与 index.bin：快速首遍回填未完成时为 false
    #[serde(default)]
    metadata_ready: bool,
    #[serde(default)]
    backfill_done: u64,
    #[serde(default)]
    backfill_total: u64,
    /// CLI 建库进度（仅 `indexing == true` 且存在 `.indexing.json` 时有值）
    #[serde(skip_serializing_if = "Option::is_none")]
    indexing_phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    indexing_volumes_total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    indexing_volumes_done: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    indexing_message: Option<String>,
    /// 建库进度文件中的累计条目数（扫描/合并/写入阶段）
    #[serde(skip_serializing_if = "Option::is_none")]
    indexing_entries_indexed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    indexing_current_volume: Option<String>,
    last_error: Option<String>,
}

#[cfg(target_os = "windows")]
#[derive(Clone, Default)]
struct IndexingProgressSnap {
    phase: Option<String>,
    volumes_total: Option<u64>,
    volumes_done: Option<u64>,
    entries_indexed: Option<u64>,
    current_volume: Option<String>,
    message: Option<String>,
}

/// `cargo build` 常见同时存在 `target/debug` 与 `target/release`；若 GUI 与 CLI 不在同一 profile，
/// 进度文件可能写在「另一套」目录，需互为回退，否则 `indexing` 恒为 false、右下角不刷新。
#[cfg(target_os = "windows")]
fn sibling_target_profile_dir(base: &Path) -> Option<PathBuf> {
    let name = base.file_name()?.to_str()?;
    let parent = base.parent()?;
    if name.eq_ignore_ascii_case("debug") {
        return Some(parent.join("release"));
    }
    if name.eq_ignore_ascii_case("release") {
        return Some(parent.join("debug"));
    }
    None
}

#[cfg(target_os = "windows")]
fn resolve_indexing_json_path(
    base: &Path,
    settings: &findx_settings::FindxGuiSettings,
) -> PathBuf {
    let primary = findx_settings::resolve_index_path(base, settings).with_extension("indexing.json");
    if primary.exists() {
        return primary;
    }
    if let Some(sibling) = sibling_target_profile_dir(base) {
        let alt = findx_settings::resolve_index_path(&sibling, settings).with_extension("indexing.json");
        if alt.exists() {
            return alt;
        }
    }
    primary
}

#[cfg(target_os = "windows")]
fn load_indexing_progress_snap(
    base: &Path,
    settings: &findx_settings::FindxGuiSettings,
) -> IndexingProgressSnap {
    let p = resolve_indexing_json_path(base, settings);
    if !p.exists() {
        return IndexingProgressSnap::default();
    }
    let Ok(bytes) = std::fs::read(&p) else {
        return IndexingProgressSnap::default();
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return IndexingProgressSnap::default();
    };
    IndexingProgressSnap {
        phase: v
            .get("phase")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        volumes_total: v.get("volumes_total").and_then(|x| x.as_u64()),
        volumes_done: v.get("volumes_completed").and_then(|x| x.as_u64()),
        entries_indexed: v.get("entries_indexed").and_then(|x| x.as_u64()),
        current_volume: v
            .get("current_volume")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        message: v
            .get("message")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
    }
}

struct IndexBuildState {
    running: bool,
    /// `desktop::setup` 在排队 `auto_start_flow` 时已判定需自动建库，但 `start_indexing_impl` 可能尚未执行；
    /// 用于避免首屏 `index_status` 误判为「未建库」且轮询间隔长达数秒。
    pending_auto_start: bool,
}

static INDEX_BUILD: Mutex<IndexBuildState> = Mutex::new(IndexBuildState {
    running: false,
    pending_auto_start: false,
});

#[cfg(target_os = "windows")]
pub(crate) fn mark_pending_auto_index_build(pending: bool) {
    if let Ok(mut g) = INDEX_BUILD.lock() {
        g.pending_auto_start = pending;
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchResult {
    name: String,
    path: String,
    extension: String,
    size: u64,
    created_unix: i64,
    modified_unix: i64,
    is_directory: bool,
    #[serde(default)]
    name_highlight: Vec<[u32; 2]>,
}

/// 搜索响应：命中列表 + 真实匹配总数 + service 端 search 耗时（毫秒）。
///
/// `total` 与 Everything 左下角语义一致——是「截断与排序前」的全部匹配数；
/// `hits.length` 受 limit 截断（默认 5000，前端虚拟滚动只渲染可见行）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchResponse {
    hits: Vec<SearchResult>,
    total: u32,
    elapsed_ms: u32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DuplicateFile {
    name: String,
    path: String,
    size: u64,
    created_unix: i64,
    modified_unix: i64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DuplicateGroup {
    group_id: String,
    size: u64,
    total_bytes: u64,
    file_count: u32,
    files: Vec<DuplicateFile>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DuplicateScanStatus {
    running: bool,
    cancel_requested: bool,
    scanned_files: u64,
    total_files: u64,
    groups_found: u64,
    progress_percent: f64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriveInfo {
    letter: String,
    path: String,
    filesystem: String,
    drive_type: String,
    is_ntfs: bool,
    can_open_volume: bool,
}

#[cfg(windows)]
pub(crate) struct OwnedItemIdList(*mut ITEMIDLIST);

#[cfg(windows)]
impl OwnedItemIdList {
    fn from_path(path: &Path) -> Result<Self, String> {
        let wide_path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let pidl = unsafe { ILCreateFromPathW(PCWSTR(wide_path.as_ptr())) };
        if pidl.is_null() {
            Err(format!(
                "Failed to create a shell item for '{}'.",
                path.display()
            ))
        } else {
            Ok(Self(pidl))
        }
    }

    fn as_ptr(&self) -> *const ITEMIDLIST {
        self.0 as *const ITEMIDLIST
    }

    fn as_mut_ptr(&self) -> *mut ITEMIDLIST {
        self.0
    }
}

#[cfg(windows)]
impl Drop for OwnedItemIdList {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_null() {
                ILFree(Some(self.0 as *const ITEMIDLIST));
                self.0 = std::ptr::null_mut();
            }
        }
    }
}

#[cfg(windows)]
#[implement(IDropSource)]
struct NativeFileDropSource;

#[cfg(windows)]
#[allow(non_snake_case)]
impl IDropSource_Impl for NativeFileDropSource_Impl {
    fn QueryContinueDrag(
        &self,
        fescapepressed: windows_core::BOOL,
        grfkeystate: MODIFIERKEYS_FLAGS,
    ) -> windows_core::HRESULT {
        if fescapepressed.as_bool() {
            DRAGDROP_S_CANCEL
        } else if grfkeystate & MK_LBUTTON == MODIFIERKEYS_FLAGS(0) {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> windows_core::HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

fn filetime_to_unix(ft: u64) -> i64 {
    (ft / 10_000_000).saturating_sub(11_644_473_600) as i64
}

fn map_search_hit(d: findx2_ipc::SearchHitDto) -> SearchResult {
    let extension = Path::new(&d.name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    SearchResult {
        name: d.name,
        path: d.path,
        extension,
        size: d.size,
        created_unix: 0,
        modified_unix: filetime_to_unix(d.mtime),
        is_directory: d.is_directory,
        name_highlight: d.name_highlight,
    }
}

async fn fetch_index_status<R: Runtime>(app: tauri::AppHandle<R>) -> IndexStatus {
    let (local_indexing, pending_auto_start) = INDEX_BUILD
        .lock()
        .map(|g| (g.running, g.pending_auto_start))
        .unwrap_or((false, false));

    #[cfg(target_os = "windows")]
    {
        let settings = findx_settings::load_findx_settings(app.clone())
            .unwrap_or_else(|_| findx_settings::FindxGuiSettings::default());
        let base = findx_settings::exe_resource_dir();
        let index_path = findx_settings::resolve_index_path(&base, &settings);
        let indexing_json_active = resolve_indexing_json_path(&base, &settings).exists();
        let idx_snap = load_indexing_progress_snap(&base, &settings);
        // 注意：`indexing_json_active` 单独**不能**判定为"建库中"。CLI 异常退出 / 用户 Ctrl+C 会
        // 残留这个 .indexing.json 文件，导致下次 service 早已 healthy 跑回填了，GUI 还死活显示
        // "建库中 · 读取 indexing.json"，前端因此跳过搜索请求 —— 表象就是"搜索完全卡住"。
        // 只有"本进程正在建" / "等首次自启" 才必然是 build_active；
        // 残留 json 的真假留给下面 service Status 兜底判定（healthy 即覆盖）。
        let build_active = local_indexing || pending_auto_start;
        if !index_path.exists() && !indexing_json_active {
            return IndexStatus {
                indexing: false,
                ready: false,
                indexed_count: 0,
                metadata_ready: true,
                backfill_done: 0,
                backfill_total: 0,
                indexing_phase: None,
                indexing_volumes_total: None,
                indexing_volumes_done: None,
                indexing_message: None,
                indexing_entries_indexed: None,
                indexing_current_volume: None,
                last_error: Some(format!(
                    "尚无索引 {}（首次启动会自动建索引；若以管理员启动仍失败请检查终端日志）",
                    index_path.display()
                )),
            };
        }

        let pipe_name = settings.pipe_name;
        let pipe_name = if pipe_name.trim().is_empty() {
            "findx2".to_string()
        } else {
            pipe_name
        };

        const STATUS_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(2500);
        let pipe_res = tokio::time::timeout(
            STATUS_TIMEOUT,
            pipe::ipc_status_for_pipe_name(&pipe_name),
        )
        .await;

        match pipe_res {
            Ok(Ok(findx2_ipc::IpcResponse::StatusResult {
                entry_count,
                healthy,
                metadata_ready,
                backfill_done,
                backfill_total,
                loading,
                ..
            })) => {
                // **关键自愈逻辑**：service 已经在线、index 已经有数据，那"是否在建库"就只能由 service
                // 的 loading 字段说了算——本进程内 INDEX_BUILD.pending_auto_start / .running 这两个
                // 标志可能因为以下原因卡住：
                //  - desktop.rs setup 时 index 还没生成 → 设了 pending=true；之后用户**手动**在终端跑了
                //    `findx2 index`，CLI 完成、service 也起来了，但 GUI 这条路径里没人来清 pending；
                //  - dev 模式 (`tauri dev`) 热重载多次执行 setup，状态被反复覆盖；
                //  - 上一轮 CLI 异常退出（panic / Ctrl+C），running 标志没清。
                // 任何一种情况下，GUI 都会永远显示"建库中"，前端因此跳过搜索请求，**表象就是"搜索完全卡住"**。
                // 既然 service 已 healthy 且 entry_count > 0，那就一定不在建库，直接清状态。
                let service_alive = healthy && !loading && entry_count > 0;
                if service_alive {
                    if let Ok(mut g) = INDEX_BUILD.lock() {
                        g.pending_auto_start = false;
                        // running 不动——本进程刚才如果真在跑 CLI（少见叠加场景），那条线程会自己清。
                    }
                    if indexing_json_active {
                        // 同一原因下 .indexing.json 也是孤儿，顺手清掉，避免下次启动又被误判。
                        let stale = findx_settings::resolve_index_path(
                            &base,
                            &findx_settings::FindxGuiSettings::default(),
                        )
                        .with_extension("indexing.json");
                        let _ = std::fs::remove_file(&stale);
                    }
                }
                let final_build_active = if service_alive { false } else { build_active };
                IndexStatus {
                indexing: final_build_active || loading,
                ready: healthy && !loading,
                indexed_count: entry_count,
                metadata_ready,
                backfill_done,
                backfill_total,
                indexing_phase: idx_snap.phase.clone(),
                indexing_volumes_total: idx_snap.volumes_total,
                indexing_volumes_done: idx_snap.volumes_done,
                indexing_message: idx_snap.message.clone(),
                indexing_entries_indexed: idx_snap.entries_indexed,
                indexing_current_volume: idx_snap.current_volume.clone(),
                last_error: if loading {
                    Some("索引加载中…（service 已启动，正在反序列化 index.bin）".into())
                } else {
                    None
                },
            }
            }
            Ok(Ok(_)) => IndexStatus {
                indexing: build_active,
                ready: false,
                indexed_count: 0,
                metadata_ready: true,
                backfill_done: 0,
                backfill_total: 0,
                indexing_phase: idx_snap.phase.clone(),
                indexing_volumes_total: idx_snap.volumes_total,
                indexing_volumes_done: idx_snap.volumes_done,
                indexing_message: idx_snap.message.clone(),
                indexing_entries_indexed: idx_snap.entries_indexed,
                indexing_current_volume: idx_snap.current_volume.clone(),
                last_error: Some("管道响应异常".into()),
            },
            Ok(Err(e)) => IndexStatus {
                indexing: build_active,
                ready: false,
                indexed_count: 0,
                metadata_ready: true,
                backfill_done: 0,
                backfill_total: 0,
                indexing_phase: idx_snap.phase.clone(),
                indexing_volumes_total: idx_snap.volumes_total,
                indexing_volumes_done: idx_snap.volumes_done,
                indexing_message: idx_snap.message.clone(),
                indexing_entries_indexed: idx_snap.entries_indexed,
                indexing_current_volume: idx_snap.current_volume.clone(),
                last_error: Some(e),
            },
            Err(_) => IndexStatus {
                indexing: build_active,
                ready: false,
                indexed_count: 0,
                metadata_ready: true,
                backfill_done: 0,
                backfill_total: 0,
                indexing_phase: idx_snap.phase.clone(),
                indexing_volumes_total: idx_snap.volumes_total,
                indexing_volumes_done: idx_snap.volumes_done,
                indexing_message: idx_snap.message.clone(),
                indexing_entries_indexed: idx_snap.entries_indexed,
                indexing_current_volume: idx_snap.current_volume.clone(),
                last_error: Some(
                    "管道状态查询超时（建库时仍可看上方进度；若持续请检查服务是否已启动）".into(),
                ),
            },
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = app;
        IndexStatus {
            indexing: local_indexing,
            ready: false,
            indexed_count: 0,
            metadata_ready: true,
            backfill_done: 0,
            backfill_total: 0,
            indexing_phase: None,
            indexing_volumes_total: None,
            indexing_volumes_done: None,
            indexing_message: None,
            indexing_entries_indexed: None,
            indexing_current_volume: None,
            last_error: Some("FindX2 GUI 当前需 Windows.".into()),
        }
    }
}

/// 将 CLI 参数拼成 `ShellExecute` 的参数字符串（与 `quote_arg` 规则一致）。
#[cfg(target_os = "windows")]
fn index_cli_args_to_shell_params(args: &[String]) -> String {
    use crate::elevate::quote_arg;
    args.iter()
        .map(|s| quote_arg(s))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 阻塞运行 `findx2 index …`：已提权时优先 `Command::spawn` 等待退出码（与拉起 findx2-service 一致），
/// 失败再回退 `runas`；未提权时仍用 `runas` 弹出 UAC。
///
/// 即便在「服务模式」下，建库本身依然需要管理员权限才能 OpenVolume、读 USN，所以只此一次 UAC 提权是合理的（与 Everything 首次需要管理员一致）。
#[cfg(target_os = "windows")]
fn run_index_cli_blocking(cli: &std::path::Path, args: &[String], work_dir: &std::path::Path) -> bool {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let params = index_cli_args_to_shell_params(args);

    if crate::elevate::process_is_elevated() {
        match Command::new(cli)
            .current_dir(work_dir)
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .status()
        {
            Ok(st) => {
                if !st.success() {
                    eprintln!(
                        "findx2: 建索引 CLI 退出码非 0（{:?}），不自动启动服务。",
                        st.code()
                    );
                }
                return st.success();
            }
            Err(e) => {
                eprintln!("findx2: 管理员会话下直接启动建索引 CLI 失败（{e}），回退 ShellExecute runas…");
            }
        }
    }

    match crate::elevate::shell_execute_runas(cli, Some(&params), work_dir, true) {
        Ok(Some(0)) => true,
        Ok(Some(code)) => {
            eprintln!("findx2: 建索引 CLI（runas）退出码 {code}，不自动启动服务。");
            false
        }
        Ok(None) => {
            eprintln!("findx2: 建索引 runas 未返回退出码，不自动启动服务。");
            false
        }
        Err(e) => {
            eprintln!("findx2: 建索引 runas 失败: {e}");
            false
        }
    }
}

/// 建库成功后拉起 findx2-service；若 `index.bin` 尚未可见或 spawn 瞬时失败则短暂重试。
#[cfg(target_os = "windows")]
fn spawn_service_after_index_build<R: Runtime>(
    app: tauri::AppHandle<R>,
    index_path: PathBuf,
) {
    let app_wait = app.clone();
    for attempt in 1..=12u32 {
        if !index_path.exists() {
            if attempt == 12 {
                eprintln!(
                    "findx2: 建库结束仍找不到 {}，跳过自动启动 findx2-service。",
                    index_path.display()
                );
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
            continue;
        }
        match findx_settings::spawn_findx_service_process(app.clone()) {
            Ok(()) => {
                std::thread::spawn(move || {
                    let rt = match tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        Ok(rt) => rt,
                        Err(_) => return,
                    };
                    let settings = findx_settings::load_findx_settings(app_wait.clone())
                        .unwrap_or_default();
                    let pn = settings.pipe_name.trim();
                    let pn = if pn.is_empty() { "findx2" } else { pn };
                    let _ = rt.block_on(wait_for_service_pipe(pn, SERVICE_PIPE_WAIT_SECS));
                });
                return;
            }
            Err(e) => {
                eprintln!(
                    "findx2: 建库后自动启动 findx2-service 失败（第 {}/12 次）: {}",
                    attempt, e
                );
                if attempt == 12 {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
        }
    }
}

/// 服务需先 `load_index.bin` 再监听管道；大索引可能需数分钟，过短会导致 GUI 误判「管道不存在」。
#[cfg(target_os = "windows")]
const SERVICE_PIPE_WAIT_SECS: u64 = 300;

/// 提权启动 findx2-service 后，管道未必立即可连；轮询直至就绪或超时。
#[cfg(target_os = "windows")]
async fn wait_for_service_pipe(pipe_name: &str, max_secs: u64) -> Result<(), String> {
    let pn = if pipe_name.trim().is_empty() {
        "findx2"
    } else {
        pipe_name.trim()
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(max_secs);
    while std::time::Instant::now() < deadline {
        if pipe::ipc_status_for_pipe_name(pn).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    Err(format!(
        "等待 findx2-service 就绪超时（{} 秒内命名管道仍未建立）。大索引加载较慢时请多等一会或重试「启动服务」。若进程已崩溃，请查看 %TEMP%\\findx2-service-last-error.txt 。",
        max_secs
    ))
}

/// 启动后根据设置自动建库或拉起 findx2-service
#[cfg(target_os = "windows")]
pub(crate) async fn auto_start_flow<R: Runtime>(app: tauri::AppHandle<R>) -> Result<(), String> {
    let settings = findx_settings::load_findx_settings(app.clone())?;
    if !settings.auto_start_service {
        return Ok(());
    }
    let base = findx_settings::exe_resource_dir();
    let index = findx_settings::resolve_index_path(&base, &settings);
    let pipe_name = settings.pipe_name.trim();
    let pipe_name = if pipe_name.is_empty() {
        "findx2"
    } else {
        pipe_name
    };

    if index.exists() {
        if pipe::ipc_status_for_pipe_name(pipe_name)
            .await
            .is_ok()
        {
            return Ok(());
        }
        findx_settings::spawn_findx_service_process(app.clone())?;
        wait_for_service_pipe(pipe_name, SERVICE_PIPE_WAIT_SECS).await?;
        return Ok(());
    }

    start_indexing_impl(app.clone(), None).await?;
    Ok(())
}

#[cfg(not(target_os = "windows"))]
async fn auto_start_flow<R: Runtime>(_app: tauri::AppHandle<R>) -> Result<(), String> {
    Ok(())
}

#[cfg(target_os = "windows")]
async fn start_indexing_impl<R: Runtime>(
    app: tauri::AppHandle<R>,
    drive_override: Option<String>,
) -> Result<IndexStatus, String> {
    {
        let st = INDEX_BUILD.lock().map_err(|e| e.to_string())?;
        if st.running {
            return Err("已有建索引任务在运行".into());
        }
    }

    let settings = findx_settings::load_findx_settings(app.clone())?;
    let base = findx_settings::exe_resource_dir();
    let cli = findx_settings::resolve_cli_exe(&base, &settings).ok_or_else(|| {
        mark_pending_auto_index_build(false);
        "未找到 findx2.exe（请放在与 FindX2 GUI / findx2-service 相同目录）".to_string()
    })?;
    let index = findx_settings::resolve_index_path(&base, &settings);

    let progress_path = index.with_extension("indexing.json");
    let mut cli_args: Vec<String> = vec![
        "index".to_string(),
        "--output".to_string(),
        index.to_string_lossy().into_owned(),
    ];
    // 优先级：调用方显式 drive_override > 设置面板 drives > CLI 默认（全盘）。
    if let Some(d) = drive_override.filter(|s| !s.trim().is_empty()) {
        cli_args.push("--volume".to_string());
        cli_args.push(d.trim().to_string());
    } else if !settings.drives.is_empty() {
        // 多卷走 `--volumes C:,D:`；CLI 解析时支持逗号分隔。
        let joined = settings
            .drives
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(",");
        if !joined.is_empty() {
            cli_args.push("--volumes".to_string());
            cli_args.push(joined);
        }
    }
    if settings.first_index_full_metadata {
        cli_args.push("--full-stat".to_string());
    }
    for dir in &settings.excluded_dirs {
        let d = dir.trim();
        if d.is_empty() {
            continue;
        }
        cli_args.push("--exclude-dir".to_string());
        cli_args.push(d.to_string());
    }
    cli_args.push("--progress-file".to_string());
    cli_args.push(progress_path.to_string_lossy().into_owned());

    let work_dir = cli
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    {
        let mut st = INDEX_BUILD.lock().map_err(|e| e.to_string())?;
        st.pending_auto_start = false;
        st.running = true;
    }

    let app_done = app.clone();
    let cli_spawn = cli.clone();
    let index_for_service = index.clone();
    std::thread::spawn(move || {
        let ok = run_index_cli_blocking(&cli_spawn, &cli_args, &work_dir);
        if let Ok(mut st) = INDEX_BUILD.lock() {
            st.running = false;
        }
        if ok {
            spawn_service_after_index_build(app_done, index_for_service);
        }
    });

    Ok(fetch_index_status(app).await)
}

#[cfg(not(target_os = "windows"))]
async fn start_indexing_impl<R: Runtime>(
    _app: tauri::AppHandle<R>,
    _drive_override: Option<String>,
) -> Result<IndexStatus, String> {
    Err("FindX2 GUI 建索引当前仅支持 Windows.".into())
}

/// 若尚无 `index.bin` 则先触发首遍建索引（默认全盘），否则直接拉起 findx2-service。
#[cfg(target_os = "windows")]
pub(crate) async fn ensure_service_running(app: tauri::AppHandle) -> Result<(), String> {
    let base = findx_settings::exe_resource_dir();
    let settings = findx_settings::load_findx_settings(app.clone())?;
    let index = findx_settings::resolve_index_path(&base, &settings);
    if !index.exists() {
        start_indexing_impl(app.clone(), None).await?;
        return Ok(());
    }
    let pipe_name = settings.pipe_name.trim();
    let pipe_name = if pipe_name.is_empty() {
        "findx2"
    } else {
        pipe_name
    };
    if pipe::ipc_status_for_pipe_name(pipe_name).await.is_ok() {
        return Ok(());
    }
    findx_settings::spawn_findx_service_process(app)?;
    wait_for_service_pipe(pipe_name, SERVICE_PIPE_WAIT_SECS).await?;
    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub(crate) async fn ensure_service_running(_app: tauri::AppHandle) -> Result<(), String> {
    Err("FindX2 索引服务仅支持 Windows".into())
}

#[cfg(target_os = "windows")]
#[tauri::command]
async fn start_findx_service(app: tauri::AppHandle) -> Result<(), String> {
    ensure_service_running(app).await
}

#[cfg(not(target_os = "windows"))]
#[tauri::command]
async fn start_findx_service(_app: tauri::AppHandle) -> Result<(), String> {
    Err("FindX2 索引服务仅支持 Windows".into())
}

#[tauri::command]
async fn start_indexing(
    app: tauri::AppHandle,
    drive: Option<String>,
    include_folders: Option<bool>,
    #[allow(non_snake_case)] includeFolders: Option<bool>,
    include_all_drives: Option<bool>,
    #[allow(non_snake_case)] includeAllDrives: Option<bool>,
) -> Result<IndexStatus, String> {
    let _ = (
        include_folders,
        includeFolders,
        include_all_drives,
        includeAllDrives,
    );
    start_indexing_impl(app, drive).await
}

#[tauri::command]
async fn index_status(app: tauri::AppHandle) -> IndexStatus {
    fetch_index_status(app).await
}

// 管道搜索；`pinyin` 未传时默认 true（与已启用 pinyin 的 findx2-service 配合）
#[tauri::command]
async fn search_files(
    app: tauri::AppHandle,
    query: String,
    extension: Option<String>,
    min_size: Option<u64>,
    max_size: Option<u64>,
    min_created_unix: Option<i64>,
    max_created_unix: Option<i64>,
    limit: Option<u32>,
    pinyin: Option<bool>,
) -> Result<SearchResponse, String> {
    #[cfg(target_os = "windows")]
    {
        let _ = (min_size, max_size, min_created_unix, max_created_unix);
        let settings = findx_settings::load_findx_settings(app.clone())
            .unwrap_or_else(|_| findx_settings::FindxGuiSettings::default());
        let pipe_name = settings.pipe_name.trim();
        let pipe_name = if pipe_name.is_empty() {
            "findx2"
        } else {
            pipe_name
        };
        let mut q = query.trim().to_string();
        if let Some(ext) = extension.filter(|s| !s.is_empty()) {
            q.push_str(&format!(" ext:{ext}"));
        }
        let lim = limit
            .unwrap_or(settings.search_limit)
            .clamp(1, 8192) as usize;
        let (dtos, total, elapsed_ms) = pipe::ipc_search_with_pipe_name(
            pipe_name,
            q,
            pinyin.unwrap_or(settings.pinyin_default),
            lim,
        )
        .await?;
        Ok(SearchResponse {
            hits: dtos.into_iter().map(map_search_hit).collect(),
            total,
            elapsed_ms,
        })
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (
            app,
            query,
            extension,
            min_size,
            max_size,
            min_created_unix,
            max_created_unix,
            limit,
            pinyin,
        );
        let _ = SearchResponse {
            hits: vec![],
            total: 0,
            elapsed_ms: 0,
        };
        Err("FindX2 GUI 搜索当前仅支持 Windows.".to_string())
    }
}

#[tauri::command]
fn cancel_search() -> Result<bool, String> {
    Ok(true)
}

/// 重建索引：停 service → 删 index.bin / sidecar / .indexing.json → 走 `start_indexing_impl`（含 drives / exclude）。
/// 完成后由现成的 `spawn_service_after_index_build` 自动起 service，无需用户再点「启动服务」。
#[cfg(target_os = "windows")]
#[tauri::command]
async fn rebuild_index(app: tauri::AppHandle) -> Result<IndexStatus, String> {
    {
        let st = INDEX_BUILD.lock().map_err(|e| e.to_string())?;
        if st.running {
            return Err("已有建索引任务在运行，无法重建".into());
        }
    }
    // 先停 service（不论 service / standalone 模式都尝试杀进程；后续的删文件就能顺利覆盖）。
    let _ = findx_settings::stop_findx_service();
    // 给 service 一点时间退出；不轮询 sleep 死等是为了尽快进入建库——若残留写盘竞争，rename 临时文件那一步会重试。
    std::thread::sleep(std::time::Duration::from_millis(800));

    let settings = findx_settings::load_findx_settings(app.clone())?;
    let base = findx_settings::exe_resource_dir();
    let index = findx_settings::resolve_index_path(&base, &settings);
    let _ = std::fs::remove_file(&index);
    // sidecar 命名约定与 findx2-core::persist::exclude_sidecar_path 保持一致：`<index>.exclude.json`。
    // 这里手拼是为了不把 findx2-core 拉成 GUI 依赖（GUI 已经依赖 findx2-windows 间接拿到，但 lib 层不直接 use）。
    {
        let mut p = index.as_os_str().to_owned();
        p.push(".exclude.json");
        let _ = std::fs::remove_file(std::path::PathBuf::from(p));
    }
    let _ = std::fs::remove_file(index.with_extension("indexing.json"));

    start_indexing_impl(app, None).await
}

#[cfg(not(target_os = "windows"))]
#[tauri::command]
async fn rebuild_index(_app: tauri::AppHandle) -> Result<IndexStatus, String> {
    Err("FindX2 GUI 重建索引当前仅支持 Windows.".into())
}

/// 切换运行模式（service ↔ standalone-UAC）：
/// - 切到 service 模式 → 调用 `findx2-service install`（提权）；
/// - 切到 standalone 模式 → 调用 `findx2-service uninstall`（提权）。
/// 注意：该命令**不**自动重启 GUI；前端在收到 Ok 后弹"需要重启"对话框，由用户点 `restart_app`。
#[cfg(target_os = "windows")]
#[tauri::command]
async fn apply_run_mode_change(app: tauri::AppHandle, target: String) -> Result<(), String> {
    let settings = findx_settings::load_findx_settings(app.clone())?;
    let base = findx_settings::exe_resource_dir();
    let exe = match findx_settings::resolve_cli_exe(&base, &settings) {
        Some(_) => {}
        None => {}
    };
    let _ = exe;
    let service_exe = if !settings.service_exe_path.trim().is_empty() {
        std::path::PathBuf::from(settings.service_exe_path.trim())
    } else {
        base.join("findx2-service.exe")
    };
    if !service_exe.exists() {
        return Err(format!(
            "找不到 findx2-service.exe（{}），无法注册/卸载系统服务。",
            service_exe.display()
        ));
    }
    let work = service_exe.parent().map(std::path::Path::to_path_buf).unwrap_or(base);
    let sub = match target.as_str() {
        "service" => "install",
        "standalone" => "uninstall",
        other => return Err(format!("未知的运行模式: {other}")),
    };

    use crate::elevate::shell_execute_runas;
    shell_execute_runas(&service_exe, Some(sub), &work, true)
        .map_err(|e| format!("提权执行 `findx2-service {sub}` 失败: {e}"))?;
    Ok(())
}

#[cfg(not(target_os = "windows"))]
#[tauri::command]
async fn apply_run_mode_change(_app: tauri::AppHandle, _target: String) -> Result<(), String> {
    Err("仅 Windows 支持服务模式切换".into())
}

/// 关闭并重启 FindX2 进程（用于「切换运行模式后请重启 FindX2 生效」的引导）。
#[tauri::command]
fn restart_app(app: tauri::AppHandle) {
    app.restart();
}

#[tauri::command]
async fn find_duplicate_groups(
    min_size: Option<u64>,
    max_groups: Option<u32>,
    max_files_per_group: Option<u32>,
) -> Result<Vec<DuplicateGroup>, String> {
    let _ = (min_size, max_groups, max_files_per_group);
    Ok(vec![])
}

#[tauri::command]
fn duplicate_scan_status() -> Result<DuplicateScanStatus, String> {
    Ok(DuplicateScanStatus {
        running: false,
        cancel_requested: false,
        scanned_files: 0,
        total_files: 0,
        groups_found: 0,
        progress_percent: 0.0,
    })
}

#[tauri::command]
fn cancel_duplicate_scan() -> Result<bool, String> {
    Ok(true)
}

#[tauri::command]
fn delete_path(
    path: String,
    recycle_bin: Option<bool>,
    #[allow(non_snake_case)] recycleBin: Option<bool>,
) -> Result<bool, String> {
    #[cfg(target_os = "windows")]
    {
        let _ = recycle_bin.or(recycleBin);
        let p = std::path::PathBuf::from(&path);
        if p.is_dir() {
            std::fs::remove_dir_all(&p).map_err(|e| e.to_string())?;
        } else {
            std::fs::remove_file(&p).map_err(|e| e.to_string())?;
        }
        Ok(true)
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (path, recycle_bin, recycleBin);
        Err("Delete is only supported on Windows.".to_string())
    }
}

#[tauri::command]
fn rename_path(path: String, new_name: String) -> Result<String, String> {
    #[cfg(target_os = "windows")]
    {
        use std::fs;
        use std::path::PathBuf;

        let current_path = PathBuf::from(path);
        if !current_path.exists() {
            return Err("File does not exist on disk.".to_string());
        }

        let trimmed_name = new_name.trim();
        if trimmed_name.is_empty() {
            return Err("Name cannot be empty.".to_string());
        }
        if trimmed_name.contains('\\') || trimmed_name.contains('/') {
            return Err("Name must not include path separators.".to_string());
        }

        let parent = current_path
            .parent()
            .ok_or_else(|| "Failed to resolve the parent directory.".to_string())?;
        let next_path = parent.join(trimmed_name);

        if next_path == current_path {
            return Ok(current_path.to_string_lossy().into_owned());
        }
        if next_path.exists() {
            return Err("An item with that name already exists.".to_string());
        }

        fs::rename(&current_path, &next_path)
            .map_err(|err| format!("Failed to rename item: {err}"))?;

        Ok(next_path.to_string_lossy().into_owned())
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (path, new_name);
        Err("Rename is only supported on Windows.".to_string())
    }
}

#[tauri::command]
fn list_drives() -> Result<Vec<DriveInfo>, String> {
    #[cfg(target_os = "windows")]
    {
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{
            GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
        };
        // GetDriveTypeW 返回值，对应 winapi 文档（windows-rs 0.61 的常量被拆到 SystemServices 子模块，直接用裸 u32 更稳）。
        const DRIVE_REMOVABLE: u32 = 2;
        const DRIVE_FIXED: u32 = 3;
        const DRIVE_REMOTE: u32 = 4;
        const DRIVE_CDROM: u32 = 5;
        const DRIVE_RAMDISK: u32 = 6;

        let mask = unsafe { GetLogicalDrives() };
        if mask == 0 {
            return Ok(vec![]);
        }
        let mut out = Vec::new();
        for i in 0..26u32 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let letter_char = (b'A' + i as u8) as char;
            // GetDriveTypeW / GetVolumeInformationW 需要 `<letter>:\` 形式（带尾反斜杠）。
            let root_w: Vec<u16> = format!("{letter_char}:\\").encode_utf16().chain([0]).collect();
            let dt = unsafe { GetDriveTypeW(PCWSTR(root_w.as_ptr())) };
            // 跳过 DRIVE_UNKNOWN / DRIVE_NO_ROOT_DIR；仅保留可用类型。
            let drive_type = match dt {
                DRIVE_FIXED => "fixed",
                DRIVE_REMOVABLE => "removable",
                DRIVE_REMOTE => "remote",
                DRIVE_CDROM => "cdrom",
                DRIVE_RAMDISK => "ramdisk",
                _ => continue,
            };
            // 拿文件系统名（NTFS / FAT32 / exFAT…）；可能因 CD 未插盘而失败，失败时填空串。
            let mut fs_buf = [0u16; 32];
            let mut vol_name = [0u16; 256];
            let mut serial = 0u32;
            let mut max_comp = 0u32;
            let mut fs_flags = 0u32;
            let ok = unsafe {
                GetVolumeInformationW(
                    PCWSTR(root_w.as_ptr()),
                    Some(&mut vol_name),
                    Some(&mut serial),
                    Some(&mut max_comp),
                    Some(&mut fs_flags),
                    Some(&mut fs_buf),
                )
            }
            .is_ok();
            let filesystem = if ok {
                String::from_utf16_lossy(&fs_buf)
                    .trim_end_matches('\0')
                    .to_string()
            } else {
                String::new()
            };
            // can_open_volume：固定盘且 NTFS 才能直接 \\.\C: 打开做 USN/MFT 全盘扫描；
            // 与 findx2-windows::volume::open_volume 的判断保持一致，避免 GUI 让用户选 FAT32 USB 后建库失败。
            let is_ntfs = filesystem.eq_ignore_ascii_case("NTFS");
            let can_open_volume = is_ntfs && (drive_type == "fixed" || drive_type == "removable");
            out.push(DriveInfo {
                letter: format!("{letter_char}:"),
                path: format!("{letter_char}:\\"),
                filesystem,
                drive_type: drive_type.to_string(),
                is_ntfs,
                can_open_volume,
            });
        }
        Ok(out)
    }

    #[cfg(not(target_os = "windows"))]
    {
        Err("磁盘列表当前未实现.".to_string())
    }
}

#[tauri::command]
fn open_file(app: tauri::AppHandle, path: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::path::PathBuf;

        let target = PathBuf::from(path);
        if !target.exists() {
            return Err("File does not exist on disk.".to_string());
        }

        let target_path = target.to_string_lossy().into_owned();
        app.opener()
            .open_path(target_path, None::<&str>)
            .map_err(|err| format!("Failed to open file: {err}"))?;
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (app, path);
        Err("File open is only supported on Windows.".to_string())
    }
}

#[tauri::command]
fn reveal_in_folder(app: tauri::AppHandle, path: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::path::PathBuf;

        let target = PathBuf::from(path);
        if !target.exists() {
            return Err("File does not exist on disk.".to_string());
        }

        app.opener()
            .reveal_item_in_dir(&target)
            .map_err(|err| format!("Failed to reveal file in folder: {err}"))?;
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (app, path);
        Err("Folder reveal is only supported on Windows.".to_string())
    }
}

#[tauri::command]
fn open_path_in_console(path: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::{os::windows::process::CommandExt, path::PathBuf, process::Command};
        use windows::Win32::System::Threading::CREATE_NEW_CONSOLE;

        let requested_path = PathBuf::from(path);
        if !requested_path.exists() {
            return Err("Path does not exist on disk.".to_string());
        }

        let target_directory = if requested_path.is_dir() {
            requested_path
        } else {
            requested_path.parent().map(std::path::Path::to_path_buf).ok_or_else(|| {
                "Failed to resolve the parent folder for the requested path.".to_string()
            })?
        };

        if !target_directory.is_dir() {
            return Err("Resolved console target is not a directory.".to_string());
        }

        Command::new("cmd.exe")
            .creation_flags(CREATE_NEW_CONSOLE.0)
            .current_dir(&target_directory)
            .spawn()
            .map_err(|err| format!("Failed to open a console for the selected path: {err}"))?;

        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = path;
        Err("Opening a console for a path is only supported on Windows.".to_string())
    }
}

#[tauri::command]
fn start_native_file_drag(window: tauri::WebviewWindow, path: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::path::PathBuf;
        use std::sync::mpsc;

        let file_path = PathBuf::from(&path);
        if !file_path.exists() {
            return Err("File does not exist on disk.".to_string());
        }
        if !file_path.is_file() {
            return Err("Only files can be dragged out of FindX2.".to_string());
        }

        let window_for_drag = window.clone();
        let path_for_drag = path.clone();
        let (tx, rx) = mpsc::channel();

        window
            .run_on_main_thread(move || {
                let result = start_native_file_drag_impl(&window_for_drag, &path_for_drag);
                let _ = tx.send(result);
            })
            .map_err(|err| format!("Failed to start the native drag request: {err}"))?;

        return rx
            .recv()
            .map_err(|_| "Failed to receive the native drag result.".to_string())?;
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (window, path);
        Err("Native file drag is only supported on Windows.".to_string())
    }
}

#[cfg(target_os = "windows")]
fn start_native_file_drag_impl<R: tauri::Runtime>(
    window: &tauri::WebviewWindow<R>,
    path: &str,
) -> Result<(), String> {
    let file_path = std::path::PathBuf::from(path);
    let hwnd = window
        .hwnd()
        .map_err(|err| format!("Failed to access the native window handle: {err}"))?;

    let folder_pidl = OwnedItemIdList::from_path(&file_path)?;
    let item_pidl = unsafe {
        let item_ptr = ILFindLastID(folder_pidl.as_ptr());
        if item_ptr.is_null() {
            return Err("Failed to resolve the dragged file in the Windows shell.".to_string());
        }

        let cloned_item = ILClone(item_ptr);
        if cloned_item.is_null() {
            return Err("Failed to clone the dragged file shell item.".to_string());
        }

        OwnedItemIdList(cloned_item)
    };

    if !unsafe { ILRemoveLastID(Some(folder_pidl.as_mut_ptr())) }.as_bool() {
        return Err("Failed to resolve the parent folder for drag and drop.".to_string());
    }

    let children = [item_pidl.as_ptr()];
    let data_object: IDataObject = unsafe {
        SHCreateDataObject(
            Some(folder_pidl.as_ptr()),
            Some(&children),
            None::<&IDataObject>,
        )
        .map_err(|err| format!("Failed to prepare the dragged file: {err}"))?
    };
    let drop_source: IDropSource = NativeFileDropSource.into();

    unsafe {
        SHDoDragDrop(Some(hwnd), &data_object, &drop_source, DROPEFFECT_COPY)
            .map_err(|err| format!("Failed to start the native file drag: {err}"))?;
    }

    Ok(())
}

/// 右键菜单必须在创建窗口的主线程上调用 `TrackPopupMenu` / Shell COM。
/// 同步 `mpsc::recv` 若在主线程上阻塞，会导致主线程无法执行 `run_on_main_thread` 投递的闭包 → 死锁「未响应」。
/// 使用 `async` + `oneshot::Receiver::await` 让出线程，主消息循环才能执行菜单。
#[tauri::command]
async fn show_hits_context_menu(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    path: String,
    screen_x: f64,
    screen_y: f64,
) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let window_for_menu = window.clone();
        let app_for_menu = app.clone();
        let path_for_menu = path;
        window
            .run_on_main_thread(move || {
                let result = (|| {
                    let hwnd = window_for_menu.hwnd().map_err(|e| e.to_string())?;
                    win_file_context_menu::run_composite_hit_menu(
                        &app_for_menu,
                        hwnd,
                        path_for_menu,
                        screen_x as i32,
                        screen_y as i32,
                    )
                })();
                let _ = tx.send(result);
            })
            .map_err(|e| format!("调度主线程失败: {e}"))?;

        return rx
            .await
            .map_err(|_| "主线程结果通道已断开。".to_string())
            .and_then(|inner| inner);
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (window, app, path, screen_x, screen_y);
        Err("右键菜单目前仅支持 Windows。".to_string())
    }
}

/// 启动系统预览处理器（Explorer 预览窗格的同一套机制）。
/// 必须在创建窗口的主线程调用 COM；用 oneshot + run_on_main_thread 避免阻塞 IPC 线程。
#[tauri::command]
async fn preview_show(
    window: tauri::WebviewWindow,
    path: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    dpr: f64,
) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let win_for_main = window.clone();
        window
            .run_on_main_thread(move || {
                let result = (|| {
                    let hwnd = win_for_main.hwnd().map_err(|e| e.to_string())?;
                    win_preview::show_preview(hwnd, path, x, y, w, h, dpr)
                })();
                let _ = tx.send(result);
            })
            .map_err(|e| format!("调度主线程失败: {e}"))?;
        rx.await
            .map_err(|_| "主线程结果通道已断开".to_string())
            .and_then(|inner| inner)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (window, path, x, y, w, h, dpr);
        Err("预览仅支持 Windows".to_string())
    }
}

#[tauri::command]
async fn preview_set_bounds(
    window: tauri::WebviewWindow,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    dpr: f64,
) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let win_for_main = window.clone();
        window
            .run_on_main_thread(move || {
                let result = (|| {
                    let hwnd = win_for_main.hwnd().map_err(|e| e.to_string())?;
                    win_preview::set_bounds(hwnd, x, y, w, h, dpr)
                })();
                let _ = tx.send(result);
            })
            .map_err(|e| format!("调度主线程失败: {e}"))?;
        rx.await
            .map_err(|_| "主线程结果通道已断开".to_string())
            .and_then(|inner| inner)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (window, x, y, w, h, dpr);
        Ok(())
    }
}

#[tauri::command]
async fn preview_hide(window: tauri::WebviewWindow, unload: Option<bool>) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let unload = unload.unwrap_or(false);
        let (tx, rx) = tokio::sync::oneshot::channel();
        window
            .run_on_main_thread(move || {
                let r = if unload {
                    win_preview::unload_preview()
                } else {
                    win_preview::hide_preview()
                };
                let _ = tx.send(r);
            })
            .map_err(|e| format!("调度主线程失败: {e}"))?;
        rx.await
            .map_err(|_| "主线程结果通道已断开".to_string())
            .and_then(|inner| inner)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (window, unload);
        Ok(())
    }
}

/// 系统预览不可用时的文件信息（大小、时间戳），供前端展示降级卡片。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviewFallbackFileInfo {
    size: u64,
    modified_unix: Option<i64>,
    created_unix: Option<i64>,
}

#[tauri::command]
fn preview_fallback_file_info(path: String) -> Result<PreviewFallbackFileInfo, String> {
    use std::time::UNIX_EPOCH;
    let p = Path::new(&path);
    if !p.exists() {
        return Err("路径不存在".to_string());
    }
    if !p.is_file() {
        return Err("不是文件".to_string());
    }
    let meta = std::fs::metadata(p).map_err(|e| format!("读取元数据失败: {e}"))?;
    let to_unix = |st: std::time::SystemTime| {
        st.duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs() as i64)
    };
    Ok(PreviewFallbackFileInfo {
        size: meta.len(),
        modified_unix: meta.modified().ok().and_then(to_unix),
        created_unix: meta.created().ok().and_then(to_unix),
    })
}

/// 文本/源码降级预览：未注册预览处理器、或前端判定可用纯文本展示时调用。
/// 读前 64 KB，按 UTF-8 lossy 解码，超出按字节裁剪。
#[tauri::command]
fn load_preview_text(path: String, max_bytes: Option<u64>) -> Result<String, String> {
    use std::fs::File;
    use std::io::Read;
    let max = max_bytes.unwrap_or(64 * 1024).min(1024 * 1024);
    let mut f = File::open(&path).map_err(|e| format!("打开文件失败: {e}"))?;
    let mut buf = vec![0u8; max as usize];
    let n = f.read(&mut buf).map_err(|e| format!("读取失败: {e}"))?;
    buf.truncate(n);
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

#[tauri::command]
fn open_external_url(app: tauri::AppHandle, url: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        app.opener()
            .open_url(url, None::<&str>)
            .map_err(|err| format!("Failed to open link: {err}"))?;
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (app, url);
        Err("Opening external links is only supported on Windows.".to_string())
    }
}

#[tauri::command]
fn load_preview_data_url(path: String) -> Result<String, String> {
    #[cfg(target_os = "windows")]
    {
        use std::fs;
        use std::path::PathBuf;

        let file_path = PathBuf::from(path);
        if !file_path.exists() {
            return Err("Preview target does not exist.".to_string());
        }
        if !file_path.is_file() {
            return Err("Preview target is not a file.".to_string());
        }

        let extension = file_path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();

        let mime = match extension.as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "bmp" => "image/bmp",
            "ico" => "image/x-icon",
            "pdf" => "application/pdf",
            "mp4" => "video/mp4",
            "webm" => "video/webm",
            "mov" => "video/quicktime",
            "m4v" => "video/x-m4v",
            "avi" => "video/x-msvideo",
            "mkv" => "video/x-matroska",
            "wmv" => "video/x-ms-wmv",
            _ => return Err("Preview not supported for this file type.".to_string()),
        };

        let metadata = fs::metadata(&file_path)
            .map_err(|err| format!("Preview metadata read failed: {err}"))?;
        let max_preview_bytes = match mime {
            "application/pdf" => 8 * 1024 * 1024_u64,
            "video/mp4" | "video/webm" | "video/quicktime" | "video/x-m4v" | "video/x-msvideo"
            | "video/x-matroska" | "video/x-ms-wmv" => 20 * 1024 * 1024_u64,
            _ => 12 * 1024 * 1024_u64,
        };

        if metadata.len() > max_preview_bytes {
            return Err(format!(
                "Preview skipped: file too large ({} bytes).",
                metadata.len()
            ));
        }

        let bytes = fs::read(&file_path).map_err(|err| format!("Preview read failed: {err}"))?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        Ok(format!("data:{mime};base64,{encoded}"))
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = path;
        Err("Preview loading is only supported on Windows.".to_string())
    }
}

/// 检测本机是否存在 FindX v1 的遗留痕迹，用于决定是否展示 v2 升级说明弹窗。
#[cfg(target_os = "windows")]
#[tauri::command]
fn detect_legacy_v1_installation(app: tauri::AppHandle) -> bool {
    let mut markers: Vec<PathBuf> = Vec::new();
    if let Ok(local_data) = app.path().local_data_dir() {
        markers.push(local_data.join("FindX").join("settings.json"));
        markers.push(local_data.join("FindX").join("engine_mem_stats.log"));
    }
    let exe_dir = findx_settings::exe_resource_dir();
    markers.push(exe_dir.join("FindX.exe"));
    markers.push(exe_dir.join("FindX.Service.exe"));
    markers.into_iter().any(|p| p.exists())
}

#[cfg(not(target_os = "windows"))]
#[tauri::command]
fn detect_legacy_v1_installation(_app: tauri::AppHandle) -> bool {
    false
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default();

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        builder = builder.manage(desktop::desktop_state_for_builder());
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            desktop::focus_existing_instance(app);
        }));
        builder = builder.plugin(tauri_plugin_global_shortcut::Builder::new().build());
        builder = builder.plugin(desktop::window_state_plugin());
    }

    builder
        .plugin(tauri_plugin_opener::init())
        .on_page_load(|webview, payload| {
            if payload.event() != PageLoadEvent::Finished {
                return;
            }
            if webview.label() != "main" {
                return;
            }
            let w = webview.window();
            if w.show().is_err() {
                return;
            }
            if !MAIN_WINDOW_SHOWN_AFTER_LOAD.swap(true, Ordering::SeqCst) {
                let _ = w.set_focus();
            }
        })
        .invoke_handler(tauri::generate_handler![
            start_indexing,
            index_status,
            search_files,
            cancel_search,
            find_duplicate_groups,
            duplicate_scan_status,
            cancel_duplicate_scan,
            delete_path,
            rename_path,
            list_drives,
            open_file,
            reveal_in_folder,
            show_hits_context_menu,
            open_path_in_console,
            start_native_file_drag,
            open_external_url,
            app_update::check_app_update,
            load_preview_data_url,
            detect_legacy_v1_installation,
            load_preview_text,
            preview_fallback_file_info,
            preview_show,
            preview_set_bounds,
            preview_hide,
            findx_settings::load_findx_settings,
            findx_settings::save_findx_settings,
            start_findx_service,
            findx_settings::stop_findx_service,
            rebuild_index,
            apply_run_mode_change,
            restart_app,
            desktop::get_desktop_settings,
            desktop::open_full_window_command,
            desktop::open_quick_window_command,
            desktop::reset_window_layout_command,
            desktop::sync_window_theme_command,
            desktop::show_settings_window,
            desktop::hide_settings_window,
            desktop::update_desktop_settings
        ])
        .setup(|app| {
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            {
                desktop::setup(app)?;
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            {
                desktop::handle_window_event(window, event);
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
