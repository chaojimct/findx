//! 前台模式：加载索引、USN、命名管道、Everything IPC。

use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::OnceLock;
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use findx2_core::{save_index_bin, SearchEngine};
use tracing::{error, info};

use crate::watch_health::set_watch_error;

/// 运行时开关：来自 CLI（`--no-everything-ipc` / `--no-backfill` / `--exclude-dir`）。
/// 抽 struct 而不是继续加位置参数，是因为 `run_foreground` 已经 5 个参数了，再扩会失控。
#[derive(Debug, Clone, Default)]
pub(crate) struct RunFlags {
    pub no_everything_ipc: bool,
    pub no_backfill: bool,
    /// CLI 注入的排除目录（与 sidecar 里的取并集，由 IndexStore 持有运行时副本）。
    pub extra_excluded_dirs: Vec<String>,
}

/// 全卷重建冻结集（P0-2 JournalGap 重建协议，按卷独立）。
/// 置位期间：该卷 watch 线程排空管道但不 apply、不推进 last_usn（事件靠 journal 重放补回，
/// 见 `rebuild_volume`）；其它卷不受影响。重建稀有，全局一个集合足够。
fn rebuild_frozen_volumes() -> &'static Mutex<std::collections::HashSet<char>> {
    static FROZEN: OnceLock<Mutex<std::collections::HashSet<char>>> = OnceLock::new();
    FROZEN.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

fn freeze_volume(letter: char, on: bool) {
    let key = letter.to_ascii_uppercase();
    if let Ok(mut g) = rebuild_frozen_volumes().lock() {
        if on {
            g.insert(key);
        } else {
            g.remove(&key);
        }
    }
}

fn is_volume_frozen(letter: char) -> bool {
    rebuild_frozen_volumes()
        .lock()
        .map(|g| g.contains(&letter.to_ascii_uppercase()))
        .unwrap_or(false)
}

/// 后台 stat worker：消费 `UsnWatchMsg::StatRefresh`，批量 `OpenFileById` 后写回。
/// watch 线程永不 stat 的另一半——随机 IO 在这里按 FRN 排序后批量消化，
/// bulk 风暴时名字秒级可见、meta 随后追上（SSD 秒级、HDD 分钟级，但绝不卡住增量）。
/// 无权限（非管理员）时 fill 恒空：排空丢弃，只记一次日志（文件本身已由 CreatePending 入库）。
fn spawn_stat_worker(
    engine: Arc<SearchEngine>,
    volume_device_path: String,
) -> std::sync::mpsc::Sender<(u64, Option<[u8; 16]>)> {
    let (tx, rx) = std::sync::mpsc::channel::<(u64, Option<[u8; 16]>)>();
    std::thread::Builder::new()
        .name("findx2-stat-worker".into())
        .spawn(move || {
            use std::collections::HashMap;
            // FRN 去重（同文件多次变更只 stat 最后一次；value 无意义，占位）。
            let mut pending: HashMap<u64, Option<[u8; 16]>> = HashMap::new();
            let mut logged_no_perm = false;
            // 主循环退出条件只有进程退出；recv_timeout 节拍顺带做批量超时。
            loop {
                match rx.recv_timeout(std::time::Duration::from_millis(1000)) {
                    Ok((frn, id128)) => {
                        if pending.len() < 500_000 {
                            pending.insert(frn, id128);
                        }
                        // 攒够一批就刷（5000 条或 1 秒超时，取先到者）。
                        if pending.len() >= 5000 {
                            drain_stat_batch(&engine, &volume_device_path, &mut pending, &mut logged_no_perm);
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if !pending.is_empty() {
                            drain_stat_batch(&engine, &volume_device_path, &mut pending, &mut logged_no_perm);
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
                // 兜底：队列过大（极端风暴）时强制刷盘，避免内存无限涨。
                if pending.len() >= 50_000 {
                    drain_stat_batch(&engine, &volume_device_path, &mut pending, &mut logged_no_perm);
                }
            }
            // 退出前把剩余的刷完（进程退出场景基本走不到，保底）。
            if !pending.is_empty() {
                drain_stat_batch(&engine, &volume_device_path, &mut pending, &mut logged_no_perm);
            }
        })
        .ok();
    tx
}

fn drain_stat_batch(
    engine: &Arc<SearchEngine>,
    volume_device_path: &str,
    pending: &mut std::collections::HashMap<u64, Option<[u8; 16]>>,
    logged_no_perm: &mut bool,
) {
    if pending.is_empty() {
        return;
    }
    // 注：重建冻结期无需特殊处理——合并后旧条目下标不变（新条目追加在后），
    // patch 到墓碑/旧条目无害；新文件的 StatRefresh 在旧布局查不到会被跳过，
    // journal 重放会重新排队。见 rebuild_volume 设计注释。
    // 按 FRN 排序后批量 stat：MFT 记录号即 FRN 低 48 位，排序≈顺序读，HDD 友好。
    let mut frns: Vec<u64> = pending.keys().copied().collect();
    frns.sort_unstable();
    let id128s: Vec<Option<[u8; 16]>> = frns.iter().map(|f| pending[f]).collect();
    let indices: Vec<usize> = (0..frns.len()).collect();
    let updates = findx2_windows::fill_metadata_by_id_pooled(
        volume_device_path,
        &frns,
        &id128s,
        &indices,
        None,
        None,
    );
    pending.clear();
    if updates.is_empty() {
        // 全失败：大概率无权限。节流日志（只记一次），否则每秒刷屏。
        if !*logged_no_perm {
            *logged_no_perm = true;
            tracing::warn!(
                "stat worker：批量 stat 持续失败（卷 {}，可能无管理员权限），新文件 size/mtime 将保持 0",
                volume_device_path
            );
        }
        return;
    }
    // FRN → entry_idx 映射一次查完（读锁），再逐条 patch（写锁短持 + revision）。
    // idx 下标稳定（entries 只追加不压缩；删除只打墓碑），映射与 patch 之间无需重查。
    // 注：墓碑条目也会被 patch（无害：搜索过滤在前；且多为“删后 stat 迟到”的无害写）。
    let targets: Vec<(usize, u64, u64, u64)> = updates
        .iter()
        .filter_map(|(local_idx, size, mtime, ctime)| {
            let frn = *frns.get(*local_idx)?;
            let idx = engine.entry_idx_by_frn(frn)?;
            Some((idx, *size, *mtime, *ctime))
        })
        .collect();
    for (idx, size, mtime, ctime) in targets {
        // patch_entry_metadata 内部 bump revision（分页缓存失效正确）。
        let _ = engine.patch_entry_metadata(idx, size, mtime, ctime);
    }
}

/// 串行化 `index.bin` 写盘：service 启动时按卷数 spawn 多个 USN watch 线程，
/// 每个线程都按 `save_interval_secs` 周期写**同一份** `index.bin`。旧实现没锁，
/// 三个卷在同一刻同时调 `save_index_bin` 会互相截断把文件写坏，下次启动报
/// `failed to fill whole buffer`。这里用 Mutex 强制串行：拿不到锁的卷直接跳过本轮。
fn persist_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// trigram 后台重建防重入标志：多卷 USN 线程 + 启动补建共享同一份全索引边车，
/// 同时跑两份构建只会互相覆盖 tmp / rename 打架。
fn trigram_rebuilding() -> &'static std::sync::atomic::AtomicBool {
    static FLAG: OnceLock<std::sync::atomic::AtomicBool> = OnceLock::new();
    FLAG.get_or_init(|| std::sync::atomic::AtomicBool::new(false))
}

struct TrigramRebuildGuard(());
impl TrigramRebuildGuard {
    fn acquire() -> Option<Self> {
        use std::sync::atomic::Ordering;
        if trigram_rebuilding().swap(true, Ordering::AcqRel) {
            None // 已有重建在进行
        } else {
            Some(Self(()))
        }
    }
}
impl Drop for TrigramRebuildGuard {
    fn drop(&mut self) {
        trigram_rebuilding().store(false, std::sync::atomic::Ordering::Release);
    }
}

/// 触发 trigram 边车后台重建（缺失补建 / 增量超阈值）。非阻塞：立刻返回。
/// 防重入由 [`TrigramRebuildGuard`] 保证，重建期间再次触发直接忽略
/// （溢出场景 pending 会继续积累到下一轮 save 周期再触发，无丢失）。
fn spawn_trigram_rebuild(engine: Arc<SearchEngine>, index: PathBuf, reason: &'static str) {
    let Some(guard) = TrigramRebuildGuard::acquire() else {
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("findx2-trigram-rebuild".into())
        .spawn(move || {
            let _guard = guard;
            info!("trigram：后台重建开始（{reason}）");
            let t0 = Instant::now();
            match engine.rebuild_trigram_sidecar(&index) {
                Ok(()) => info!(
                    "trigram：后台重建完成，耗时 {:.1}s",
                    t0.elapsed().as_secs_f64()
                ),
                Err(e) => error!("trigram：后台重建失败（下一轮再试）: {e}"),
            }
        });
    if spawned.is_err() {
        // spawn 失败时 guard 还没进线程，手动释放（否则标志永远卡在 true）。
        trigram_rebuilding().store(false, std::sync::atomic::Ordering::Release);
    }
}

/// save 周期后调用：增量超阈值时后台重建（阈值见 [`findx2_core::IndexStore::tri_pending_overflow`]，
/// 默认 max(条目数/32, 65536)）。未超阈值是常态，检查本身只是一次位图 len 读取。
fn maybe_rebuild_trigram(engine: &Arc<SearchEngine>, index: &Path) {
    if engine.index_store().tri_pending_overflow() {
        spawn_trigram_rebuild(
            engine.clone(),
            index.to_path_buf(),
            "USN 增量超过阈值",
        );
    }
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
    let slot: crate::ipc_dispatch::EngineSlot = Arc::new(RwLock::new(None));

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

    // 边车缺失 / 加载时被判不可信（重建窗口内崩溃过）：后台补建一次，
    // 本轮搜索先走全表扫描兜底，构建完成后自动切到剪枝路径。
    if engine.index_store().trigram.is_none() {
        spawn_trigram_rebuild(
            engine.clone(),
            index.clone(),
            "启动时边车缺失或校验失败",
        );
    }

    let _everything: Option<JoinHandle<()>> = if flags.no_everything_ipc {
        info!("已通过 --no-everything-ipc 关闭 Everything 兼容窗口（老客户端将无法连接）");
        None
    } else if crate::session_spawn::current_session_id() == 0 {
        info!("当前为 Session 0 系统服务，改在用户会话拉起 Everything 兼容窗口");
        crate::session_spawn::spawn_everything_host_watchdog(pipe_name.clone());
        None
    } else {
        Some(crate::everything_ipc::spawn_everything_ipc(engine.clone()))
    };

    if flags.no_backfill {
        info!("已通过 --no-backfill 关闭后台元数据回填（fast 首遍后 size/mtime 可能为 0）");
        engine.set_backfill_error(Some(
            "元数据回填已关闭（设置或 --no-backfill）；时间与大小筛选可能不准".into(),
        ));
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
    // P0-3 journal 保障 + P1-6 MFT 碎片预警（每卷一次，需管理员；失败只记日志）：
    // journal 太小是“长期运行后静默丢变更”的根因，Everything 同款做法。
    for (vol_path, letter) in &volumes_watch {
        match findx2_windows::ensure_usn_journal(vol_path) {
            Ok(true) => info!("卷 {letter} USN journal 已保障（创建/放大到 512MB）"),
            Ok(false) => {}
            Err(e) => tracing::warn!("卷 {letter} journal 保障失败: {e}"),
        }
        if let Some(n) = findx2_windows::mft_extent_count(vol_path) {
            if n > 64 {
                tracing::warn!(
                    "卷 {letter} $MFT 碎片 {n} 段（>64）：首遍枚举在 HDD 上会明显变慢，空闲时可整理碎片"
                );
            } else {
                info!("卷 {letter} $MFT 碎片 {n} 段");
            }
        }
    }
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
    let letter = (volume_letter as char).to_ascii_uppercase();
    // 初始 resume 由下面的 make_worker 每次重读（重建后游标已更新），这里不再预读。
    // P2 ReFS/无 journal 卷：journal_id==0 表示建库时就不可用，不启动监听
    //（否则 query 恒失败→无限重启刷屏）。状态进健康上报，用户可见。
    {
        let g = engine.index_store();
        let no_journal = g
            .volumes
            .iter()
            .find(|v| (v.volume_letter as char).to_ascii_uppercase() == letter)
            .map(|v| v.usn_journal_id == 0)
            .unwrap_or(false);
        if no_journal {
            set_watch_error(
                letter,
                Some("该卷无 USN journal（如 ReFS），不做增量监听；改动需手动重建".into()),
            );
            info!("卷 {letter} 无 USN journal，跳过增量监听");
            return Ok(());
        }
    }

    // 后台 stat worker（watch 线程只解析 journal，stat 全走这里；volume 固定，随 loop 常驻）。
    let vol_dev = format!(r"\\.\{}:", volume_letter as char);
    let stat_tx = spawn_stat_worker(engine.clone(), vol_dev);

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
        // 直接改 store 绕过了 engine 的写入方法：手动推进 revision，使分页缓存失效。
        // 一批只 bump 一次（不必每条）， Staleness 窗口 ≤ 一次 flush。
        engine.note_external_mutation();
        let ms = t0.elapsed().as_millis();
        if ms > 100 {
            // 超过 100ms 的 flush 大概率是历史回放或大批增量；记下来便于复盘。
            tracing::info!("USN flush 偏慢：{} 条 / {} ms", total, ms);
        }
    };

    // P0-2 保活：worker 死亡不结束本线程，而是判因重启。
    // - JournalGap（ID 变化/游标被覆写）→ 全卷重建后用新游标续跑（≤3 次，防疯狂 wrap 死循环）；
    // - 其它错误/panic → 指数退避重启；
    // - 健康状态实时进 watch_health → IPC Status → GUI 状态栏。
    let mut backoff = Duration::from_secs(1);
    let mut gap_rebuilds: u32 = 0;
    // journal 覆写预警（P2）：每次 checkpoint 查 FirstUsn（QUERY 一次 ioctl，很轻）。

    // worker 启动闭包：每次都重读最新游标（重建后 journal 游标已更新）。
    // 返回 None = 卷元数据都没了（索引被换），退避后外层重试。
    let make_worker = |tx: mpsc::Sender<findx2_windows::UsnWatchMsg>|
        -> Option<std::thread::JoinHandle<findx2_core::Result<()>>>
    {
        let vol_meta = {
            let g = engine.index_store();
            g.volumes
                .iter()
                .find(|v| {
                    (v.volume_letter as char).to_ascii_uppercase() == letter
                })
                .cloned()
                .or_else(|| g.volumes.first().cloned())
        }?;
        let resume = findx2_windows::UsnResume {
            journal_id: vol_meta.usn_journal_id,
            start_usn: vol_meta.last_usn,
        };
        let vol_path = volume_path.clone();
        Some(std::thread::spawn(move || {
            findx2_windows::usn_watch_forever(&vol_path, Some(resume), tx)
        }))
    };

    // worker 持有发送端，主循环只收不发；重启时整套重建（resume 重读新游标）。
    let mut rx: mpsc::Receiver<findx2_windows::UsnWatchMsg>;
    let mut worker = {
        let (tx0, rx0) = mpsc::channel::<findx2_windows::UsnWatchMsg>();
        rx = rx0;
        match make_worker(tx0) {
            Some(w) => w,
            None => {
                set_watch_error(letter, Some("索引中无卷元数据，等待…".into()));
                return Err(anyhow::anyhow!("索引中无卷元数据"));
            }
        }
    };

    loop {
        // 重建冻结期（本卷）：排空丢弃，不 apply 不推进游标；journal 重放会补回。
        // 其它卷不受影响（标志按卷）。
        if is_volume_frozen(letter) {
            pending.clear();
            while rx.try_recv().is_ok() {}
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
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
                findx2_windows::UsnWatchMsg::StatRefresh { file_id, file_id_128 } => {
                    // 路由给后台 stat worker（watch 线程永不 stat）；发送失败（worker 退出）
                    // 则丢弃——下次同文件变更会重新排队，极端下靠全量重建兜底。
                    let _ = stat_tx.send((file_id, file_id_128));
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
                    // 存活确认：收到 journal 心跳即视为健康，复位退避/重建计数。
                    backoff = Duration::from_secs(1);
                    gap_rebuilds = 0;
                    if let Some(w) = probe_journal_wrap_warning(&volume_path, letter, next_usn)
                    {
                        set_watch_error(letter, Some(w));
                    } else {
                        set_watch_error(letter, None);
                    }
                    // 注：这里只改 USN 游标，不影响搜索结果/排序，故不 bump revision
                    //（否则每次 checkpoint 都会误杀分页缓存）。真正改条目的是上面的 flush_pending。
                    // 回填未完成时不写盘：overlay 不在 index.bin 里，写出去的还是
                    // 和回填前一模一样的 549MB 旧数据，纯纯浪费磁盘 IO（30s 一次 = 每分钟 1GB），
                    // 而且会和正在做 NtQueryDirectoryFile 的 backfill 抢同一块物理盘的 IO 通道，
                    // 直接把回填速度拖慢 30%+。回填完成后 spawn_final_persist 会写一次完整的。
                    if engine.metadata_ready() && last_save.elapsed() >= save_every {
                        persist_index(&engine, &index_path)?;
                        last_save = Instant::now();
                        maybe_rebuild_trigram(&engine, &index_path);
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
                    maybe_rebuild_trigram(&engine, &index_path);
                }
            },
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // worker 死了：join 判因后重启（先排空残留消息已由 Disconnected 语义保证）。
                flush_pending(&mut pending);
                let exit = worker.join();
                let reason = match &exit {
                    Ok(Ok(())) => "watch 线程异常返回（理论不可达）".to_string(),
                    Ok(Err(e)) => format!("{e}"),
                    Err(_) => "watch 线程 panic".to_string(),
                };
                let is_gap = matches!(
                    &exit,
                    Ok(Err(findx2_core::Error::JournalGap(_)))
                );
                if is_gap && gap_rebuilds < 3 {
                    gap_rebuilds += 1;
                    set_watch_error(
                        letter,
                        Some(format!("USN 日志断档，全卷重建中（{gap_rebuilds}/3）…")),
                    );
                    error!("卷 {letter} {reason}，触发全卷重建");
                    if let Err(re) = rebuild_volume(&engine, letter, &index_path) {
                        set_watch_error(
                            letter,
                            Some(format!("重建失败: {re}，退避重试…")),
                        );
                        error!("卷 {letter} 重建失败: {re:#}");
                    } else {
                        info!("卷 {letter} 重建完成，恢复增量监听");
                        set_watch_error(letter, None);
                        gap_rebuilds = 0;
                    }
                } else if is_gap {
                    set_watch_error(
                        letter,
                        Some("USN 断档且自动重建已达上限，请手动重建索引".into()),
                    );
                    error!("卷 {letter} USN 断档重建达上限，监听线程退出");
                    return Err(anyhow::anyhow!("卷 {letter} USN 断档重建达上限"));
                } else {
                    let denied = reason.contains("拒绝访问")
                        || reason.contains("0x80070005")
                        || reason.contains("Access is denied");
                    let msg = if denied {
                        format!(
                            "打开卷被拒绝（权限不足）。请确认系统服务 FindX2Search 正在运行，而不是普通权限的 findx2-service。{reason}"
                        )
                    } else {
                        format!("监听中断，重试中: {reason}")
                    };
                    set_watch_error(letter, Some(msg));
                    error!("卷 {letter} {reason}，退避重启监听");
                }
                // 重建 worker（resume 重读：重建后游标已回绕到 frozen 点）。
                // 任一分支都必须重建 worker 或直接返回，否则下轮循环用的是已 move 的旧句柄。
                match make_worker({
                    let (tx_new, rx_new) = mpsc::channel::<findx2_windows::UsnWatchMsg>();
                    rx = rx_new;
                    tx_new
                }) {
                    Some(w) => {
                        worker = w;
                    }
                    None => {
                        return Err(anyhow::anyhow!("卷 {letter} 重启监听时索引中无卷元数据"));
                    }
                }
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(60));
                continue;
            }
        }
        if !pending.is_empty() && batch_started.elapsed() >= flush_every {
            flush_pending(&mut pending);
        }
    }
}

/// 单卷全量重建（JournalGap 后的退路；调用方 usn_watch_loop 负责 freeze/解冻之外的全部）。
///
/// 协议（重建期间零丢失）：
/// 1. 入口即 `freeze_volume(letter)`：watch 线程排空丢弃、不推进游标；
/// 2. 记下内存 `last_usn` 为 frozen 点（pending 已排空、checkpoint 先 flush 的不变量
///    保证 frozen ≤ 所有丢弃事件的 USN，重放全覆盖）；
/// 3. 重新 MFT 枚举 + build 新 store（数分钟，无锁；旧索引继续服务）；
/// 4. 读锁下克隆旧 store → 离线（无锁）：旧卷区间墓碑 + 摘除旧 VolumeState → merge
///    （新 store 后挂，FRN 冲突归新；excluded_dirs 从旧 store 恢复）；
/// 5. 写锁换入（µs）+ revision bump + `last_usn` 回绕到 frozen 点；
/// 6. 全量落盘 + trigram 边车重建；解冻后 worker 重建，用回绕游标从 journal 重放补齐
///    （重复事件幂等：Create→upsert 更新、Delete 缺失→ok、Rename→ok）。
/// 任何一步失败都解冻并返回 Err（外层退避重试；3 次后放弃并上报，需手动重建）。
fn rebuild_volume(
    engine: &Arc<SearchEngine>,
    volume_letter: char,
    index_path: &Path,
) -> anyhow::Result<()> {
    let letter = volume_letter.to_ascii_uppercase();
    let vol_str = format!("{letter}:");
    freeze_volume(letter, true);
    // 作用域守卫：任何返回路径都解冻（成功/失败一律）。
    struct Unfreeze(char);
    impl Drop for Unfreeze {
        fn drop(&mut self) {
            freeze_volume(self.0, false);
        }
    }
    let _unfreeze = Unfreeze(letter);

    let frozen: u64 = {
        let g = engine.index_store();
        g.volumes
            .iter()
            .find(|v| (v.volume_letter as char).to_ascii_uppercase() == letter)
            .map(|v| v.last_usn)
            .unwrap_or(0)
    };
    info!("卷 {letter} 开始全量重建（frozen 游标 {frozen}，旧索引继续服务）…");

    // 1) 重新扫描（fast 首遍；size/mtime 由回填补——合并后 ready=false 会自动触发）。
    let (files, dirs) = findx2_windows::scan_volume_fast(&vol_str)
        .map_err(|e| anyhow::anyhow!("卷 {letter} MFT 重枚举失败: {e}"))?;
    let serial = findx2_windows::get_volume_serial_number(&vol_str).unwrap_or(0);
    let usn = findx2_windows::UsnJournalWatcher::new(&vol_str)
        .probe()
        .map_err(|e| anyhow::anyhow!("卷 {letter} journal 探测失败: {e}"))?;
    let fresh = findx2_core::IndexBuilder::new(letter as u8, serial, usn.journal_id, usn.next_usn)
        .build_from_raw(files, dirs, false)
        .map_err(|e| anyhow::anyhow!("卷 {letter} 索引构建失败: {e}"))?;
    info!(
        "卷 {letter} 重枚举完成：{} 条（旧索引仍在服务，开始离线合并）…",
        fresh.entry_count()
    );

    // 2) 读锁下克隆（memcpy 级，搜索不阻塞）→ 离线墓碑旧区间 + 合并。
    let old_snapshot = { engine.index_store().clone() };
    let merged = merge_rebuilt_volume(old_snapshot, fresh, letter)?;

    // 3) 写锁换入 + 游标回绕 + revision。
    {
        let mut g = engine.index_store_mut();
        *g = merged;
        if let Some(v) = g
            .volumes
            .iter_mut()
            .find(|v| (v.volume_letter as char).to_ascii_uppercase() == letter)
        {
            v.last_usn = frozen;
        }
    }
    engine.note_external_mutation();

    // 4) 落盘 + trigram 重建（合并后 trigram=None，先回全表扫描，重建完自动切回剪枝）。
    persist_index(engine, index_path)?;
    spawn_trigram_rebuild(engine.clone(), index_path.to_path_buf(), "单卷重建后边车重建");
    // 合并后 metadata_ready=false（fast 重枚举），必须重新拉起回填；旧 overlay 下标已失效。
    engine.clear_metadata_overlay();
    crate::backfill::spawn_backfill(Arc::clone(engine), index_path.to_path_buf());
    info!("卷 {letter} 重建完成（已回绕到 frozen={frozen} 重放），恢复增量监听");
    Ok(())
}

/// 离线合并：旧卷区间墓碑 + 摘除旧 VolumeState + 新 store 后挂合并。
fn merge_rebuilt_volume(
    mut old: findx2_core::IndexStore,
    fresh: findx2_core::IndexStore,
    letter: char,
) -> anyhow::Result<findx2_core::IndexStore> {
    // 旧卷区间（按 first_entry_idx 排序后定位）。
    let mut ranges: Vec<(char, u32)> = old
        .volumes
        .iter()
        .map(|v| {
            (
                (v.volume_letter as char).to_ascii_uppercase(),
                v.first_entry_idx,
            )
        })
        .collect();
    ranges.sort_by_key(|(_, idx)| *idx);
    let (start, end) = match ranges.iter().position(|(l, _)| *l == letter) {
        Some(p) => {
            let s = ranges[p].1 as usize;
            let e = if p + 1 < ranges.len() {
                ranges[p + 1].1 as usize
            } else {
                old.entries.len()
            };
            (s, e.min(old.entries.len()))
        }
        None => {
            // 卷不在旧索引里（理论上重建只发生在已有卷，防御性：纯追加）。
            info!("卷 {letter} 不在旧索引中，重建退化为纯追加合并");
            (0, 0)
        }
    };
    for i in start..end {
        old.delete_entry(i as u32);
    }
    old.volumes
        .retain(|v| (v.volume_letter as char).to_ascii_uppercase() != letter);
    let excluded = old.excluded_dirs.clone();
    let mut merged = findx2_core::merge_index_stores(vec![old, fresh])
        .map_err(|e| anyhow::anyhow!("卷 {letter} 索引合并失败: {e}"))?;
    merged.excluded_dirs = excluded;
    Ok(merged)
}

/// P2 journal 覆写预警：FirstUsn 逼近尚未读到的游标时返回状态栏文案。
///
/// 不能用 `next - cursor` 当「剩余」。监听追上时 cursor ≈ next，该值恒为 0，
/// 而 `next - first`（已占用跨度）会随文件活动一直涨——状态栏就会显示
/// 「剩余 0 / 分母狂增」，纯属误报。
///
/// 真正有风险的是：我们还落后（next > cursor），且 FirstUsn 已经逼近 cursor
/// （历史缓冲 < 占用跨度的 5%）。追上后覆写的是早已处理过的旧记录，不必报警。
fn probe_journal_wrap_warning(
    volume_path: &str,
    letter: char,
    cursor_usn: u64,
) -> Option<String> {
    let Ok((_, first, next)) = findx2_windows::query_journal_state(volume_path) else {
        return None;
    };
    if next <= first {
        return None;
    }
    let cursor = cursor_usn as i64;
    if next <= cursor {
        return None;
    }
    let span = (next - first) as u64;
    let slack = cursor.saturating_sub(first).max(0) as u64;
    if span > 0 && slack < span / 20 {
        tracing::warn!(
            "卷 {letter} USN journal 未读游标接近覆写前沿 (缓冲 {slack}/{span})"
        );
        return Some(format!(
            "USN journal 未读记录即将被覆写（缓冲 {slack}/{span}），停机过久会触发全量重建"
        ));
    }
    None
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
