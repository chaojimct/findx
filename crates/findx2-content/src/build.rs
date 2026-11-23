//! 全量构建：文件名索引 → 内容倒排（CLI `content-index` 与 service 共用）。
//!
//! 单线程 M1（IO 主导，瓶颈在抽取；rayon 化是 M2 优化项）。
//! 分批 commit：bound writer 内存，崩溃最多丢一批（下次重建/增量补回）。
//!
//! service 在线构建不能长持 store 读锁（会饿死 USN 增量与 checkpoint），
//! 因此拆成两步：`plan_range_from_store`（纯内存规划，锁只拿毫秒级，
//! service 按 50k 分片循环调用）+ `build_from_plan`（无锁抽取入库，
//! 可跨分片追加）。CLI 单线程独占，直接用 `build_from_store`。

use std::path::Path;

use findx2_core::{volume_key_for, IndexStore};

use crate::{extract_text, open_or_create, should_extract, DocMeta, Error, Result};

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

/// 纯内存规划 `[start, end)`：解析路径/卷归属/frn，不读文件。
/// 调用方持读锁（service 按分片短持），`st.scanned/failed` 同步累加，
/// 语义与 `build_from_store` 单遍一致。
pub fn plan_range_from_store(
    store: &IndexStore,
    vols: &[(String, String)],
    start: usize,
    end: usize,
    st: &mut BuildStats,
) -> Vec<PlanItem> {
    let mut out = Vec::new();
    for idx in start.min(store.entries.len())..end.min(store.entries.len()) {
        let e = &store.entries[idx];
        if e.is_dir_entry() || e.is_deleted() {
            continue;
        }
        st.scanned += 1;
        let p = match store.entry_display_path(idx) {
            Ok(p) => p,
            Err(_) => {
                st.failed += 1;
                continue;
            }
        };
        let pl = p.to_lowercase();
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
pub fn build_from_plan(
    items: &[PlanItem],
    cdir: &Path,
    rebuild: bool,
    st: &mut BuildStats,
    mut progress: impl FnMut(&BuildStats) -> bool,
) -> Result<u64> {
    let existed = cdir.exists();
    if rebuild && existed {
        std::fs::remove_dir_all(cdir).map_err(|e| Error::Io(cdir.display().to_string(), e))?;
    }
    let fresh = rebuild || !existed;
    let cidx = open_or_create(cdir)?;
    let mut since_commit = 0u64;
    let mut done = 0u64;
    // progress 返回 false = 调用方要求中止：跳出循环，统一在循环后清理
    //（`drop(cidx)` 释放 mmap 后再删目录，Windows 下才能删掉）。
    let mut cancelled = false;
    for item in items {
        let path = Path::new(&item.path);
        if should_extract(path).is_none() {
            st.skipped += 1;
        } else {
            match extract_text(path) {
                Ok(t) => {
                    let ok = cidx
                        .index_doc(
                            &DocMeta {
                                frn: item.frn,
                                volume_id: item.volume_key.clone(),
                                path: item.path.clone(),
                                mtime: item.mtime,
                                size: item.size,
                            },
                            &t.text,
                        )
                        .is_ok();
                    if !ok {
                        st.failed += 1;
                    } else {
                        st.extracted += 1;
                        st.bytes += t.raw_bytes;
                        since_commit += 1;
                        if since_commit >= 20_000 {
                            cidx.commit()?;
                            since_commit = 0;
                        }
                    }
                }
                Err(_) => st.failed += 1,
            }
        }
        // 首文件 + 每 1024 文件查一次取消（中止响应有界，代价一次闭包调用）。
        done += 1;
        if done == 1 || done % 1024 == 0 {
            if !progress(st) {
                cancelled = true;
                break;
            }
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

/// CLI/单线程独占路径：规划 + 执行一遍（持锁全程，调用方保证无并发写）。
/// `progress` 透传给 `build_from_plan`（首文件 + 每 1024 回调，false 即中止）；
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
