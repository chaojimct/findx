//! Windows 服务安装、卸载与入口调度。
//!
//! 停止协议（2.4.5 起）：STOP 控制码 → 置 `stop` 标志 → 上报 StopPending（wait_hint 10s）
//! → 持续更新 StopPending checkpoint，等待 run_foreground 的真实 Result → 上报相应 STOPPED。
//! 不再按 10s 硬退；IO 卡住时安装器必须超时拒绝替换，而不是强杀工作线程。
//! 之前直接 `process::exit(0)`：工作线程被腰斩（最多丢一个落盘间隔的内存增量，靠 journal
//! 重放兜底），且 exit 若卡在 C runtime flush 就表现为「STOP 30s 无响应」。
//! 关键节点全部落 `service-win.log`（ProgramData\FindX），为历史上无证据的 ExitCode 1067
//! 留下证据链：下次启动失败时看日志断在哪一步。

use clap::Parser;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tracing::error;
use windows_service::{
    define_windows_service,
    service::{
        ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
        ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
    service_manager::{ServiceManager, ServiceManagerAccess},
};

use crate::cli::Cli;

/// 与 `create_service` / `dispatcher::start` 一致的服务名。
pub const SERVICE_NAME: &str = "FindX2Search";

/// 分发层日志：service_main 各关键节点 append 一行，退出/崩溃时最后几行即现场。
pub(crate) fn append_service_log(msg: &str) {
    svc_log(msg);
}

fn svc_log(msg: &str) {
    #[cfg(test)]
    { let _ = msg; return; }
    #[cfg(not(test))]
    svc_log_production(msg);
}

#[cfg(not(test))]
fn svc_log_production(msg: &str) {
    let dir = match std::env::var_os("ProgramData") {
        Some(pd) => PathBuf::from(pd).join("FindX"),
        None => return,
    };
    let _ = std::fs::create_dir_all(&dir);
    let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let line = format!("[{stamp}] {msg}\n");
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("service-win.log"))
        .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
}

/// 上报服务状态（失败只记日志，不中断——状态机收尾尽力而为）。
fn set_status(
    handle: &windows_service::service_control_handler::ServiceStatusHandle,
    state: ServiceState,
    controls: ServiceControlAccept,
    wait_hint: Duration,
    exit_code: ServiceExitCode,
) {
    set_status_checkpoint(handle, state, controls, wait_hint, exit_code, 0);
}

fn set_status_checkpoint(
    handle: &windows_service::service_control_handler::ServiceStatusHandle,
    state: ServiceState, controls: ServiceControlAccept, wait_hint: Duration,
    exit_code: ServiceExitCode, checkpoint: u32,
) {
    let r = handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: controls,
        exit_code,
        checkpoint,
        wait_hint,
        process_id: None,
    });
    if let Err(e) = r {
        error!("set_service_status({state:?}): {e}");
    }
}

define_windows_service!(ffi_service_main, service_main_impl);

pub fn dispatch() -> anyhow::Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    Ok(())
}

fn service_main_impl(_arguments: Vec<OsString>) {
    svc_log("service_main 进入");
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            svc_log(&format!("服务入口解析参数失败: {e}"));
            error!("服务入口解析参数失败: {e}");
            return;
        }
    };

    let stop = Arc::new(AtomicBool::new(false));
    let stop_cb = stop.clone();

    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                svc_log("收到 STOP/SHUTDOWN 控制码");
                stop_cb.store(true, Ordering::SeqCst);
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle = match service_control_handler::register(SERVICE_NAME, event_handler) {
        Ok(h) => h,
        Err(e) => {
            svc_log(&format!("register service handler 失败: {e}"));
            error!("register service handler: {e}");
            return;
        }
    };

    set_status(
        &status_handle,
        ServiceState::Running,
        ServiceControlAccept::STOP,
        Duration::default(),
        ServiceExitCode::Win32(0),
    );
    svc_log("状态=Running 已上报");

    let idx = cli.index.clone();
    let vol = cli.volume.clone();
    let pipe = cli.pipe.clone();
    let save = cli.save_interval_secs;
    let full_stat = cli.full_stat;
    let max_scan_threads = cli.max_scan_threads;
    let flags = crate::run::RunFlags {
        no_everything_ipc: cli.no_everything_ipc,
        no_backfill: cli.no_backfill,
        no_watch: cli.no_watch,
        usage_memory_mib: cli.usage_memory_mib,
        shutdown_file: cli.shutdown_file.clone(),
        fixture_events: None,
        extra_excluded_dirs: cli.exclude_dir.clone(),
        fold_policy: crate::run::FoldPolicy::from_cli(
            cli.fold_size_mb,
            cli.fold_interval_min,
            cli.fold_dirty_entries,
        ),
    };
    let runner = {
        let stop_runner = stop.clone();
        std::thread::spawn(move || {
            let result = crate::run::run_foreground(
                idx,
                vol,
                pipe,
                save,
                full_stat,
                max_scan_threads,
                flags,
                stop_runner,
            );
            if let Err(e) = &result {
                svc_log(&format!("run_foreground 失败: {e:#}"));
                error!("run_foreground: {e}");
            }
            svc_log("run_foreground 线程返回");
            result
        })
    };
    svc_log("run_foreground 线程已启动");

    // 主循环：等 STOP，同时监测工作线程自行退出（启动失败等异常路径也要结束状态机）。
    while !stop.load(Ordering::SeqCst) {
        if runner.is_finished() {
            svc_log("工作线程已自行退出（未收到 STOP）——上报 STOPPED");
            set_status(
                &status_handle,
                ServiceState::Stopped,
                ServiceControlAccept::empty(),
                Duration::default(),
                runner_exit_code(runner.join()),
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(300));
    }

    // STOP is a request to finish, not permission to force termination or claim
    // durability. Renew SCM progress while retaining the real worker JoinHandle.
    let mut checkpoint = 1u32;
    loop {
        set_status_checkpoint(
            &status_handle, ServiceState::StopPending, ServiceControlAccept::empty(),
            Duration::from_secs(10), ServiceExitCode::Win32(0), checkpoint,
        );
        if runner.is_finished() { break; }
        checkpoint = checkpoint.saturating_add(1);
        std::thread::sleep(Duration::from_secs(1));
    }
    let exit_code = runner_exit_code(runner.join());
    set_status(&status_handle, ServiceState::Stopped, ServiceControlAccept::empty(),
        Duration::default(), exit_code);
}

fn runner_exit_code(result: std::thread::Result<anyhow::Result<()>>) -> ServiceExitCode {
    match result {
        Ok(Ok(())) => ServiceExitCode::Win32(0),
        Ok(Err(e)) => {
            svc_log(&format!("worker failed; SCM service-specific exit=1; NO durable receipt: {e:#}"));
            ServiceExitCode::ServiceSpecific(1)
        }
        Err(_) => {
            svc_log("worker panicked; SCM service-specific exit=2; NO durable receipt");
            ServiceExitCode::ServiceSpecific(2)
        }
    }
}

#[cfg(test)]
mod shutdown_tests {
    use super::*;
    #[test]
    fn shutdown_runner_result_maps_only_real_success_to_zero() {
        assert!(matches!(runner_exit_code(Ok(Ok(()))), ServiceExitCode::Win32(0)));
        assert!(matches!(runner_exit_code(Ok(Err(anyhow::anyhow!("owned fixture write error")))), ServiceExitCode::ServiceSpecific(1)));
        assert!(matches!(runner_exit_code(Err(Box::new("owned fixture panic"))), ServiceExitCode::ServiceSpecific(2)));
    }

}

pub fn install(
    index: std::path::PathBuf,
    volume: String,
    pipe: String,
    save_interval_secs: u64,
    fold_size_mb: u64,
    fold_interval_min: u64,
    fold_dirty_entries: usize,
    full_stat: bool,
    max_scan_threads: usize,
    no_everything_ipc: bool,
    no_backfill: bool,
    exclude_dir: Vec<String>,
) -> anyhow::Result<()> {
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .map_err(|e| anyhow::anyhow!("{}", e))?;

    let exe = std::env::current_exe()?;

    let mut launch_arguments = vec![
        OsString::from("--service"),
        OsString::from("--index"),
        index.as_os_str().to_owned(),
        OsString::from("--volume"),
        OsString::from(volume),
        OsString::from("--pipe"),
        OsString::from(pipe),
        OsString::from("--save-interval-secs"),
        OsString::from(format!("{save_interval_secs}")),
    ];
    // Fold knobs only when non-default (keeps the SCM command line readable;
    // service defaults match the CLI defaults).
    if fold_size_mb != 256 {
        launch_arguments.push(OsString::from("--fold-size-mb"));
        launch_arguments.push(OsString::from(format!("{fold_size_mb}")));
    }
    if fold_interval_min != 120 {
        launch_arguments.push(OsString::from("--fold-interval-min"));
        launch_arguments.push(OsString::from(format!("{fold_interval_min}")));
    }
    if fold_dirty_entries != 1_000_000 {
        launch_arguments.push(OsString::from("--fold-dirty-entries"));
        launch_arguments.push(OsString::from(format!("{fold_dirty_entries}")));
    }
    if full_stat {
        launch_arguments.push(OsString::from("--full-stat"));
    }
    if max_scan_threads != 4 {
        launch_arguments.push(OsString::from("--max-scan-threads"));
        launch_arguments.push(OsString::from(format!("{max_scan_threads}")));
    }
    if no_everything_ipc {
        launch_arguments.push(OsString::from("--no-everything-ipc"));
    }
    if no_backfill {
        launch_arguments.push(OsString::from("--no-backfill"));
    }
    for dir in &exclude_dir {
        if dir.trim().is_empty() {
            continue;
        }
        launch_arguments.push(OsString::from("--exclude-dir"));
        launch_arguments.push(OsString::from(dir));
    }

    let service_info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from("FindX2 Search Index"),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe,
        launch_arguments,
        dependencies: vec![],
        account_name: None,
        account_password: None,
    };

    // 升级安装时旧服务常处于「标记删除」，SCM 要等进程/句柄释放才让重建。
    let mut last_err = None;
    for attempt in 0..20 {
        match manager.create_service(&service_info, ServiceAccess::QUERY_STATUS) {
            Ok(_) => {
                tracing::info!("已注册服务 {SERVICE_NAME}，请使用 services.msc 或 sc start 启动");
                return Ok(());
            }
            Err(e) => {
                tracing::warn!("注册服务第 {} 次失败: {e}", attempt + 1);
                last_err = Some(e);
                let _ = try_delete_existing_service();
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    }
    Err(anyhow::anyhow!(
        "注册服务 {SERVICE_NAME} 失败: {}",
        last_err
            .map(|e| e.to_string())
            .unwrap_or_else(|| "未知错误".into())
    ))
}

fn try_delete_existing_service() -> anyhow::Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let service = manager
        .open_service(
            SERVICE_NAME,
            ServiceAccess::DELETE | ServiceAccess::STOP | ServiceAccess::QUERY_STATUS,
        )
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let _ = service.stop();
    service.delete().map_err(|e| anyhow::anyhow!("{}", e))?;
    Ok(())
}

pub fn uninstall() -> anyhow::Result<()> {
    match try_delete_existing_service() {
        Ok(()) => {
            tracing::info!("已标记删除服务 {SERVICE_NAME}（停止后生效）");
            Ok(())
        }
        Err(e) => {
            tracing::info!("卸载服务 {SERVICE_NAME}：{e}（可能尚未注册）");
            Ok(())
        }
    }
}
