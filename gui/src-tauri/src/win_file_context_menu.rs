//! Windows：在资源管理器 Shell 右键菜单**上方**插入「打开 / 打开路径 / 复制路径」，再显示系统原生右键菜单。

use crate::OwnedItemIdList;
use arboard::Clipboard;
use std::path::{Path, PathBuf};
use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;
use windows::core::PCSTR;
use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    CMINVOKECOMMANDINFO, CMF_EXTENDEDVERBS, CMF_NORMAL, IContextMenu, IShellFolder, SHBindToParent,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, SetForegroundWindow, TrackPopupMenu, HMENU, MF_SEPARATOR,
    MF_STRING, SW_SHOWNORMAL, TPM_LEFTALIGN, TPM_RETURNCMD,
    TRACK_POPUP_MENU_FLAGS,
};

const CMD_OPEN: usize = 1;
const CMD_OPEN_PARENT: usize = 2;
const CMD_COPY_PATH: usize = 3;
const SHELL_CMD_FIRST: u32 = 0x1000;

fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

struct MenuGuard(HMENU);

impl Drop for MenuGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyMenu(self.0);
        }
    }
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

    let pidl_root = OwnedItemIdList::from_path(Path::new(&path))?;

    unsafe {
        let mut pidl_last: *mut ITEMIDLIST = std::ptr::null_mut();
        let folder: IShellFolder = SHBindToParent(pidl_root.as_ptr(), Some(&mut pidl_last))
            .map_err(|e| format!("SHBindToParent: {e}"))?;

        let pcm: IContextMenu = folder
            .GetUIObjectOf(hwnd, &[pidl_last as *const ITEMIDLIST], None)
            .map_err(|e| format!("GetUIObjectOf(IContextMenu): {e}"))?;

        let hmenu = CreatePopupMenu().map_err(|e| format!("CreatePopupMenu: {e}"))?;
        let _guard = MenuGuard(hmenu);

        let open_lbl = to_wide_null("打开(&O)");
        let open_path_lbl = to_wide_null("打开路径(&O)");
        let copy_lbl = to_wide_null("复制完整路径和文件名(&F)");

        AppendMenuW(
            hmenu,
            MF_STRING,
            CMD_OPEN,
            PCWSTR(open_lbl.as_ptr()),
        )
        .map_err(|e| format!("AppendMenu: {e}"))?;

        AppendMenuW(
            hmenu,
            MF_STRING,
            CMD_OPEN_PARENT,
            PCWSTR(open_path_lbl.as_ptr()),
        )
        .map_err(|e| format!("AppendMenu: {e}"))?;

        AppendMenuW(
            hmenu,
            MF_STRING,
            CMD_COPY_PATH,
            PCWSTR(copy_lbl.as_ptr()),
        )
        .map_err(|e| format!("AppendMenu: {e}"))?;

        // 首项「打开」为默认（粗体）
        windows::Win32::UI::WindowsAndMessaging::SetMenuDefaultItem(hmenu, 0, 1)
            .map_err(|e| format!("SetMenuDefaultItem: {e}"))?;

        AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null())
            .map_err(|e| format!("AppendMenu(分隔符): {e}"))?;

        let index_shell = 4u32;
        let hr = pcm.QueryContextMenu(
            hmenu,
            index_shell,
            SHELL_CMD_FIRST,
            0x7FFF,
            CMF_NORMAL | CMF_EXTENDEDVERBS,
        );
        hr.ok().map_err(|e| format!("QueryContextMenu: {e}"))?;

        let _ = SetForegroundWindow(hwnd);

        let tpm = TRACK_POPUP_MENU_FLAGS(TPM_LEFTALIGN.0 | TPM_RETURNCMD.0);
        let picked = TrackPopupMenu(hmenu, tpm, screen_x, screen_y, None, hwnd, None);

        // WebView2/Win32 文档：TPM_RETURNCMD 时返回值为选中项 ID，但绑定为 BOOL，按数值读取。
        let cmd = picked.0 as u32;
        if cmd == 0 {
            return Ok(());
        }

        match cmd as usize {
            CMD_OPEN => {
                app.opener()
                    .open_path(&path, None::<&str>)
                    .map_err(|e| format!("打开失败: {e}"))?;
            }
            CMD_OPEN_PARENT => {
                app.opener()
                    .reveal_item_in_dir(&target)
                    .map_err(|e| format!("打开所在文件夹失败: {e}"))?;
            }
            CMD_COPY_PATH => {
                Clipboard::new()
                    .map_err(|e| format!("剪贴板: {e}"))?
                    .set_text(path.clone())
                    .map_err(|e| format!("复制失败: {e}"))?;
            }
            shell_id if shell_id >= SHELL_CMD_FIRST as usize => {
                let offset = shell_id - SHELL_CMD_FIRST as usize;
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
            _ => {}
        }
    }

    Ok(())
}
