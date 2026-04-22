//! 前台模式：加载索引、USN、命名管道、Everything IPC。

use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::OnceLock;
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use findx2_core::{save_index_bin, QueryParser, SearchEngine, SearchOptions};
use tracing::{error, info};

/// 运行时开关：来自 CLI（`--no-everything-ipc` / `--no-backfill` / `--exclude-dir`）。
/// 抽 struct 而不是继续加位置参数，是因为 `run_foreground` 已经 5 个参数了，再扩会失控。
#[derive(Debug, Clone, Default)]
pub(crate) struct RunFlags {
    pub no_everything_ipc: bool,
    pub no_backfill: bool,
    /// CLI 注入的排除目录（与 sidecar 里的取并集，由 IndexStore 持有运行时副本）。
    pub extra_excluded_dirs: Vec<String>,
}

/// 串行化 `index.bin` 写盘：service 启动时按卷数 spawn 多个 USN watch 线程，
/// 每个线程都按 `save_interval_secs` 周期写**同一份** `index.bin`。旧实现没锁，
/// 三个卷在同一刻同时调 `save_index_bin` 会互相截断把文件写坏，下次启动报
/// `failed to fill whole buffer`。这里用 Mutex 强制串行：拿不到锁的卷直接跳过本轮。
fn persist_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// 前台运行。（非 Windows 下不提供本模块）
pub(crate) fn run_foreground(
    index: PathBuf,
    _volume: String,
    pipe_name: String,
    save_interval_secs: u64,
    full_stat: bool,
    max_scan_threads: usize,
    flags: RunFlags,
) -> anyhow::Result<()> {
    let _ = (full_stat, max_scan_threads);
    if !index.exists() {
        return Err(anyhow::anyhow!(
            "索引文件 {} 不存在。findx2-service 不再自动建库；请先用 `findx2 index --output {}` 建库（建库需管理员权限）后再启动服务。",
            index.display(),
            index.display(),
        ));
    }

    // 关键：管道在 load_index_bin 之前就开起来。
    // 大索引（千万级）反序列化要十几秒甚至几十秒，旧顺序下 GUI / IPC 会一直撞「管道超时」。
    // 现在改为：先挂 EngineSlot（None）→ 起 pipe 线程 → 加载索引 → 注入 Some(engine)。
    let slot: crate::pipe_server::EngineSlot = Arc::new(RwLock::new(None));

    let pipe_path_join = normalize_pipe_path(&pipe_name);
    let slot_pipe = slot.clone();
    let pipe_thread = std::thread::Builder::new()
        .name("findx2-named-pipe".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    error!("tokio runtime 创建失败: {e}");
                    return;
                }
            };
            if let Err(e) = rt.block_on(crate::pipe_server::pipe_accept_loop(
                pipe_path_join,
                slot_pipe,
            )) {
                error!("pipe_accept_loop: {e}");
            }
        })
        .map_err(|e| anyhow::anyhow!("spawn named pipe 线程失败: {e}"))?;

    info!("加载索引 {:?}", index);
    let load_t0 = Instant::now();
    let mut store = findx2_core::load_index_bin(&index)?;
    info!(
        "索引加载完成：{} 条目（耗时 {:.2}s）",
        store.entry_count(),
        load_t0.elapsed().as_secs_f64()
    );

    // 合并 CLI 追加的排除目录到 store；并把命中条目一次性打墓碑，避免「sidecar 没改，CLI 临时加目录」时旧数据漏网。
    if !flags.extra_excluded_dirs.is_empty() {
        let mut union = store.excluded_dirs.clone();
        for d in &flags.extra_excluded_dirs {
            if let Some(n) = findx2_core::normalize_excluded_dir(d) {
                if !union.contains(&n) {
                    union.push(n);
                }
            }
        }
        let marked = store.mark_excluded_entries(&union);
        if marked > 0 {
            info!("CLI 排除目录命中：{} 条历史条目已标记为已删除", marked);
        }
        store.excluded_dirs = union;
    }

    let engine = Arc::new(SearchEngine::new(store));
    {
        let mut g = slot.write().expect("EngineSlot 写锁中毒");
        *g = Some(engine.clone());
    }

    let _everything: Option<JoinHandle<()>> = if flags.no_everything_ipc {
        info!("已通过 --no-everything-ipc 关闭 Everything 兼容窗口（老客户端将无法连接）");
        None
    } else {
        Some(crate::everything_ipc::spawn_everything_ipc(engine.clone()))
    };

    if flags.no_backfill {
        info!("已通过 --no-backfill 关闭后台元数据回填（fast 首遍后 size/mtime 可能为 0）");
    } else {
        crate::backfill::spawn_backfill(engine.clone(), index.clone());
    }

    let volumes_watch: Vec<(String, u8)> = {
        let g = engine.index_store();
        if g.volumes.is_empty() {
            let c = _volume
                .chars()
                .find(|ch| ch.is_ascii_alphabetic())
                .unwrap_or('C')
                .to_ascii_uppercase();
            vec![(
                format!("{}:", c),
                c as u8,
            )]
        } else {
            g.volumes
                .iter()
                .map(|v| {
                    let ch = (v.volume_letter as char).to_ascii_uppercase();
                    (format!("{}:", ch), ch as u8)
                })
                .collect()
        }
    };

    let save_iv = save_interval_secs.max(1);
    for (vol_path, letter) in volumes_watch {
        let index_for_watch = index.clone();
        let engine_watch = engine.clone();
        std::thread::spawn(move || {
            if let Err(e) =
                usn_watch_loop(engine_watch, vol_path, letter, index_for_watch, save_iv)
            {
                error!("USN 监听线程退出 ({letter}): {e}");
            }
        });
    }

    pipe_thread
        .join()
        .map_err(|_| anyhow::anyhow!("named pipe 线程异常结束"))?;
    Ok(())
}

pub(crate) fn normalize_pipe_path(pipe: &str) -> String {
    if pipe.starts_with(r"\\") {
        pipe.to_string()
    } else {
        format!(r"\\.\pipe\{pipe}")
    }
}

/// USN 写入批处理参数：
///
/// 一旦攒齐 [`USN_BATCH_MAX_EVENTS`] 条，或自上一次 batch 起超过 [`USN_BATCH_FLUSH_MS`]，
/// 就一次性拿 write lock 串行 apply，确保搜索读路径只被「批之间」短暂阻塞，
/// 而不是被每条增量都打断（写盘抖动场景下原实现搜索 P99 会被严重拉高）。
const USN_BATCH_MAX_EVENTS: usize = 4096;
const USN_BATCH_FLUSH_MS: u64 = 50;

fn usn_watch_loop(
    engine: Arc<SearchEngine>,
    volume_path: String,
    volume_letter: u8,
    index_path: PathBuf,
    save_interval_secs: u64,
) -> anyhow::Result<()> {
    let vol_meta = {
        let g = engine.index_store();
        g.volumes.iter().find(|v| {
            (v.volume_letter as char).to_ascii_uppercase()
                == (volume_letter as char).to_ascii_uppercase()
        })
            .cloned()
            .or_else(|| g.volumes.first().cloned())
            .ok_or_else(|| anyhow::anyhow!("索引中无卷元数据"))?
    };

    let resume = findx2_windows::UsnResume {
        journal_id: vol_meta.usn_journal_id,
        start_usn: vol_meta.last_usn,
    };

    let (tx, rx) = mpsc::channel::<findx2_windows::UsnWatchMsg>();
    let vol_path = volume_path.clone();
    let worker = std::thread::spawn(move || {
        let _ = findx2_windows::usn_watch_forever(&vol_path, Some(resume), tx);
    });

    let save_every = Duration::from_secs(save_interval_secs);
    let mut last_save = Instant::now();

    let mut pending: Vec<findx2_core::ChangeEvent> = Vec::with_capacity(USN_BATCH_MAX_EVENTS);
    let mut batch_started = Instant::now();
    let flush_every = Duration::from_millis(USN_BATCH_FLUSH_MS);

    // USN flush：拿一次写锁 apply 一批。**关键约束**：单次写锁持有时间必须 <几十 ms，
    // 否则历史回放期间（service 启动后从 last_usn 追到当前，可能几十万条事件瞬间涌入）
    // 写锁会被持有数秒，期间所有 search 都被阻塞——这就是 GUI/CLI 表现"卡 10 秒"的元凶。
    //
    // 用 sub-batch（512 一组），每个 sub-batch 一把短写锁，组间不 sleep（让历史回放不至于
    // 拖太久），但每个 sub-batch 之间是新写锁——parking_lot 下让排队中的 search reader
    // 有插队窗口。
    const SUB_BATCH: usize = 512;
    let flush_pending = |pending: &mut Vec<findx2_core::ChangeEvent>| {
        if pending.is_empty() {
            return;
        }
        let total = pending.len();
        let t0 = Instant::now();
        let drained: Vec<findx2_core::ChangeEvent> = pending.drain(..).collect();
        for chunk in drained.chunks(SUB_BATCH) {
            let mut g = engine.index_store_mut();
            for ev in chunk {
                if let Err(e) = g.apply_change_event(ev) {
                    error!("apply_change_event: {e}");
                }
            }
            // g 在每个 chunk 末尾自动释放，下个 chunk 会重新拿写锁——
            // search reader 有机会在两次写锁之间插进来。
        }
        let ms = t0.elapsed().as_millis();
        if ms > 100 {
            // 超过 100ms 的 flush 大概率是历史回放或大批增量；记下来便于复盘。
            tracing::info!("USN flush 偏慢：{} 条 / {} ms", total, ms);
        }
    };

    loop {
        match rx.recv_timeout(flush_every) {
            Ok(msg) => match msg {
                findx2_windows::UsnWatchMsg::Event(ev) => {
                    if pending.is_empty() {
                        batch_started = Instant::now();
                    }
                    pending.push(ev);
                    if pending.len() >= USN_BATCH_MAX_EVENTS {
                        flush_pending(&mut pending);
                    }
                }
                findx2_windows::UsnWatchMsg::Checkpoint {
                    journal_id,
                    next_usn,
                } => {
                    flush_pending(&mut pending);
                    if let Some(v) = engine.index_store_mut().volumes.iter_mut().find(|x| {
                        (x.volume_letter as char).to_ascii_uppercase()
                            == (volume_letter as char).to_ascii_uppercase()
                    })
                    {
                        v.usn_journal_id = journal_id;
                        v.last_usn = next_usn;
                    }
                    // 回填未完成时不写盘：overlay 不在 index.bin 里，写出去的还是
                    // 和回填前一模一样的 549MB 旧数据，纯纯浪费磁盘 IO（30s 一次 = 每分钟 1GB），
                    // 而且会和正在做 NtQueryDirectoryFile 的 backfill 抢同一块物理盘的 IO 通道，
                    // 直接把回填速度拖慢 30%+。回填完成后 spawn_final_persist 会写一次完整的。
                    if engine.metadata_ready() && last_save.elapsed() >= save_every {
                        persist_index(&engine, &index_path)?;
                        last_save = Instant::now();
                    }
                }
            },
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if !pending.is_empty() && batch_started.elapsed() >= flush_every {
                    flush_pending(&mut pending);
                }
                if engine.metadata_ready() && last_save.elapsed() >= save_every {
                    persist_index(&engine, &index_path)?;
                    last_save = Instant::now();
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if !pending.is_empty() && batch_started.elapsed() >= flush_every {
            flush_pending(&mut pending);
        }
        if worker.is_finished() {
            break;
        }
    }

    flush_pending(&mut pending);
    let _ = worker.join();
    persist_index(&engine, &index_path)?;
    Ok(())
}

pub(crate) fn persist_index(engine: &SearchEngine, path: &Path) -> anyhow::Result<()> {
    // try_lock：同一时刻只允许一个卷线程进入 save_index_bin。
    // 拿不到锁的线程说明已有人在写，本轮直接跳过；下一次 USN flush 周期它还会再来。
    // 这样既保证文件一致（叠加 persist.rs 内的原子 rename），又不会在多卷场景重复写盘。
    let guard = match persist_lock().try_lock() {
        Ok(g) => g,
        Err(_) => {
            tracing::debug!("persist_index: 另一卷线程正在写入 {}，本轮跳过", path.display());
            return Ok(());
        }
    };
    let store = engine.index_store();
    save_index_bin(path, &store)?;
    drop(guard);
    Ok(())
}

/// 共享给 Everything IPC 与管道：解析查询并搜索。
pub(crate) fn search_ipc(
    engine: &SearchEngine,
    query: &str,
    pinyin: bool,
    limit_override: usize,
) -> Result<(Vec<findx2_ipc::SearchHitDto>, u32, u32), String> {
    let started = std::time::Instant::now();
    let t_parse = std::time::Instant::now();
    let mut pq = QueryParser::parse(query).map_err(|e| e.to_string())?;
    if limit_override > 0 {
        pq.limit = limit_override.min(u32::MAX as usize) as u32;
    }
    let parse_ms = t_parse.elapsed().as_micros() as u64;
    let t_search = std::time::Instant::now();
    let (hits, total) = engine
        .search(
            &pq,
            &SearchOptions {
                allow_pinyin: pinyin,
                ..Default::default()
            },
        )
        .map_err(|e| e.to_string())?;
    let search_us = t_search.elapsed().as_micros() as u64;
    let t_format = std::time::Instant::now();
    let store = engine.index_store();
    let dtos: Vec<findx2_ipc::SearchHitDto> = hits
        .into_iter()
        .map(|h| {
            let is_directory = store
                .entries
                .get(h.entry_idx as usize)
                .map(|e| e.is_dir_entry())
                .unwrap_or(false);
            findx2_ipc::SearchHitDto {
                entry_idx: h.entry_idx,
                name: h.name,
                path: h.path,
                size: h.size,
                mtime: h.mtime,
                is_directory,
                name_highlight: h.name_highlight,
            }
        })
        .collect();
    let format_us = t_format.elapsed().as_micros() as u64;
    let elapsed_ms = started.elapsed().as_millis().min(u32::MAX as u128) as u32;
    findx2_core::progress!(
        "search [{}] -> {} hits / total {} : parse {}μs · core {}μs ({:.2}ms) · format {}μs · sum {}ms",
        query,
        dtos.len(),
        total,
        parse_ms,
        search_us,
        (search_us as f64) / 1000.0,
        format_us,
        elapsed_ms
    );
    Ok((dtos, total, elapsed_ms))
}
