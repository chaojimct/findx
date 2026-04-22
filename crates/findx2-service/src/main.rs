//! findx2 常驻服务：`install` | `uninstall` | `--service`（SCM）| 默认前台。

#[cfg(not(windows))]
fn main() {
    eprintln!("findx2-service 仅支持 Windows");
    std::process::exit(1);
}

#[cfg(windows)]
mod cli;
#[cfg(windows)]
mod everything_ipc;
#[cfg(windows)]
mod pipe_server;
#[cfg(windows)]
mod run;
#[cfg(windows)]
mod backfill;
#[cfg(windows)]
mod win_service;

#[cfg(windows)]
use clap::Parser;

/// 与 GUI 超时提示一致：进程启动失败或 run_foreground 返回 Err 时写入，便于无控制台时排查。
#[cfg(windows)]
pub(crate) const SERVICE_LAST_ERROR_FILENAME: &str = "findx2-service-last-error.txt";

#[cfg(windows)]
fn main() {
    if let Err(e) = try_main() {
        let path = std::env::temp_dir().join(SERVICE_LAST_ERROR_FILENAME);
        let text = format!("{e:#}\n");
        let _ = std::fs::write(&path, &text);
        eprintln!("findx2-service 启动失败: {e:#}\n（详情已写入 {}）", path.display());
        std::process::exit(1);
    }
}

#[cfg(windows)]
struct LocalTimer;
#[cfg(windows)]
impl tracing_subscriber::fmt::time::FormatTime for LocalTimer {
    fn format_time(
        &self,
        w: &mut tracing_subscriber::fmt::format::Writer<'_>,
    ) -> std::fmt::Result {
        write!(w, "{}", chrono::Local::now().format("%H:%M:%S%.3f"))
    }
}

#[cfg(windows)]
fn try_main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .with_timer(LocalTimer)
        .with_target(false)
        .init();

    let cli = cli::Cli::parse();

    if matches!(cli.cmd, Some(cli::ServiceCmd::Install)) {
        return win_service::install(
            cli.index.clone(),
            cli.volume.clone(),
            cli.pipe.clone(),
            cli.save_interval_secs,
            cli.full_stat,
            cli.max_scan_threads,
            cli.no_everything_ipc,
            cli.no_backfill,
            cli.exclude_dir.clone(),
        );
    }
    if matches!(cli.cmd, Some(cli::ServiceCmd::Uninstall)) {
        return win_service::uninstall();
    }

    if cli.service {
        return win_service::dispatch();
    }

    let _ = std::fs::remove_file(std::env::temp_dir().join(SERVICE_LAST_ERROR_FILENAME));

    run::run_foreground(
        cli.index,
        cli.volume,
        cli.pipe,
        cli.save_interval_secs,
        cli.full_stat,
        cli.max_scan_threads,
        run::RunFlags {
            no_everything_ipc: cli.no_everything_ipc,
            no_backfill: cli.no_backfill,
            extra_excluded_dirs: cli.exclude_dir,
        },
    )
}
