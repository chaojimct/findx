//! 全量构建：文件名索引 → 内容倒排（CLI `content-index` 与 service 共用）。
//!
//! 流水线：`plan_range_from_store`（纯内存规划，service 按 50k 分片循环调用，
//! 扩展名/体积/目录三层预过滤，无路径拼接、无 syscall，被过滤项只计 `skipped`）
//! + `build_from_plan`（rayon 并行抽取 IO+解析，tantivy 单线程串行写入，
//! 1024 子批次定界内存，跨分片追加到同一 `cdir`）。
//!
//! service 在线构建不能长持 store 读锁（会饿死 USN 增量与 checkpoint），
//! 因此拆成两步：`plan_range_from_store`（store 读锁只拿毫秒级）
//! + `build_from_plan`（无锁抽取入库）。CLI 单线程独占，直接用 `build_from_store`。

use std::path::Path;

use findx2_core::{volume_key_for, IndexStore};
use rayon::prelude::*;

use crate::{
    extract_text, index::ContentIndex, lower_name_supported, open_or_create, should_extract,
    DocMeta, Error, Result,
};

#[derive(Debug, Default, Clone)]
pub struct BuildStats {
    pub scanned: u64,
    pub extracted: u64,
    pub skipped: u64,
    pub failed: u64,
    pub bytes: u64,
    pub docs: u64,
    /// 前 100 个失败样本（路径 + 原因），供诊断失败构成用；多了不存。
    pub failed_samples: Vec<String>,
}

/// 规划项：解析好的待抽取文件（service 分片规划后脱离锁执行）。
#[derive(Debug, Clone)]
pub struct PlanItem {
    pub frn: u64,
    pub volume_key: String,
    pub path: String,
    pub mtime: u64,
    pub size: u64,
}

/// 构建资源配置：按机器自动分档，环境变量可覆盖。
/// 低配机默认保守（AnyTXT 在低配机上直接停实时监控，我们降线程/堆/IO），
/// 高配机放开。`FINDX_INDEX_THREADS` / `FINDX_INDEX_HEAP_MB` /
/// `FINDX_READ_MBPS`（0=不限）/`FINDX_BATCH` 覆盖对应项。
#[derive(Debug, Clone, Copy)]
pub struct BuildConfig {
    /// tantivy 索引线程数。
    pub index_threads: usize,
    /// writer 总堆（字节）。
    pub heap_bytes: usize,
    /// 抽取子批次大小（定界内存）。
    pub batch: usize,
    /// 磁盘读取上限 MB/s，0=不限。低配机默认限速，做后台好公民。
    pub read_mbps: u64,
}

impl BuildConfig {
    pub fn auto() -> Self {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let mut cfg = if cpus <= 4 {
            // 低配（核少，内存通常也小）：2 线程/512MB/40MB/s。
            Self {
                index_threads: 2,
                heap_bytes: 512 << 20,
                batch: 512,
                read_mbps: 40,
            }
        } else if cpus <= 8 {
            Self {
                index_threads: 4,
                heap_bytes: 768 << 20,
                batch: 1024,
                read_mbps: 80,
            }
        } else {
            Self {
                index_threads: 8,
                heap_bytes: 1536 << 20,
                batch: 1024,
                read_mbps: 0,
            }
        };
        if let Some(v) = std::env::var("FINDX_INDEX_THREADS")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
        {
            cfg.index_threads = v.max(1);
        }
        if let Some(v) = std::env::var("FINDX_INDEX_HEAP_MB")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
        {
            cfg.heap_bytes = v.max(64) << 20;
        }
        if let Some(v) = std::env::var("FINDX_READ_MBPS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
        {
            cfg.read_mbps = v;
        }
        if let Some(v) = std::env::var("FINDX_BATCH")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
        {
            cfg.batch = v.max(128);
        }
        cfg
    }
}

/// 把自己降为后台优先级（Windows `BELOW_NORMAL`：CPU/IO/内存优先级全降，
/// 前台干活时自动让路；空闲时照样跑满）。CLI/service 共用构建入口，调一次即可。
fn lower_self_priority() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        #[cfg(windows)]
        {
            #[link(name = "kernel32")]
            extern "system" {
                fn SetPriorityClass(h: *mut std::ffi::c_void, c: u32) -> i32;
                fn GetCurrentProcess() -> *mut std::ffi::c_void;
            }
            const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
            unsafe {
                SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);
            }
        }
    });
}

/// 读盘令牌桶：按秒窗口统计已读字节，超配额 sleep 补齐。开销一次时间检查。
struct IoThrottle {
    mbps: u64,
    window_start: std::time::Instant,
    window_bytes: u64,
}

impl IoThrottle {
    fn new(mbps: u64) -> Self {
        Self {
            mbps,
            window_start: std::time::Instant::now(),
            window_bytes: 0,
        }
    }

    fn add(&mut self, bytes: u64) {
        if self.mbps == 0 {
            return;
        }
        self.window_bytes += bytes;
        let quota = self.mbps * 1_048_576;
        if self.window_bytes >= quota {
            let el = self.window_start.elapsed();
            if el < std::time::Duration::from_secs(1) {
                std::thread::sleep(std::time::Duration::from_secs(1) - el);
            }
            self.window_start = std::time::Instant::now();
            self.window_bytes = 0;
        }
    }
}

/// commit 带退避重试：Windows 下 Defender 会对新段文件短暂持锁，
/// 直接 commit 可能撞上 Os code 5。3 次退避基本能过去；都失败再报错。
fn commit_retry(cidx: &ContentIndex) -> Result<()> {
    let mut last = None;
    for wait_ms in [500, 2000, 8000] {
        match cidx.commit() {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(wait_ms));
            }
        }
    }
    Err(last.expect("commit 重试至少执行一次"))
}

/// 卷前缀（小写）→ 归一化卷键：条目按最长前缀归属卷。
pub fn build_vols(store: &IndexStore) -> Vec<(String, String)> {
    store
        .volumes
        .iter()
        .enumerate()
        .map(|(i, v)| {
            (
                v.root_prefix.to_lowercase(),
                volume_key_for(&v.volume_id, i),
            )
        })
        .collect()
}

/// 默认排除的路径片段（小写，`contains` 匹配）：系统/程序/缓存目录。
/// 与 AnyTXT 默认排除 `C:\Windows` 同理：这些目录下几乎没有可索引文本，
/// 逐文件 stat 它们是全盘构建最主要的时间黑洞。用户文档不受影响。
const EXCLUDED_PATH_PARTS: &[&str] = &[
    ":\\windows\\",
    ":\\program files\\",
    ":\\program files (x86)\\",
    ":\\programdata\\",
    ":\\$recycle.bin\\",
    ":\\system volume information\\",
    "\\node_modules\\",
    "\\.git\\",
    "\\__pycache__\\",
    "\\appdata\\local\\temp\\",
    "\\appdata\\local\\microsoft\\",
];

fn is_excluded_path(lower_full_path: &str) -> bool {
    EXCLUDED_PATH_PARTS
        .iter()
        .any(|p| lower_full_path.contains(p))
}

/// 纯内存规划 `[start, end)`：解析路径/卷归属/frn，不读文件。
/// 调用方持读锁（service 按分片短持），`st.scanned/failed` 同步累加，
/// 语义与 `build_from_store` 单遍一致。
///
/// 三层预过滤（全部无 syscall，命中只计 `skipped`）：
/// 1. 扩展名白名单（`file_name_supported`，只看文件名）——全盘 ~99% 的
///    exe/dll/sys 在这里被过滤，连完整路径都不拼；
/// 2. 目录排除（`EXCLUDED_PATH_PARTS`）。
/// 只有候选文件才走 `entry_display_path` 做卷归属。
/// 注意：体积不在规划阶段过滤——store 的 size 可能是陈旧值（0 不代表真空），
/// 超限/空文件由执行阶段 `should_extract` 实测 stat 后判定。
pub fn plan_range_from_store(
    store: &IndexStore,
    vols: &[(String, String)],
    start: usize,
    end: usize,
    st: &mut BuildStats,
) -> Vec<PlanItem> {
    let mut out = Vec::new();
    // 栈中小写缓冲：449 万条目 × 零堆分配（超长名才走堆，万分之一都不到）。
    let mut lower_buf = [0u8; 256];
    for idx in start.min(store.entries.len())..end.min(store.entries.len()) {
        let e = &store.entries[idx];
        if e.is_dir_entry() || e.is_deleted() {
            continue;
        }
        st.scanned += 1;
        // 1) 扩展名预检：无路径拼接、无 stat、无分配。
        let name_ok = lower_name_supported(&store.name_lower_into(e, &mut lower_buf));
        if !name_ok {
            st.skipped += 1;
            continue;
        }
        let p = match store.entry_display_path(idx) {
            Ok(p) => p,
            Err(_) => {
                st.failed += 1;
                continue;
            }
        };
        let pl = p.to_lowercase();
        // 2) 目录排除。
        if is_excluded_path(&pl) {
            st.skipped += 1;
            continue;
        }
        let mut slot = 0usize;
        let mut best = 0usize;
        for (i, (pre, _)) in vols.iter().enumerate() {
            if !pre.is_empty() && pl.starts_with(pre) && pre.len() >= best {
                best = pre.len();
                slot = i;
            }
        }
        if slot >= vols.len() {
            st.failed += 1;
            continue;
        }
        let frn = store.frns.get(idx).copied().unwrap_or(0);
        out.push(PlanItem {
            frn,
            volume_key: vols[slot].1.clone(),
            path: p,
            mtime: e.mtime as u64,
            size: e.size,
        });
    }
    out
}

/// 无锁执行：抽取 + tantivy 入库，可跨分片追加到同一 `cdir`
///（`open_or_create` 打开已有多批的分片目录继续写）。
/// `progress` 每 ~1024 文件回调一次，返回 false 即中止
///（`Err(Error::Cancelled)`；`fresh` 目录会被删掉）。
/// `st` 跨分片累加（调用方持有）。返回最终 `docs` 数。
///
/// 流水线：自适应子批次 × rayon 并行抽取（stat + 解码 + Office/PDF 解析，
/// IO 与 CPU 混合主导）→ 同批 rayon 并行入库（ASCII 快道/结巴分词/写入，
/// 纯 CPU 主导，tantivy 多线程 writer）。子批次定界内存；入库顺序不保证
///（doc id 仅内部使用，搜索按分排序）。低配机自动降线程/堆/读盘速率，
/// 并把进程降为后台优先级。
pub fn build_from_plan(
    items: &[PlanItem],
    cdir: &Path,
    rebuild: bool,
    st: &mut BuildStats,
    mut progress: impl FnMut(&BuildStats) -> bool,
) -> Result<u64> {
    const COMMIT_EVERY: u64 = 50_000;
    let cfg = BuildConfig::auto();
    lower_self_priority();
    let mut throttle = IoThrottle::new(cfg.read_mbps);
    let existed = cdir.exists();
    let existed = cdir.exists();
    if rebuild && existed {
        std::fs::remove_dir_all(cdir).map_err(|e| Error::Io(cdir.display().to_string(), e))?;
    }
    let fresh = rebuild || !existed;
    let cidx = open_or_create(cdir)?;
    let mut since_commit = 0u64;
    // progress 返回 false = 调用方要求中止：跳出循环，统一在循环后清理
    //（`drop(cidx)` 释放 mmap 后再删目录，Windows 下才能删掉）。
    let mut cancelled = false;
    // 启动即回调一次：调用方（service）第一时间看到 extracted/skipped 开始跳动，
    // 也让“取消”在抽取开始前就生效。返回 false 直接走统一取消清理。
    if !progress(st) {
        cancelled = true;
    }
    for batch in items.chunks(cfg.batch) {
        if cancelled {
            break;
        }
        // 并行抽取（保序 collect）：None = 执行阶段复检跳过（文件刚好被删/改大等竞态），
        // Some(Ok) = 待写入，Some(Err(reason)) = 抽取失败（reason 含路径，供采样）。
        let extracted: Vec<Option<std::result::Result<(DocMeta, String, u64), String>>> = batch
            .par_iter()
            .map(|item| {
                let path = Path::new(&item.path);
                if should_extract(path).is_none() {
                    return None;
                }
                // 慢文件抓现行：单文件抽取超 10 秒打一行（正常文件毫秒级）。
                let t0 = std::time::Instant::now();
                let r = extract_text(path);
                let dt = t0.elapsed();
                if dt.as_secs() >= 10 {
                    eprintln!("[content] slow extract {:?} {}", dt, item.path);
                }
                match r {
                    Ok(t) => Some(Ok((
                        DocMeta {
                            frn: item.frn,
                            volume_id: item.volume_key.clone(),
                            path: item.path.clone(),
                            mtime: item.mtime,
                            size: item.size,
                        },
                        t.text,
                        t.raw_bytes,
                    ))),
                    Err(e) => Some(Err(format!("{e}"))),
                }
            })
            .collect();
        // 并行入库：`ContentIndex` 线程安全，同批 key 唯一无冲突；
        // 计数用 reduce 汇总（无锁、无原子），顺序不保证但总量精确。
        // 失败原因只采样前 100（路径+原因），供诊断失败构成。
        let (ext_n, byte_n, fail_n, skip_n, mut fail_samples) = extracted
            .par_iter()
            .map(|slot| -> (u64, u64, u64, u64, Vec<String>) {
                match slot {
                    None => (0, 0, 0, 1, Vec::new()),
                    Some(Err(reason)) => (0, 0, 1, 0, vec![reason.clone()]),
                    Some(Ok((meta, text, raw_bytes))) => {
                        // 慢入库抓现行：单篇超 10 秒打一行（分词/写入病态输入）。
                        let t0 = std::time::Instant::now();
                        let ok = cidx.index_doc(meta, text).is_ok();
                        let dt = t0.elapsed();
                        if dt.as_secs() >= 10 {
                            eprintln!("[content] slow index {:?} {}", dt, meta.path);
                        }
                        if ok {
                            (1, *raw_bytes, 0, 0, Vec::new())
                        } else {
                            (0, 0, 1, 0, vec![format!("入库失败 {}", meta.path)])
                        }
                    }
                }
            })
            .reduce(
                || (0, 0, 0, 0, Vec::new()),
                |mut a, b| {
                    a.0 += b.0;
                    a.1 += b.1;
                    a.2 += b.2;
                    a.3 += b.3;
                    if a.4.len() < 100 {
                        a.4.extend(b.4.into_iter().take(100 - a.4.len()));
                    }
                    a
                },
            );
        st.extracted += ext_n;
        st.bytes += byte_n;
        st.failed += fail_n;
        st.skipped += skip_n;
        if st.failed_samples.len() < 100 {
            let room = 100 - st.failed_samples.len();
            fail_samples.truncate(room);
            st.failed_samples.append(&mut fail_samples);
        }
        throttle.add(byte_n);
        since_commit += ext_n;
        if since_commit >= COMMIT_EVERY {
            commit_retry(&cidx)?;
            since_commit = 0;
        }
        // 每子批次查一次取消（中止响应有界，代价一次闭包调用）。
        if !progress(st) {
            cancelled = true;
        }
    }
    if cancelled {
        drop(cidx);
        if fresh {
            let _ = std::fs::remove_dir_all(cdir);
        }
        return Err(Error::Cancelled);
    }
    commit_retry(&cidx)?;
    // 终局合并：段数归一，查询更快、体积最小。失败不影响使用（索引可用，
    // 只是没压到最小），降级为警告继续。
    if let Err(e) = cidx.optimize() {
        eprintln!("[content] 终局合并跳过（不影响使用）: {e}");
    }
    Ok(cidx.num_docs())
}

/// CLI 独占路径：规划 + 执行一遍（持锁全程，调用方保证无并发写）。
/// `progress` 透传给 `build_from_plan`（启动 + 每 1024 回调，false 即中止）；
/// 结束时再回调一次（返回值忽略）。时间节流（如 5 秒打一次日志）由调用方
/// 闭包内部完成，库侧不代劳（否则会把取消信号也节流掉）。
pub fn build_from_store(
    store: &IndexStore,
    cdir: &Path,
    rebuild: bool,
    mut progress: impl FnMut(&BuildStats) -> bool,
) -> Result<BuildStats> {
    eprintln!("[content] plan start entries={}", store.entries.len());
    let vols = build_vols(store);
    let mut st = BuildStats::default();
    let items = plan_range_from_store(store, &vols, 0, store.entries.len(), &mut st);
    eprintln!(
        "[content] plan done candidates={} skipped={}",
        items.len(),
        st.skipped
    );
    let docs = build_from_plan(&items, cdir, rebuild, &mut st, |s| progress(s))?;
    st.docs = docs;
    progress(&st);
    Ok(st)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excluded_system_dirs() {
        assert!(is_excluded_path("c:\\windows\\system32\\a.txt"));
        assert!(is_excluded_path("c:\\program files\\app\\a.txt"));
        assert!(is_excluded_path("c:\\program files (x86)\\app\\a.txt"));
        assert!(is_excluded_path("c:\\programdata\\x\\a.txt"));
        assert!(is_excluded_path("d:\\proj\\node_modules\\x\\a.txt"));
        assert!(is_excluded_path("d:\\proj\\.git\\objects\\a.txt"));
        assert!(!is_excluded_path("c:\\users\\chaoj\\docs\\report.docx"));
        assert!(!is_excluded_path("d:\\proj\\src\\main.rs"));
    }
}
