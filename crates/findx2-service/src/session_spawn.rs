//! 在交互用户会话里拉起进程（Session 0 服务看不到 `FindWindow`）。

use std::ffi::OsString;
use std::os::windows::ffi::OsStrExt;
use std::time::Duration;

use tracing::{info, warn};
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{
    DuplicateTokenEx, SecurityImpersonation, TokenPrimary, TOKEN_ALL_ACCESS,
};
use windows::Win32::System::RemoteDesktop::{
    ProcessIdToSessionId, WTSGetActiveConsoleSessionId, WTSQueryUserToken,
};
use windows::Win32::System::Threading::{
    CreateProcessAsUserW, GetCurrentProcessId, WaitForSingleObject, CREATE_NO_WINDOW,
    CREATE_UNICODE_ENVIRONMENT, PROCESS_INFORMATION, STARTF_USESHOWWINDOW, STARTUPINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

pub(crate) fn current_session_id() -> u32 {
    let mut sid = 0u32;
    if unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut sid) }.is_ok() {
        sid
    } else {
        0
    }
}

/// Session 0 里不要自己建 Everything 窗口；在活动用户会话循环拉起 `--everything-host`。
pub(crate) fn spawn_everything_host_watchdog(pipe: String) {
    let _ = std::thread::Builder::new()
        .name("everything-host-wd".into())
        .spawn(move || loop {
            match spawn_everything_host(&pipe) {
                Ok(proc) => {
                    info!("已在用户会话启动 Everything 兼容宿主");
                    unsafe {
                        let _ = WaitForSingleObject(proc, u32::MAX);
                        let _ = CloseHandle(proc);
                    }
                    warn!("Everything 兼容宿主已退出，5 秒后重拉");
                }
                Err(e) => {
                    warn!("拉起 Everything 兼容宿主失败: {e}");
                }
            }
            std::thread::sleep(Duration::from_secs(5));
        });
}

fn spawn_everything_host(pipe: &str) -> anyhow::Result<HANDLE> {
    let session = unsafe { WTSGetActiveConsoleSessionId() };
    if session == 0 || session == u32::MAX {
        return Err(anyhow::anyhow!("当前没有交互用户会话"));
    }

    let exe = std::env::current_exe()?;
    let mut cmd = OsString::from("\"");
    cmd.push(exe.as_os_str());
    cmd.push("\" --everything-host --pipe ");
    cmd.push(pipe);

    unsafe { create_process_in_session(session, &exe, &cmd) }
}

unsafe fn create_process_in_session(
    session: u32,
    exe: &std::path::Path,
    cmd: &OsString,
) -> anyhow::Result<HANDLE> {
    let mut user = HANDLE::default();
    WTSQueryUserToken(session, &mut user).map_err(|e| anyhow::anyhow!("WTSQueryUserToken: {e}"))?;

    let mut primary = HANDLE::default();
    let dup = DuplicateTokenEx(
        user,
        TOKEN_ALL_ACCESS,
        None,
        SecurityImpersonation,
        TokenPrimary,
        &mut primary,
    );
    let _ = CloseHandle(user);
    dup.map_err(|e| anyhow::anyhow!("DuplicateTokenEx: {e}"))?;

    let exe_w: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut cmd_w: Vec<u16> = cmd.encode_wide().chain(Some(0)).collect();
    let mut desktop: Vec<u16> = "winsta0\\default"
        .encode_utf16()
        .chain(Some(0))
        .collect();

    let si = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        lpDesktop: PWSTR(desktop.as_mut_ptr()),
        dwFlags: STARTF_USESHOWWINDOW,
        wShowWindow: SW_HIDE.0 as u16,
        ..Default::default()
    };
    let mut pi = PROCESS_INFORMATION::default();

    let created = CreateProcessAsUserW(
        primary,
        PCWSTR(exe_w.as_ptr()),
        PWSTR(cmd_w.as_mut_ptr()),
        None,
        None,
        false,
        CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
        None,
        None,
        &si,
        &mut pi,
    );
    let _ = CloseHandle(primary);
    created.map_err(|e| anyhow::anyhow!("CreateProcessAsUser: {e}"))?;

    let _ = CloseHandle(pi.hThread);
    Ok(pi.hProcess)
}
