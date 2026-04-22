//! findx2 命令行入口。

use clap::{Parser, Subcommand, ValueEnum};
use findx2_core::{
    ParsedQuery, QueryParser, Result, SearchEngine, SearchHit, SearchOptions, load_index_bin,
    save_index_bin,
};
#[cfg(windows)]
mod remote;
#[cfg(windows)]
use std::sync::mpsc;
#[cfg(windows)]
use std::time::{Duration, Instant};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "findx2", version, about = "findx2 — 高速文件索引搜索")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 扫描卷并构建索引（未指定 -v/--volumes 时默认枚举本机全部固定盘与可移动盘）
    Index {
        /// 卷，如 `C:` 或 `C:\`（与 --volumes 二选一）
        #[arg(short, long)]
        volume: Option<String>,
        /// 多卷，如 `C:,D:`；与 -v 二选一。省略时由程序自动枚举本地卷
        #[arg(long, value_delimiter = ',', alias = "volumes")]
        volumes: Option<Vec<String>>,
        /// 输出 index.bin 路径
        #[arg(short, long, default_value = "index.bin")]
        output: std::path::PathBuf,
        /// 首遍全量读 $MFT 元数据与 OpenFileById（较慢；时间与大小筛选一上来即准）
        #[arg(long, default_value_t = false)]
        full_stat: bool,
        /// 并行扫描的最大卷线程数（仅多卷生效）
        #[arg(long, default_value_t = 4)]
        max_scan_threads: usize,
        /// 建库进度 JSON 文件路径（供 GUI 轮询；默认与 --output 同目录，扩展名为 .indexing.json）
        #[arg(long)]
        progress_file: Option<std::path::PathBuf>,
        /// 排除目录（可重复传入；写为完整路径，例如 `--exclude-dir C:\Windows\WinSxS`）。
        /// 对已扫到的条目打"已删除"墓碑，并写入 `<output>.exclude.json` 边车供 service 增量复用。
        #[arg(long = "exclude-dir", value_name = "PATH")]
        exclude_dir: Vec<String>,
    },
    /// 在已加载索引上搜索
    Search {
        /// index.bin 路径
        #[arg(short, long, default_value = "index.bin")]
        index: std::path::PathBuf,
        /// 查询字符串
        query: String,
        #[arg(long)]
        json: bool,
        /// 启用拼音（需构建时启用 findx2-core 的 `pinyin` feature）
        #[arg(long, default_value_t = false)]
        pinyin: bool,
        /// 输出列（默认全部）
        #[arg(long, value_delimiter = ',', alias = "cols")]
        columns: Option<Vec<OutColumn>>,
    },
    /// 显示索引元信息
    Status {
        #[arg(short, long, default_value = "index.bin")]
        index: std::path::PathBuf,
    },
    /// 连接 findx2-service 命名管道搜索（失败且指定 `--index` 时回退本地索引）
    #[cfg(windows)]
    Remote {
        /// 管道名（默认 findx2，实际路径 \\\\.\\pipe\\findx2）
        #[arg(long)]
        pipe: Option<String>,
        /// 离线回退：`index.bin` 路径（管道不可用时使用）
        #[arg(short, long)]
        index: Option<std::path::PathBuf>,
        /// 查询字符串
        query: String,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = false)]
        pinyin: bool,
        #[arg(long, default_value_t = 500)]
        limit: usize,
        #[arg(long, value_delimiter = ',', alias = "cols")]
        columns: Option<Vec<OutColumn>>,
    },
    /// 加载索引并轮询 USN 增量，周期性将 `last_usn` 写回 index.bin（别名 `daemon`）
    #[cfg(windows)]
    #[command(alias = "daemon")]
    Watch {
        #[arg(short, long, default_value = "index.bin")]
        index: std::path::PathBuf,
        /// 卷，如 `C:`（须与建索引时一致）
        #[arg(short, long, default_value = "C:")]
        volume: String,
        /// 落盘间隔（秒）
        #[arg(long, default_value_t = 30)]
        save_interval_secs: u64,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum OutColumn {
    Name,
    Path,
    Size,
    Modified,
}

/// CLI / Service 终端日志的本地时间格式：`HH:MM:SS.mmm`，与 `findx2_core::progress!` 对齐。
struct LocalTimer;
impl tracing_subscriber::fmt::time::FormatTime for LocalTimer {
    fn format_time(
        &self,
        w: &mut tracing_subscriber::fmt::format::Writer<'_>,
    ) -> std::fmt::Result {
        write!(w, "{}", chrono::Local::now().format("%H:%M:%S%.3f"))
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_timer(LocalTimer)
        .with_target(false)
        .init();

    if let Err(e) = run() {
        eprintln!("错误: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Index {
            volume,
            volumes,
            output,
            full_stat,
            max_scan_threads,
            progress_file,
            exclude_dir,
        } => {
            #[cfg(not(windows))]
            {
                let _ = (
                    volume,
                    volumes,
                    output,
                    full_stat,
                    max_scan_threads,
                    progress_file,
                    exclude_dir,
                );
                return Err(findx2_core::Error::Platform(
                    "`findx2 index` 需要 Windows 与管理员权限".into(),
                ));
            }

            #[cfg(windows)]
            {
                findx2_windows::build_full_disk_index(
                    &output,
                    full_stat,
                    max_scan_threads,
                    progress_file.as_deref(),
                    volume,
                    volumes,
                    exclude_dir,
                )?;
            }
        }
        Commands::Search {
            index,
            query,
            json,
            pinyin,
            columns,
        } => {
            let t_load = std::time::Instant::now();
            let store = load_index_bin(&index)?;
            let load_ms = t_load.elapsed().as_millis();
            let entries_n = store.entries.len();
            let t_parse = std::time::Instant::now();
            let pq: ParsedQuery = QueryParser::parse(&query)?;
            let parse_us = t_parse.elapsed().as_micros();
            let engine = SearchEngine::new(store);
            // 预热一次，避免线程池/页缓存冷启动算到第一次的耗时里。
            let _ = engine.search(&pq, &SearchOptions { allow_pinyin: pinyin, ..Default::default() })?;
            let mut samples: Vec<u128> = Vec::with_capacity(5);
            let mut last_total: u32 = 0;
            let mut last_hits_len: usize = 0;
            for _ in 0..5 {
                let t = std::time::Instant::now();
                let (h, total) = engine.search(
                    &pq,
                    &SearchOptions { allow_pinyin: pinyin, ..Default::default() },
                )?;
                samples.push(t.elapsed().as_micros());
                last_total = total;
                last_hits_len = h.len();
            }
            let avg = samples.iter().sum::<u128>() / samples.len() as u128;
            let min = *samples.iter().min().unwrap();
            let max = *samples.iter().max().unwrap();
            eprintln!(
                "[bench] entries={} load={}ms parse={}μs · search 5 runs avg {:.2}ms (min {:.2} max {:.2}) hits={} total={}",
                entries_n,
                load_ms,
                parse_us,
                avg as f64 / 1000.0,
                min as f64 / 1000.0,
                max as f64 / 1000.0,
                last_hits_len,
                last_total,
            );
            let (hits, total) = engine.search(
                &pq,
                &SearchOptions {
                    allow_pinyin: pinyin,
                    ..Default::default()
                },
            )?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "total": total,
                        "hits": hits,
                    }))?
                );
            } else {
                let cols = columns.unwrap_or_else(|| {
                    vec![
                        OutColumn::Name,
                        OutColumn::Path,
                        OutColumn::Size,
                        OutColumn::Modified,
                    ]
                });
                print_table(&hits, &cols);
            }
        }
        #[cfg(windows)]
        Commands::Remote {
            pipe,
            index,
            query,
            json,
            pinyin,
            limit,
            columns,
        } => {
            let pipe_name = pipe.as_deref().unwrap_or("findx2");
            let hits: Vec<SearchHit> = match remote::remote_search_blocking(pipe_name, &query, pinyin, limit) {
                Ok(dtos) => dtos
                    .into_iter()
                    .map(|d| SearchHit {
                        entry_idx: d.entry_idx,
                        name: d.name,
                        path: d.path,
                        size: d.size,
                        mtime: d.mtime,
                        name_highlight: d.name_highlight,
                    })
                    .collect(),
                Err(e) => {
                    if let Some(idx) = index {
                        eprintln!("连接服务失败（{e}），回退本地索引 …");
                        let store = load_index_bin(&idx)?;
                        let mut pq: ParsedQuery = QueryParser::parse(&query)?;
                        pq.limit = limit.min(8192) as u32;
                        let engine = SearchEngine::new(store);
                        let (h, _t) = engine.search(
                            &pq,
                            &SearchOptions {
                                allow_pinyin: pinyin,
                                ..Default::default()
                            },
                        )?;
                        h
                    } else {
                        return Err(e);
                    }
                }
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&hits)?);
            } else {
                let cols = columns.unwrap_or_else(|| {
                    vec![
                        OutColumn::Name,
                        OutColumn::Path,
                        OutColumn::Size,
                        OutColumn::Modified,
                    ]
                });
                print_table(&hits, &cols);
            }
        }
        Commands::Status { index } => {
            let store = load_index_bin(&index)?;
            println!("条目数: {}", store.entry_count());
            println!("目录数: {}", store.dirs.len());
            if let Some(v) = store.volumes.first() {
                println!(
                    "卷 {} serial={} journal_id={} last_usn={}",
                    v.volume_letter as char, v.volume_serial, v.usn_journal_id, v.last_usn
                );
            }
        }
        #[cfg(windows)]
        Commands::Watch {
            index,
            volume,
            save_interval_secs,
        } => {
            let mut store = load_index_bin(&index)?;
            let vol = store
                .volumes
                .first()
                .ok_or_else(|| findx2_core::Error::Platform("索引中无卷元数据".into()))?
                .clone();
            let resume = findx2_windows::UsnResume {
                journal_id: vol.usn_journal_id,
                start_usn: vol.last_usn,
            };
            let (tx, rx) = mpsc::channel::<findx2_windows::UsnWatchMsg>();
            let vol_path = volume.clone();
            let worker = std::thread::spawn(move || {
                findx2_windows::usn_watch_forever(&vol_path, Some(resume), tx)
            });
            let save_every = Duration::from_secs(save_interval_secs.max(1));
            let mut last_save = Instant::now();
            println!(
                "开始监听 {volume} ，从 journal_id={} last_usn={} 续跑；每 {:?} 落盘",
                vol.usn_journal_id,
                vol.last_usn,
                save_every
            );
            loop {
                match rx.recv_timeout(Duration::from_secs(1)) {
                    Ok(msg) => match msg {
                        findx2_windows::UsnWatchMsg::Event(ev) => {
                            store.apply_change_event(&ev)?;
                        }
                        findx2_windows::UsnWatchMsg::Checkpoint {
                            journal_id,
                            next_usn,
                        } => {
                            if let Some(v) = store.volumes.get_mut(0) {
                                v.usn_journal_id = journal_id;
                                v.last_usn = next_usn;
                            }
                            if last_save.elapsed() >= save_every {
                                save_index_bin(&index, &store)?;
                                eprintln!("[findx2] 已保存 checkpoint last_usn={next_usn}");
                                last_save = Instant::now();
                            }
                        }
                    },
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if last_save.elapsed() >= save_every {
                            save_index_bin(&index, &store)?;
                            eprintln!("[findx2] 定时落盘（游标） last_usn={}", {
                                store.volumes.first().map(|v| v.last_usn).unwrap_or(0)
                            });
                            last_save = Instant::now();
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        break;
                    }
                }
                if worker.is_finished() {
                    break;
                }
            }
            match worker.join() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => return Err(e),
                Err(_) => {
                    return Err(findx2_core::Error::Platform(
                        "USN 监听线程异常结束".into(),
                    ));
                }
            }
            save_index_bin(&index, &store)?;
        }
    }
    Ok(())
}

fn print_table(hits: &[findx2_core::SearchHit], cols: &[OutColumn]) {
    if hits.is_empty() {
        return;
    }
    let term_w = std::env::var("COLUMNS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(80);
    let sep: String = std::iter::repeat('—').take(term_w.min(120)).collect();
    println!("{}", sep);

    let mut headers: Vec<&str> = Vec::new();
    for c in cols {
        headers.push(match c {
            OutColumn::Name => "Name",
            OutColumn::Path => "Path",
            OutColumn::Size => "Size",
            OutColumn::Modified => "Date Modified",
        });
    }
    println!("{}", headers.join("\t"));

    for h in hits {
        let mut cells: Vec<String> = Vec::new();
        for c in cols {
            let cell = match c {
                OutColumn::Name => h.name.clone(),
                OutColumn::Path => h.path.clone(),
                OutColumn::Size => format_size(h.size),
                OutColumn::Modified => format_filetime_local(h.mtime),
            };
            cells.push(cell);
        }
        println!("{}", cells.join("\t"));
    }
}

fn format_size(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if n >= GB {
        format!("{:.2} GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.2} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.2} KB", n as f64 / KB as f64)
    } else {
        format!("{n} B")
    }
}

/// Windows FILETIME（100ns）转本地时间显示
fn format_filetime_local(ft: u64) -> String {
    const EPOCH_DIFF: u64 = 11_644_473_600;
    let secs = (ft / 10_000_000).saturating_sub(EPOCH_DIFF);
    use chrono::{Local, TimeZone};
    Local
        .timestamp_opt(secs as i64, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "-".into())
}
