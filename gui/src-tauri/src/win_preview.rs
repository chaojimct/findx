//! Windows 资源管理器预览窗格的复用实现。
//!
//! 思路（与 Explorer 完全一致，所有已注册的预览扩展自动可用）：
//!
//! 1. 按文件扩展名 → ProgID → 在注册表查 `shellex\{8895b1c6-b41f-4c1c-a562-0d564250836f}` 拿 CLSID。
//! 2. `CoCreateInstance(CLSID, CLSCTX_LOCAL_SERVER | INPROC)` —— 优先 LOCAL_SERVER 让 prevhost.exe
//!    宿主，崩溃也不会带崩本进程；某些只注册 INPROC 的处理器会自动回退。
//! 3. 依次尝试 `IInitializeWithStream` / `IInitializeWithFile` / `IInitializeWithItem`，调 Initialize。
//! 4. 用 STATIC 预定义窗口类做顶层 WS_POPUP 承载（无标题文本），前端把 webview 客户区矩形传过来。
//! 5. `IPreviewHandler::SetWindow(child, rect)` → `DoPreview()` 渲染；尺寸变化只调 `SetRect`。
//! 6. 切换/关闭时 `Unload()` + 释放 COM 对象 + `DestroyWindow` 子窗口。
//!
//! 设计取舍：
//! - 子 HWND 是原生窗口，叠在 WebView 上，因此「关闭面板」必须 `ShowWindow(SW_HIDE)`，
//!   缩放/拖拽分隔条都要重新 `SetWindowPos`。Outlook / Explorer 也是这么做。
//! - 所有 COM/UI 调用必须在创建窗口的主线程上做（STA + 父子窗口规则）。
//!   外层 Tauri 命令负责 `run_on_main_thread`；本模块假定调用方已在主线程。

use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::Mutex;

use windows::core::{Interface, PCWSTR, GUID};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX, CLSCTX_ACTIVATE_32_BIT_SERVER,
    CLSCTX_ACTIVATE_64_BIT_SERVER, CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER,
    COINIT_APARTMENTTHREADED, STGM_READ, STGM_SHARE_DENY_NONE,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CLASSES_ROOT, HKEY_LOCAL_MACHINE,
    KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY, REG_SAM_FLAGS, REG_VALUE_TYPE,
};
use windows::Win32::System::Com::IPersistFile;
use windows::Win32::UI::Shell::PropertiesSystem::{IInitializeWithFile, IInitializeWithStream};
use windows::Win32::UI::Shell::{
    IInitializeWithItem, IPreviewHandler, IShellItem, SHCreateItemFromParsingName, SHCreateStreamOnFileEx,
};
use windows::Win32::Graphics::Gdi::{ClientToScreen, InvalidateRect, UpdateWindow};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, CreateWindowExW, DestroyWindow, EnumChildWindows, GetClassNameW, SetWindowPos,
    ShowWindow, HWND_TOP, SET_WINDOW_POS_FLAGS, SWP_NOACTIVATE, SWP_NOZORDER,
    SW_HIDE, SW_SHOW, WS_CLIPCHILDREN, WS_POPUP,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};
use windows::Win32::Foundation::POINT;

/// 预览处理器的 shellex 子键 GUID。Explorer 与 prevhost 都查这一项。
const PREVIEW_HANDLER_SHELLEX_KEY: &str = "shellex\\{8895b1c6-b41f-4c1c-a562-0d564250836f}";

/// `RPC_E_SERVERCALL_RETRYLATER`：COM 返回「应用程序正在使用中」，常见于 prevhost/Office
/// 正在处理其它预览或 STA 忙；短延迟重试往往成功。
const RPC_E_SERVERCALL_RETRYLATER: u32 = 0x8001_010A;

fn is_com_server_busy(e: &windows::core::Error) -> bool {
    (e.code().0 as u32) == RPC_E_SERVERCALL_RETRYLATER
}

/// 全局保存当前承载的预览状态；切换前一律 unload + drop。
static PREVIEW_STATE: Mutex<Option<PreviewState>> = Mutex::new(None);
/// 主线程 STA 是否已初始化。Tauri 主线程已 init COM，但我们在第一次进入时再补一次保险（S_FALSE 也算 OK）。
static COM_INIT_DONE: Mutex<bool> = Mutex::new(false);
struct PreviewState {
    /// 承载的顶级 popup HWND。
    host_hwnd: HWND,
    /// 当前文件路径，用于避免对同一路径反复重建。
    path: String,
    /// 必须保持 IPreviewHandler 存活；drop 时 unload。
    handler: IPreviewHandler,
    /// 主窗口顶级 HWND（owner），用于跟随移动。
    owner_top_hwnd: HWND,
    /// 当前定位用的 webview 容器 HWND（owner_top_hwnd 内部），客户区原点 = popup 锚点。
    owner_webview_hwnd: HWND,
    /// 前端最近一次给的 webview 客户区坐标（CSS px × DPR），用于轮询时根据 webview 位置算 popup 屏幕坐标。
    last_client_x: i32,
    last_client_y: i32,
    last_client_w: i32,
    last_client_h: i32,
    /// 上一帧 popup 屏幕坐标，用于变化检测，避免无意义 SetWindowPos。
    last_screen_rect: RECT,
}

impl Drop for PreviewState {
    fn drop(&mut self) {
        unsafe {
            // 即使 Unload 失败也要继续销毁窗口，避免泄漏。
            let _ = self.handler.Unload();
            if !self.host_hwnd.is_invalid() {
                let _ = DestroyWindow(self.host_hwnd);
            }
        }
    }
}

// HWND / COM 指针的 Send 是手动保证：所有访问都强制在主线程上执行。
// Mutex 需要 Send，Tauri 命令通过 run_on_main_thread 把闭包派发到主线程后再访问 PREVIEW_STATE。
unsafe impl Send for PreviewState {}
unsafe impl Sync for PreviewState {}

/// UTF-16 + null 终止。
fn to_wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn ensure_com_init() {
    let mut g = match COM_INIT_DONE.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if *g {
        return;
    }
    unsafe {
        // STA：失败码 RPC_E_CHANGED_MODE / S_FALSE 都不影响后续调用，忽略即可。
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    *g = true;
}

/// 读 `HKCR\<sub>\Default`，存在且非空即返回。
fn read_hkcr_default(sub: &str) -> Option<String> {
    read_hkcr_default_view(sub, REG_SAM_FLAGS(0))
}

/// 同上，但可指定注册表视图（KEY_WOW64_32KEY / 64KEY / 0）。
/// 64 位进程默认看 64 位视图，但 WPS / 32 位 Office 的 CLSID 注册在 32 位视图（WOW6432Node），
/// 必须用 KEY_WOW64_32KEY 才能看到。
fn read_hkcr_default_view(sub: &str, view: REG_SAM_FLAGS) -> Option<String> {
    unsafe {
        let mut hkey = HKEY::default();
        let wsub = to_wide(sub);
        if RegOpenKeyExW(HKEY_CLASSES_ROOT, PCWSTR(wsub.as_ptr()), Some(0), KEY_READ | view, &mut hkey)
            .is_err()
        {
            return None;
        }
        let mut buf = [0u16; 256];
        let mut len_bytes: u32 = (buf.len() * 2) as u32;
        let mut ty = REG_VALUE_TYPE::default();
        let r = RegQueryValueExW(
            hkey,
            PCWSTR(std::ptr::null()),
            None,
            Some(&mut ty),
            Some(buf.as_mut_ptr() as *mut u8),
            Some(&mut len_bytes),
        );
        let _ = RegCloseKey(hkey);
        if r.is_err() || len_bytes == 0 {
            return None;
        }
        let n = (len_bytes as usize / 2).saturating_sub(1);
        let s: String = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
        let s = s.trim_end_matches('\0').trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
}

/// 把 `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}` 字符串解析为 GUID。
fn parse_clsid(s: &str) -> Option<GUID> {
    let trimmed = s.trim().trim_start_matches('{').trim_end_matches('}');
    GUID::try_from(trimmed).ok()
}

/// 读 `HKLM\SOFTWARE\Classes\<sub>\Default`（仅机器级，不含 HKCU 覆盖），
/// 用于在 HKCR 合并视图查到的是用户级伪 handler 时回退到 Office 等系统级真处理器。
fn read_hklm_classes_default(sub: &str) -> Option<String> {
    unsafe {
        let mut hkey = HKEY::default();
        let path = format!("SOFTWARE\\Classes\\{sub}");
        let wsub = to_wide(&path);
        if RegOpenKeyExW(HKEY_LOCAL_MACHINE, PCWSTR(wsub.as_ptr()), Some(0), KEY_READ, &mut hkey)
            .is_err()
        {
            return None;
        }
        let mut buf = [0u16; 256];
        let mut len_bytes: u32 = (buf.len() * 2) as u32;
        let mut ty = REG_VALUE_TYPE::default();
        let r = RegQueryValueExW(
            hkey,
            PCWSTR(std::ptr::null()),
            None,
            Some(&mut ty),
            Some(buf.as_mut_ptr() as *mut u8),
            Some(&mut len_bytes),
        );
        let _ = RegCloseKey(hkey);
        if r.is_err() || len_bytes == 0 {
            return None;
        }
        let n = (len_bytes as usize / 2).saturating_sub(1);
        let s: String = String::from_utf16_lossy(&buf[..n.min(buf.len())]);
        let s = s.trim_end_matches('\0').trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    }
}

/// 找 HKLM 系统级注册的 PreviewHandler CLSID（绕过 HKCU 用户级伪 handler，比如 WPS）。
fn find_preview_handler_clsid_hklm(path: &Path) -> Option<GUID> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    if ext.is_empty() {
        return None;
    }
    let dot_ext = format!(".{ext}");
    if let Some(clsid) = read_hklm_classes_default(&format!("{dot_ext}\\{PREVIEW_HANDLER_SHELLEX_KEY}"))
        .and_then(|s| parse_clsid(&s))
    {
        return Some(clsid);
    }
    if let Some(progid) = read_hklm_classes_default(&dot_ext) {
        if let Some(clsid) =
            read_hklm_classes_default(&format!("{progid}\\{PREVIEW_HANDLER_SHELLEX_KEY}"))
                .and_then(|s| parse_clsid(&s))
        {
            return Some(clsid);
        }
    }
    if let Some(clsid) = read_hklm_classes_default(&format!(
        "SystemFileAssociations\\{dot_ext}\\{PREVIEW_HANDLER_SHELLEX_KEY}"
    ))
    .and_then(|s| parse_clsid(&s))
    {
        return Some(clsid);
    }
    None
}

/// 按 Explorer 的查找顺序找扩展名对应的 PreviewHandler CLSID。
/// 1. `HKCR\.ext\shellex\{...}` 直查；
/// 2. `HKCR\.ext\(Default)` 拿 ProgID，再查 `HKCR\<ProgID>\shellex\{...}`；
/// 3. `HKCR\SystemFileAssociations\.ext\shellex\{...}`（Win10+ 文本类常用）。
fn find_preview_handler_clsid(path: &Path) -> Option<GUID> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    if ext.is_empty() {
        return None;
    }
    let dot_ext = format!(".{ext}");

    // 1. 直接查
    if let Some(clsid) = read_hkcr_default(&format!("{dot_ext}\\{PREVIEW_HANDLER_SHELLEX_KEY}"))
        .and_then(|s| parse_clsid(&s))
    {
        return Some(clsid);
    }
    // 2. ProgID 跳板
    if let Some(progid) = read_hkcr_default(&dot_ext) {
        if let Some(clsid) = read_hkcr_default(&format!("{progid}\\{PREVIEW_HANDLER_SHELLEX_KEY}"))
            .and_then(|s| parse_clsid(&s))
        {
            return Some(clsid);
        }
    }
    // 3. SystemFileAssociations 兜底
    if let Some(clsid) = read_hkcr_default(&format!(
        "SystemFileAssociations\\{dot_ext}\\{PREVIEW_HANDLER_SHELLEX_KEY}"
    ))
    .and_then(|s| parse_clsid(&s))
    {
        return Some(clsid);
    }
    None
}

/// CLSID 注册位数与服务器类型探测结果，决定 CoCreateInstance 的 CLSCTX 组合。
#[derive(Debug, Clone, Copy)]
struct ClsidInfo {
    /// 在 64 位视图（HKCR / HKLM\Software\Classes / HKCU\Software\Classes）能找到。
    has_64: bool,
    /// 在 32 位视图（WOW6432Node 子树）能找到。
    has_32: bool,
    /// 注册了 InprocServer32（任意视图）—— 可以 INPROC 加载 DLL。
    has_inproc_server: bool,
    /// 注册了 LocalServer32（任意视图）—— 可以 LOCAL_SERVER 起独立进程。
    has_local_server: bool,
    /// 仅 InprocHandler32（无 InprocServer32 / LocalServer32）—— 这种是 surrogate-only，
    /// 必须由 prevhost.exe 之类的代理进程加载，因此实际只能走 LOCAL_SERVER 跨进程。
    handler_only: bool,
}

fn format_clsid(clsid: &GUID) -> String {
    format!("{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        clsid.data1, clsid.data2, clsid.data3,
        clsid.data4[0], clsid.data4[1], clsid.data4[2], clsid.data4[3],
        clsid.data4[4], clsid.data4[5], clsid.data4[6], clsid.data4[7])
}

/// 在指定注册表视图下探测某个 CLSID 子键是否存在。
fn clsid_subkey_exists(s: &str, sub: &str, view: REG_SAM_FLAGS) -> bool {
    let path = if sub.is_empty() {
        format!("CLSID\\{s}")
    } else {
        format!("CLSID\\{s}\\{sub}")
    };
    unsafe {
        let mut hkey = HKEY::default();
        let wsub = to_wide(&path);
        let r = RegOpenKeyExW(
            HKEY_CLASSES_ROOT,
            PCWSTR(wsub.as_ptr()),
            Some(0),
            KEY_READ | view,
            &mut hkey,
        );
        if r.is_err() {
            return false;
        }
        let _ = RegCloseKey(hkey);
        true
    }
}

/// 探测 CLSID 的注册情况：64 位视图 / 32 位视图、InprocServer / LocalServer / 仅 Handler。
/// WPS、32 位 Office 的预览处理器只在 WOW6432Node 注册，且常常只有 InprocHandler32；
/// 这种情况必须用 `CLSCTX_LOCAL_SERVER | CLSCTX_ACTIVATE_32_BIT_SERVER`，由 SysWOW64\prevhost.exe
/// 作 surrogate 进程加载，64 位进程通过 COM RPC 调用。
fn probe_clsid(clsid: &GUID) -> ClsidInfo {
    let s = format_clsid(clsid);
    let v64 = KEY_WOW64_64KEY;
    let v32 = KEY_WOW64_32KEY;

    let has_64 = clsid_subkey_exists(&s, "", v64);
    let has_32 = clsid_subkey_exists(&s, "", v32);
    let inproc_64 = clsid_subkey_exists(&s, "InprocServer32", v64);
    let inproc_32 = clsid_subkey_exists(&s, "InprocServer32", v32);
    let local_64 = clsid_subkey_exists(&s, "LocalServer32", v64);
    let local_32 = clsid_subkey_exists(&s, "LocalServer32", v32);
    let handler_64 = clsid_subkey_exists(&s, "InprocHandler32", v64);
    let handler_32 = clsid_subkey_exists(&s, "InprocHandler32", v32);

    let has_inproc_server = inproc_64 || inproc_32;
    let has_local_server = local_64 || local_32;
    let handler_only = !has_inproc_server && !has_local_server && (handler_64 || handler_32);

    ClsidInfo {
        has_64,
        has_32,
        has_inproc_server,
        has_local_server,
        handler_only,
    }
}

/// EnumChildWindows 回调用来定位 WebView2 的真实窗口（命中即写入 OUT 参数并停止枚举）。
unsafe extern "system" fn enum_find_webview2(hwnd: HWND, lparam: windows::Win32::Foundation::LPARAM) -> windows::core::BOOL {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n > 0 {
        let cls = String::from_utf16_lossy(&buf[..n as usize]);
        // WebView2 的承载链上常见类名：
        //   Chrome_WidgetWin_0/1/2（Edge/Chromium 主窗口）
        //   Microsoft.UI.Content.DesktopChildSiteBridge（WinAppSDK）
        if cls.starts_with("Chrome_WidgetWin")
            || cls.contains("Microsoft.UI.Content")
            || cls == "Intermediate D3D Window"
        {
            unsafe {
                let out = lparam.0 as *mut HWND;
                *out = hwnd;
            }
            return false.into();
        }
    }
    true.into()
}

/// 找 Tauri 顶级窗口里的 WebView2 容器 HWND；找不到就回退顶级 HWND。
fn find_webview_host(top: HWND) -> HWND {
    let mut found: HWND = HWND(std::ptr::null_mut());
    let lparam = windows::Win32::Foundation::LPARAM(&mut found as *mut HWND as isize);
    unsafe {
        let _ = EnumChildWindows(Some(top), Some(enum_find_webview2), lparam);
    }
    if found.is_invalid() {
        top
    } else {
        found
    }
}

/// 创建预览宿主窗口。
///
/// 关键设计：WebView2（WRY/Tauri 默认）启用 DComp 把整块客户区合成在 GPU 表面之上，
/// 任何「webview/顶级窗口的子 HWND」在视觉上都会被压在合成层之下完全不可见。
/// 唯一可靠方案是使用独立顶级窗口（`WS_POPUP`），并把它的 owner 设为主窗口
/// 让 Windows 自动跟随主窗口最小化、激活、销毁；同时位置由前端用屏幕坐标驱动。
///
/// `screen_rect` 已经是屏幕坐标。
unsafe fn create_host_window(owner: HWND, screen_rect: RECT) -> Result<HWND, String> {
    // 使用 STATIC 仅作轻量容器；**标题必须为空**：否则 SS_LEFT 默认会把窗口名画成灰底上的文字，
    // PDF 子 HWND 因跨线程 SetWindowPos 等异常被挡掉后，用户就会看到 "FindX2.PreviewHost" 白屏。
    let class = to_wide("STATIC");
    let w = (screen_rect.right - screen_rect.left).max(1);
    let h = (screen_rect.bottom - screen_rect.top).max(1);
    eprintln!(
        "[findx2-preview] create_host owner=0x{:X} screen_rect=({},{},{}x{})",
        owner.0 as usize, screen_rect.left, screen_rect.top, w, h
    );
    // 不要 WS_VISIBLE / SWP_SHOWWINDOW：否则空宿主会在 Initialize/DoPreview 完成前就盖住 WebView，
    // 前端「云同步 / 加载中」遮罩（DOM）会被压在下面用户永远看不到。
    let hwnd = CreateWindowExW(
        WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        PCWSTR(class.as_ptr()),
        PCWSTR::null(),
        WS_POPUP | WS_CLIPCHILDREN,
        screen_rect.left,
        screen_rect.top,
        w,
        h,
        Some(owner),
        None,
        None,
        None,
    )
    .map_err(|e| format!("创建预览容器窗口失败: {e}"))?;
    eprintln!("[findx2-preview] host created hwnd=0x{:X}", hwnd.0 as usize);
    // 浮在 owner 之上、保持不抢焦点（仍保持隐藏，直到 show_preview 里 DoPreview 后再 ShowWindow）。
    let _ = SetWindowPos(
        hwnd,
        Some(HWND_TOP),
        screen_rect.left,
        screen_rect.top,
        w,
        h,
        SET_WINDOW_POS_FLAGS(SWP_NOACTIVATE.0),
    );
    Ok(hwnd)
}

/// 路径是否落在常见云同步目录（用于强制分块水化，不依赖属性位）。
fn path_looks_like_cloud_storage(p: &Path) -> bool {
    let s = p.to_string_lossy().to_ascii_lowercase();
    s.contains("onedrive")
        || s.contains("wps cloud files")
        || s.contains("dropbox")
        || s.contains("google drive")
        || s.contains("icloud")
}

/// OneDrive 等占位文件只读 1 字节往往不够，Office 预览仍会失败；分块读到本地或占位属性消失为止。
/// `ERROR_CLOUD_FILE_PROVIDER_NOT_RUNNING` (362)：云筛选器/OneDrive 未运行，无法拉取占位文件内容。
fn hydrate_cloud_file_best_effort(path: &Path) -> Result<(), String> {
    use std::io::Read;
    use std::os::windows::fs::MetadataExt;

    const ERROR_CLOUD_FILE_PROVIDER_NOT_RUNNING: i32 = 362;

    const FILE_ATTRIBUTE_OFFLINE: u32 = 0x1000;
    const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x40000;
    const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x400000;
    const FILE_ATTRIBUTE_SPARSE_FILE: u32 = 0x200;

    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(());
    };
    let attrs = meta.file_attributes();
    let len = meta.len() as usize;
    let cloud_dir = path_looks_like_cloud_storage(path);
    // 勿把任意本地 SPARSE 当云占位；仅 OFFLINE/RECALL，或「云目录下的 SPARSE」。
    let placeholder_like = attrs
        & (FILE_ATTRIBUTE_OFFLINE
            | FILE_ATTRIBUTE_RECALL_ON_OPEN
            | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
        != 0
        || (cloud_dir && (attrs & FILE_ATTRIBUTE_SPARSE_FILE) != 0);
    if !cloud_dir && !placeholder_like {
        return Ok(());
    }

    eprintln!(
        "[findx2-preview] cloud hydrate start len={} attr=0x{:X} cloud_dir={}",
        len, attrs, cloud_dir
    );

    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            if e.raw_os_error() == Some(ERROR_CLOUD_FILE_PROVIDER_NOT_RUNNING) {
                return Err(
                    "云文件提供程序未运行（系统错误 362）。请启动并登录 OneDrive，或将该文件/文件夹设为「始终保留在此设备上」后再预览。"
                        .to_string(),
                );
            }
            eprintln!("[findx2-preview] cloud hydrate open failed: {e}");
            return Ok(());
        }
    };
    let mut buf = vec![0u8; 512 * 1024];
    let max_read = len.min(64 * 1024 * 1024);
    let mut total = 0usize;
    let mut since_check = 0usize;

    while total < max_read {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                total += n;
                since_check += n;
            }
            Err(e) => {
                if e.raw_os_error() == Some(ERROR_CLOUD_FILE_PROVIDER_NOT_RUNNING) {
                    return Err(
                        "云文件提供程序未运行（系统错误 362）。请启动并登录 OneDrive，或将该文件/文件夹设为「始终保留在此设备上」后再预览。"
                            .to_string(),
                    );
                }
                eprintln!("[findx2-preview] cloud hydrate read: {e}");
                break;
            }
        }
        // 每约 2MB 检查一次占位属性是否已清除，可提前结束。
        if since_check >= 2 * 1024 * 1024 {
            since_check = 0;
            if let Ok(m2) = std::fs::metadata(path) {
                let a2 = m2.file_attributes();
                if a2 & (FILE_ATTRIBUTE_OFFLINE | FILE_ATTRIBUTE_RECALL_ON_OPEN | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
                    == 0
                {
                    eprintln!(
                        "[findx2-preview] cloud placeholder attrs cleared after {} bytes",
                        total
                    );
                    break;
                }
            }
        }
    }
    eprintln!("[findx2-preview] cloud hydrate done, read {} / {} bytes", total, len);
    Ok(())
}

/// 把「webview 客户区像素坐标」转换为「屏幕物理像素坐标」。
/// owner 是 webview 容器 HWND。
fn client_to_screen_rect(owner: HWND, x: i32, y: i32, w: i32, h: i32) -> RECT {
    let mut pt = POINT { x, y };
    unsafe {
        let _ = ClientToScreen(owner, &mut pt);
    }
    RECT {
        left: pt.x,
        top: pt.y,
        right: pt.x + w.max(1),
        bottom: pt.y + h.max(1),
    }
}

/// 用 IInitializeWithStream / File / Item 中可用的一种来初始化处理器。
/// Stream 优先（更安全、不锁文件），Item / File 兜底（部分老处理器只支持其中之一）。
unsafe fn initialize_handler(handler: &IPreviewHandler, path: &Path) -> Result<(), String> {
    // 云盘占位文件：先尽力水化到本地，再交给 IPreviewHandler（读 1 字节对大型 Office 往往不够）。
    hydrate_cloud_file_best_effort(path)?;

    let mut errs: Vec<String> = Vec::new();
    // 1. Stream
    match handler.cast::<IInitializeWithStream>() {
        Ok(init_stream) => {
            let wpath = to_wide(&path.to_string_lossy());
            match SHCreateStreamOnFileEx(
                PCWSTR(wpath.as_ptr()),
                (STGM_READ | STGM_SHARE_DENY_NONE).0,
                0,
                false,
                None,
            ) {
                Ok(stream) => match init_stream.Initialize(&stream, STGM_READ.0) {
                    Ok(_) => {
                        eprintln!("[findx2-preview] Initialize via Stream ok");
                        return Ok(());
                    }
                    Err(e) => errs.push(format!("Stream.Initialize: {e}")),
                },
                Err(e) => errs.push(format!("SHCreateStreamOnFileEx: {e}")),
            }
        }
        Err(e) => errs.push(format!("cast IInitializeWithStream: {e}")),
    }
    // 2. File
    match handler.cast::<IInitializeWithFile>() {
        Ok(init_file) => {
            let wpath = to_wide(&path.to_string_lossy());
            match init_file.Initialize(PCWSTR(wpath.as_ptr()), STGM_READ.0) {
                Ok(_) => {
                    eprintln!("[findx2-preview] Initialize via File ok");
                    return Ok(());
                }
                Err(e) => errs.push(format!("File.Initialize: {e}")),
            }
        }
        Err(e) => errs.push(format!("cast IInitializeWithFile: {e}")),
    }
    // 3. Item
    match handler.cast::<IInitializeWithItem>() {
        Ok(init_item) => {
            let wpath = to_wide(&path.to_string_lossy());
            match SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(wpath.as_ptr()), None) {
                Ok(item) => match init_item.Initialize(&item, STGM_READ.0) {
                    Ok(_) => {
                        eprintln!("[findx2-preview] Initialize via Item ok");
                        return Ok(());
                    }
                    Err(e) => errs.push(format!("Item.Initialize: {e}")),
                },
                Err(e) => errs.push(format!("SHCreateItemFromParsingName: {e}")),
            }
        }
        Err(e) => errs.push(format!("cast IInitializeWithItem: {e}")),
    }
    // 4. IPersistFile —— OLE 复合文档老格式 (.ppt/.doc/.xls) 常用，IInitializeWithFile 拒绝时兜底
    match handler.cast::<IPersistFile>() {
        Ok(persist) => {
            let wpath = to_wide(&path.to_string_lossy());
            match persist.Load(PCWSTR(wpath.as_ptr()), STGM_READ) {
                Ok(_) => {
                    eprintln!("[findx2-preview] Initialize via IPersistFile ok");
                    return Ok(());
                }
                Err(e) => errs.push(format!("IPersistFile.Load: {e}")),
            }
        }
        Err(e) => errs.push(format!("cast IPersistFile: {e}")),
    }
    let msg = format!("预览处理器拒绝所有 Initialize 接口: [{}]", errs.join("; "));
    eprintln!("[findx2-preview] {msg}");
    Err(msg)
}


/// 显示/切换预览。`path` 是绝对路径。
///
/// 必须在主线程调用。命令侧用 `run_on_main_thread` 包一层。
pub fn show_preview(top_hwnd: HWND, path: String, x: i32, y: i32, w: i32, h: i32) -> Result<(), String> {
    ensure_com_init();
    let p = Path::new(&path);
    if !p.exists() {
        return Err("预览目标不存在".to_string());
    }
    if p.is_dir() {
        return Err("文件夹无可用预览".to_string());
    }
    // 关键：WebView2 用 DComp 合成，子 HWND 一律被压在合成层下不可见；
    // 因此宿主窗口必须是独立顶级窗口（WS_POPUP），owner = Tauri 顶级窗口（自动跟随激活/最小化）。
    // 前端给的是 webview 客户区坐标，这里转成屏幕坐标。
    let webview = find_webview_host(top_hwnd);
    let owner_for_pos = if webview.is_invalid() { top_hwnd } else { webview };
    let screen_rect = client_to_screen_rect(owner_for_pos, x, y, w, h);
    eprintln!(
        "[findx2-preview] show path='{}' top=0x{:X} webview=0x{:X} client=({},{},{}x{}) screen=({},{},{}x{})",
        path, top_hwnd.0 as usize, webview.0 as usize,
        x, y, w, h,
        screen_rect.left, screen_rect.top,
        screen_rect.right - screen_rect.left, screen_rect.bottom - screen_rect.top
    );

    // 1. 同路径只更新位置，避免反复重建（防抖 + 滚动选中变化）
    {
        let maybe = {
            let mut g = PREVIEW_STATE.lock().map_err(|e| e.to_string())?;
            if let Some(st) = g.as_mut() {
                if st.path == path {
                    let rect = screen_rect;
                    st.owner_top_hwnd = top_hwnd;
                    st.owner_webview_hwnd = webview;
                    st.last_client_x = x;
                    st.last_client_y = y;
                    st.last_client_w = w;
                    st.last_client_h = h;
                    st.last_screen_rect = rect;
                    let host = st.host_hwnd;
                    let handler = st.handler.clone();
                    Some((host, handler, rect))
                } else {
                    None
                }
            } else {
                None
            }
        };
        if let Some((host, handler, rect)) = maybe {
            unsafe {
                use windows::Win32::UI::WindowsAndMessaging::IsWindow;
                if IsWindow(Some(host)).as_bool() {
                    let _ = SetWindowPos(
                        host,
                        None,
                        rect.left,
                        rect.top,
                        rect.right - rect.left,
                        rect.bottom - rect.top,
                        SET_WINDOW_POS_FLAGS(SWP_NOZORDER.0 | SWP_NOACTIVATE.0),
                    );
                    let inner = RECT {
                        left: 0,
                        top: 0,
                        right: rect.right - rect.left,
                        bottom: rect.bottom - rect.top,
                    };
                    let _ = handler.SetRect(&inner);
                    let _ = ShowWindow(host, SW_SHOW);
                }
            }
            return Ok(());
        }
    }

    // 2. 先释放上一个（drop 会 Unload + DestroyWindow）
    {
        let mut g = PREVIEW_STATE.lock().map_err(|e| e.to_string())?;
        *g = None;
    }

    // 3. 找处理器：优先 HKCR 合并视图（与 Explorer 一致），但 HKCU 优先级更高
    //    会被某些应用（如 WPS）写入伪 IPreviewHandler 覆盖，因此还要准备一个 HKLM
    //    机器级回退 CLSID，CoCreateInstance 失败时再用机器级真处理器（如 Office）重试。
    let clsid_primary = find_preview_handler_clsid(p)
        .ok_or_else(|| "该文件类型未注册系统预览处理器".to_string())?;
    let clsid_fallback = find_preview_handler_clsid_hklm(p)
        .filter(|c| format_clsid(c) != format_clsid(&clsid_primary));
    eprintln!(
        "[findx2-preview] clsid primary={:?} hklm_fallback={:?}",
        clsid_primary, clsid_fallback
    );

    // 内联辅助：基于一个 CLSID 尝试 CoCreateInstance（按位数智能选 ctx）。
    let try_create = |clsid: &GUID| -> Result<IPreviewHandler, String> {
        let info = probe_clsid(clsid);
        eprintln!(
            "[findx2-preview] clsid {:?} info: has_64={} has_32={} inproc_server={} local_server={} handler_only={}",
            clsid, info.has_64, info.has_32, info.has_inproc_server, info.has_local_server, info.handler_only
        );
        let mut attempts: Vec<CLSCTX> = Vec::new();
        let mut push = |c: CLSCTX, list: &mut Vec<CLSCTX>| {
            if !list.iter().any(|x| x.0 == c.0) {
                list.push(c);
            }
        };
        if info.handler_only {
            if info.has_32 { push(CLSCTX_LOCAL_SERVER | CLSCTX_ACTIVATE_32_BIT_SERVER, &mut attempts); }
            if info.has_64 { push(CLSCTX_LOCAL_SERVER | CLSCTX_ACTIVATE_64_BIT_SERVER, &mut attempts); }
            push(CLSCTX_LOCAL_SERVER, &mut attempts);
        } else {
            if info.has_64 {
                push(CLSCTX_INPROC_SERVER | CLSCTX_ACTIVATE_64_BIT_SERVER, &mut attempts);
                push(CLSCTX_LOCAL_SERVER | CLSCTX_ACTIVATE_64_BIT_SERVER, &mut attempts);
            }
            if info.has_32 {
                push(CLSCTX_LOCAL_SERVER | CLSCTX_ACTIVATE_32_BIT_SERVER, &mut attempts);
                push(CLSCTX_INPROC_SERVER | CLSCTX_ACTIVATE_32_BIT_SERVER, &mut attempts);
            }
            push(CLSCTX_LOCAL_SERVER, &mut attempts);
            push(CLSCTX_INPROC_SERVER, &mut attempts);
        }
        let mut last_err: Option<String> = None;
        for ctx in attempts {
            eprintln!("[findx2-preview]   try ctx=0x{:x}", ctx.0);
            let mut got: Option<IPreviewHandler> = None;
            for attempt in 0..6u32 {
                match unsafe { CoCreateInstance::<_, IPreviewHandler>(clsid, None, ctx) } {
                    Ok(h) => {
                        got = Some(h);
                        break;
                    }
                    Err(e) => {
                        if is_com_server_busy(&e) && attempt < 5 {
                            eprintln!(
                                "[findx2-preview]   busy (0x8001010A) retry {}/5 ctx=0x{:x}",
                                attempt + 1,
                                ctx.0
                            );
                            std::thread::sleep(std::time::Duration::from_millis(80));
                            continue;
                        }
                        eprintln!("[findx2-preview]   fail ctx=0x{:x}: {}", ctx.0, e);
                        last_err = Some(format!("ctx=0x{:x}: {}", ctx.0, e));
                        break;
                    }
                }
            }
            if let Some(h) = got {
                eprintln!("[findx2-preview]   ok ctx=0x{:x}", ctx.0);
                return Ok(h);
            }
        }
        Err(last_err.unwrap_or_else(|| "未知错误".into()))
    };

    // 4. 创建 COM 对象：先用 primary（HKCR 合并视图，可能被 HKCU 用户级覆盖）；
    //    primary 失败且存在 HKLM 机器级回退 CLSID（说明被 WPS 等用户级伪 handler 抢占了）则重试。
    let handler: IPreviewHandler = match try_create(&clsid_primary) {
        Ok(h) => h,
        Err(e_primary) => {
            if let Some(fb) = clsid_fallback {
                eprintln!("[findx2-preview] primary failed, retry HKLM fallback");
                match try_create(&fb) {
                    Ok(h) => h,
                    Err(e_fb) => {
                        return Err(format!(
                            "未找到可用的系统预览处理器：primary({e_primary}); fallback({e_fb})"
                        ));
                    }
                }
            } else if e_primary.contains("0x80040154") {
                return Err("未找到可用的系统预览处理器（注册的处理器与本程序不兼容）".to_string());
            } else {
                return Err(format!("CoCreateInstance 预览处理器失败：{e_primary}"));
            }
        }
    };

    // 5. Initialize
    unsafe {
        initialize_handler(&handler, p)?;
    }
    eprintln!("[findx2-preview] Initialize ok");

    // 6. 创建承载窗口 + SetWindow + DoPreview
    let host = unsafe { create_host_window(top_hwnd, screen_rect)? };
    let inner = RECT {
        left: 0,
        top: 0,
        right: (screen_rect.right - screen_rect.left).max(1),
        bottom: (screen_rect.bottom - screen_rect.top).max(1),
    };
    unsafe {
        handler
            .SetWindow(host, &inner)
            .map_err(|e| format!("IPreviewHandler::SetWindow 失败: {e}"))?;
        handler
            .DoPreview()
            .map_err(|e| format!("IPreviewHandler::DoPreview 失败: {e}"))?;
        // 部分预览处理器（WPS、Office）DoPreview 后才创建自己的子窗口；
        // 这时再调一次 SetRect 触发它把内容布局到我们容器里，并强制重绘。
        let _ = handler.SetRect(&inner);
        let _ = ShowWindow(host, SW_SHOW);
        let _ = BringWindowToTop(host);
        let _ = InvalidateRect(Some(host), None, true);
        let _ = UpdateWindow(host);
        eprintln!(
            "[findx2-preview] DoPreview ok, host shown at ({}x{})",
            inner.right, inner.bottom
        );
    }

    let mut g = PREVIEW_STATE.lock().map_err(|e| e.to_string())?;
    *g = Some(PreviewState {
        host_hwnd: host,
        path,
        handler,
        owner_top_hwnd: top_hwnd,
        owner_webview_hwnd: webview,
        last_client_x: x,
        last_client_y: y,
        last_client_w: w,
        last_client_h: h,
        last_screen_rect: screen_rect,
    });
    drop(g);
    // 位置仅由主线程 `preview_set_bounds`（及前端 ResizeObserver / onMoved）驱动；
    // 勿在后台线程对主线程创建的 HWND 调 SetWindowPos，否则会破坏 prevhost/WebView2 子窗口绘制（数秒后白屏）。
    Ok(())
}

/// 仅同步当前承载窗口的位置/大小（拖动分隔条 / resize 时高频调用，开销极小）。
/// `x/y` 是相对于 webview 客户区的像素，会换算为屏幕坐标后应用到顶级 popup 宿主。
pub fn set_bounds(top_hwnd: HWND, x: i32, y: i32, w: i32, h: i32) -> Result<(), String> {
    let webview = find_webview_host(top_hwnd);
    let owner = if webview.is_invalid() { top_hwnd } else { webview };
    let r = client_to_screen_rect(owner, x, y, w, h);
    let inner = RECT {
        left: 0,
        top: 0,
        right: (r.right - r.left).max(1),
        bottom: (r.bottom - r.top).max(1),
    };
    let host_handler = {
        let mut g = PREVIEW_STATE.lock().map_err(|e| e.to_string())?;
        if let Some(st) = g.as_mut() {
            st.owner_top_hwnd = top_hwnd;
            st.owner_webview_hwnd = webview;
            st.last_client_x = x;
            st.last_client_y = y;
            st.last_client_w = w;
            st.last_client_h = h;
            st.last_screen_rect = r;
            Some((st.host_hwnd, st.handler.clone()))
        } else {
            None
        }
    };
    if let Some((host, handler)) = host_handler {
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::IsWindow;
            if IsWindow(Some(host)).as_bool() {
                let _ = SetWindowPos(
                    host,
                    None,
                    r.left,
                    r.top,
                    (r.right - r.left).max(1),
                    (r.bottom - r.top).max(1),
                    SET_WINDOW_POS_FLAGS(SWP_NOZORDER.0 | SWP_NOACTIVATE.0),
                );
                let _ = handler.SetRect(&inner);
            }
        }
    }
    Ok(())
}

/// 隐藏预览（保留对象，便于下次同路径快速恢复）。
pub fn hide_preview() -> Result<(), String> {
    let host = {
        let g = PREVIEW_STATE.lock().map_err(|e| e.to_string())?;
        g.as_ref().map(|st| st.host_hwnd)
    };
    if let Some(host) = host {
        unsafe {
            let _ = ShowWindow(host, SW_HIDE);
        }
    }
    Ok(())
}

/// 彻底卸载（关闭面板时调用，释放 prevhost 进程占用）。
pub fn unload_preview() -> Result<(), String> {
    let mut g = PREVIEW_STATE.lock().map_err(|e| e.to_string())?;
    *g = None;
    Ok(())
}
