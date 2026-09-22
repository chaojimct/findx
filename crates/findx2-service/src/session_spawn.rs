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
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
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

    // 2.4.5：宿主挂进 KILL_ON_JOB_CLOSE 的 Job。服务进程退出（优雅停止/崩溃/硬杀）时
    // job 句柄由 OS 关闭 → 宿主随之被杀。否则宿主成孤儿：服务每次重启泄漏一个
    // --everything-host 进程，且它锁住服务 exe，阻碍升级时的 exe 替换。
    // job 句柄故意不关——进程存活期间保持打开正是「退出即杀」的开关。
    if let Err(e) = unsafe { assign_kill_on_close(pi.hProcess) } {
        warn!("宿主未挂 kill-on-close Job（泄漏风险）: {e}");
    }

    Ok(pi.hProcess)
}

/// 把进程挂进 kill-on-close Job（失败不阻断——宿主照常工作，只是退出时可能残留）。
unsafe fn assign_kill_on_close(process: HANDLE) -> anyhow::Result<()> {
    let job = CreateJobObjectW(None, PCWSTR::null())
        .map_err(|e| anyhow::anyhow!("CreateJobObjectW: {e}"))?;
    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    SetInformationJobObject(
        job,
        JobObjectExtendedLimitInformation,
        &info as *const _ as *const core::ffi::c_void,
        std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
    )
    .map_err(|e| anyhow::anyhow!("SetInformationJobObject: {e}"))?;
    AssignProcessToJobObject(job, process)
        .map_err(|e| anyhow::anyhow!("AssignProcessToJobObject: {e}"))?;
    Ok(())
}
