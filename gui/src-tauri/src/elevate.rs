//! Windows：以 `runas` 提权启动子进程（UAC），供建索引与 findx2-service 使用。
//!
//! 若用户已「以管理员身份运行」启动 GUI，当前进程已提升，子进程应优先用 `Command::spawn`
//! 继承权限；此时再对 `findx2-service` 使用 `runas` 往往多余且易失败。

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows::Win32::Security::{
    GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, OpenProcessToken, WaitForSingleObject, INFINITE,
};
use windows::Win32::UI::Shell::{ShellExecuteExW, SHELLEXECUTEINFOW, SEE_MASK_NOCLOSEPROCESS};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

fn to_wide_os(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

fn to_wide_str(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 当前进程是否已 UAC 提升为管理员。
pub fn process_is_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut ret_len: u32 = 0;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(
                (&mut elevation as *mut TOKEN_ELEVATION)
                    .cast::<std::ffi::c_void>(),
            ),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret_len,
        );
        let _ = CloseHandle(token);
        if ok.is_err() {
            return false;
        }
        elevation.TokenIsElevated != 0
    }
}

/// 为 `ShellExecuteW` 参数字符串中需转义的空格路径加引号。
pub fn quote_arg(s: &str) -> String {
    if s.is_empty() {
        return "\"\"".to_string();
    }
    if s.chars().any(|c| c.is_whitespace() || c == '"') {
        format!("\"{}\"", s.replace('"', r#"\""#))
    } else {
        s.to_string()
    }
}

/// 以管理员身份启动 `exe`（弹出 UAC）。
/// - `wait_for_exit == true`：等待进程结束并返回退出码（建索引）。
/// - `wait_for_exit == false`：仅发起提升启动（服务进程）。
pub fn shell_execute_runas(
    exe: &Path,
    params: Option<&str>,
    cwd: &Path,
    wait_for_exit: bool,
) -> Result<Option<u32>, String> {
    let verb = to_wide_str("runas");
    let file = to_wide_os(exe.as_os_str());
    let params_owned = params.map(to_wide_str);
    let dir = to_wide_os(cwd.as_os_str());

    let lp_parameters = params_owned
        .as_ref()
        .map(|p| PCWSTR(p.as_ptr()))
        .unwrap_or(PCWSTR::null());

    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: if wait_for_exit {
            SEE_MASK_NOCLOSEPROCESS
        } else {
            0
        },
        hwnd: HWND::default(),
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: lp_parameters,
        lpDirectory: PCWSTR(dir.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };

    unsafe {
        ShellExecuteExW(&mut sei).map_err(|e| format!("UAC 启动失败: {e}"))?;
    }

    if !wait_for_exit {
        // 未加 NOCLOSEPROCESS 时 hProcess 无有效值，仅表示已提交 UAC/启动。
        return Ok(None);
    }

    let h = sei.hProcess;
    if h.is_invalid() || h == HANDLE::default() {
        return Err("未能获取提权子进程句柄".into());
    }

    unsafe {
        let _ = WaitForSingleObject(h, INFINITE);
        let mut code: u32 = 0;
        GetExitCodeProcess(h, &mut code).map_err(|e| format!("读取退出码失败: {e}"))?;
        CloseHandle(h).map_err(|e| format!("CloseHandle: {e}"))?;
        Ok(Some(code))
    }
}
