//! 结果列表右键：先出 FindX 常用菜单，再按需弹出系统 `IContextMenu`。
//!
//! 系统菜单又长又爱自绘，第三方扩展一多就会顶满屏幕、叠字、滚轮失灵。
//! 搜索场景 90% 只要打开 / 路径 / 复制 / 删除，不必每次 `QueryContextMenu`。

use crate::OwnedItemIdList;
use arboard::Clipboard;
use std::cell::{Cell, RefCell};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;
use windows::core::{Interface, PCSTR, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_core::BOOL;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_DOWN, VK_SHIFT, VK_UP};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    CMINVOKECOMMANDINFO, CMF_EXTENDEDVERBS, CMF_NORMAL, IContextMenu, IContextMenu2, IContextMenu3,
    IShellFolder, SHBindToParent, SHFileOperationW, FO_DELETE, FOF_ALLOWUNDO, SHFILEOPSTRUCTW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CallNextHookEx, CallWindowProcW, CreatePopupMenu, DestroyMenu, EnumThreadWindows,
    GetClassNameW, GetCursorPos, GetWindowLongPtrW, IsWindowVisible, SendMessageW,
    SetForegroundWindow, SetMenuInfo, SetWindowLongPtrW, SetWindowsHookExW, TrackPopupMenu,
    UnhookWindowsHookEx, WindowFromPoint, GWLP_WNDPROC, HHOOK, HMENU, MENUINFO, MF_SEPARATOR,
    MF_STRING, MIM_MAXHEIGHT, MSGF_MENU, MSG, SW_SHOWNORMAL, TPM_LEFTALIGN, TPM_RETURNCMD,
    TRACK_POPUP_MENU_FLAGS, WH_MSGFILTER, WM_DRAWITEM, WM_INITMENUPOPUP, WM_KEYDOWN,
    WM_MEASUREITEM, WM_MENUCHAR, WM_MOUSEWHEEL, WNDPROC,
};

const CMD_OPEN: usize = 1;
const CMD_OPEN_PARENT: usize = 2;
const CMD_COPY_PATH: usize = 3;
const CMD_COPY_NAME: usize = 4;
const CMD_DELETE: usize = 5;
const CMD_MORE: usize = 6;
const SHELL_CMD_FIRST: u32 = 0x1000;
const WHEEL_DELTA: i32 = 120;
const WHEEL_LINES_PER_NOTCH: i32 = 3;

fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// WebView2 传入的 `screenX`/`screenY` 在多屏、混合缩放下常与 Win32 物理坐标不一致。
fn track_popup_anchor_screen(screen_x: i32, screen_y: i32) -> (i32, i32) {
    unsafe {
        let mut pt = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut pt).is_ok() {
            (pt.x, pt.y)
        } else {
            (screen_x, screen_y)
        }
    }
}

fn work_area_max_menu_height(anchor: POINT) -> u32 {
    unsafe {
        let mon = MonitorFromPoint(anchor, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(mon, &mut mi).as_bool() {
            let h = mi.rcWork.bottom - mi.rcWork.top;
            (h - 24).max(240) as u32
        } else {
            720
        }
    }
}

struct MenuGuard(HMENU);

impl Drop for MenuGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyMenu(self.0);
        }
    }
}

/// `TrackPopupMenu` 模态循环里要把自绘消息转给 `IContextMenu2/3`，
/// 否则 WPS / 网盘 / 互传 等 owner-draw 项量高为 0，会叠在一起。
struct ShellMenuHook {
    old_proc: WNDPROC,
    cm2: Option<IContextMenu2>,
    cm3: Option<IContextMenu3>,
}

thread_local! {
    static SHELL_MENU_HOOK: RefCell<Option<ShellMenuHook>> = const { RefCell::new(None) };
    static MSG_FILTER_HOOK: Cell<HHOOK> = const { Cell::new(HHOOK(std::ptr::null_mut())) };
    static WHEEL_ACCUM: Cell<i32> = const { Cell::new(0) };
}

struct SubclassGuard {
    hwnd: HWND,
    old_proc: WNDPROC,
}

impl Drop for SubclassGuard {
    fn drop(&mut self) {
        unsafe {
            SetWindowLongPtrW(
                self.hwnd,
                GWLP_WNDPROC,
                std::mem::transmute::<WNDPROC, isize>(self.old_proc),
            );
        }
        SHELL_MENU_HOOK.with(|h| h.replace(None));
    }
}

struct MsgFilterGuard {
    hook: HHOOK,
}

impl Drop for MsgFilterGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = UnhookWindowsHookEx(self.hook);
        }
        MSG_FILTER_HOOK.set(HHOOK(std::ptr::null_mut()));
        WHEEL_ACCUM.set(0);
    }
}

fn install_menu_wheel_hook() -> Option<MsgFilterGuard> {
    unsafe {
        let hook = SetWindowsHookExW(
            WH_MSGFILTER,
            Some(menu_msg_filter),
            None,
            GetCurrentThreadId(),
        )
        .ok()?;
        MSG_FILTER_HOOK.set(hook);
        Some(MsgFilterGuard { hook })
    }
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 32];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

unsafe extern "system" fn enum_popup_menu(hwnd: HWND, lparam: LPARAM) -> BOOL {
    if unsafe { IsWindowVisible(hwnd) }.as_bool() && class_name(hwnd) == "#32768" {
        unsafe {
            *(lparam.0 as *mut HWND) = hwnd;
        }
        return false.into();
    }
    true.into()
}

fn find_popup_menu_hwnd(pt: POINT) -> HWND {
    unsafe {
        let hit = WindowFromPoint(pt);
        if !hit.is_invalid() && class_name(hit) == "#32768" {
            return hit;
        }
        let mut found = HWND::default();
        let _ = EnumThreadWindows(
            GetCurrentThreadId(),
            Some(enum_popup_menu),
            LPARAM(&mut found as *mut HWND as isize),
        );
        found
    }
}

fn wheel_delta(wparam: WPARAM) -> i32 {
    (wparam.0 as u32 >> 16) as i16 as i32
}

/// `#32768` 系统菜单不处理滚轮，只认键盘。把滚轮折成 ↑/↓，碰到可见区边缘就会滚动。
fn scroll_popup_menu(msg: &MSG) {
    let delta = wheel_delta(msg.wParam);
    if delta == 0 {
        return;
    }
    let accum = WHEEL_ACCUM.get() + delta;
    let notches = accum / WHEEL_DELTA;
    WHEEL_ACCUM.set(accum - notches * WHEEL_DELTA);
    if notches == 0 {
        return;
    }

    let hwnd = find_popup_menu_hwnd(msg.pt);
    if hwnd.is_invalid() {
        return;
    }

    let key = if notches > 0 { VK_UP } else { VK_DOWN };
    let steps = notches.unsigned_abs() * WHEEL_LINES_PER_NOTCH as u32;
    for _ in 0..steps {
        unsafe {
            let _ = SendMessageW(
                hwnd,
                WM_KEYDOWN,
                Some(WPARAM(key.0 as usize)),
                Some(LPARAM(0)),
            );
        }
    }
}

unsafe extern "system" fn menu_msg_filter(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == MSGF_MENU as i32 && lparam.0 != 0 {
        let msg = unsafe { &*(lparam.0 as *const MSG) };
        if msg.message == WM_MOUSEWHEEL {
            scroll_popup_menu(msg);
            return LRESULT(1);
        }
    }
    let hook = MSG_FILTER_HOOK.get();
    unsafe { CallNextHookEx(Some(hook), code, wparam, lparam) }
}

unsafe extern "system" fn shell_menu_subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let handled = SHELL_MENU_HOOK.with(|cell| {
        let hook = cell.borrow();
        let Some(st) = hook.as_ref() else {
            return None;
        };
        match msg {
            WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR => {
                if let Some(cm3) = &st.cm3 {
                    let mut lr = LRESULT(0);
                    if cm3
                        .HandleMenuMsg2(msg, wparam, lparam, Some(&mut lr))
                        .is_ok()
                    {
                        return Some(lr);
                    }
                } else if let Some(cm2) = &st.cm2 {
                    if cm2.HandleMenuMsg(msg, wparam, lparam).is_ok() {
                        return Some(LRESULT(0));
                    }
                }
                None
            }
            _ => None,
        }
    });
    if let Some(lr) = handled {
        return lr;
    }
    let old = SHELL_MENU_HOOK.with(|cell| cell.borrow().as_ref().and_then(|s| s.old_proc));
    unsafe { CallWindowProcW(old, hwnd, msg, wparam, lparam) }
}

fn track_popup(hwnd: HWND, hmenu: HMENU, px: i32, py: i32) -> u32 {
    let _wheel = install_menu_wheel_hook();
    let _ = unsafe { SetForegroundWindow(hwnd) };
    let tpm = TRACK_POPUP_MENU_FLAGS(TPM_LEFTALIGN.0 | TPM_RETURNCMD.0);
    let picked = unsafe { TrackPopupMenu(hmenu, tpm, px, py, None, hwnd, None) };
    picked.0 as u32
}

fn append_string(hmenu: HMENU, id: usize, label: &str) -> Result<(), String> {
    let wide = to_wide_null(label);
    unsafe { AppendMenuW(hmenu, MF_STRING, id, PCWSTR(wide.as_ptr())) }
        .map_err(|e| format!("AppendMenu: {e}"))
}

fn recycle_path(hwnd: HWND, path: &str) -> Result<(), String> {
    let wide_path: Vec<u16> = Path::new(path)
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .chain(Some(0))
        .collect();
    let mut op = SHFILEOPSTRUCTW {
        hwnd,
        wFunc: FO_DELETE,
        pFrom: PCWSTR(wide_path.as_ptr()),
        pTo: PCWSTR::null(),
        fFlags: FOF_ALLOWUNDO.0 as u16,
        fAnyOperationsAborted: Default::default(),
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: PCWSTR::null(),
    };
    let result = unsafe { SHFileOperationW(&mut op) };
    if result != 0 || op.fAnyOperationsAborted.as_bool() {
        if op.fAnyOperationsAborted.as_bool() {
            return Ok(());
        }
        return Err(format!(
            "移至回收站失败（SHFileOperationW 返回 {result}）。请确认路径存在且未被占用。"
        ));
    }
    Ok(())
}

enum FindxPick {
    None,
    Open,
    OpenParent,
    CopyPath,
    CopyName,
    Delete,
    More,
}

fn show_findx_menu(hwnd: HWND, px: i32, py: i32) -> Result<FindxPick, String> {
    unsafe {
        let hmenu = CreatePopupMenu().map_err(|e| format!("CreatePopupMenu: {e}"))?;
        let _guard = MenuGuard(hmenu);

        append_string(hmenu, CMD_OPEN, "打开(&O)")?;
        append_string(hmenu, CMD_OPEN_PARENT, "打开路径(&P)")?;
        append_string(hmenu, CMD_COPY_PATH, "复制路径(&C)")?;
        append_string(hmenu, CMD_COPY_NAME, "复制文件名(&N)")?;
        AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null())
            .map_err(|e| format!("AppendMenu(分隔符): {e}"))?;
        append_string(hmenu, CMD_DELETE, "删除(&D)")?;
        AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null())
            .map_err(|e| format!("AppendMenu(分隔符): {e}"))?;
        append_string(hmenu, CMD_MORE, "更多 Windows 操作(&M)...")?;

        windows::Win32::UI::WindowsAndMessaging::SetMenuDefaultItem(hmenu, 0, 1)
            .map_err(|e| format!("SetMenuDefaultItem: {e}"))?;

        Ok(match track_popup(hwnd, hmenu, px, py) as usize {
            0 => FindxPick::None,
            CMD_OPEN => FindxPick::Open,
            CMD_OPEN_PARENT => FindxPick::OpenParent,
            CMD_COPY_PATH => FindxPick::CopyPath,
            CMD_COPY_NAME => FindxPick::CopyName,
            CMD_DELETE => FindxPick::Delete,
            CMD_MORE => FindxPick::More,
            _ => FindxPick::None,
        })
    }
}

fn show_shell_menu(hwnd: HWND, path: &str, px: i32, py: i32, extended: bool) -> Result<(), String> {
    let pidl_root = OwnedItemIdList::from_path(Path::new(path))?;

    unsafe {
        let mut pidl_last: *mut ITEMIDLIST = std::ptr::null_mut();
        let folder: IShellFolder = SHBindToParent(pidl_root.as_ptr(), Some(&mut pidl_last))
            .map_err(|e| format!("SHBindToParent: {e}"))?;

        let pcm: IContextMenu = folder
            .GetUIObjectOf(hwnd, &[pidl_last as *const ITEMIDLIST], None)
            .map_err(|e| format!("GetUIObjectOf(IContextMenu): {e}"))?;
        let cm3 = pcm.cast::<IContextMenu3>().ok();
        let cm2 = pcm.cast::<IContextMenu2>().ok();

        let hmenu = CreatePopupMenu().map_err(|e| format!("CreatePopupMenu: {e}"))?;
        let _guard = MenuGuard(hmenu);

        let mut qcm_flags = CMF_NORMAL;
        if extended {
            qcm_flags |= CMF_EXTENDEDVERBS;
        }

        pcm.QueryContextMenu(hmenu, 0, SHELL_CMD_FIRST, 0x7FFF, qcm_flags)
            .ok()
            .map_err(|e| format!("QueryContextMenu: {e}"))?;

        let max_h = work_area_max_menu_height(POINT { x: px, y: py });
        let mi = MENUINFO {
            cbSize: std::mem::size_of::<MENUINFO>() as u32,
            fMask: MIM_MAXHEIGHT,
            cyMax: max_h,
            ..Default::default()
        };
        SetMenuInfo(hmenu, &mi).map_err(|e| format!("SetMenuInfo: {e}"))?;

        let old_ptr = GetWindowLongPtrW(hwnd, GWLP_WNDPROC);
        let old_proc: WNDPROC = std::mem::transmute(old_ptr);
        SHELL_MENU_HOOK.with(|cell| {
            cell.replace(Some(ShellMenuHook {
                old_proc,
                cm2,
                cm3,
            }));
        });
        SetWindowLongPtrW(
            hwnd,
            GWLP_WNDPROC,
            shell_menu_subclass_proc as *const () as isize,
        );
        let _subclass = SubclassGuard { hwnd, old_proc };

        let cmd = track_popup(hwnd, hmenu, px, py);
        drop(_subclass);

        if cmd == 0 {
            return Ok(());
        }
        if (cmd as usize) < SHELL_CMD_FIRST as usize {
            return Ok(());
        }

        let offset = cmd - SHELL_CMD_FIRST;
        if offset > 0xFFFF {
            return Err("无效的系统菜单命令。".to_string());
        }
        let cmi = CMINVOKECOMMANDINFO {
            cbSize: std::mem::size_of::<CMINVOKECOMMANDINFO>() as u32,
            hwnd,
            lpVerb: PCSTR(offset as *const u8),
            lpParameters: PCSTR::null(),
            lpDirectory: PCSTR::null(),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        pcm.InvokeCommand(std::ptr::addr_of!(cmi))
            .map_err(|e| format!("InvokeCommand: {e}"))?;
    }

    Ok(())
}

/// 必须在主线程、对已存在路径调用。
pub(crate) fn run_composite_hit_menu(
    app: &AppHandle,
    hwnd: HWND,
    path: String,
    screen_x: i32,
    screen_y: i32,
) -> Result<(), String> {
    let target = PathBuf::from(&path);
    if !target.exists() {
        return Err("所选路径在磁盘上不存在。".to_string());
    }

    let (px, py) = track_popup_anchor_screen(screen_x, screen_y);
    let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;

    // Shift+右键：直接出完整系统菜单（含扩展动词），和资源管理器习惯对齐。
    if shift {
        return show_shell_menu(hwnd, &path, px, py, true);
    }

    match show_findx_menu(hwnd, px, py)? {
        FindxPick::None => Ok(()),
        FindxPick::Open => app
            .opener()
            .open_path(&path, None::<&str>)
            .map_err(|e| format!("打开失败: {e}")),
        FindxPick::OpenParent => app
            .opener()
            .reveal_item_in_dir(&target)
            .map_err(|e| format!("打开所在文件夹失败: {e}")),
        FindxPick::CopyPath => Clipboard::new()
            .map_err(|e| format!("剪贴板: {e}"))?
            .set_text(path)
            .map_err(|e| format!("复制失败: {e}")),
        FindxPick::CopyName => {
            let name = target
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| path.clone());
            Clipboard::new()
                .map_err(|e| format!("剪贴板: {e}"))?
                .set_text(name)
                .map_err(|e| format!("复制失败: {e}"))
        }
        FindxPick::Delete => recycle_path(hwnd, &path),
        FindxPick::More => show_shell_menu(hwnd, &path, px, py, false),
    }
}
