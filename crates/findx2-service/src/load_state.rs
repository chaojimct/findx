//! 索引加载阶段的全局观测点。
//!
//! 千万级库（`index.bin` 十几 GB）的 `load_index_bin` 要几十秒到几分钟。此期间
//! service 已把命名管道挂起，`Search` 必然返回「索引加载中」，`Status` 返回
//! `loading=true` —— 上层只看到一个静止的「加载中」，既分不清"正在加载"和"卡死"，
//! 也无法判断还要等多久，用户体验等同卡死。
//!
//! 这里把 `load_index_bin_with_progress` 的阶段回调落成一个进程级快照，供
//! `ipc_dispatch::process_request` 的 `Status` 分支读取，再经 IPC 透出到 GUI 状态栏。
//!
//! 用 `OnceLock` 而非 `lazy_static`：无额外依赖，Rust 1.70+ 标准库即可。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use findx2_core::LoadPhase;

/// 加载阶段快照。仅当加载进行中才会被构造——已结束的加载 `snapshot()` 直接返回 `None`，
/// 所以这里只暴露"进行中"所需的信息，含不带入 `LoadPhase` 本身与其结束时刻。
#[derive(Debug, Clone)]
pub struct LoadSnapshot {
    /// 阶段中文标签（直接进状态栏）
    pub label: &'static str,
    /// 已开始加载的时间点
    pub started: Instant,
    /// 当前是第几个阶段（1 起算，含当前阶段）
    pub phases_done: u32,
    /// 总阶段数
    pub phases_total: u32,
    /// 本次加载的条目总数（读到头之后才有值，之前为 0）
    pub entry_count: u64,
}

struct LoadState {
    phase: Mutex<Option<LoadPhase>>,
    started: Mutex<Option<Instant>>,
    finished: Mutex<Option<Instant>>,
    entry_count: AtomicU64,
}

fn state() -> &'static LoadState {
    static S: OnceLock<LoadState> = OnceLock::new();
    S.get_or_init(|| LoadState {
        phase: Mutex::new(None),
        started: Mutex::new(None),
        finished: Mutex::new(None),
        entry_count: AtomicU64::new(0),
    })
}

/// `LoadPhase` 的全量有序列表——用于计算「第几/共几阶段」。
pub const LOAD_PHASES: [LoadPhase; 8] = [
    LoadPhase::Open,
    LoadPhase::Header,
    LoadPhase::Entries,
    LoadPhase::Dirs,
    LoadPhase::Frns,
    LoadPhase::Indexes,
    LoadPhase::Filters,
    LoadPhase::CjkBitmap,
];

fn phase_ordinal(p: LoadPhase) -> u32 {
    LOAD_PHASES
        .iter()
        .position(|x| *x == p)
        .map(|i| i as u32 + 1)
        .unwrap_or(0)
}

/// 记录一次加载阶段的进入。由 `load_index_bin_with_progress` 的回调直接调用。
pub fn note_phase(p: LoadPhase) {
    let s = state();
    if let Ok(mut g) = s.phase.lock() {
        // 「打开索引文件」总是第一站；再次见到它说明是新一轮加载，重置状态。
        if p == LoadPhase::Open {
            s.entry_count.store(0, Ordering::Relaxed);
            if let Ok(mut f) = s.finished.lock() {
                *f = None;
            }
            if let Ok(mut st) = s.started.lock() {
                *st = Some(Instant::now());
            }
        }
        *g = Some(p);
    }
}

/// 记录条目总数（解析完文件头即可知）。
pub fn note_entry_count(n: u64) {
    state().entry_count.store(n, Ordering::Relaxed);
}

/// 加载收尾（成功或失败都调）。此后 `phase` 不再对外暴露。
pub fn note_finished() {
    let s = state();
    if let Ok(mut g) = s.phase.lock() {
        *g = None;
    }
    if let Ok(mut f) = s.finished.lock() {
        *f = Some(Instant::now());
    }
}

/// 取当前快照；仍在加载中返回 `Some`，已结束或未开始返回 `None`。
///
/// 两个锁分开取会留下一个并发窗口：可能在读到 `phase` 之后、读 `finished` 之前，
/// 加载刚好收尾。此时按"已结束"处理——宁可漏报一次进行中，也不要把一个已经不再
/// 推进的加载当成进行中报给上层（那正是用户看到的"卡死"）。
pub fn snapshot() -> Option<LoadSnapshot> {
    let s = state();
    let phase = s.phase.lock().ok().and_then(|g| *g)?;
    if s.finished.lock().ok().and_then(|g| *g).is_some() {
        return None;
    }
    let started = s
        .started
        .lock()
        .ok()
        .and_then(|g| *g)
        .unwrap_or_else(Instant::now);
    Some(LoadSnapshot {
        label: phase.label(),
        started,
        // 以 `phase` 为准现算序号，而不是读一个由回调自增的计数器——
        // 后者若某轮加载漏调一次就会与真实阶段错位。
        phases_done: phase_ordinal(phase),
        phases_total: LOAD_PHASES.len() as u32,
        entry_count: s.entry_count.load(Ordering::Relaxed),
    })
}

/// 拼一行可读文案：`解析条目（5/8 阶段，已 12s，共 3.1 亿条）`。
///
/// 调用方先取一次 [`snapshot`]，再把引用递进来 —— 同一次 `Status` 里还要取耗时与
/// 阶段号，共用一个快照才不会出现字段跨帧不一致。
pub fn progress_line_of(snap: &LoadSnapshot) -> String {
    let secs = snap.started.elapsed().as_secs();
    let mut s = format!(
        "{}（{}/{} 阶段",
        snap.label, snap.phases_done, snap.phases_total
    );
    if secs >= 1 {
        s.push_str(&format!("，已 {secs}s"));
    }
    if snap.entry_count > 0 {
        s.push_str(&format!("，共 {} 条", snap.entry_count));
    }
    s.push('）');
    s
}
