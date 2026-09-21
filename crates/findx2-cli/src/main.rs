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
        /// 启用拼音（默认开启，与 GUI 一致；`--pinyin=false` 关闭）
        #[arg(long, default_value_t = true)]
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
    /// 压缩索引：物理移除墓碑条目（删除 / 排除 / 卷重建留下的「只标记不删除」残留），
    /// 重建全部下标并原子落盘。墓碑比高时（`status` 会提示）用它回收空间与加载时间。
    Compact {
        /// index.bin 路径
        #[arg(short, long, default_value = "index.bin")]
        index: std::path::PathBuf,
        /// 只报告墓碑情况，不实际压缩
        #[arg(long, default_value_t = false)]
        dry_run: bool,
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
        #[arg(long, default_value_t = true)]
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

            #[cfg(target_os = "macos")]
            {
                let _ = progress_file;
                let roots = match (volumes, volume) {
                    (Some(vs), _) if !vs.is_empty() => vs
                        .into_iter()
                        .filter(|s| {
                            let t = s.trim();
                            !t.is_empty() && !t.eq_ignore_ascii_case("C:") && t != r"C:\"
                        })
                        .collect(),
                    (_, Some(v))
                        if !v.trim().is_empty()
                            && !v.eq_ignore_ascii_case("C:")
                            && v != r"C:\" =>
                    {
                        vec![v]
                    }
                    _ => Vec::new(),
                };
                findx2_macos::build_full_disk_index(
                    &output,
                    roots,
                    exclude_dir,
                    full_stat,
                    max_scan_threads,
                )?;
            }

            #[cfg(target_os = "linux")]
            {
                let _ = progress_file;
                let roots = match (volumes, volume) {
                    (Some(vs), _) if !vs.is_empty() => vs,
                    (_, Some(v)) => vec![v],
                    _ => Vec::new(),
                };
                findx2_linux::build_full_disk_index(
                    &output,
                    roots,
                    exclude_dir,
                    full_stat,
                    max_scan_threads,
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
            let tomb = store.deleted.len();
            println!(
                "墓碑数: {tomb}（{:.2}%）{}",
                store.tombstone_ratio() * 100.0,
                if store.should_compact() {
                    " ← 建议执行 `findx2 compact`"
                } else {
                    ""
                }
            );
            if let Some(v) = store.volumes.first() {
                println!(
                    "卷 letter={} id={} prefix={} serial={} journal_id={} last_usn={}",
                    v.volume_letter as char,
                    v.volume_id,
                    v.root_prefix,
                    v.volume_serial,
                    v.usn_journal_id,
                    v.last_usn
                );
            }
        }
        Commands::Compact { index, dry_run } => {
            let t0 = std::time::Instant::now();
            let mut store = load_index_bin(&index)?;
            let n_before = store.entry_count();
            let tomb = store.deleted.len() as usize;
            let bytes_before = std::fs::metadata(&index).map(|m| m.len()).unwrap_or(0);

            println!("压缩前: {n_before} 条目，其中墓碑 {tomb}（{:.2}%）",
                store.tombstone_ratio() * 100.0);
            println!("文件大小: {:.2} GB", bytes_before as f64 / 1073741824.0);

            if dry_run {
                println!("--dry-run：不实际压缩。");
                return Ok(());
            }
            if tomb == 0 {
                println!("没有墓碑，无需压缩。");
                return Ok(());
            }

            let removed = store.compact_tombstones();
            println!(
                "已移除 {removed} 条墓碑 → 剩余 {} 条目（耗时 {:.1}s）",
                store.entry_count(),
                t0.elapsed().as_secs_f64()
            );

            // 灾难性损失守卫：压缩只该删墓碑，存活条目必须原样保留。
            // 若剩余条目数明显少于「压缩前 - 墓碑数」，说明保留集算法出了偏差
            // （历史上就发生过一次区间推断把整库当墓碑清空的事故），
            // 此时**绝不落盘**，原文件保持不动。
            let expected_survivors = n_before.saturating_sub(tomb);
            let actual = store.entry_count();
            if actual < expected_survivors {
                return Err(findx2_core::Error::Persist(format!(
                    "压缩结果异常：预期保留 {expected_survivors} 条（{n_before} - {tomb} 墓碑），\
                     实际只剩 {actual} 条。已放弃落盘，原索引未改动。这是 bug，请反馈。"
                )));
            }

            // 原子落盘：先写同目录临时文件，再 rename 覆盖。避免压缩中途崩溃留下半截索引。
            let dir = index.parent().unwrap_or_else(|| std::path::Path::new("."));
            let tmp = dir.join(format!(
                "{}.compact.tmp",
                index
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "index.bin".into())
            ));
            save_index_bin(&tmp, &store)?;
            std::fs::rename(&tmp, &index)?;

            // trigram 边车的 posting 用的是旧下标，压缩后必须重建（否则剪枝会漏报）。
            store.trigram = None;
            match findx2_core::build_trigram_sidecar(&store, &index) {
                Ok(()) => println!("trigram 边车已重建。"),
                // 剪枝层缺失只影响性能，搜索会回退全表扫描，不阻断压缩结果。
                Err(e) => println!("trigram 边车重建失败（搜索将回退全表扫描）: {e}"),
            }

            let bytes_after = std::fs::metadata(&index).map(|m| m.len()).unwrap_or(0);
            println!(
                "压缩后: {} 条目，文件 {:.2} GB（回收 {:.2} GB）",
                store.entry_count(),
                bytes_after as f64 / 1073741824.0,
                (bytes_before.saturating_sub(bytes_after)) as f64 / 1073741824.0
            );
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
                            // CreatePending 在 service 侧由后台补 meta；CLI 前台watch 保持旧语义：
                            // 新建条目立刻同步拉一次 meta（CLI 本来就是管理员+前台，无后台线程）。
                            if let findx2_core::ChangeEvent::CreatePending {
                                file_id,
                                file_id_128,
                                ..
                            } = &ev
                            {
                                store.apply_change_event(&ev)?;
                                sync_fetch_one(&volume, *file_id, *file_id_128, &mut store);
                            } else {
                                store.apply_change_event(&ev)?;
                            }
                        }
                        findx2_windows::UsnWatchMsg::StatRefresh { file_id, file_id_128 } => {
                            sync_fetch_one(&volume, file_id, file_id_128, &mut store);
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
                                maybe_rebuild_trigram_cli(&mut store, &index);
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
                            maybe_rebuild_trigram_cli(&mut store, &index);
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

/// `watch` 调试模式：trigram 增量超阈值时同步重建边车（watch 是单线程串行，
/// 构建期间无并发 apply，构建完成后 pending 可直接清零）。
/// CLI watch：同步拉一条 meta 并写回（service 侧走后台 worker，这里前台直接拉，
/// 保持与旧行为一致的完整 meta）。
#[cfg(windows)]
fn sync_fetch_one(
    volume: &str,
    file_id: u64,
    file_id_128: Option<[u8; 16]>,
    store: &mut findx2_core::IndexStore,
) {
    let dev = if volume.starts_with(r"\\.\") {
        volume.to_string()
    } else {
        format!(
            r"\\.\{}:",
            volume.trim_end_matches([':', '\\'])
        )
    };
    let frns = [file_id];
    let ids = [file_id_128];
    let idx = [0usize];
    let updates =
        findx2_windows::fill_metadata_by_id_pooled(&dev, &frns, &ids, &idx, None, None);
    for (_, size, mt, ct) in updates {
        let _ = store.apply_change_event(&findx2_core::ChangeEvent::DataOrMeta {
            file_id,
            size: Some(size),
            mtime: Some(mt),
            ctime: Some(ct),
        });
    }
}

fn maybe_rebuild_trigram_cli(store: &mut findx2_core::IndexStore, index: &std::path::Path) {
    if !store.tri_pending_overflow() {
        return;
    }
    match findx2_core::build_trigram_sidecar(store, index) {
        Ok(()) => {
            if let Some(t) = findx2_core::TrigramIndex::load(&findx2_core::tri_sidecar_path(index))
                .ok()
                .flatten()
            {
                store.trigram = Some(std::sync::Arc::new(t));
                store.tri_pending.clear();
            }
        }
        Err(e) => eprintln!("[findx2] trigram 重建失败: {e}"),
    }
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
