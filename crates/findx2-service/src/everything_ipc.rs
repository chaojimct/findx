//! Everything SDK v2 兼容 IPC 兼容层。
//!
//! 与 Everything 1.4 SDK 一致的窗口类与消息：
//!
//! - `EVERYTHING_TASKBAR_NOTIFICATION`：消息接收窗口（QUERY/QUERY2 等）
//! - `EVERYTHING`：兼容历史 SDK 的副窗口（部分客户端通过 `FindWindow("EVERYTHING")` 找）
//!
//! 与 voidtools SDK / FindX v1 对齐要点：
//! - `EVERYTHING_IPC_QUERYW` 布局：`max_results, offset, reply_copydata_message, search_flags, reply_hwnd, search_string`
//!   （`reply_copydata_message` 在偏移 **8**，回复时作 `COPYDATASTRUCT.dwData`；偏移 16 为 `reply_hwnd`，
//!   与 `WM_COPYDATA` 的 `wParam` 重复；旧实现误读 16 为 reply_msg 会导致客户端收不到结果。）
//! - QUERYA / QUERYW 按字符宽度回包（A → 本地窄字节、W → UTF-16）
//! - QUERY2 的 `sort_type` 映射为 `ParsedQuery::sort_by/sort_desc`
//! - `IS_DB_LOADED` 表示索引已可搜索（含空库和未完成的元数据）。`IS_DB_BUSY` 只表示正在加载或回填。
//! - 回复：`WM_COPYDATA` 在独立线程中延迟约 5ms 再 `SendMessage`，与 v1 一致，避免与部分客户端的发送路径重入

#![allow(unsafe_code)]

use std::io::Write;
use std::mem;
use std::slice;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use findx2_core::{ParsedQuery, QueryParser, SearchEngine, SearchHit, SearchOptions, SortField};
use findx2_ipc::{IpcRequest, IpcResponse, SearchHitDto};
use tracing::{error, info};
use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

/// 持有当前 SearchEngine 的全局句柄，IPC 窗口过程通过此读引擎。
static ENGINE: Mutex<Option<Arc<SearchEngine>>> = Mutex::new(None);

/// Session 0 宿主模式：不持有引擎，查询转发到系统服务命名管道。
static PIPE_NAME: Mutex<Option<String>> = Mutex::new(None);

/// 外部模块（如服务的建库/回填子系统）通过此 hook 报告 IS_DB_BUSY；返回 true 表示当前忙。
static IS_DB_BUSY_HOOK: OnceLock<Box<dyn Fn() -> bool + Send + Sync>> = OnceLock::new();

/// 注册 IS_DB_BUSY 状态读取闭包；只允许设置一次（多次设置忽略）。
#[allow(dead_code)]
pub(crate) fn set_is_db_busy_hook<F>(f: F)
where
    F: Fn() -> bool + Send + Sync + 'static,
{
    let _ = IS_DB_BUSY_HOOK.set(Box::new(f));
}

const COPYDATA_COMMAND_LINE_UTF8: isize = 0;
const COPYDATA_QUERYA: isize = 1;
const COPYDATA_QUERYW: isize = 2;
const COPYDATA_QUERY2A: isize = 17;
const COPYDATA_QUERY2W: isize = 18;
const COPYDATA_GET_RUN_COUNTA: isize = 19;
const COPYDATA_GET_RUN_COUNTW: isize = 20;
const COPYDATA_SET_RUN_COUNTA: isize = 21;
const COPYDATA_SET_RUN_COUNTW: isize = 22;
const COPYDATA_INC_RUN_COUNTA: isize = 23;
const COPYDATA_INC_RUN_COUNTW: isize = 24;

const IPC_GET_MAJOR_VERSION: usize = 0;
const IPC_GET_MINOR_VERSION: usize = 1;
const IPC_GET_REVISION: usize = 2;
const IPC_GET_BUILD_NUMBER: usize = 3;
const IPC_GET_TARGET_MACHINE: usize = 5;
const IPC_IS_NTFS_DRIVE_INDEXED: usize = 400;
const IPC_IS_DB_LOADED: usize = 401;
const IPC_IS_DB_BUSY: usize = 402;
const IPC_IS_FAST_SORT: usize = 410;
const IPC_IS_FILE_INFO_INDEXED: usize = 411;

/// Everything IPC `search_flags`（everything_ipc.h）
const IPC_REGEX: u32 = 0x0001;
const IPC_MATCHCASE: u32 = 0x0002;
const IPC_MATCHWHOLEWORD: u32 = 0x0004;
const IPC_MATCHPATH: u32 = 0x0008;

const WS_POPUP: WINDOW_STYLE = WINDOW_STYLE(0x8000_0000);

pub(crate) fn spawn_everything_ipc(engine: Arc<SearchEngine>) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("findx2-everything-ipc".into())
        .spawn(move || {
            {
                let mut g = ENGINE.lock().unwrap();
                *g = Some(engine);
            }
            unsafe {
                if let Err(e) = message_loop() {
                    error!("Everything IPC 线程退出: {e:?}");
                }
            }
            let mut g = ENGINE.lock().unwrap();
            *g = None;
        })
        .expect("spawn everything ipc")
}

/// 用户会话进程入口：只建 Everything 窗口，搜索走已有 `findx2` 管道。
pub(crate) fn run_everything_host_via_pipe(pipe: String) -> anyhow::Result<()> {
    {
        let mut g = PIPE_NAME.lock().unwrap();
        *g = Some(pipe);
    }
    let r = unsafe { message_loop() };
    let mut g = PIPE_NAME.lock().unwrap();
    *g = None;
    r.map_err(|e| anyhow::anyhow!("Everything 兼容窗口: {e}"))
}

fn wide_z(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

unsafe extern "system" fn visit_everything_window(hwnd: HWND, data: LPARAM) -> windows::core::BOOL {
    let mut class = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut class);
    if n <= 0 {
        return true.into();
    }
    let name = String::from_utf16_lossy(&class[..n as usize]);
    if name != crate::everything_status::SDK_WINDOW_CLASS_IPC
        && name != crate::everything_status::SDK_WINDOW_CLASS_MAIN
    {
        return true.into();
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid != std::process::id() {
        let count = &mut *(data.0 as *mut u32);
        *count += 1;
        info!(
            "Everything 类名冲突 hwnd={:?} pid={pid} class={name}。{}",
            hwnd,
            crate::everything_status::COMPETING_PROCESS_POLICY
        );
    }
    true.into()
}

fn log_foreign_everything_windows() {
    let mut foreign = 0u32;
    unsafe {
        let _ = EnumWindows(
            Some(visit_everything_window),
            LPARAM(&mut foreign as *mut u32 as isize),
        );
    }
    if foreign == 0 {
        info!("没有其他进程占用 Everything SDK 窗口类");
    }
}

unsafe fn message_loop() -> windows::core::Result<()> {
    let hinst = GetModuleHandleW(None)?;
    let hi = HINSTANCE(hinst.0);

    let class_ipc_w = wide_z(crate::everything_status::SDK_WINDOW_CLASS_IPC);
    let class_alt_w = wide_z(crate::everything_status::SDK_WINDOW_CLASS_MAIN);
    let title_w = wide_z(crate::everything_status::SDK_WINDOW_CLASS_MAIN);
    let class_ipc = PCWSTR(class_ipc_w.as_ptr());
    let class_alt = PCWSTR(class_alt_w.as_ptr());
    let title = PCWSTR(title_w.as_ptr());

    // 与 FindX v1 一致：始终在本进程注册窗口类。若与真 Everything 同时运行，`FindWindow` 只会找到其中一个，
    // 并行双开时请自行退出 Everything 或仅用其一；此前用 FindWindow 直接 return 会导致「未装 Everything 也从未创建 IPC」的误判场景更难排查。

    let wc_ipc = WNDCLASSEXW {
        cbSize: mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wnd_proc),
        hInstance: hi,
        lpszClassName: class_ipc,
        ..Default::default()
    };
    if RegisterClassExW(&wc_ipc) == 0 {
        let err = windows::core::Error::from_thread();
        error!("RegisterClassEx EVERYTHING_TASKBAR_NOTIFICATION 失败: {err}");
        return Err(err);
    }

    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        class_ipc,
        title,
        WS_POPUP,
        0,
        0,
        0,
        0,
        Some(HWND::default()),
        None,
        Some(hi),
        None,
    )
    .unwrap_or_default();

    let wc_alt = WNDCLASSEXW {
        cbSize: mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wnd_proc),
        hInstance: hi,
        lpszClassName: class_alt,
        ..Default::default()
    };

    let mut hwnd_alt = HWND::default();
    if RegisterClassExW(&wc_alt) != 0 {
        hwnd_alt = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_alt,
            title,
            WS_POPUP,
            0,
            0,
            0,
            0,
            Some(HWND::default()),
            None,
            Some(hi),
            None,
        )
        .unwrap_or_default();
    }

    if !hwnd.is_invalid() {
        allow_lower_integrity_ipc(hwnd)?;
    }
    if !hwnd_alt.is_invalid() {
        allow_lower_integrity_ipc(hwnd_alt)?;
    }

    if hwnd.is_invalid() && hwnd_alt.is_invalid() {
        error!("Everything IPC：窗口创建失败");
        return Err(windows::core::Error::from_thread());
    }

    log_foreign_everything_windows();
    info!(
        "Everything IPC 兼容层已启动。{}",
        crate::everything_status::COMPETING_PROCESS_POLICY
    );

    loop {
        let mut msg = MSG::default();
        let r = GetMessageW(&mut msg, Some(HWND::default()), 0, 0);
        if !r.as_bool() {
            break;
        }
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }

    if !hwnd.is_invalid() {
        let _ = DestroyWindow(hwnd);
    }
    if !hwnd_alt.is_invalid() {
        let _ = DestroyWindow(hwnd_alt);
    }
    let _ = UnregisterClassW(class_ipc, Some(hi));
    let _ = UnregisterClassW(class_alt, Some(hi));

    Ok(())
}

unsafe fn allow_lower_integrity_ipc(hwnd: HWND) -> windows::core::Result<()> {
    ChangeWindowMessageFilterEx(hwnd, WM_COPYDATA, MSGFLT_ALLOW, None)?;
    ChangeWindowMessageFilterEx(hwnd, WM_USER, MSGFLT_ALLOW, None)?;
    Ok(())
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_COPYDATA {
        return handle_copydata(hwnd, wparam, lparam).unwrap_or(LRESULT(0));
    }
    if msg == WM_USER {
        return handle_wm_user(wparam, lparam).unwrap_or(LRESULT(0));
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

fn external_busy() -> bool {
    IS_DB_BUSY_HOOK.get().is_some_and(|hook| hook())
}

fn current_snapshot() -> crate::everything_status::EngineSnapshot {
    if PIPE_NAME.lock().ok().and_then(|g| g.clone()).is_some() {
        return crate::everything_status::snapshot_from_status(pipe_status());
    }
    let g = match ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return crate::everything_status::snapshot_from_status(None),
    };
    let Some(eng) = g.as_ref() else {
        return crate::everything_status::snapshot_from_status(None);
    };
    let store = eng.index_store();
    crate::everything_status::snapshot_from_status(Some(crate::everything_status::StatusParts {
        loading: false,
        metadata_ready: eng.metadata_ready(),
        backfill_done: 0,
        backfill_total: 0,
        drive_mask: crate::everything_status::drive_mask_from_letters(
            store.volumes.iter().map(|v| v.volume_letter),
        ),
        volume_letter: None,
    }))
}

fn pipe_status() -> Option<crate::everything_status::StatusParts> {
    let pipe = PIPE_NAME.lock().ok()?.clone()?;
    match pipe_rpc(&pipe, &IpcRequest::Status)? {
        IpcResponse::StatusResult {
            metadata_ready,
            loading,
            backfill_done,
            backfill_total,
            indexed_drive_mask,
            volume_letter,
            ..
        } => Some(crate::everything_status::StatusParts {
            loading,
            metadata_ready,
            backfill_done,
            backfill_total,
            drive_mask: indexed_drive_mask,
            volume_letter,
        }),
        _ => None,
    }
}

fn handle_wm_user(wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    let cmd = wparam.0 as usize;
    let snapshot = current_snapshot();
    let flags = crate::everything_status::sdk_flags(snapshot, external_busy());
    let r = match cmd {
        IPC_GET_MAJOR_VERSION => 1usize,
        IPC_GET_MINOR_VERSION => 4,
        IPC_GET_REVISION => 1,
        IPC_GET_BUILD_NUMBER => 1026,
        IPC_GET_TARGET_MACHINE => target_machine_value(),
        IPC_IS_NTFS_DRIVE_INDEXED => {
            crate::everything_status::drive_indexed(snapshot.drive_mask, lparam.0 as u32) as usize
        }
        IPC_IS_DB_LOADED => flags.loaded as usize,
        IPC_IS_DB_BUSY => flags.busy as usize,
        IPC_IS_FAST_SORT => {
            crate::everything_status::fast_sort_supported(flags.loaded == 1, lparam.0 as u32) as usize
        }
        IPC_IS_FILE_INFO_INDEXED => flags.file_info_indexed as usize,
        _ => 0,
    };
    Some(LRESULT(r as isize))
}

#[cfg(target_arch = "x86")]
fn target_machine_value() -> usize {
    1
}
#[cfg(target_arch = "x86_64")]
fn target_machine_value() -> usize {
    2
}
#[cfg(target_arch = "aarch64")]
fn target_machine_value() -> usize {
    3
}
#[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
fn target_machine_value() -> usize {
    2
}

unsafe fn handle_copydata(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    let cds = &*(lparam.0 as *const COPYDATASTRUCT);
    let reply_hwnd = HWND(wparam.0 as *mut _);
    match cds.dwData as isize {
        COPYDATA_COMMAND_LINE_UTF8 => Some(LRESULT(1)),
        COPYDATA_QUERYA => handle_query(cds, reply_hwnd, hwnd, false, true),
        COPYDATA_QUERYW => handle_query(cds, reply_hwnd, hwnd, true, true),
        COPYDATA_QUERY2A => handle_query2(cds, reply_hwnd, hwnd, false),
        COPYDATA_QUERY2W => handle_query2(cds, reply_hwnd, hwnd, true),
        COPYDATA_GET_RUN_COUNTA | COPYDATA_GET_RUN_COUNTW => Some(LRESULT(0)),
        COPYDATA_SET_RUN_COUNTA | COPYDATA_SET_RUN_COUNTW => Some(LRESULT(1)),
        COPYDATA_INC_RUN_COUNTA | COPYDATA_INC_RUN_COUNTW => Some(LRESULT(0)),
        _ => Some(LRESULT(0)),
    }
}

/// 处理 QUERY / QUERY2 公共部分；`wide` 控制请求字符串编码与回复字符宽度。
unsafe fn handle_query(
    cds: &COPYDATASTRUCT,
    reply_hwnd: HWND,
    my_hwnd: HWND,
    wide: bool,
    _legacy_query1: bool,
) -> Option<LRESULT> {
    if cds.cbData < 20 {
        return Some(LRESULT(0));
    }
    let data = slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize);
    // SDK：`reply_copydata_message` @8，`search_flags` @12，`reply_hwnd` @16（与 wParam 一致）
    let parsed = crate::everything_status::parse_query1(data, wide)?;
    let mut max = parsed.max_results as usize;
    if max == 0 {
        max = 100;
    }
    let results = do_search(
        &parsed.search,
        parsed.offset as usize + max,
        parsed.search_flags,
        SortField::Name,
        false,
    )?;
    send_query1_reply(
        reply_hwnd, my_hwnd, parsed.reply_msg, results, parsed.offset, max as u32, wide,
    );
    Some(LRESULT(1))
}

unsafe fn handle_query2(
    cds: &COPYDATASTRUCT,
    reply_hwnd: HWND,
    my_hwnd: HWND,
    wide: bool,
) -> Option<LRESULT> {
    if cds.cbData < 28 {
        return Some(LRESULT(0));
    }
    let data = slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize);
    let parsed = crate::everything_status::parse_query2(data, wide)?;
    let mut max = parsed.max_results as usize;
    if max == 0 {
        max = 100;
    }
    let (sort_field, sort_desc) = map_sort_type(parsed.sort_type);
    let results = do_search(
        &parsed.search,
        parsed.offset as usize + max,
        parsed.search_flags,
        sort_field,
        sort_desc,
    )?;
    send_query2_reply(
        reply_hwnd,
        my_hwnd,
        parsed.reply_msg,
        results,
        parsed.offset,
        max as u32,
        parsed.request_flags,
        wide,
    );
    Some(LRESULT(1))
}

/// 将 Everything SDK `EVERYTHING_IPC_SORT_*` 数值映射到 `(SortField, sort_desc)`。
fn map_sort_type(t: u32) -> (SortField, bool) {
    match t {
        1 => (SortField::Name, false),
        2 => (SortField::Name, true),
        3 => (SortField::Path, false),
        4 => (SortField::Path, true),
        5 => (SortField::Size, false),
        6 => (SortField::Size, true),
        // 7/8 EXTENSION：先按 Name 退化
        7 | 8 => (SortField::Name, t == 8),
        // 9/10 TYPE_NAME 同样按 Name
        9 | 10 => (SortField::Name, t == 10),
        11 => (SortField::Created, false),
        12 => (SortField::Created, true),
        13 => (SortField::Modified, false),
        14 => (SortField::Modified, true),
        // 15/16 ATTRIBUTES：按 Name
        15 | 16 => (SortField::Name, t == 16),
        17 | 18 => (SortField::Name, t == 18),
        19 | 20 => (SortField::Name, t == 20),
        21 => (SortField::Modified, false),
        22 => (SortField::Modified, true),
        23 | 24 => (SortField::Name, t == 24),
        25 | 26 => (SortField::Name, t == 26),
        _ => (SortField::Name, false),
    }
}

/// `query`：查询字符串；`max_total`：上限（含 offset）；`ipc_flags`：`EVERYTHING_IPC_*` search_flags。
fn do_search(
    query: &str,
    max_total: usize,
    ipc_flags: u32,
    sort_field: SortField,
    sort_desc: bool,
) -> Option<Vec<IpcHit>> {
    let q = query.trim();
    if q.is_empty() {
        return Some(vec![]);
    }
    if PIPE_NAME.lock().ok()?.is_some() {
        return do_search_via_pipe(q, max_total);
    }
    let engine = ENGINE.lock().ok()?.as_ref()?.clone();
    let mut pq: ParsedQuery = QueryParser::parse(q).ok()?;
    apply_ipc_search_flags(&mut pq, ipc_flags);
    pq.limit = max_total.min(8192) as u32;
    pq.sort_by = sort_field;
    pq.sort_desc = sort_desc;
    // 与命名管道 search_ipc、GUI 默认一致：Everything 协议不传拼音开关，第三方（IbEverythingExt 等）
    // 发来的简拼如 mctjl 需走 lita 拼音路径；查询串里 `;en` / `;np` 仍可通过 ParsedQuery 关闭拼音。
    let opt = SearchOptions {
        allow_pinyin: true,
        content: crate::content_index::content_opt(),
        ..Default::default()
    };
    let (hits, _total) = engine.search(&pq, &opt).ok()?;
    Some(
        hits.iter()
            .filter_map(|h| ipc_hit_from_search(&engine, h))
            .collect(),
    )
}

fn do_search_via_pipe(query: &str, max_total: usize) -> Option<Vec<IpcHit>> {
    let pipe = PIPE_NAME.lock().ok()?.clone()?;
    let req = IpcRequest::Search {
        query: query.to_string(),
        pinyin: true,
        limit: max_total.min(8192),
        offset: 0,
        pinyin_initials: true,
        pinyin_full: true,
    };
    match pipe_rpc(&pipe, &req)? {
        IpcResponse::SearchResult { hits, .. } => {
            Some(hits.into_iter().map(dto_to_ipc_hit).collect())
        }
        _ => None,
    }
}

fn dto_to_ipc_hit(h: SearchHitDto) -> IpcHit {
    let parent = h
        .path
        .rfind('\\')
        .map(|i| h.path[..i].to_string())
        .unwrap_or_default();
    IpcHit {
        full_path: h.path,
        name: h.name,
        parent,
        is_dir: h.is_directory,
        size: h.size,
        mtime: h.mtime,
        ctime: 0,
        attrs: 0,
    }
}

fn pipe_rpc(pipe: &str, req: &IpcRequest) -> Option<IpcResponse> {
    use std::fs::OpenOptions;
    use std::io::{BufRead, BufReader, Write};
    let path = if pipe.starts_with(r"\\") {
        pipe.to_string()
    } else {
        format!(r"\\.\pipe\{pipe}")
    };
    let mut f = OpenOptions::new().read(true).write(true).open(&path).ok()?;
    let mut body = serde_json::to_string(req).ok()?;
    body.push('\n');
    f.write_all(body.as_bytes()).ok()?;
    f.flush().ok()?;
    let mut line = String::new();
    BufReader::new(f).read_line(&mut line).ok()?;
    serde_json::from_str(line.trim()).ok()
}

fn apply_ipc_search_flags(pq: &mut ParsedQuery, flags: u32) {
    if flags == 0 {
        return;
    }
    if flags & IPC_MATCHCASE != 0 {
        pq.case_sensitive = true;
    }
    if flags & IPC_MATCHWHOLEWORD != 0 {
        pq.whole_word = true;
    }
    if flags & IPC_MATCHPATH != 0 {
        // 在全路径上匹配（不仅文件名）
        pq.nowfn = false;
    }
    if flags & IPC_REGEX != 0 && pq.regex_pattern.is_none() {
        if !pq.name_terms.is_empty() {
            let s = pq.name_terms.join(" ");
            pq.name_terms.clear();
            pq.substring = None;
            pq.regex_pattern = Some(s);
        } else if let Some(s) = pq.substring.clone() {
            if !s.is_empty() {
                pq.substring = None;
                pq.regex_pattern = Some(s);
            }
        }
    }
}

type IpcHit = crate::everything_status::IpcHit;

fn ipc_hit_from_search(engine: &SearchEngine, h: &SearchHit) -> Option<IpcHit> {
    let store = engine.index_store();
    let e = store.entries.get(h.entry_idx as usize)?;
    let is_dir = e.is_dir_entry();
    let full_path = h.path.clone();
    let name = h.name.clone();
    let parent = full_path
        .rfind('\\')
        .map(|i| full_path[..i].to_string())
        .unwrap_or_default();
    Some(IpcHit {
        full_path,
        name,
        parent,
        is_dir,
        size: h.size,
        mtime: h.mtime,
        // entries 内部存 u32 unix-secs，IPC 需 FILETIME 100ns。
        ctime: findx2_core::index::unix_secs_to_filetime(e.ctime),
        attrs: e.attrs & 0xff,
    })
}

fn send_query1_reply(
    reply_hwnd: HWND,
    my_hwnd: HWND,
    reply_msg: u32,
    results: Vec<IpcHit>,
    offset: u32,
    max_results: u32,
    wide: bool,
) {
    let start = offset.min(results.len() as u32) as usize;
    let remain = results.len().saturating_sub(start);
    let count = (max_results as usize).min(remain);
    let buf = if wide {
        crate::everything_status::build_list_w(&results, offset, start, count)
    } else {
        crate::everything_status::build_list_a(&results, offset, start, count)
    };
    spawn_copydata_reply(reply_hwnd, my_hwnd, reply_msg, buf);
}

fn send_query2_reply(
    reply_hwnd: HWND,
    my_hwnd: HWND,
    reply_msg: u32,
    results: Vec<IpcHit>,
    offset: u32,
    max_results: u32,
    request_flags: u32,
    wide: bool,
) {
    let start = offset.min(results.len() as u32) as usize;
    let remain = results.len().saturating_sub(start);
    let count = (max_results as usize).min(remain);
    let buf = crate::everything_status::build_list2(&results, offset, start, count, request_flags, wide);
    spawn_copydata_reply(reply_hwnd, my_hwnd, reply_msg, buf);
}

/// 与 FindX v1 一致：短暂延迟后再 `SendMessage(WM_COPYDATA)`，避免客户端同步路径死锁。
fn spawn_copydata_reply(reply_hwnd: HWND, my_hwnd: HWND, reply_msg: u32, buf: Vec<u8>) {
    let reply_raw = reply_hwnd.0 as usize;
    let my_raw = my_hwnd.0 as usize;
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(5));
        unsafe {
            let reply_hwnd = HWND(reply_raw as *mut _);
            let mut cds = COPYDATASTRUCT {
                dwData: reply_msg as usize,
                cbData: buf.len() as u32,
                lpData: buf.as_ptr() as *mut _,
            };
            let p: *mut COPYDATASTRUCT = &mut cds;
            let _ = SendMessageW(
                reply_hwnd,
                WM_COPYDATA,
                Some(WPARAM(my_raw)),
                Some(LPARAM(p as isize)),
            );
        }
    });
}

