//! Windows 服务安装、卸载与入口调度。

use clap::Parser;
use std::ffi::OsString;
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

define_windows_service!(ffi_service_main, service_main_impl);

pub fn dispatch() -> anyhow::Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    Ok(())
}

fn service_main_impl(_arguments: Vec<OsString>) {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            error!("服务入口解析参数失败: {e}");
            return;
        }
    };

    let stop = Arc::new(AtomicBool::new(false));
    let stop_cb = stop.clone();

    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            ServiceControl::Stop | ServiceControl::Shutdown => {
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
            error!("register service handler: {e}");
            return;
        }
    };

    let _ = status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    });

    let idx = cli.index.clone();
    let vol = cli.volume.clone();
    let pipe = cli.pipe.clone();
    let save = cli.save_interval_secs;
    let full_stat = cli.full_stat;
    let max_scan_threads = cli.max_scan_threads;
    let flags = crate::run::RunFlags {
        no_everything_ipc: cli.no_everything_ipc,
        no_backfill: cli.no_backfill,
        extra_excluded_dirs: cli.exclude_dir.clone(),
    };
    std::thread::spawn(move || {
        if let Err(e) = crate::run::run_foreground(
            idx,
            vol,
            pipe,
            save,
            full_stat,
            max_scan_threads,
            flags,
        ) {
            error!("run_foreground: {e}");
        }
    });

    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(300));
    }

    std::process::exit(0);
}

pub fn install(
    index: std::path::PathBuf,
    volume: String,
    pipe: String,
    save_interval_secs: u64,
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
