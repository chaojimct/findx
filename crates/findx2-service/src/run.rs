//! 前台模式：加载索引、USN、命名管道、Everything IPC。

use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::OnceLock;
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use findx2_core::{
    load_index_bin_with_progress, v6_load, v6_save_full, v6_try_commit, CommitOutcome,
    SearchEngine, V6Layout,
};
use tracing::{error, info};

use crate::load_state;
use crate::watch_health::set_watch_error;

/// 运行时开关：来自 CLI（`--no-everything-ipc` / `--no-backfill` / `--exclude-dir`）。
/// 抽 struct 而不是继续加位置参数，是因为 `run_foreground` 已经 5 个参数了，再扩会失控。
#[derive(Debug, Clone, Default)]
pub(crate) struct RunFlags {
    pub no_everything_ipc: bool,
    pub no_backfill: bool,
    pub no_watch: bool,
    pub usage_memory_mib: Option<usize>,
    pub shutdown_file: Option<std::path::PathBuf>,
    pub fixture_events: Option<std::path::PathBuf>,
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

/// 返回时先断开发送端，再 join。调用方不能在还握着索引写锁时丢弃它。
/// 已经进入 `OpenFileById` 的调用会跑完；其余排队文件由 worker 写成未验证后退出。
struct JoinedStatWorker {
    tx: Option<std::sync::mpsc::SyncSender<crate::stat_queue::StatMessage>>,
    handle: Option<JoinHandle<()>>,
}
impl JoinedStatWorker {
    fn tx(&self) -> &std::sync::mpsc::SyncSender<crate::stat_queue::StatMessage> {
        self.tx.as_ref().expect("stat sender live until shutdown")
    }
}
impl Drop for JoinedStatWorker {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn spawn_stat_worker(
    engine: Arc<SearchEngine>,
    volume_device_path: String,
    stop: Arc<AtomicBool>,
) -> JoinedStatWorker {
    let (tx, rx) = std::sync::mpsc::sync_channel::<crate::stat_queue::StatMessage>(
        (4 * 1024 * 1024) / std::mem::size_of::<crate::stat_queue::StatMessage>(),
    );
    let device = volume_device_path.clone();
    let handle = std::thread::Builder::new()
        .name("findx2-stat-worker".into())
        .spawn(move || {
            crate::stat_queue::run_stat_worker(rx, engine, volume_device_path, stop, 5_000, move |frns, ids, indices, cancel| {
                findx2_windows::fill_metadata_by_id_pooled(&device, frns, ids, indices, None, cancel)
            });
        })
        .ok();
    JoinedStatWorker { tx: Some(tx), handle }
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
    let lifecycle=match crate::runtime_lifecycle::reserve(&engine,"trigram-publisher") {Ok(v)=>v.map(|p|p.require_result()),Err(_)=>return};
    let spawned = std::thread::Builder::new()
        .name("findx2-trigram-rebuild".into())
        .spawn(move || {
            let mut lifecycle=lifecycle.map(|p|p.activate());
            let _guard = guard;
            info!("trigram：后台重建开始（{reason}）");
            let t0 = Instant::now();
            let result=engine.rebuild_trigram_sidecar(&index);
            match &result{Ok(())=>crate::runtime_lifecycle::success_optional(&mut lifecycle),Err(e)=>crate::runtime_lifecycle::error_optional(&mut lifecycle,e.to_string())}
            match result {
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
        spawn_trigram_rebuild(engine.clone(), index.to_path_buf(), "USN 增量超过阈值");
    }
}

/// 前台运行。（非 Windows 下不提供本模块）
/// 控制台模式用的「永不停止」标志：无 Ctrl+C 优雅停（journal 重放兜底），签名对齐服务模式。
pub(crate) fn never_stop_flag() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

// A shutdown-file request must publish the same shared STOP observed by
// watch workers; merely leaving the waiter made every shutdown consume 10 s.
fn shutdown_requested(stop: &AtomicBool, marker: Option<&Path>) -> bool {
    if stop.load(Ordering::SeqCst) { return true; }
    if marker.is_some_and(|path| path.exists()) {
        stop.store(true, Ordering::SeqCst);
        return true;
    }
    false
}

fn acquire_index_owner(index:&Path)->anyhow::Result<std::fs::File>{
    use std::os::windows::fs::OpenOptionsExt;
    let parent=index.parent().ok_or_else(||anyhow::anyhow!("index parent unavailable"))?.canonicalize()?;
    let name=index.file_name().ok_or_else(||anyhow::anyhow!("index filename missing"))?;
    let path=parent.join(format!("{}.runtime-owner",name.to_string_lossy()));
    std::fs::OpenOptions::new().read(true).write(true).create(true).share_mode(0).open(path).map_err(|e|anyhow::anyhow!("exclusive index runtime owner refused: {e}"))
}

pub(crate) fn run_foreground(
    index: PathBuf,
    _volume: String,
    pipe_name: String,
    save_interval_secs: u64,
    full_stat: bool,
    max_scan_threads: usize,
    flags: RunFlags,
    stop: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let _ = (full_stat, max_scan_threads);
    if flags.no_watch && normalize_pipe_path(&pipe_name).eq_ignore_ascii_case(r"\\.\pipe\findx2") {
        anyhow::bail!("strict fixture must not use the formal findx2 pipe");
    }
    if flags.no_watch && (!flags.no_backfill || !flags.no_everything_ipc) {
        anyhow::bail!("--no-watch requires --no-backfill --no-everything-ipc (strict isolation)");
    }
    if !index.exists() {
        return Err(anyhow::anyhow!(
            "索引文件 {} 不存在。findx2-service 不再自动建库；请先用 `findx2 index --output {}` 建库（建库需管理员权限）后再启动服务。",
            index.display(),
            index.display(),
        ));
    }

    // Exclusive OS handle spans load, all publishers, prepared STOP and teardown.
    // Different pipe names cannot open the same index in another new-engine process.
    let _index_owner=acquire_index_owner(&index)?;

    // 关键：管道在 load_index_bin 之前就开起来。
    // 大索引（千万级）反序列化要十几秒甚至几十秒，旧顺序下 GUI / IPC 会一直撞「管道超时」。
    // 现在改为：先挂 EngineSlot（None）→ 起 pipe 线程 → 加载索引 → 注入 Some(engine)。
    let slot: crate::ipc_dispatch::EngineSlot = Arc::new(RwLock::new(None));

    let pipe_path_join = normalize_pipe_path(&pipe_name);
    let slot_pipe = slot.clone();
    // detached：不 join。停机时进程退出由 OS 收割，命名管道句柄随进程关闭。
    let _pipe_thread = std::thread::Builder::new()
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
    // v6 优先（增量分段文件）；v5 老库或 v6 损坏时回退 v5 并重建 v6（自愈，一次性全量写）。
    // v5 回退路径保留上游的 load_state 进度上报（千万级库加载要几十秒，Status 不能静默）。
    let v6 = V6Layout::for_index(&index);
    let mut store = match v6_load(&v6) {
        Ok(s) => {
            load_state::note_entry_count(s.entry_count() as u64);
            load_state::note_finished();
            info!(
                "索引加载完成：{} 条目（v6，耗时 {:.2}s）",
                s.entry_count(),
                load_t0.elapsed().as_secs_f64()
            );
            s
        }
        Err(e) => {
            if v6.manifest_exists() {
                info!("v6 加载失败，回退 v5 并重建（{e:#}）");
            }
            let s = match load_index_bin_with_progress(&index, &|p| {
                load_state::note_phase(p);
            }) {
                Ok(s) => {
                    load_state::note_entry_count(s.entry_count() as u64);
                    s
                }
                Err(e) => {
                    load_state::note_finished();
                    return Err(e.into());
                }
            };
            info!(
                "索引加载完成：{} 条目（v5，耗时 {:.2}s），开始 v6 转换…",
                s.entry_count(),
                load_t0.elapsed().as_secs_f64()
            );
            let t0 = Instant::now();
            let gen = v6_save_full(&v6, &s)?;
            load_state::note_finished();
            info!(
                "v5→v6 一次性转换完成（generation {gen}，耗时 {:.2}s）；后续增量落盘不再全量重写",
                t0.elapsed().as_secs_f64()
            );
            s
        }
    };
    // v6 变更记录从此刻开始：之后的 mark/compact/USN 都会进日志，commit 时回放进分段文件。
    store.set_track_mutations(true);

    // 合并 CLI 追加的排除目录到 store；并把命中条目一次性打墓碑，避免「sidecar 没改，CLI 临时加目录」时旧数据漏网。
    // 注意顺序：先打排除墓碑，下面的启动自动压缩才能把新旧墓碑同轮回收。
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

    // 墓碑自动压缩：体检命中（≥10 万条目且墓碑比 >25%）就在启动窗口内无感回收，
    // Status 显示「自动压缩墓碑」阶段（load_state 上报），GUI 状态栏可见。
    // 落盘失败只降级提示（内存已回收，下次启动重试）；守卫触发则 Err 退出，
    // 宁可不服务也不带可疑索引上线（历史事故教训，见 startup_compact 模块注释）。
    match crate::startup_compact::auto_compact_if_needed(&mut store, &index)? {
        crate::startup_compact::AutoCompactOutcome::Skipped => {}
        crate::startup_compact::AutoCompactOutcome::Compacted { saved_ok, .. } => {
            if !saved_ok {
                if let Some(v) = store.volumes.first() {
                    let letter = (v.volume_letter as char).to_ascii_uppercase();
                    set_watch_error(
                        letter,
                        Some("索引已自动压缩，但落盘失败（请检查磁盘空间）；下次启动将重试".into()),
                    );
                }
            }
        }
    }

    // 停止检查点：加载（可能几十秒）完成后若已收到 STOP，直接退出——
    // 大库加载不可中断，这里保证至多多等一轮加载而不是整套启动流程走完。
    if stop.load(Ordering::SeqCst) {
        info!("加载完成后检测到停机指令，跳过启动直接退出");
        return Ok(());
    }

    let engine = Arc::new(SearchEngine::new(store));
    let lifecycle=crate::runtime_lifecycle::Owner::attach(engine.clone(),index.clone(),stop.clone()).map_err(anyhow::Error::msg)?;
    let budget = if let Some(mib) = flags.usage_memory_mib {
        mib.checked_mul(1024 * 1024)
            .ok_or_else(|| anyhow::anyhow!("usage byte budget overflow"))?
    } else {
        use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut m = MEMORYSTATUSEX::default();
        m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        unsafe { GlobalMemoryStatusEx(&mut m) }.map_err(|e| {
            anyhow::anyhow!("available RAM query failed; specify --usage-memory-mib: {e}")
        })?;
        (m.ullAvailPhys / 4) as usize
    };
    info!(
        usage_extra_budget_bytes = budget,
        "logical tree budget (additional resident/staging, not total process RSS)"
    );
    if let Err(e) = engine.enable_usage(budget) {
        tracing::error!("logical usage tree unavailable: {e}; search continues without indexed usage");
    }
    {
        let mut g = slot.write().expect("EngineSlot 写锁中毒");
        *g = Some(engine.clone());
    }

    // legacy 多卷索引拓扑自动升级：2.4.x 旧库没有每卷自父根（顶层目录 parent_idx==0
    // 哨兵），usage（目录占用树）等新特性在旧拓扑上无法构建——启动期的 enable_usage
    // 在上面刚失败过（搜索不受影响，rebuild_scoped_identity_maps 已退回 slot-0 全局表）。
    // 装好新引擎后按卷顺序后台重建：rebuild_volume 走 MFT 重扫 + 物理摘除旧卷 + 合并
    // 换入（merge 的 +dir_base 平移会为每卷产生自父根），全部完成后索引即为新格式，
    // 最后一卷的 rebuild_volume 内置 enable_usage 重试自然成功。期间旧索引持续服务，
    // 冻结协议保证重建窗口的事件由 frozen 游标回放，零丢失。个别卷失败（如移动盘
    // 不在线）只保留旧数据并记日志，下次启动自动重试（部分升级后 roots 仍对不上
    // 卷数，legacy 标志继续置位）。
    // 注意：进度**不得**写 watch_error——Status.healthy 按 watch 位图汇总，任何卷带
    // error 都会把 healthy 打成 false，安装器 Verify 的 Wait-NewServiceReady 门会超时。
    if engine.index_store().legacy_global_scope && !flags.no_watch {
        let letters: Vec<char> = {
            let g = engine.index_store();
            g.volumes
                .iter()
                .map(|v| (v.volume_letter as char).to_ascii_uppercase())
                .collect()
        };
        info!(
            "检测到 2.4.x 旧版多卷索引拓扑（{} 卷 {}）：后台按卷全量重建，转换为新引擎格式（占用等新特性随后可用）…",
            letters.len(),
            letters.iter().map(|c| format!("{c}:")).collect::<Vec<_>>().join(" ")
        );
        let eng = engine.clone();
        let idx = index.clone();
        let total = letters.len();
        std::thread::Builder::new()
            .name("legacy-index-upgrade".into())
            .spawn(move || {
                // 20s 启动宽限：避开安装器 Verify 的就绪窗口（Wait-NewServiceReady 会
                // 轮询 Status；重建期间不可设置 watch_error——healthy 按位图汇总，
                // 任何卷带 error 都会把 healthy 打成 false，安装器 12s 门必超时）。
                // 进度只走日志，不打扰 healthy / 代数回执校验。
                std::thread::sleep(std::time::Duration::from_secs(20));
                let mut ok = 0usize;
                for letter in letters {
                    info!(
                        "卷 {letter} 旧拓扑升级：MFT 重扫开始（搜索走旧数据不受影响）…"
                    );
                    match rebuild_volume(&eng, letter, &idx) {
                        Ok(()) => {
                            ok += 1;
                            info!("卷 {letter} 拓扑升级完成");
                        }
                        Err(e) => {
                            error!("卷 {letter} 拓扑升级失败: {e:#}（保留旧数据，下次启动重试）");
                        }
                    }
                }
                if ok == total {
                    info!("旧版索引拓扑升级完成（{total} 卷）：目录占用树已随最后重建挂载");
                } else {
                    tracing::warn!(
                        "旧版索引拓扑升级部分完成（{ok}/{total}）：占用树仍不可用，失败卷下次启动自动重试"
                    );
                }
            })
            .map_err(|e| anyhow::anyhow!("spawn legacy-index-upgrade 失败: {e}"))?;
    }

    // 边车缺失 / 加载时被判不可信（重建窗口内崩溃过）：后台补建一次，
    // 本轮搜索先走全表扫描兜底，构建完成后自动切到剪枝路径。
    if !flags.no_watch && engine.index_store().trigram.is_none() {
        spawn_trigram_rebuild(engine.clone(), index.clone(), "启动时边车缺失或校验失败");
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
        if flags.no_watch {
            Vec::new()
        } else if g.volumes.is_empty() {
            let c = _volume
                .chars()
                .find(|ch| ch.is_ascii_alphabetic())
                .unwrap_or('C')
                .to_ascii_uppercase();
            vec![(format!("{}:", c), c as u8)]
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

    if flags.no_watch {
        crate::watch_health::set_watch_error(
            'F',
            Some("isolated no-watch mode: no real USN/MFT; fixture events only".into()),
        );
        if let Some(script) = flags.fixture_events.clone() {
            crate::usage_fixture::spawn_events(engine.clone(), script, stop.clone());
        }
    }
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
    let mut usn_handles: Vec<JoinHandle<anyhow::Result<()>>> = Vec::new();
    for (vol_path, letter) in volumes_watch {
        let index_for_watch = index.clone();
        let engine_watch = engine.clone();
        let stop_watch = stop.clone();
        usn_handles.push(std::thread::spawn(move || {
            let result = usn_watch_loop(
                engine_watch,
                vol_path,
                letter,
                index_for_watch,
                save_iv,
                stop_watch,
            );
            if let Err(e) = &result {
                crate::watch_health::set_watch_error(
                    letter as char,
                    Some(format!(
                        "watch mutation failed; cursor not acknowledged: {e}"
                    )),
                );
                error!("USN 监听线程退出 ({letter}): {e}");
            }
            result
        }));
    }

    // 停机等待：STOP 置位（或控制台模式的永不置位标志）→ 等 USN 线程 flush+落盘后返回。
    // pipe 线程 detached：进程退出时由 OS 收割（管道句柄随进程关闭）。
    // 之前这里 `pipe_thread.join()` 永久阻塞，服务停止完全靠 win_service 的 process::exit 硬杀。
    while !shutdown_requested(&stop, flags.shutdown_file.as_deref()) {
        std::thread::sleep(Duration::from_millis(50));
    }
    info!("Shutdown requested; waiting for watch workers without forced termination");
    let mut worker_errors = Vec::new();
    for h in usn_handles {
        // Do not detach unfinished writers on a deadline. SCM renews StopPending;
        // the installer must time out/refuse replacement, never kill this process.
        match h.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => worker_errors.push(format!("watch worker failed: {e:#}")),
            Err(_) => worker_errors.push("watch worker panicked during shutdown".into()),
        }
    }
    // An uncertain committed outcome MUST NOT unwind Owner/drop its RAM/fences.
    // A real joined worker failure is permanent until explicit operator recovery;
    // transient receipt/pointer IO is retried while the same sealed RAM lives.
    if !worker_errors.is_empty(){lifecycle.retain_terminal_failure(format!("watch worker shutdown failed: {}",worker_errors.join("; ")));}
    let prepared=loop{match lifecycle.prepared_stop(){Ok(v)=>break v,Err(e)=>{error!("STOP PENDING: terminal receipt verification failed; sealed RAM retained, no ACK: {e}");std::thread::sleep(Duration::from_secs(1));}}};
    if let Some(receipt)=prepared {
        info!(?receipt,"exact sealed terminal checkpoint retained through normal STOP; process_stopped remains externally observed");
        return Ok(());
    }
    let _stop_work=loop {
        match lifecycle.drain_stop(Instant::now()+Duration::from_secs(10)) {
            Ok(p)=>break p,
            Err(e)=>{error!("stop pending; RAM retained; runtime not drained: {e}");std::thread::sleep(Duration::from_secs(1));}
        }
    };
    // A successful IO observation is NOT an upgrade receipt: metadata/backfill/
    // stat producers still need a shared admission fence and drain in the next step.
    loop {
        let result = persist_shutdown_checkpoint(&engine, &index);
        match crate::shutdown::require_checkpoint_result(result, || engine.has_pending_persistence()) {
            Ok(())=>break,
            Err(e)=>{error!("STOP PENDING: save failed, RAM/IPC retained; retrying, NO ACK: {e}");std::thread::sleep(Duration::from_secs(5));}
        }
    }
    let runtime_errors=lifecycle.retained_errors();
    if !runtime_errors.is_empty(){anyhow::bail!("saved RAM but runtime source failure remains: {}",runtime_errors.join("; "));}
    if !worker_errors.is_empty(){anyhow::bail!("saved accepted RAM, but watch shutdown failed: {}",worker_errors.join("; "));}
    info!("Shutdown checkpoint IO complete; NOT a producer-fenced final RAM durability receipt");
    info!(counts=?findx2_windows::metadata_terminal_counts(), "metadata terminal buckets: missing/access/sharing/invalid_id/not_found/other/success");
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
    stop: Arc<AtomicBool>,
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
    let stat_worker = spawn_stat_worker(engine.clone(), vol_dev, stop.clone());

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
    // 返回实际 apply 的条数（0 = 无事发生），调用方据此置 dirty，避免空闲时也全量写盘。
    let flush_pending = |pending: &mut Vec<findx2_core::ChangeEvent>| -> anyhow::Result<usize> {
        let total = pending.len();
        if total == 0 { return Ok(0); }
        let (journal_id, last_usn) = {
            let g = engine.index_store();
            let v = g.volumes.iter().find(|v| v.volume_letter == volume_letter)
                .ok_or_else(|| anyhow::anyhow!("pending namespace volume unavailable"))?;
            (v.usn_journal_id, v.last_usn)
        };
        engine.apply_volume_batch(volume_letter, pending, journal_id, last_usn)
            .map_err(|e| {
                set_watch_error(letter, Some(format!("legacy namespace batch failed; cursor retained: {e}")));
                anyhow::anyhow!(e)
            })?;
        pending.clear();
        Ok(total)
    };

    // 脏计数：累计已 apply 但未提交的 ChangeEvent 条数。
    // 纯 Checkpoint（游标推进但无事件）只更新内存里的 last_usn，不触发落盘。
    // 提交条件 = 脏数达阈值（忙时及时落盘， bound 崩溃回放量）或间隔到（闲时兜底）。
    // Coalesced metadata delta cost scales with dirty slots, not total corpus size.
    let mut dirty_count: usize = 0;

    // Checkpoint coverage is independent of consistency; retries retain dirty state.
    let mut checkpoint_retry_after = None;
    let mut try_maybe_persist = |dirty_count: &mut usize,
                             last_save: &mut Instant,
                             engine: &Arc<SearchEngine>,
                             index_path: &PathBuf| -> anyhow::Result<()> {
        persist_periodic_checkpoint_retrying(engine, index_path, dirty_count, last_save,
            &mut checkpoint_retry_after, save_every);
        Ok(())
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
    let make_worker = |tx: mpsc::SyncSender<findx2_windows::UsnWatchMsg>|
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
        let (tx0, rx0) = mpsc::sync_channel::<findx2_windows::UsnWatchMsg>(1);
        rx = rx0;
        match make_worker(tx0) {
            Some(w) => w,
            None => {
                set_watch_error(letter, Some("索引中无卷元数据，等待…".into()));
                return Err(anyhow::anyhow!("索引中无卷元数据"));
            }
        }
    };

    // A fatal namespace failure fences all subsequent cursors. Do not consume a later
    // successful batch as proof the failed prefix committed. Search remains available;
    // restart/rebuild is required and resumes from the retained durable watermark.
    let watch_source=crate::runtime_lifecycle::watch_source(letter as u8).ok_or_else(||anyhow::anyhow!("invalid watch volume identity"))?;
    let mut namespace_gate = crate::watch_health::NamespaceGate::default();
    let mut unlink_resolver:Option<findx2_windows::NtfsUnlinkResolver>=None;
    let mut namespace_retry=crate::watch_health::NamespaceRetry::default();
    let mut namespace_retry_delay=Duration::from_millis(100);
    loop {
        // 停机检查：recv_timeout 只有 50ms 粒度，这里最多 50ms 响应一次。
        // 退出前 flush 内存 pending（游标停在最后 Checkpoint，journal 重放覆盖剩余变更）
        // Try a partial-coverage checkpoint without claiming crash atomicity.
        if stop.load(Ordering::SeqCst) {
            flush_pending(&mut pending)?;
            // 返回后 JoinedStatWorker 先断开发送端再 join。已经在内核里的 OpenFileById
            // 不会被打断；没打开的队列写成未验证。这不是时间上限。
            info!("卷 {letter} USN 监听线程停机退出");
            return Ok(());
        }
        let mut lifecycle=match crate::runtime_lifecycle::enter(&engine,watch_source) {Ok(p)=>p.require_result(),Err(_)=>return Ok(())};
        if namespace_gate.blocked {
            let _ = rx.recv_timeout(Duration::from_millis(100));
            std::thread::sleep(Duration::from_millis(100));
            lifecycle.success();
            continue;
        }
        // 重建冻结期（本卷）：排空丢弃，不 apply 不推进游标；journal 重放会补回。
        // 其它卷不受影响（标志按卷）。
        if is_volume_frozen(letter) {
            pending.clear();
            while rx.try_recv().is_ok() {}
            std::thread::sleep(Duration::from_millis(100));
            lifecycle.success();
            continue;
        }
        let (received,retry_binding)=match namespace_retry.take(){
            Some((events,journal_id,next_usn,binding))=>{
                std::thread::sleep(namespace_retry_delay);
                (Ok(findx2_windows::UsnWatchMsg::NamespaceBatch{events,journal_id,next_usn}),Some(binding))
            },None=>(rx.recv_timeout(flush_every),None)
        };
        match received {
            Ok(msg) => match msg {
                findx2_windows::UsnWatchMsg::NamespaceBatch {
                    events,
                    journal_id,
                    next_usn,
                } => {
                    dirty_count += flush_pending(&mut pending)?;
                    let binding=retry_binding.unwrap_or_else(||{let g=engine.index_store();(engine.metadata_epoch(),g.volumes.iter().find(|v|v.volume_letter==volume_letter).map(|v|v.volume_serial).unwrap_or(0))});
                    let prepared=(||->Result<Vec<findx2_core::ChangeEvent>,findx2_windows::NamespaceResolveError>{
                        if engine.metadata_epoch()!=binding.0{return Err("retained namespace batch belongs to replaced index generation".into());}
                        let (missing,parked)=engine.namespace_recovery_inputs(volume_letter,&events);
                        if missing.is_empty() && unlink_resolver.is_none(){return Ok(events.clone());}
                        if unlink_resolver.is_none(){
                            let serial={let g=engine.index_store();g.volumes.iter().find(|v|v.volume_letter==volume_letter).ok_or("unlink volume unavailable")?.volume_serial};
                            unlink_resolver=Some(findx2_windows::NtfsUnlinkResolver::new(&volume_path,volume_letter,serial,journal_id)?);
                        }
                        unlink_resolver.as_mut().unwrap().rewrite(&events,&parked,&missing,journal_id,|p|engine.namespace_parent_resolvable(volume_letter,p,&events),|id|engine.namespace_identity_live(volume_letter,id))
                    })();
                    let events=match prepared{Ok(e)=>{namespace_retry_delay=Duration::from_millis(100);e},Err(e)=>{
                        let detail=format!("namespace resolution failed; exact prefix/cursor retained: {e}");
                        tracing::warn!("watch={letter}: {detail}");
                        crate::win_service::append_service_log(&format!("watch={letter}: {detail}"));
                        set_watch_error(letter,Some(detail));
                        if e.requires_replay(){
                            // Drop receiver BEFORE joining: an old bounded sender must
                            // unblock normally. New native reader starts at retained disk
                            // cursor, reports real JournalGap, and existing scoped rebuild
                            // scans/replays before adopting a new RAM index generation.
                            let(tx_new,rx_new)=mpsc::sync_channel::<findx2_windows::UsnWatchMsg>(1);
                            let old=std::mem::replace(&mut rx,rx_new);drop(old);let _=worker.join();
                            worker=make_worker(tx_new).ok_or_else(||anyhow::anyhow!("exact watch volume unavailable for journal recovery"))?;
                            unlink_resolver=None;namespace_gate=crate::watch_health::NamespaceGate::default();
                        }else if e.retryable(){namespace_retry.hold((events,journal_id,next_usn,binding)).map_err(anyhow::Error::msg)?;namespace_retry_delay=(namespace_retry_delay*2).min(Duration::from_secs(1));}else{namespace_gate.blocked=true;}
                        lifecycle.error(e.to_string());continue;
                    }};
                    match namespace_gate.apply_bound(&engine, volume_letter, &events, journal_id, next_usn,Some(binding)) {
                        Ok(()) => {
                            set_watch_error(letter, engine.has_pending_namespace(volume_letter).then(||
                                format!("missing-parent namespace events pending; durable cursor pinned for restart replay; {}", engine.pending_namespace_diagnostics(volume_letter))));
                            dirty_count += events.len();
                            // Only successful atomic application of the real retained
                            // prefix repairs this volume. Mere timeouts/fence drains or
                            // another volume's healthy batch cannot clear its failure.
                            if !engine.has_pending_namespace(volume_letter){lifecycle.recovered_source();}
                        }
                        Err(e) => {
                            set_watch_error(
                                letter,
                                Some(format!("watch mutation failed; cursor not acknowledged: {e}")),
                            );
                            error!("USN batch not committed ({letter}); namespace fenced until restart/rebuild: {e}");
                            namespace_gate.blocked = true;
                            lifecycle.error(e);
                        }
                    }
                }
                findx2_windows::UsnWatchMsg::Event(ev) => {
                    if pending.is_empty() {
                        batch_started = Instant::now();
                    }
                    pending.push(ev);
                    if pending.len() >= USN_BATCH_MAX_EVENTS {
                        dirty_count += flush_pending(&mut pending)?;
                    }
                }
                findx2_windows::UsnWatchMsg::StatRefresh {
                    file_id,
                    file_id_128,
                } => {
                    dirty_count += flush_pending(&mut pending)?;
                    let _ = crate::stat_queue::offer_stat(stat_worker.tx(), &engine, volume_letter, file_id, file_id_128, &stop)?;
                }
                findx2_windows::UsnWatchMsg::StatBatch { items } => {
                    dirty_count += flush_pending(&mut pending)?;
                    // 队列满以后，其余已存在的文件写成持久未验证，不打开、不挡住下一次读取。
                    let mut room = true;
                    for (file_id, file_id_128) in items {
                        if room {
                            room = crate::stat_queue::offer_stat(stat_worker.tx(), &engine, volume_letter, file_id, file_id_128, &stop)?;
                        } else if file_id_128.is_some_and(|id| id[8..].iter().any(|&b| b != 0)) {
                            anyhow::bail!("full 128-bit stat binding unresolved; refusing low64 alias");
                        } else {
                            engine.retain_unverified_metadata(volume_letter, file_id).map_err(anyhow::Error::msg)?;
                        }
                    }
                }
                findx2_windows::UsnWatchMsg::Checkpoint {
                    journal_id,
                    next_usn,
                } => {
                    dirty_count += flush_pending(&mut pending)?;
                    // 停车事件在重放消费后仍无法解析 = 引用已被替换的 store 快照
                    // （升级/回滚换库窗口的典型产物；USN 单调序保证父目录的创建事件
                    // 不可能晚于子事件，继续钉死游标只会让 Status.healthy 永久 false，
                    // 安装器与升级器全数卡死——本机实测 64 条 C: 临时文件事件）。
                    // 丢弃放行；父目录当前可解析的事件保留；该区域由全卷重建
                    // （legacy 拓扑升级 / journal 断档自愈）兜底。
                    if engine.has_pending_namespace(volume_letter) {
                        let dropped = engine.discard_unresolved_namespace(volume_letter);
                        if dropped > 0 {
                            info!(
                                "卷 {letter} 丢弃 {dropped} 条引用已替换快照的 namespace 停车事件，游标放行"
                            );
                        }
                        if engine.has_pending_namespace(volume_letter) {
                            set_watch_error(letter, Some(format!("missing-parent namespace events pending; durable cursor pinned for restart replay; {}", engine.pending_namespace_diagnostics(volume_letter))));
                            lifecycle.error("unresolved namespace parents; cursor retained".into());
                            continue;
                        }
                        set_watch_error(letter, None);
                    }
                    if let Err(e) = namespace_gate.apply(&engine, volume_letter, &[], journal_id, next_usn) {
                        set_watch_error(letter, Some(format!("checkpoint rejected; namespace fenced: {e}")));
                        namespace_gate.blocked = true;
                        lifecycle.error(e);
                        continue;
                    }
                    // 存活确认：收到 journal 心跳即视为健康，复位退避/重建计数。
                    backoff = Duration::from_secs(1);
                    gap_rebuilds = 0;
                    if let Some(w) = probe_journal_wrap_warning(&volume_path, letter, next_usn) {
                        set_watch_error(letter, Some(w));
                    } else {
                        set_watch_error(letter, None);
                        // Actual native reader + accepted current-journal checkpoint
                        // proves reconnect/rebuild recovery, not an idle timer.
                        lifecycle.recovered_source();
                    }
                    // 注：这里只改 USN 游标，不影响搜索结果/排序，故不 bump revision
                    //（否则每次 checkpoint 都会误杀分页缓存）。真正改条目的是上面的 flush_pending。
                    // v6：落盘走增量提交（KB 级），脏门控在 try_maybe_persist 里；
                    // Accepted partial metadata is mirrored and can be checkpointed.
                    try_maybe_persist(&mut dirty_count, &mut last_save, &engine, &index_path)?;
                }
            },
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if !pending.is_empty() && batch_started.elapsed() >= flush_every {
                    dirty_count += flush_pending(&mut pending)?;
                }
                try_maybe_persist(&mut dirty_count, &mut last_save, &engine, &index_path)?;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // worker 死了：join 判因后重启（先排空残留消息已由 Disconnected 语义保证）。
                dirty_count += flush_pending(&mut pending)?;
                let exit = worker.join();
                let reason = match &exit {
                    Ok(Ok(())) => "watch 线程异常返回（理论不可达）".to_string(),
                    Ok(Err(e)) => format!("{e}"),
                    Err(_) => "watch 线程 panic".to_string(),
                };
                lifecycle.error(reason.clone());
                let is_gap = matches!(&exit, Ok(Err(findx2_core::Error::JournalGap(_))));
                if is_gap && gap_rebuilds < 3 {
                    gap_rebuilds += 1;
                    set_watch_error(
                        letter,
                        Some(format!("USN 日志断档，全卷重建中（{gap_rebuilds}/3）…")),
                    );
                    error!("卷 {letter} {reason}，触发全卷重建");
                    if let Err(re) = rebuild_volume(&engine, letter, &index_path) {
                        set_watch_error(letter, Some(format!("重建失败: {re}，退避重试…")));
                        error!("卷 {letter} 重建失败: {re:#}");
                    } else {
                        info!("卷 {letter} 重建完成，恢复增量监听");
                        set_watch_error(letter, None);
                        // 成功只说明新扫描已换入。若游标仍落在 journal 已覆写的
                        // 区间，下一轮会立刻再断档。计数留到真正收到 Checkpoint
                        // 再清零，否则状态栏永远停在「1/3」，重建会无限循环。
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
                    let (tx_new, rx_new) = mpsc::sync_channel::<findx2_windows::UsnWatchMsg>(1);
                    rx = rx_new;
                    tx_new
                }) {
                    Some(w) => {
                        worker = w;
                        unlink_resolver=None;namespace_gate=crate::watch_health::NamespaceGate::default();
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
            dirty_count += flush_pending(&mut pending)?;
        }
        dirty_count += flush_pending(&mut pending)?; // No accepted legacy batch crosses the prepare barrier.
        lifecycle.success();
    }
}

/// 单卷全量重建（JournalGap 后的退路；调用方 usn_watch_loop 负责 freeze/解冻之外的全部）。
///
/// 协议（重建期间零丢失）：
/// 1. 入口即 `freeze_volume(letter)`：watch 线程排空丢弃、不推进游标；
/// 2. 记下内存 `last_usn` 为 frozen 点（pending 已排空、checkpoint 先 flush 的不变量
///    保证 frozen ≤ 所有丢弃事件的 USN，重放全覆盖）；
/// 3. 重新 MFT 枚举 + build 新 store（数分钟，无锁；旧索引继续服务）；
/// 4. 读锁下克隆旧 store → 离线（无锁）：多卷时摘除旧卷再合并；单卷时整库换成新扫描
///    （`remove_volume_entries` 对「区间=整库」会拒绝，不能追加）。`excluded_dirs` 从旧库恢复；
/// 5. 写锁换入（µs）+ revision bump。`frozen` 仍在 journal 可读范围才回绕重放，
///    否则保留扫描结束时的 NextUsn（停机过久时旧游标已被覆写，回绕会立刻再断档）；
/// 6. 全量落盘 + trigram 边车重建；解冻后 worker 从换入后的游标续听
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

    let (frozen, frozen_journal_id, expected_serial) = {
        let g = engine.index_store();
        g.volumes
            .iter()
            .find(|v| (v.volume_letter as char).to_ascii_uppercase() == letter)
            .map(|v| (v.last_usn, v.usn_journal_id, v.volume_serial))
            .ok_or_else(||anyhow::anyhow!("exact rebuild volume absent; no foreign fallback"))?
    };
    info!("卷 {letter} 开始全量重建（frozen 游标 {frozen}，旧索引继续服务）…");

    // 1) 重新扫描（fast 首遍；size/mtime 由回填补——合并后 ready=false 会自动触发）。
    let serial = findx2_windows::get_volume_serial_number(&vol_str)
        .map_err(|e|anyhow::anyhow!("rebuild volume identity unavailable: {e}"))?;
    if serial!=expected_serial{return Err(anyhow::anyhow!("rebuild refused foreign volume identity"));}
    // Capture the NEW journal before enumeration, not after it. A changed journal
    // cannot rewind to the old cursor; using an end-of-scan watermark loses changes
    // that happened behind the scanner. Existing replay uses this pre-scan mark.
    let usn = findx2_windows::UsnJournalWatcher::new(&vol_str)
        .probe()
        .map_err(|e| anyhow::anyhow!("卷 {letter} journal 探测失败: {e}"))?;
    let (files, dirs) = findx2_windows::scan_volume_fast(&vol_str)
        .map_err(|e| anyhow::anyhow!("卷 {letter} MFT 重枚举失败: {e}"))?;
    let after_serial=findx2_windows::get_volume_serial_number(&vol_str)
        .map_err(|e|anyhow::anyhow!("rebuild post-scan volume identity unavailable: {e}"))?;
    let(after_journal,_,_)=findx2_windows::query_journal_state(&vol_str)
        .map_err(|e|anyhow::anyhow!("rebuild post-scan journal unavailable: {e}"))?;
    if after_serial!=serial||after_journal!=usn.journal_id{return Err(anyhow::anyhow!("rebuild identity/journal changed during native scan; old index retained"));}
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

    // 3) 写锁换入 + 游标。
    // 扫描期间的增量只有在 frozen 仍落在 journal 可读范围内时才回绕重放。
    // 服务停太久、FirstUsn 已经越过 frozen 时，回绕会让下一轮立刻再断档，
    // 全卷重建无限循环。这时新扫描自带的 NextUsn 才是能接上的游标。
    {
        let mut g = merged;
        // v6：整库换入后旧分段文件整体失效。恢复 tracking 并记 FullResync，
        // 下次 commit 走全量分段重写（换入的 store 可能 tracking 已关/日志为空，
        // 不显式标记就会静默分叉）。
        g.set_track_mutations(true);
        g.mark_full_resync();
        if let Some(v) = g
            .volumes
            .iter_mut()
            .find(|v| (v.volume_letter as char).to_ascii_uppercase() == letter)
        {
            let fresh_usn = v.last_usn;
            let resume = match findx2_windows::query_journal_state(&vol_str) {
                Ok((jid, first, _)) => {
                    resume_usn_after_rebuild(frozen, frozen_journal_id, fresh_usn, jid, first)
                }
                Err(e) => {
                    info!("卷 {letter} 重建后无法复查 journal（{e}），保留扫描游标 {fresh_usn}");
                    fresh_usn
                }
            };
            if resume == frozen && resume != fresh_usn {
                info!("卷 {letter} 回绕到 frozen={frozen} 重放扫描期间的增量");
            } else if resume != frozen {
                info!("卷 {letter} 断档游标 {frozen} 已不可读，保留扫描结束时的游标 {resume}");
            }
            v.last_usn = resume;
        }
        engine.replace_index_store(g);
    }
    if let Err(e) = engine.enable_usage(engine.usage_budget()) {
        tracing::error!("logical usage tree unavailable after rebuild: {e}; search continues");
    }

    // 4) 落盘 + trigram 重建（合并后 trigram=None，先回全表扫描，重建完自动切回剪枝）。
    persist_index(engine, index_path)?;
    spawn_trigram_rebuild(
        engine.clone(),
        index_path.to_path_buf(),
        "单卷重建后边车重建",
    );
    // 合并后 metadata_ready=false（fast 重枚举），必须重新拉起回填；旧 overlay 下标已失效。
    crate::backfill::spawn_backfill(Arc::clone(engine), index_path.to_path_buf());
    info!("卷 {letter} 重建完成（frozen={frozen}），恢复增量监听");
    Ok(())
}

/// 离线合并：**物理摘除**旧卷数据（不打墓碑）+ 新 store 后挂合并。
///
/// 历史：这里原本对旧卷区间逐条 `delete_entry`（只打墓碑不删除），因为物理删除
/// 要牵动 `frns` / `dir_idx` / `dir_index` / `dir_path_ranges` 全套下标，当时判断
/// 「重建成本高、容易破坏不变量」而选择打墓碑。代价是**条目数只增不减**：
/// 每次全卷重建都把整卷旧数据留成永不回收的垃圾，实测某库 3.09 亿条目里 98.75%
/// 是墓碑、17.7 GB 里约 16.5 GB 是废数据、加载要 80 s。
///
/// 现在 core 提供了 [`findx2_core::IndexStore::remove_volume_entries`]，用「先算保留集、
/// 再整体重写」的方式统一重映射全部下标（不是逐个 remove），所以既物理回收了空间，
/// 又不必手工维护下标一致性。
///
/// 单卷库例外：该卷区间就是整库，`remove_volume_entries` 会拒绝摘除（防止误清空）。
/// 若这时仍走「追加合并」，每次全卷重建都把同一卷再挂一份，条目数翻倍，并且
/// 断档未消除时会一直翻。单卷重建的结果就是这次扫描，直接换掉旧库。
fn merge_rebuilt_volume(
    old: findx2_core::IndexStore,
    mut fresh: findx2_core::IndexStore,
    letter: char,
) -> anyhow::Result<findx2_core::IndexStore> {
    let up = letter.to_ascii_uppercase();
    let has_letter = old
        .volumes
        .iter()
        .any(|v| (v.volume_letter as char).to_ascii_uppercase() == up);
    let sole = has_letter
        && old
            .volumes
            .iter()
            .all(|v| (v.volume_letter as char).to_ascii_uppercase() == up);
    if sole {
        info!(
            "卷 {letter} 是索引中的唯一卷，整库替换为新扫描（{} 条 → {} 条）",
            old.entry_count(),
            fresh.entry_count()
        );
        fresh.excluded_dirs = old.excluded_dirs.clone();
        let excluded = fresh.excluded_dirs.clone();
        let marked = fresh.mark_excluded_entries(&excluded);
        if marked > 0 {
            info!("卷 {letter} 重建后重新套用排除目录，标记 {marked} 条");
        }
        return Ok(fresh);
    }

    let mut old = old;
    let removed = old.remove_volume_entries(letter);
    if removed == 0 {
        info!("卷 {letter} 不在旧索引中，重建退化为纯追加合并");
    } else {
        info!(
            "卷 {letter} 旧数据已物理摘除 {} 条（不打墓碑），旧库剩余 {} 条",
            removed,
            old.entry_count()
        );
    }
    let excluded = old.excluded_dirs.clone();
    let mut merged = findx2_core::merge_index_stores(vec![old, fresh])
        .map_err(|e| anyhow::anyhow!("卷 {letter} 索引合并失败: {e}"))?;
    merged.excluded_dirs = excluded.clone();
    let marked = merged.mark_excluded_entries(&excluded);
    if marked > 0 {
        info!("卷 {letter} 重建后重新套用排除目录，标记 {marked} 条");
    }
    Ok(merged)
}

/// 重建换入后用哪条 USN 续听。
///
/// `frozen` 仍在当前 journal 里（ID 相同且 `frozen >= FirstUsn`）时回绕，
/// 重放扫描期间的增量。否则 journal 已经把这条游标覆写掉，回绕会让下一轮
/// 立刻再报断档；改用扫描结束时读到的 `NextUsn`。
fn resume_usn_after_rebuild(
    frozen: u64,
    frozen_journal_id: u64,
    fresh_usn: u64,
    live_journal_id: u64,
    first_usn: i64,
) -> u64 {
    if frozen_journal_id == live_journal_id && (frozen as i64) >= first_usn {
        frozen
    } else {
        fresh_usn
    }
}

/// P2 journal 覆写预警：FirstUsn 逼近尚未读到的游标时返回状态栏文案。
///
/// 不能用 `next - cursor` 当「剩余」。监听追上时 cursor ≈ next，该值恒为 0，
/// 而 `next - first`（已占用跨度）会随文件活动一直涨——状态栏就会显示
/// 「剩余 0 / 分母狂增」，纯属误报。
///
/// 真正有风险的是：我们还落后（next > cursor），且 FirstUsn 已经逼近 cursor
/// （历史缓冲 < 占用跨度的 5%）。追上后覆写的是早已处理过的旧记录，不必报警。
fn probe_journal_wrap_warning(volume_path: &str, letter: char, cursor_usn: u64) -> Option<String> {
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
        tracing::warn!("卷 {letter} USN journal 未读游标接近覆写前沿 (缓冲 {slack}/{span})");
        return Some(format!(
            "USN journal 未读记录即将被覆写（缓冲 {slack}/{span}），停机过久会触发全量重建"
        ));
    }
    None
}

fn persist_shutdown_checkpoint(engine: &SearchEngine, path: &Path) -> anyhow::Result<bool> {
    // No paged overlay fold: accepted producers mirror into the store.
    persist_index(engine, path)
}

// Disk failures are not namespace failures: keep the watcher alive and retry
// independently of last successful save. Do not hot-loop full repair on ENOSPC.
fn persist_periodic_checkpoint_retrying(
    engine: &Arc<SearchEngine>, path: &Path, dirty_count: &mut usize,
    last_save: &mut Instant, retry_after: &mut Option<Instant>, save_every: Duration,
) {
    if retry_after.is_some_and(|next| Instant::now() < next) { return; }
    match persist_periodic_checkpoint(engine, path, dirty_count, last_save, save_every) {
        Ok(()) => *retry_after = None,
        Err(error) => {
            *retry_after = Some(Instant::now() + Duration::from_secs(5));
            tracing::warn!(%error, state="waiting", dirty=true, waiting="retry_backoff",
                retry_after_ms=5000, durable=false,
                "Checkpoint IO failure does not terminate the namespace watcher");
        }
    }
}

fn persist_periodic_checkpoint(
    engine: &Arc<SearchEngine>, path: &Path, dirty_count: &mut usize,
    last_save: &mut Instant, save_every: Duration,
) -> anyhow::Result<()> {
    const THRESHOLD: usize = 4096;
    if *dirty_count == 0 && !engine.has_pending_persistence() { return Ok(()); }
    if *dirty_count < THRESHOLD && last_save.elapsed() < save_every { return Ok(()); }
    if persist_index(engine, path)? {
        *dirty_count = 0;
        *last_save = Instant::now();
        if engine.metadata_ready() { maybe_rebuild_trigram(engine, path); }
    }
    Ok(())
}

#[derive(Default)]
struct PersistenceIoHealth {
    last_io_success_ms: Option<u128>,
    last_io_failure_ms: Option<u128>,
    last_error: Option<String>,
}
fn persistence_io_health() -> &'static std::sync::Mutex<PersistenceIoHealth> {
    static HEALTH: std::sync::OnceLock<std::sync::Mutex<PersistenceIoHealth>> = std::sync::OnceLock::new();
    HEALTH.get_or_init(Default::default)
}

pub(crate) fn last_persistence_error() -> Option<String> {
    persistence_io_health().lock().unwrap_or_else(|e| e.into_inner()).last_error.clone()
}

// Retain a drained batch on error AND unwinding. Recover the writer mutex after
// poison so a panic cannot silently disable all future persistence attempts.
struct UnacknowledgedBatch<'a> {
    engine: &'a SearchEngine,
    batch: Option<findx2_core::CommitBatch>,
}
impl Drop for UnacknowledgedBatch<'_> {
    fn drop(&mut self) {
        if let Some(batch) = self.batch.take() { self.engine.restore_persistence_batch(batch); }
    }
}

pub(crate) fn persist_index(engine: &SearchEngine, path: &Path) -> anyhow::Result<bool> {
    persist_index_with(engine, path, |layout, batch| {
        v6_try_commit(layout, batch).map_err(|e| anyhow::anyhow!("{e:#}"))
    })
}

pub(crate) fn persist_index_with(
    engine: &SearchEngine, path: &Path,
    writer: impl FnOnce(&V6Layout, &findx2_core::CommitBatch) -> anyhow::Result<CommitOutcome>,
) -> anyhow::Result<bool> {
    let lifecycle=crate::runtime_lifecycle::enter(engine,"persister").map_err(anyhow::Error::msg)?.require_result();
    lifecycle.finish_persistence(persist_index_with_inner(engine,path,writer))
}
fn persist_index_with_inner(
    engine:&SearchEngine,path:&Path,
    writer:impl FnOnce(&V6Layout,&findx2_core::CommitBatch)->anyhow::Result<CommitOutcome>,
)->anyhow::Result<bool>{
    let _guard = match persist_lock().try_lock() {
        Ok(g) => g,
        Err(std::sync::TryLockError::Poisoned(e)) => {
            tracing::warn!("Recovering checkpoint writer after panic; retained batch requires full repair");
            e.into_inner()
        }
        Err(std::sync::TryLockError::WouldBlock) => {
            tracing::debug!(state="waiting", dirty=true, waiting="writer_busy", durable=false,
                "Checkpoint deferred; dirty state retained");
            return Ok(false);
        }
    };
    persist_index_locked(engine, path, writer)
}

/// The metadata finalizer keeps its producer permit while the existing writer
/// finishes. This is a mutex wait, not success-on-busy or a polling timeout.
/// false means obsolete epoch, NEVER a durability receipt for that epoch.
pub(crate) fn persist_metadata_final(engine: &SearchEngine, path: &Path, epoch: Option<u64>) -> anyhow::Result<bool> {
    persist_metadata_final_with(engine, path, epoch, |layout, batch| {
        v6_try_commit(layout, batch).map_err(|e| anyhow::anyhow!("{e:#}"))
    })
}
fn persist_metadata_final_with(
    engine: &SearchEngine, path: &Path, epoch: Option<u64>,
    writer: impl FnOnce(&V6Layout, &findx2_core::CommitBatch) -> anyhow::Result<CommitOutcome>,
) -> anyhow::Result<bool> {
    // No producer is joined and no admission lock is held while waiting. The
    // caller already owns an active finalizer, so STOP must drain it, not free RAM.
    let _guard = persist_lock().lock().unwrap_or_else(|e| e.into_inner());
    if epoch.is_some_and(|e| engine.metadata_epoch() != e) { return Ok(false); }
    anyhow::ensure!(engine.metadata_ready(), "metadata coverage changed before final commit");
    persist_index_locked(engine, path, writer)?;
    // An epoch replacement during real IO cannot complete the old finalizer.
    if epoch.is_some_and(|e| engine.metadata_epoch() != e) { return Ok(false); }
    anyhow::ensure!(engine.metadata_ready(), "metadata coverage changed during final commit");
    Ok(true)
}
fn persist_index_locked(
    engine: &SearchEngine, path: &Path,
    writer: impl FnOnce(&V6Layout, &findx2_core::CommitBatch) -> anyhow::Result<CommitOutcome>,
) -> anyhow::Result<bool> {
    let Some(batch) = engine.take_persistence_batch() else { return Ok(true); };
    let mut pending = UnacknowledgedBatch { engine, batch: Some(batch) };
    let batch = pending.batch.as_ref().unwrap();
    let layout = V6Layout::for_index(path);
    let started = Instant::now();
    let result = (|| -> anyhow::Result<()> {
        match writer(&layout, batch)? {
            CommitOutcome::Delta { applied, bytes, generation } => {
                tracing::debug!(applied, bytes, generation, "Checkpoint delta IO completed");
            }
            CommitOutcome::NeedFull { reason } => {
                info!(%reason, "Checkpoint requires full repair/snapshot");
                let store = engine.index_store();
                let generation = v6_save_full(&layout, &store).map_err(|e| anyhow::anyhow!("{e:#}"))?;
                info!(generation, "Checkpoint full snapshot IO completed");
            }
        }
        Ok(())
    })();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
    let mut health = persistence_io_health().lock().unwrap_or_else(|e| e.into_inner());
    match result {
        Ok(()) => {
            pending.batch.take(); // acknowledge only after the ENTIRE writer succeeds
            health.last_io_success_ms = Some(now);
            health.last_error = None;
            info!(state="io_completed", dirty=engine.has_pending_persistence(),
                metadata_ready=engine.metadata_ready(),
                last_io_success_ms=?health.last_io_success_ms, last_io_failure_ms=?health.last_io_failure_ms,
                elapsed_ms=started.elapsed().as_millis() as u64,
                durable=true, transactional=true,
                "Checkpoint generation published; protocol durable, not a certified physical power-loss result");
            Ok(true)
        }
        Err(error) => {
            drop(pending); // restore first, including mutations arriving during IO
            health.last_io_failure_ms = Some(now);
            health.last_error = Some(format!("{error:#}"));
            tracing::error!(state="failed", dirty=true, retry="full_snapshot", durable=false,
                metadata_ready=engine.metadata_ready(),
                last_io_success_ms=?health.last_io_success_ms, last_io_failure_ms=?health.last_io_failure_ms,
                error=%error, "Checkpoint failed; batch retained for repair");
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn shutdown_marker_publishes_shared_stop_and_releases_fake_watcher() {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let marker = std::env::temp_dir().join(format!("findx-shutdown-unit-{}-{stamp}.signal", std::process::id()));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        assert!(!super::shutdown_requested(&stop, Some(&marker)));
        let worker_stop = stop.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            while !worker_stop.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            tx.send(()).unwrap();
        });
        std::fs::write(&marker, b"owned test stop").unwrap();
        let started = std::time::Instant::now();
        assert!(super::shutdown_requested(&stop, Some(&marker)));
        assert!(stop.load(std::sync::atomic::Ordering::SeqCst));
        rx.recv_timeout(std::time::Duration::from_secs(1)).expect("watcher must observe stop, not sit through the ten-second grace");
        worker.join().unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        std::fs::remove_file(marker).unwrap();
    }
    #[test]
    fn preexisting_external_stop_does_not_require_a_marker() {
        let stop = std::sync::atomic::AtomicBool::new(true);
        assert!(super::shutdown_requested(&stop, None));
    }
    use super::{merge_rebuilt_volume, resume_usn_after_rebuild};
    use findx2_core::{IndexBuilder, RawEntry};

    fn tiny_store(letter: u8, journal: u64, usn: u64, name: &str) -> findx2_core::IndexStore {
        let dirs = vec![RawEntry {
            file_id: 100,
            file_id_128: None,
            parent_id: 0,
            name: "Users".into(),
            size: 0,
            mtime: 0,
            ctime: 0,
            attrs: 0x10,
            is_dir: true,
        }];
        let files = vec![RawEntry {
            file_id: 1,
            file_id_128: None,
            parent_id: 100,
            name: name.into(),
            size: 10,
            mtime: 1,
            ctime: 1,
            attrs: 0,
            is_dir: false,
        }];
        IndexBuilder::new(letter, 1, journal, usn)
            .build_from_raw(files, dirs, true)
            .unwrap()
    }

    #[test]
    fn partial_backfill_shutdown_persists_namespace_cursor_without_claiming_ready() {
        let dir=std::env::temp_dir().join(format!("findx-partial-shutdown-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();let path=dir.join("synthetic.bin");let layout=findx2_core::V6Layout::for_index(&path);
        let mut store=tiny_store(b'C',7,100,"public-file.txt");store.metadata_ready=false;store.set_track_mutations(true);
        findx2_core::v6_save_full(&layout,&store).unwrap();
        let engine=findx2_core::SearchEngine::new(store);engine.enable_usage(16*1024*1024).unwrap();
        let ticket=engine.metadata_ticket_by_id(b'C',1).unwrap();assert!(engine.commit_metadata(ticket,456,0,0).unwrap());
        engine.apply_volume_batch(b'C',&[findx2_core::ChangeEvent::Create {entry:RawEntry{file_id:2,file_id_128:None,parent_id:100,name:"public-new.txt".into(),size:123,mtime:0,ctime:0,attrs:0,is_dir:false}}],7,200).unwrap();
        assert!(super::persist_shutdown_checkpoint(&engine,&path).unwrap());
        let restored=findx2_core::v6_load(&layout).unwrap();
        assert!(!restored.metadata_ready);assert_eq!(restored.volumes[0].last_usn,200);
        let i=restored.entry_on_volume(b'C',1).unwrap();assert_eq!(restored.entries[i as usize].size,456);
        assert!(restored.entry_on_volume(b'C',2).is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }



    #[test]
    fn shutdown_actual_writer_faults_reach_expected_stage_and_ipc_without_ack() {
        use findx2_core::PersistFault;
        for (label, fault, expected) in [
            ("quota", PersistFault::DiskFull { max_bytes: 0 }, "disk full (injected quota"),
            ("short", PersistFault::ShortWrite { file: "entries.dat".into(), max_bytes: 0 }, "injected short write: entries.dat"),
            ("access", PersistFault::AccessDenied { file: "entries.dat".into() }, "injected access failure: entries.dat"),
            ("synced", PersistFault::AfterDataSync, "injected failure after data sync"),
            ("before-publish", PersistFault::BeforePublish, "injected failure before publish"),
            ("rename", PersistFault::RenameFail, "injected rename failure"),
        ] {
            let (path, engine) = checkpoint_fixture(label);
            let layout=findx2_core::V6Layout::for_index(&path);
            let pointer=layout.dir.join("published.json");
            let before=std::fs::read(&pointer).unwrap();
            checkpoint_patch(&engine,456);
            findx2_core::set_persist_fault(fault);
            let io=super::persist_shutdown_checkpoint(&engine,&path);
            findx2_core::clear_persist_fault();
            let error=format!("{:#}",io.as_ref().expect_err("fault must reach actual writer"));
            assert!(error.contains(expected),"{label} reached wrong failure stage: {error}");
            assert!(matches!(crate::shutdown::require_checkpoint_result(io, || engine.has_pending_persistence()), Err(crate::shutdown::CheckpointFailure::Writer(_))));
            assert!(engine.has_pending_persistence());
            assert_eq!(std::fs::read(&pointer).unwrap(),before,"failure must not replace published pointer");
            assert_eq!(restored_size(&path),10,"failed save must retain prior complete generation");
            let slot=std::sync::Arc::new(std::sync::RwLock::new(Some(engine.clone())));
            match crate::ipc_dispatch::process_request(&slot,findx2_ipc::IpcRequest::UpgradeCapabilities { protocol_version: 1 }) {
                findx2_ipc::IpcResponse::UpgradeCapabilitiesResult { durable_prepare:false, last_checkpoint_error:Some(e), .. } => assert!(e.contains(expected),"IPC lost actual cause: {e}"),
                response=>panic!("missing failure or falsely enabled prepare: {response:?}"),
            }
            assert!(super::persist_shutdown_checkpoint(&engine,&path).unwrap());
            assert!(!engine.has_pending_persistence());
            assert_eq!(restored_size(&path),456);
            assert!(super::last_persistence_error().is_none());
            std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        }
    }

    fn checkpoint_fixture(label: &str) -> (std::path::PathBuf, std::sync::Arc<findx2_core::SearchEngine>) {
        let dir = std::env::temp_dir().join(format!("findx-t153-{label}-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let path=dir.join("synthetic.bin");
        let mut store=tiny_store(b'C',7,100,"public-file.txt");
        store.metadata_ready=false;store.set_track_mutations(true);
        findx2_core::v6_save_full(&findx2_core::V6Layout::for_index(&path),&store).unwrap();
        let engine=std::sync::Arc::new(findx2_core::SearchEngine::new(store));
        engine.enable_usage(16*1024*1024).unwrap();
        (path,engine)
    }
    fn metadata_finalizer_fixture(label: &str) -> (std::path::PathBuf, std::sync::Arc<findx2_core::SearchEngine>, crate::runtime_lifecycle::Owner) {
        let (path,e)=checkpoint_fixture(label);
        let owner=crate::runtime_lifecycle::Owner::attach(e.clone(),path.clone(),std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))).unwrap();
        checkpoint_patch(&e,456);
        assert!(e.complete_metadata_if_covered(e.metadata_epoch()).unwrap());
        (path,e,owner)
    }
    #[test]
    fn metadata_finalizer_real_writer_contention_retains_owner_then_commits() {
        use std::{sync::mpsc,thread,time::Duration};
        let (path,e,owner)=metadata_finalizer_fixture("finalizer-busy");
        let (entered,seen)=mpsc::channel();let (release,go)=mpsc::channel();
        let w=e.clone();let p=path.clone();
        let writer=thread::spawn(move||super::persist_index_with(&w,&p,|layout,batch|{
            entered.send(()).unwrap();go.recv_timeout(Duration::from_secs(5)).unwrap();
            findx2_core::v6_try_commit(layout,batch).map_err(|e|anyhow::anyhow!("{e:#}"))
        }));
        seen.recv_timeout(Duration::from_secs(5)).unwrap();
        let permit=crate::runtime_lifecycle::reserve(&e,"metadata-finalizer").unwrap().unwrap().require_result();
        let (done,result)=mpsc::channel();let f=e.clone();let p=path.clone();let epoch=e.metadata_epoch();
        let finalizer=thread::spawn(move||{let activity=permit.activate();done.send(activity.finish_persistence(super::persist_metadata_final(&f,&p,Some(epoch)))).unwrap();});
        assert!(result.recv_timeout(Duration::from_millis(40)).is_err());
        assert!(owner.retained_errors().is_empty());
        release.send(()).unwrap();assert!(writer.join().unwrap().unwrap());
        assert!(result.recv_timeout(Duration::from_secs(5)).unwrap().unwrap());finalizer.join().unwrap();
        assert!(owner.retained_errors().is_empty());
        let disk=findx2_core::v6_load(&findx2_core::V6Layout::for_index(&path)).unwrap();assert!(disk.metadata_ready);
        assert_eq!(disk.entries[disk.entry_on_volume(b'C',1).unwrap() as usize].size,456);
        drop(owner);std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn metadata_finalizer_epoch_replaced_while_waiting_or_during_io_cannot_complete() {
        use std::{sync::mpsc,thread,time::Duration};
        let (path,e,owner)=metadata_finalizer_fixture("finalizer-epoch");let epoch=e.metadata_epoch();
        let guard=super::persist_lock().lock().unwrap_or_else(|e|e.into_inner());
        let f=e.clone();let p=path.clone();let (started,begin)=mpsc::channel();
        let job=thread::spawn(move||{started.send(()).unwrap();super::persist_metadata_final_with(&f,&p,Some(epoch),|_,_|panic!("obsolete writer must not run"))});
        begin.recv_timeout(Duration::from_secs(5)).unwrap();e.enable_usage(16*1024*1024).unwrap();drop(guard);
        assert!(!job.join().unwrap().unwrap());
        checkpoint_patch(&e,789);assert!(e.complete_metadata_if_covered(e.metadata_epoch()).unwrap());let epoch=e.metadata_epoch();
        let replacement=std::cell::Cell::new(None);
        assert!(!super::persist_metadata_final_with(&e,&path,Some(epoch),|layout,batch|{
            let outcome=findx2_core::v6_try_commit(layout,batch).map_err(|e|anyhow::anyhow!("{e:#}"))?;
            e.enable_usage(16*1024*1024).unwrap();replacement.set(Some((e.metadata_epoch(),e.metadata_ready())));Ok(outcome)
        }).unwrap());
        assert_ne!(epoch,e.metadata_epoch());
        // Re-enabling tracking over already complete metadata need not make the
        // new store incomplete. The old finalizer must leave its actual state
        // unchanged and still MUST return obsolete, not a completion receipt.
        assert_eq!(Some((e.metadata_epoch(),e.metadata_ready())),replacement.get());
        drop(owner);std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn metadata_finalizer_real_writer_failure_retains_ram_and_requires_real_retry() {
        let (path,e,owner)=metadata_finalizer_fixture("finalizer-failure");let epoch=e.metadata_epoch();
        let permit=crate::runtime_lifecycle::reserve(&e,"metadata-finalizer").unwrap().unwrap().require_result().activate();
        findx2_core::set_persist_fault(findx2_core::PersistFault::AfterDataSync);
        let failed=permit.finish_persistence(super::persist_metadata_final(&e,&path,Some(epoch)));
        findx2_core::clear_persist_fault();
        assert!(format!("{:#}",failed.unwrap_err()).contains("after data sync"));
        assert!(e.has_pending_persistence());assert!(!owner.retained_errors().is_empty());
        assert_eq!(e.index_store().entries[e.index_store().entry_on_volume(b'C',1).unwrap() as usize].size,456);
        let permit=crate::runtime_lifecycle::reserve(&e,"metadata-finalizer").unwrap().unwrap().require_result().activate();
        assert!(permit.finish_persistence(super::persist_metadata_final(&e,&path,Some(epoch))).unwrap());
        assert!(owner.retained_errors().is_empty());assert!(!e.has_pending_persistence());
        let disk=findx2_core::v6_load(&findx2_core::V6Layout::for_index(&path)).unwrap();assert!(disk.metadata_ready);
        drop(owner);std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    fn checkpoint_patch(engine: &findx2_core::SearchEngine, size:u64) {
        let ticket=engine.metadata_ticket_by_id(b'C',1).unwrap();
        assert!(engine.commit_metadata(ticket,size,0,0).unwrap());
    }
    fn restored_size(path:&std::path::Path) -> u64 {
        let store=findx2_core::v6_load(&findx2_core::V6Layout::for_index(path)).unwrap();
        assert!(!store.metadata_ready);
        let size=store.entries[store.entry_on_volume(b'C',1).unwrap() as usize].size;
        let reader=findx2_core::SearchEngine::new(store);
        let q=findx2_core::QueryParser::parse("public-file.txt").unwrap();
        let (hits,total)=reader.search(&q,&findx2_core::SearchOptions::default()).unwrap();
        assert_eq!(total,1);assert_eq!(hits.len(),1);assert_eq!(hits[0].size,size);
        size
    }

    #[test]
    fn checkpoint_partial_metadata_is_journaled_before_any_overlay_drain() {
        let (path,engine)=checkpoint_fixture("mirroring");
        checkpoint_patch(&engine,456);
        engine.set_backfill_error(Some("80 metadata access errors remain".into()));
        let batch=engine.take_persistence_batch().expect("accepted partial metadata must create a persistence batch");
        assert!(!batch.metadata_ready);
        assert!(batch.mutations.iter().any(|m| matches!(m,findx2_core::Mutation::MetadataBatch(items) if items.iter().any(|(_,bytes)| u64::from_le_bytes(bytes[..8].try_into().unwrap())==456))));
        let store=engine.index_store();
        assert_eq!(store.entries[store.entry_on_volume(b'C',1).unwrap() as usize].size,456);
        drop(store);
        engine.restore_persistence_batch(batch);
        assert!(!engine.metadata_ready());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn checkpoint_periodic_partial_coverage_preserves_namespace_and_cursor() {
        let (path,engine)=checkpoint_fixture("periodic");
        checkpoint_patch(&engine,456);
        engine.set_backfill_error(Some("80 metadata access errors remain".into()));
        engine.apply_volume_batch(b'C',&[findx2_core::ChangeEvent::Create {entry:RawEntry{file_id:2,file_id_128:None,parent_id:100,name:"public-new.txt".into(),size:123,mtime:0,ctime:0,attrs:0,is_dir:false}}],7,200).unwrap();
        let new_ticket=engine.metadata_ticket_by_id(b'C',2).unwrap();
        assert!(engine.commit_metadata(new_ticket,999,0,0).unwrap());
        let mut dirty=1;
        let mut last=std::time::Instant::now();
        super::persist_periodic_checkpoint(&engine,&path,&mut dirty,&mut last,std::time::Duration::from_secs(30)).unwrap();
        assert_eq!(restored_size(&path),10,"not due must remain unchanged");
        last-=std::time::Duration::from_secs(31);
        super::persist_periodic_checkpoint(&engine,&path,&mut dirty,&mut last,std::time::Duration::from_secs(30)).unwrap();
        assert_eq!(dirty,0);
        assert_eq!(restored_size(&path),456);
        let restored=findx2_core::v6_load(&findx2_core::V6Layout::for_index(&path)).unwrap();
        assert_eq!(restored.volumes[0].last_usn,200);
        let new=restored.entry_on_volume(b'C',2).unwrap() as usize;
        assert_eq!(restored.entries[new].size,999);
        assert!(!engine.metadata_ready());
        assert_eq!(engine.backfill_error_snapshot().as_deref(),Some("80 metadata access errors remain"));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn checkpoint_io_failure_retains_batch_and_concurrent_writes_then_retries() {
        let (path,engine)=checkpoint_fixture("failure-retry");
        // Real filesystem failure: a required segment is missing. The writer
        // fails after draining; no permission changes or real index are involved.
        checkpoint_patch(&engine,456);
        // Do not sabotage the published generation file; an injected writer failure
        // keeps the committed generation intact and still retains the drained batch.
        findx2_core::set_persist_fault(findx2_core::PersistFault::AfterDataSync);
        assert!(super::persist_index(&engine,&path).is_err());
        findx2_core::clear_persist_fault();
        assert!(engine.has_pending_persistence());
        checkpoint_patch(&engine,789);
        engine.apply_volume_batch(b'C',&[],7,201).unwrap();
        assert!(super::persist_index(&engine,&path).unwrap());
        assert_eq!(restored_size(&path),789);
        let restored=findx2_core::v6_load(&findx2_core::V6Layout::for_index(&path)).unwrap();
        assert_eq!(restored.volumes[0].last_usn,201);
        assert!(!engine.has_pending_persistence());
        // A mutation arriving during the drained batch IO remains queued; on a
        // reported failure it is repaired together with the old batch.
        checkpoint_patch(&engine,900);
        assert!(super::persist_index_with(&engine,&path,|_,_| {
            let worker=engine.clone();
            std::thread::spawn(move||checkpoint_patch(&worker,901)).join().unwrap();
            anyhow::bail!("injected writer access failure after concurrent accepted metadata")
        }).is_err());
        assert!(engine.has_pending_persistence());
        assert!(super::persist_index(&engine,&path).unwrap());
        assert_eq!(restored_size(&path),901);
        assert!(super::persist_index(&engine,&path).unwrap());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }



    #[test]
    fn checkpoint_success_does_not_acknowledge_metadata_arriving_during_io() {
        let (path,engine)=checkpoint_fixture("late-after-snapshot");
        checkpoint_patch(&engine,456);
        let io = super::persist_index_with(&engine,&path,|layout,batch| {
            let result=findx2_core::v6_try_commit(layout,batch).map_err(|e|anyhow::anyhow!("{e:#}"))?;
            let writer=engine.clone();
            std::thread::spawn(move||checkpoint_patch(&writer,789)).join().unwrap();
            Ok(result)
        });
        assert_eq!(crate::shutdown::require_checkpoint_result(io, || engine.has_pending_persistence()),
            Err(crate::shutdown::CheckpointFailure::NewerMutations));
        assert_eq!(restored_size(&path),456);
        assert!(engine.has_pending_persistence(),"newer accepted metadata cannot be acknowledged by an older batch");
        assert!(super::persist_index(&engine,&path).unwrap());
        assert_eq!(restored_size(&path),789);
        assert!(!engine.has_pending_persistence());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn checkpoint_disk_failure_does_not_end_watcher_and_retry_is_backed_off() {
        let (path,engine)=checkpoint_fixture("periodic-retry");
        checkpoint_patch(&engine,456);
        findx2_core::set_persist_fault(findx2_core::PersistFault::DiskFull { max_bytes: 16 });
        let mut dirty=2;
        let mut last=std::time::Instant::now()-std::time::Duration::from_secs(31);
        let original=last;
        let mut retry=None;
        super::persist_periodic_checkpoint_retrying(&engine,&path,&mut dirty,&mut last,&mut retry,std::time::Duration::from_secs(30));
        findx2_core::clear_persist_fault();
        assert_eq!(last,original);assert_eq!(dirty,2);assert!(retry.is_some());
        assert!(engine.has_pending_persistence());
        super::persist_periodic_checkpoint_retrying(&engine,&path,&mut dirty,&mut last,&mut retry,std::time::Duration::from_secs(30));
        assert_eq!(last,original,"backoff is separate from the successful-save clock");
        assert_eq!(restored_size(&path),10);
        retry=Some(std::time::Instant::now()-std::time::Duration::from_secs(1));
        super::persist_periodic_checkpoint_retrying(&engine,&path,&mut dirty,&mut last,&mut retry,std::time::Duration::from_secs(30));
        assert_eq!(dirty,0);assert!(last>original);assert!(retry.is_none());
        assert_eq!(restored_size(&path),456);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn checkpoint_repeated_metadata_writes_coalesce_and_observation_is_logged() {
        let (path,engine)=checkpoint_fixture("coalesced");
        for size in 0..10_000 { checkpoint_patch(&engine,size); }
        assert_eq!(engine.pending_metadata_bitmap().0,1);
        assert!(!engine.index_store().has_pending_mutations(),"no retained per-update enum log");
        let batch=engine.take_persistence_batch().unwrap();
        assert_eq!(batch.mutations.len(),1);
        match &batch.mutations[0] {
            findx2_core::Mutation::MetadataBatch(items) => {
                assert_eq!(items.len(),1);
                assert_eq!(u64::from_le_bytes(items[0].1[..8].try_into().unwrap()),9999);
                assert_eq!(std::mem::size_of_val(&items[0]),24);
            }
            _ => panic!("expected compact final metadata projection"),
        }
        engine.restore_persistence_batch(batch);
        assert!(super::persist_index(&engine,&path).unwrap());
        assert_eq!(restored_size(&path),9999);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        // Exercise the directory/FRN observation producer separately from tickets.
        let (path,engine)=checkpoint_fixture("observation");
        let stamp=engine.begin_metadata_observation(engine.metadata_epoch()).unwrap();
        assert_eq!(engine.commit_metadata_observation(b'C',stamp,&[(1,321,0,0)],4096).unwrap(),1);
        assert!(engine.has_pending_persistence());
        assert!(super::persist_index(&engine,&path).unwrap());
        assert_eq!(restored_size(&path),321);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn checkpoint_busy_keeps_dirty_and_clock_and_does_not_drain() {
        let (path,engine)=checkpoint_fixture("busy");
        checkpoint_patch(&engine,456);
        let guard=super::persist_lock().lock().unwrap_or_else(|e|e.into_inner());
        let mut dirty=5;
        let mut last=std::time::Instant::now()-std::time::Duration::from_secs(31);
        let before=last;
        super::persist_periodic_checkpoint(&engine,&path,&mut dirty,&mut last,std::time::Duration::from_secs(30)).unwrap();
        assert_eq!(dirty,5);assert_eq!(last,before);
        assert!(engine.has_pending_persistence());
        drop(guard);
        assert!(super::persist_index(&engine,&path).unwrap());
        assert_eq!(restored_size(&path),456);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn checkpoint_unwind_requeues_and_does_not_permanently_poison_writer() {
        let (path,engine)=checkpoint_fixture("unwind");
        checkpoint_patch(&engine,456);
        let unwind=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||{
            let _=super::persist_index_with(&engine,&path,|_,_| panic!("owned fixture writer panic"));
        }));
        assert!(unwind.is_err());
        assert!(engine.has_pending_persistence());
        assert!(super::persist_index(&engine,&path).unwrap());
        assert_eq!(restored_size(&path),456);
        assert!(!engine.has_pending_persistence());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn sole_volume_rebuild_replaces_instead_of_appending() {
        let mut old = tiny_store(b'C', 7, 100, "old.txt");
        old.excluded_dirs = vec![r"C:\Users".into()];
        let fresh = tiny_store(b'C', 7, 50_000, "new.txt");
        let old_n = old.entry_count();
        let fresh_n = fresh.entry_count();
        let merged = merge_rebuilt_volume(old, fresh, 'C').unwrap();
        assert_eq!(merged.entry_count(), fresh_n);
        assert_ne!(merged.entry_count(), old_n + fresh_n);
        assert_eq!(merged.volumes.len(), 1);
        assert_eq!(merged.volumes[0].last_usn, 50_000);
        assert_eq!(merged.excluded_dirs, vec![r"C:\Users".to_string()]);
        assert!(merged.deleted.len() > 0, "排除目录下的新扫描条目应被打墓碑");
    }

    #[test]
    fn resume_usn_keeps_scan_cursor_when_frozen_was_overwritten() {
        let fresh = 85_472_796_576u64;
        let first = 84_474_593_280i64;
        let frozen = 80_312_296_848u64;
        assert_eq!(resume_usn_after_rebuild(frozen, 9, fresh, 9, first), fresh);
        assert_eq!(resume_usn_after_rebuild(fresh, 9, fresh, 9, first), fresh);
        assert_eq!(
            resume_usn_after_rebuild(first as u64, 9, fresh, 9, first),
            first as u64
        );
        assert_eq!(resume_usn_after_rebuild(fresh, 1, fresh, 9, first), fresh);
    }
}

#[cfg(test)]mod index_owner_tests{
 use super::*;
 #[test]fn actual_os_owner_handle_excludes_same_index_until_dropped(){
  let d=std::env::temp_dir().join(format!("findx-owner-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));std::fs::create_dir(&d).unwrap();let p=d.join("index.bin");let a=acquire_index_owner(&p).unwrap();assert!(acquire_index_owner(&p).is_err());drop(a);assert!(acquire_index_owner(&p).is_ok());std::fs::remove_dir_all(d).unwrap();
 }
}
