//! 打开或在资源管理器中显示路径。只允许在后台任务调用。

pub fn open_path(path: &str) -> Result<(), String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("路径为空".into());
    }
    #[cfg(windows)]
    {
        use windows::{
            Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
            core::PCWSTR,
        };
        let operation = "open\0".encode_utf16().collect::<Vec<_>>();
        let wide = trimmed
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let result = unsafe {
            ShellExecuteW(
                None,
                PCWSTR(operation.as_ptr()),
                PCWSTR(wide.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        if result.0 as isize <= 32 {
            Err(format!(
                "系统无法打开此文件（代码 {}），请检查文件关联或访问权限。",
                result.0 as isize
            ))
        } else {
            Ok(())
        }
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(trimmed)
            .spawn()
            .map_err(|e| format!("打开失败: {e}"))?;
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(trimmed)
            .spawn()
            .map_err(|e| format!("打开失败: {e}"))?;
        Ok(())
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        Err("当前平台未实现打开路径".into())
    }
}

pub fn reveal_in_folder(path: &str) -> Result<(), String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("路径为空".into());
    }
    #[cfg(windows)]
    {
        std::process::Command::new("explorer")
            .arg(format!("/select,{trimmed}"))
            .spawn()
            .map_err(|e| format!("定位失败: {e}"))?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .args(["-R", trimmed])
            .spawn()
            .map_err(|e| format!("定位失败: {e}"))?;
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(parent) = std::path::Path::new(trimmed).parent() {
            std::process::Command::new("xdg-open")
                .arg(parent)
                .spawn()
                .map_err(|e| format!("定位失败: {e}"))?;
            return Ok(());
        }
        Err("无法确定父目录".into())
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        Err("当前平台未实现定位路径".into())
    }
}

pub fn requires_run_confirmation(path: &str) -> bool {
    matches!(
        std::path::Path::new(path)
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase()
            .as_str(),
        "exe"
            | "com"
            | "bat"
            | "cmd"
            | "ps1"
            | "vbs"
            | "vbe"
            | "js"
            | "jse"
            | "wsf"
            | "wsh"
            | "msi"
            | "lnk"
            | "url"
            | "scr"
            | "hta"
            | "reg"
    )
}
#[cfg(test)]
mod tests {
    #[test]
    fn run_confirmation_is_conservative() {
        assert!(super::requires_run_confirmation("C:/Run.CMD"));
        assert!(super::requires_run_confirmation("C:/shortcut.lnk"));
        assert!(!super::requires_run_confirmation("C:/safe & notes.txt"));
    }
}

#[derive(Clone, Copy)]
pub enum ShellDialog {
    OpenWith,
    Properties,
}
/// Public shell dialogs; never interpolate a path into a command line.
pub fn shell_dialog(path: &str, dialog: ShellDialog) -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows::{
            Win32::{
                System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize},
                UI::{
                    Shell::{
                        OAIF_EXEC, OPENASINFO, SEE_MASK_INVOKEIDLIST, SHELLEXECUTEINFOW,
                        SHOpenWithDialog, ShellExecuteExW,
                    },
                    WindowsAndMessaging::SW_SHOWNORMAL,
                },
            },
            core::PCWSTR,
        };
        let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        let wide = path.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let result = unsafe {
            match dialog {
                ShellDialog::OpenWith => SHOpenWithDialog(
                    None,
                    &OPENASINFO {
                        pcszFile: PCWSTR(wide.as_ptr()),
                        pcszClass: PCWSTR::null(),
                        oaifInFlags: OAIF_EXEC,
                    },
                ),
                ShellDialog::Properties => {
                    let verb = "properties\0".encode_utf16().collect::<Vec<_>>();
                    let mut info = SHELLEXECUTEINFOW {
                        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
                        fMask: SEE_MASK_INVOKEIDLIST,
                        lpVerb: PCWSTR(verb.as_ptr()),
                        lpFile: PCWSTR(wide.as_ptr()),
                        nShow: SW_SHOWNORMAL.0,
                        ..Default::default()
                    };
                    ShellExecuteExW(&mut info)
                }
            }
        };
        if initialized {
            unsafe { CoUninitialize() }
        };
        result.map_err(|e| format!("无法打开系统对话框；请检查关联与访问权限。诊断：{e}"))
    }
    #[cfg(not(windows))]
    {
        let _ = (path, dialog);
        Err("当前平台不支持此 Windows 对话框".into())
    }
}
