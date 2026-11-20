//! findx2 常驻服务：Windows 命名管道 / Unix socket；默认前台运行。

mod cli;
mod everything_status;
mod ipc_dispatch;
mod load_state;
mod startup_compact;
mod shutdown;
mod runtime_lifecycle;
mod usage_fixture;
mod watch_health;

mod backfill;
mod metadata_pipeline;
#[cfg(windows)]
mod everything_ipc;
#[cfg(windows)]
mod pipe_server;
#[cfg(windows)]
mod run;
#[cfg(windows)]
mod session_spawn;
#[cfg(windows)]
mod win_service;

#[cfg(unix)]
mod run_unix;
#[cfg(unix)]
mod unix_server;

use clap::Parser;

/// 与 GUI 超时提示一致：进程启动失败或 run_foreground 返回 Err 时写入，便于无控制台时排查。
pub(crate) const SERVICE_LAST_ERROR_FILENAME: &str = "findx2-service-last-error.txt";

// Isolated foreground fixtures must not write the formal service diagnostic file,
// including parse/startup failures before RunFlags exist.
fn startup_error_path() -> std::path::PathBuf {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--no-watch" || a == "--fixture-root") {
        if let Some(i) = args.iter().position(|a| a == "--index" || a == "-i") {
            if let Some(index) = args.get(i + 1) { return std::path::PathBuf::from(index).with_extension("last-error.txt"); }
        }
        return std::env::temp_dir().join(format!("findx2-fixture-{}-last-error.txt", std::process::id()));
    }
    std::env::temp_dir().join(SERVICE_LAST_ERROR_FILENAME)
}

fn main() {
    if let Err(e) = try_main() {
        let path = startup_error_path();
        let text = format!("{e:#}\n");
        let _ = std::fs::write(&path, &text);
        eprintln!(
            "findx2-service 启动失败: {e:#}\n（详情已写入 {}）",
            path.display()
        );
        std::process::exit(1);
    }
}

struct LocalTimer;
impl tracing_subscriber::fmt::time::FormatTime for LocalTimer {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        write!(w, "{}", chrono::Local::now().format("%H:%M:%S%.3f"))
    }
}

fn try_main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .with_timer(LocalTimer)
        .with_target(false)
        .init();

    let cli = match cli::Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            let path = startup_error_path();
            let text = format!("{e}");
            let _ = std::fs::write(&path, &text);
            e.exit();
        }
    };

    #[cfg(windows)]
    {
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

        if cli.everything_host {
            return everything_ipc::run_everything_host_via_pipe(cli.pipe);
        }

        if cli.service {
            return win_service::dispatch();
        }

        if let Some(root) = &cli.fixture_root {
            if cli.service || !cli.no_watch || !cli.no_backfill || !cli.no_everything_ipc {
                anyhow::bail!(
                    "fixture requires foreground --no-watch --no-backfill --no-everything-ipc"
                );
            }
            usage_fixture::create_index(root, &cli.index)?;
        }
        // Strict fixtures must not clear the formal service's shared diagnostic file.
        if !cli.no_watch {
            let _ = std::fs::remove_file(std::env::temp_dir().join(SERVICE_LAST_ERROR_FILENAME));
        }

        return run::run_foreground(
            cli.index,
            cli.volume,
            cli.pipe,
            cli.save_interval_secs,
            cli.full_stat,
            cli.max_scan_threads,
            run::RunFlags {
                no_everything_ipc: cli.no_everything_ipc,
                no_backfill: cli.no_backfill,
                no_watch: cli.no_watch,
                usage_memory_mib: cli.usage_memory_mib,
                shutdown_file: cli.shutdown_file.clone(),
                fixture_events: cli.fixture_events.clone(),
                extra_excluded_dirs: cli.exclude_dir,
            },
            // 控制台模式：无 Ctrl+C 优雅停（journal 重放兜底），传永不置位标志对齐签名。
            run::never_stop_flag(),
        );
    }

    #[cfg(unix)]
    {
        if cli.cmd.is_some() {
            anyhow::bail!("install/uninstall 仅支持 Windows 服务");
        }
        // Strict fixtures must not clear the formal service's shared diagnostic file.
        if !cli.no_watch {
            let _ = std::fs::remove_file(std::env::temp_dir().join(SERVICE_LAST_ERROR_FILENAME));
        }
        return run_unix::run_foreground(
            cli.index,
            run_unix::normalize_unix_volume(&cli.volume),
            cli.pipe,
            cli.save_interval_secs,
            cli.exclude_dir,
            cli.full_stat,
            cli.no_backfill,
            cli.max_scan_threads,
        );
    }

    #[cfg(not(any(windows, unix)))]
    {
        let _ = cli;
        anyhow::bail!("findx2-service 不支持当前平台");
    }
}
