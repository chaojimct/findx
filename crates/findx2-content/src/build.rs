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
    extract_text, lower_name_supported, open_or_create, should_extract, DocMeta, Error, Result,
};

#[derive(Debug, Default, Clone)]
pub struct BuildStats {
    pub scanned: u64,
    pub extracted: u64,
    pub skipped: u64,
    pub failed: u64,
    pub bytes: u64,
    pub docs: u64,
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
/// 流水线：1024 子批次 × rayon 并行抽取（stat + 解码 + Office/PDF 解析，
/// IO 与 CPU 混合主导）→ 同批 rayon 并行入库（ASCII 快道/结巴分词/写入，
/// 纯 CPU 主导，tantivy 多线程 writer）。子批次定界内存：最多同时持有
/// 1024 篇正文；入库顺序不保证（doc id 仅内部使用，搜索按分排序）。
pub fn build_from_plan(
    items: &[PlanItem],
    cdir: &Path,
    rebuild: bool,
    st: &mut BuildStats,
    mut progress: impl FnMut(&BuildStats) -> bool,
) -> Result<u64> {
    const EXTRACT_BATCH: usize = 1024;
    // 索引 commit 节流：writer 堆 1.5GB，5 万篇一批，崩溃最多丢一批。
    const COMMIT_EVERY: u64 = 50_000;
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
    for batch in items.chunks(EXTRACT_BATCH) {
        if cancelled {
            break;
        }
        // 并行抽取（保序 collect）：None = 执行阶段复检跳过（文件刚好被删/改大等竞态），
        // Some(Ok) = 待写入，Some(Err) = 抽取失败。
        let extracted: Vec<Option<std::result::Result<(DocMeta, String, u64), ()>>> = batch
            .par_iter()
            .map(|item| {
                let path = Path::new(&item.path);
                if should_extract(path).is_none() {
                    return None;
                }
                match extract_text(path) {
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
                    Err(_) => Some(Err(())),
                }
            })
            .collect();
        // 并行入库：`ContentIndex` 线程安全，同批 key 唯一无冲突；
        // 计数用 reduce 汇总（无锁、无原子），顺序不保证但总量精确。
        let (ext_n, byte_n, fail_n, skip_n) = extracted
            .par_iter()
            .map(|slot| -> (u64, u64, u64, u64) {
                match slot {
                    None => (0, 0, 0, 1),
                    Some(Err(())) => (0, 0, 1, 0),
                    Some(Ok((meta, text, raw_bytes))) => {
                        if cidx.index_doc(meta, text).is_ok() {
                            (1, *raw_bytes, 0, 0)
                        } else {
                            (0, 0, 1, 0)
                        }
                    }
                }
            })
            .reduce(|| (0, 0, 0, 0), |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2, a.3 + b.3));
        st.extracted += ext_n;
        st.bytes += byte_n;
        st.failed += fail_n;
        st.skipped += skip_n;
        since_commit += ext_n;
        if since_commit >= COMMIT_EVERY {
            cidx.commit()?;
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
    cidx.commit()?;
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
    let vols = build_vols(store);
    let mut st = BuildStats::default();
    let items = plan_range_from_store(store, &vols, 0, store.entries.len(), &mut st);
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
