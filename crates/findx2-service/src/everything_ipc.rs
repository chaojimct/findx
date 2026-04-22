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
//! - `IS_DB_BUSY` / `IS_DB_LOADED` 接 `SearchEngine::metadata_ready` 等
//! - 回复：`WM_COPYDATA` 在独立线程中延迟约 5ms 再 `SendMessage`，与 v1 一致，避免与部分客户端的发送路径重入

#![allow(unsafe_code)]

use std::io::Write;
use std::mem;
use std::slice;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use findx2_core::{ParsedQuery, QueryParser, SearchEngine, SearchHit, SearchOptions, SortField};
use tracing::{error, info};
use windows::Win32::Foundation::*;
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

/// 持有当前 SearchEngine 的全局句柄，IPC 窗口过程通过此读引擎。
static ENGINE: Mutex<Option<Arc<SearchEngine>>> = Mutex::new(None);

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

const REQUEST_NAME: u32 = 0x0000_0001;
const REQUEST_PATH: u32 = 0x0000_0002;
const REQUEST_FULL_PATH_AND_NAME: u32 = 0x0000_0004;
const REQUEST_EXTENSION: u32 = 0x0000_0008;
const REQUEST_SIZE: u32 = 0x0000_0010;
const REQUEST_DATE_CREATED: u32 = 0x0000_0020;
const REQUEST_DATE_MODIFIED: u32 = 0x0000_0040;
const REQUEST_DATE_ACCESSED: u32 = 0x0000_0080;
const REQUEST_ATTRIBUTES: u32 = 0x0000_0100;
const REQUEST_FILE_LIST_FILE_NAME: u32 = 0x0000_0200;
const REQUEST_RUN_COUNT: u32 = 0x0000_0400;
const REQUEST_DATE_RUN: u32 = 0x0000_0800;
const REQUEST_DATE_RECENTLY_CHANGED: u32 = 0x0000_1000;
const REQUEST_HIGHLIGHTED_NAME: u32 = 0x0000_2000;
const REQUEST_HIGHLIGHTED_PATH: u32 = 0x0000_4000;
const REQUEST_HIGHLIGHTED_FULL_PATH_AND_NAME: u32 = 0x0000_8000;

const ITEM_FOLDER: u32 = 0x01;

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

unsafe fn message_loop() -> windows::core::Result<()> {
    let hinst = GetModuleHandleW(None)?;
    let hi = HINSTANCE(hinst.0);

    let class_ipc = windows::core::w!("EVERYTHING_TASKBAR_NOTIFICATION");
    let class_alt = windows::core::w!("EVERYTHING");

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
        let err = windows::core::Error::from_win32();
        error!("RegisterClassEx EVERYTHING_TASKBAR_NOTIFICATION 失败: {err}");
        return Err(err);
    }

    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        class_ipc,
        windows::core::w!("EVERYTHING"),
        WS_POPUP,
        0,
        0,
        0,
        0,
        HWND::default(),
        None,
        hi,
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
            windows::core::w!("EVERYTHING"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            HWND::default(),
            None,
            hi,
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
        return Err(windows::core::Error::from_win32());
    }

    info!("Everything IPC 兼容层已启动");

    loop {
        let mut msg = MSG::default();
        let r = GetMessageW(&mut msg, HWND::default(), 0, 0);
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
    let _ = UnregisterClassW(class_ipc, hi);
    let _ = UnregisterClassW(class_alt, hi);

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
        return handle_wm_user(wparam).unwrap_or(LRESULT(0));
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

fn db_busy_now() -> bool {
    if let Some(hook) = IS_DB_BUSY_HOOK.get() {
        if (hook)() {
            return true;
        }
    }
    let g = match ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let Some(eng) = g.as_ref() else {
        return false;
    };
    !eng.metadata_ready()
}

fn db_is_loaded() -> bool {
    let g = match ENGINE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    g.as_ref()
        .map(|eng| eng.index_store().entry_count() > 0)
        .unwrap_or(false)
}

fn handle_wm_user(wparam: WPARAM) -> Option<LRESULT> {
    let cmd = wparam.0 as usize;
    let r = match cmd {
        IPC_GET_MAJOR_VERSION => 1usize,
        IPC_GET_MINOR_VERSION => 4,
        IPC_GET_REVISION => 1,
        IPC_GET_BUILD_NUMBER => 1026,
        IPC_GET_TARGET_MACHINE => target_machine_value(),
        IPC_IS_NTFS_DRIVE_INDEXED => 1,
        IPC_IS_DB_LOADED => {
            if db_is_loaded() {
                1
            } else {
                0
            }
        }
        IPC_IS_DB_BUSY => {
            if db_busy_now() {
                1
            } else {
                0
            }
        }
        IPC_IS_FAST_SORT => 1,
        IPC_IS_FILE_INFO_INDEXED => 1,
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
    let max_results = read_u32_le(data, 0).unwrap_or(100);
    let offset = read_u32_le(data, 4).unwrap_or(0);
    // SDK：`reply_copydata_message` @8，`search_flags` @12，`reply_hwnd` @16（与 wParam 一致）
    let reply_msg = read_u32_le(data, 8).unwrap_or(0);
    if reply_msg == 0 {
        return Some(LRESULT(0));
    }
    let search_flags = read_u32_le(data, 12).unwrap_or(0);
    let search_str = if wide {
        extract_wstring(&data[20..])
    } else {
        extract_astring(&data[20..])
    };
    let mut max = max_results as usize;
    if max == 0 {
        max = 100;
    }
    let results = do_search(
        &search_str,
        offset as usize + max,
        search_flags,
        SortField::Name,
        false,
    )?;
    send_query1_reply(reply_hwnd, my_hwnd, reply_msg, results, offset, max as u32, wide);
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
    let reply_msg = read_u32_le(data, 4)?;
    let search_flags = read_u32_le(data, 8).unwrap_or(0);
    let offset = read_u32_le(data, 12).unwrap_or(0);
    let max_results = read_u32_le(data, 16).unwrap_or(100);
    let request_flags = read_u32_le(data, 20).unwrap_or(0);
    let sort_type = read_u32_le(data, 24).unwrap_or(0);
    let search_str = if wide {
        extract_wstring(&data[28..])
    } else {
        extract_astring(&data[28..])
    };
    let mut max = max_results as usize;
    if max == 0 {
        max = 100;
    }
    let (sort_field, sort_desc) = map_sort_type(sort_type);
    let results = do_search(
        &search_str,
        offset as usize + max,
        search_flags,
        sort_field,
        sort_desc,
    )?;
    send_query2_reply(
        reply_hwnd,
        my_hwnd,
        reply_msg,
        results,
        offset,
        max as u32,
        request_flags,
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

fn read_u32_le(data: &[u8], off: usize) -> Option<u32> {
    data.get(off..off + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn extract_wstring(data: &[u8]) -> String {
    if data.len() < 2 {
        return String::new();
    }
    let mut u16s = Vec::with_capacity(data.len() / 2);
    for chunk in data.chunks_exact(2) {
        let v = u16::from_le_bytes([chunk[0], chunk[1]]);
        if v == 0 {
            break;
        }
        u16s.push(v);
    }
    String::from_utf16_lossy(&u16s)
}

fn extract_astring(data: &[u8]) -> String {
    let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    String::from_utf8_lossy(&data[..end]).into_owned()
}

/// `query`：查询字符串；`max_total`：上限（含 offset）；`ipc_flags`：`EVERYTHING_IPC_*` search_flags。
fn do_search(
    query: &str,
    max_total: usize,
    ipc_flags: u32,
    sort_field: SortField,
    sort_desc: bool,
) -> Option<Vec<IpcHit>> {
    let engine = ENGINE.lock().ok()?.as_ref()?.clone();
    let q = query.trim();
    if q.is_empty() {
        return Some(vec![]);
    }
    let mut pq: ParsedQuery = QueryParser::parse(q).ok()?;
    apply_ipc_search_flags(&mut pq, ipc_flags);
    pq.limit = max_total.min(8192) as u32;
    pq.sort_by = sort_field;
    pq.sort_desc = sort_desc;
    // 与命名管道 search_ipc、GUI 默认一致：Everything 协议不传拼音开关，第三方（IbEverythingExt 等）
    // 发来的简拼如 mctjl 需走 lita 拼音路径；查询串里 `;en` / `;np` 仍可通过 ParsedQuery 关闭拼音。
    let opt = SearchOptions {
        allow_pinyin: true,
        ..Default::default()
    };
    let (hits, _total) = engine.search(&pq, &opt).ok()?;
    Some(
        hits.iter()
            .filter_map(|h| ipc_hit_from_search(&engine, h))
            .collect(),
    )
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

struct IpcHit {
    full_path: String,
    name: String,
    parent: String,
    is_dir: bool,
    size: u64,
    mtime: u64,
    ctime: u64,
    attrs: u32,
}

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
        build_list_w(&results, offset, start, count)
    } else {
        build_list_a(&results, offset, start, count)
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
    let buf = build_list2(&results, offset, start, count, request_flags, wide);
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
                WPARAM(my_raw),
                LPARAM(p as isize),
            );
        }
    });
}

fn build_list_w(results: &[IpcHit], offset: u32, start: usize, count: usize) -> Vec<u8> {
    let mut tot_folders = 0u32;
    let mut tot_files = 0u32;
    for r in results {
        if r.is_dir {
            tot_folders += 1;
        } else {
            tot_files += 1;
        }
    }
    let mut strings: Vec<u8> = Vec::new();
    let mut items: Vec<(u32, usize, usize)> = Vec::new();
    let mut num_folders = 0u32;
    let mut num_files = 0u32;
    for i in start..start + count {
        let r = &results[i];
        let fn_off = strings.len() / 2;
        for u in r.name.encode_utf16() {
            strings.extend_from_slice(&u.to_le_bytes());
        }
        strings.extend_from_slice(&[0, 0]);
        let path_off = strings.len() / 2;
        for u in r.parent.encode_utf16() {
            strings.extend_from_slice(&u.to_le_bytes());
        }
        strings.extend_from_slice(&[0, 0]);
        let fl = if r.is_dir {
            num_folders += 1;
            ITEM_FOLDER
        } else {
            num_files += 1;
            0
        };
        items.push((fl, fn_off, path_off));
    }

    let header_size = 28usize;
    let str_base = header_size + count * 12;
    let total = str_base + strings.len();
    let mut buf = vec![0u8; total];

    buf[0..4].copy_from_slice(&tot_folders.to_le_bytes());
    buf[4..8].copy_from_slice(&tot_files.to_le_bytes());
    buf[8..12].copy_from_slice(&(results.len() as u32).to_le_bytes());
    buf[12..16].copy_from_slice(&num_folders.to_le_bytes());
    buf[16..20].copy_from_slice(&num_files.to_le_bytes());
    buf[20..24].copy_from_slice(&(count as u32).to_le_bytes());
    buf[24..28].copy_from_slice(&offset.to_le_bytes());

    for (i, (fl, fn_off, pa_off)) in items.iter().enumerate() {
        let pos = header_size + i * 12;
        buf[pos..pos + 4].copy_from_slice(&fl.to_le_bytes());
        buf[pos + 4..pos + 8].copy_from_slice(&((str_base + fn_off * 2) as u32).to_le_bytes());
        buf[pos + 8..pos + 12].copy_from_slice(&((str_base + pa_off * 2) as u32).to_le_bytes());
    }
    buf[str_base..str_base + strings.len()].copy_from_slice(&strings);
    buf
}

/// QUERYA 回复：与 QUERYW 同 header（28B）+ 同 item 表（12B），但字符串区按 ANSI 字节、偏移以字节计算。
fn build_list_a(results: &[IpcHit], offset: u32, start: usize, count: usize) -> Vec<u8> {
    let mut tot_folders = 0u32;
    let mut tot_files = 0u32;
    for r in results {
        if r.is_dir {
            tot_folders += 1;
        } else {
            tot_files += 1;
        }
    }
    let mut strings: Vec<u8> = Vec::new();
    let mut items: Vec<(u32, usize, usize)> = Vec::new();
    let mut num_folders = 0u32;
    let mut num_files = 0u32;
    for i in start..start + count {
        let r = &results[i];
        let fn_off = strings.len();
        strings.extend_from_slice(r.name.as_bytes());
        strings.push(0);
        let path_off = strings.len();
        strings.extend_from_slice(r.parent.as_bytes());
        strings.push(0);
        let fl = if r.is_dir {
            num_folders += 1;
            ITEM_FOLDER
        } else {
            num_files += 1;
            0
        };
        items.push((fl, fn_off, path_off));
    }

    let header_size = 28usize;
    let str_base = header_size + count * 12;
    let total = str_base + strings.len();
    let mut buf = vec![0u8; total];

    buf[0..4].copy_from_slice(&tot_folders.to_le_bytes());
    buf[4..8].copy_from_slice(&tot_files.to_le_bytes());
    buf[8..12].copy_from_slice(&(results.len() as u32).to_le_bytes());
    buf[12..16].copy_from_slice(&num_folders.to_le_bytes());
    buf[16..20].copy_from_slice(&num_files.to_le_bytes());
    buf[20..24].copy_from_slice(&(count as u32).to_le_bytes());
    buf[24..28].copy_from_slice(&offset.to_le_bytes());

    for (i, (fl, fn_off, pa_off)) in items.iter().enumerate() {
        let pos = header_size + i * 12;
        buf[pos..pos + 4].copy_from_slice(&fl.to_le_bytes());
        buf[pos + 4..pos + 8].copy_from_slice(&((str_base + fn_off) as u32).to_le_bytes());
        buf[pos + 8..pos + 12].copy_from_slice(&((str_base + pa_off) as u32).to_le_bytes());
    }
    buf[str_base..str_base + strings.len()].copy_from_slice(&strings);
    buf
}

fn build_list2(
    results: &[IpcHit],
    offset: u32,
    start: usize,
    count: usize,
    request_flags: u32,
    wide: bool,
) -> Vec<u8> {
    let header_size = 20usize;
    let items_size = count * 8;
    let data_start = header_size + items_size;

    let mut payload: Vec<u8> = Vec::new();
    let mut data_offsets: Vec<u32> = Vec::with_capacity(count);

    for i in start..start + count {
        let r = &results[i];
        data_offsets.push((data_start + payload.len()) as u32);
        write_item2_payload(&mut payload, r, request_flags, wide);
    }

    let mut buf = vec![0u8; data_start + payload.len()];
    buf[data_start..].copy_from_slice(&payload);

    buf[0..4].copy_from_slice(&(results.len() as u32).to_le_bytes());
    buf[4..8].copy_from_slice(&(count as u32).to_le_bytes());
    buf[8..12].copy_from_slice(&offset.to_le_bytes());
    buf[12..16].copy_from_slice(&request_flags.to_le_bytes());
    buf[16..20].copy_from_slice(&1u32.to_le_bytes());

    for i in 0..count {
        let pos = header_size + i * 8;
        let r = &results[start + i];
        let flags = if r.is_dir { ITEM_FOLDER } else { 0 };
        buf[pos..pos + 4].copy_from_slice(&flags.to_le_bytes());
        buf[pos + 4..pos + 8].copy_from_slice(&data_offsets[i].to_le_bytes());
    }

    buf
}

fn write_ipc_string(b: &mut Vec<u8>, s: &str, wide: bool) {
    if wide {
        let n = s.encode_utf16().count() as u32;
        b.write_all(&n.to_le_bytes()).ok();
        for u in s.encode_utf16() {
            b.write_all(&u.to_le_bytes()).ok();
        }
        b.write_all(&0u16.to_le_bytes()).ok();
    } else {
        let bytes = s.as_bytes();
        let n = bytes.len() as u32;
        b.write_all(&n.to_le_bytes()).ok();
        b.write_all(bytes).ok();
        b.write_all(&[0u8]).ok();
    }
}

fn write_item2_payload(ms: &mut Vec<u8>, r: &IpcHit, rf: u32, wide: bool) {
    if rf & REQUEST_NAME != 0 {
        write_ipc_string(ms, &r.name, wide);
    }
    if rf & REQUEST_PATH != 0 {
        write_ipc_string(ms, &r.parent, wide);
    }
    if rf & REQUEST_FULL_PATH_AND_NAME != 0 {
        write_ipc_string(ms, &r.full_path, wide);
    }
    if rf & REQUEST_EXTENSION != 0 {
        let ext = if r.is_dir {
            String::new()
        } else {
            r.name
                .rfind('.')
                .map(|i| r.name[i + 1..].to_string())
                .unwrap_or_default()
        };
        write_ipc_string(ms, &ext, wide);
    }
    if rf & REQUEST_SIZE != 0 {
        ms.write_all(&r.size.to_le_bytes()).ok();
    }
    if rf & REQUEST_DATE_CREATED != 0 {
        ms.write_all(&(r.ctime as i64).to_le_bytes()).ok();
    }
    if rf & REQUEST_DATE_MODIFIED != 0 {
        ms.write_all(&(r.mtime as i64).to_le_bytes()).ok();
    }
    if rf & REQUEST_DATE_ACCESSED != 0 {
        ms.write_all(&0i64.to_le_bytes()).ok();
    }
    if rf & REQUEST_ATTRIBUTES != 0 {
        // Win32 文件属性宏：低 8 位由 USN/MFT 直接给出（READONLY=0x1, HIDDEN=0x2, SYSTEM=0x4, DIRECTORY=0x10, ARCHIVE=0x20, …）
        ms.write_all(&r.attrs.to_le_bytes()).ok();
    }
    if rf & REQUEST_FILE_LIST_FILE_NAME != 0 {
        write_ipc_string(ms, "", wide);
    }
    if rf & REQUEST_RUN_COUNT != 0 {
        ms.write_all(&0u32.to_le_bytes()).ok();
    }
    if rf & REQUEST_DATE_RUN != 0 {
        ms.write_all(&0i64.to_le_bytes()).ok();
    }
    if rf & REQUEST_DATE_RECENTLY_CHANGED != 0 {
        ms.write_all(&0i64.to_le_bytes()).ok();
    }
    if rf & REQUEST_HIGHLIGHTED_NAME != 0 {
        write_ipc_string(ms, &r.name, wide);
    }
    if rf & REQUEST_HIGHLIGHTED_PATH != 0 {
        write_ipc_string(ms, &r.parent, wide);
    }
    if rf & REQUEST_HIGHLIGHTED_FULL_PATH_AND_NAME != 0 {
        write_ipc_string(ms, &r.full_path, wide);
    }
}
