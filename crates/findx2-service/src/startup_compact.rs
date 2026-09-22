//! 启动时墓碑自动压缩：把「索引体检提示 + 用户手动压缩」升级为无感的启动期自动回收。
//!
//! ## 为什么放启动窗口
//!
//! 压缩是 O(n) 全库重写（entries 重排 + 全部耦合下标重映射 + 原子落盘），跑在
//! 运行期会卡住搜索热路径；而「加载完成后、引擎挂上 EngineSlot 前」这个窗口
//! 天然零查询并发，GUI 反正还在等加载 —— 多显示一段「自动压缩墓碑」阶段
//! （经 [`crate::load_state`] 上报为第 9/9 阶段），用户无感且全程可见。
//!
//! 墓碑主要来自 USN 增量删除的日积月累，增长缓慢（实测一天约 2-4%）。
//! 每次服务启动体检一次（阈值见 [`IndexStore::should_compact`]：≥10 万条目且
//! 墓碑比 >25%），墓碑永远到不了高水位 —— GUI 的手动「一键压缩」保留作兜底。
//!
//! ## 失败语义
//!
//! - 守卫触发（存活数 < 预期）：**Err 上抛，拒绝以可疑索引启动** —— 与 CLI
//!   `compact` 的拒绝落盘同一逻辑，历史事故（区间推断清空整库）的教训。
//! - 落盘失败（磁盘满/权限）：内存里的压缩结果依然有效（搜索照常），只降级
//!   提示，文件等下次启动再收口。
//! - trigram 边车 posting 用旧下标，压缩后内存置 `None`；调用方既有的
//!   「边车缺失 → 后台补建」路径会重建并热替换，期间搜索走全表扫描兜底。

use std::path::Path;

use findx2_core::IndexStore;
use tracing::{info, warn};

use crate::load_state;

/// 自动压缩结果。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AutoCompactOutcome {
    /// 体检未命中（低于阈值），什么都不做。
    Skipped,
    /// 已压缩。`saved_ok=false` 表示内存已回收但落盘失败（文件下次启动再试）。
    Compacted {
        removed: usize,
        saved_ok: bool,
        elapsed_secs: f64,
    },
}

/// 体检命中则自动压缩 + 原子落盘；未命中原样返回。
///
/// 压缩期间经 [`load_state`] 上报「自动压缩墓碑」阶段；返回前（无论成败）
/// 必定离开该窗口。
///
/// # Errors
/// 灾难性损失守卫触发时返回 Err（见模块注释「失败语义」）。
pub fn auto_compact_if_needed(
    store: &mut IndexStore,
    index: &Path,
) -> anyhow::Result<AutoCompactOutcome> {
    if !store.should_compact() {
        return Ok(AutoCompactOutcome::Skipped);
    }
    let n_before = store.entry_count();
    let tomb = store.deleted.len();
    let pct = (store.tombstone_ratio() * 100.0).round() as u64;
    info!("索引墓碑比超阈值：{tomb} / {n_before}（{pct}%），启动窗口内自动压缩");

    load_state::note_compaction_started(tomb as u64);
    let outcome = compact_and_save(store, index);
    load_state::note_compaction_finished();
    outcome
}

/// 压缩主体（守卫 → 落盘 → tri 置空）。任何路径离开前都会 [`load_state::note_compaction_finished`]
/// （由调用方包住）。
fn compact_and_save(store: &mut IndexStore, index: &Path) -> anyhow::Result<AutoCompactOutcome> {
    let t0 = std::time::Instant::now();
    // deleted 是 RoaringBitmap，len() 返回 u64（与 CLI compact 同一换算）。
    let n_before = store.entry_count();
    let tomb = store.deleted.len() as usize;

    let removed = store.compact_tombstones();

    // 灾难性损失守卫（与 CLI compact 同一逻辑）：压缩只该删墓碑，存活条目必须
    // 原样保留。触发即算法出了偏差 —— 绝不落盘，也不带病服务。
    let expected = n_before.saturating_sub(tomb);
    let actual = store.entry_count();
    if actual < expected {
        return Err(anyhow::anyhow!(
            "启动自动压缩守卫触发：预期保留 {expected} 条（{n_before} - {tomb} 墓碑），\
             实际 {actual} 条。已放弃落盘且拒绝以可疑索引启动。这是 bug，请反馈。"
        ));
    }

    // 原子落盘：tmp + rename。失败只降级不阻断 —— 内存里的压缩结果依然有效，
    // 文件等下次启动（或 save 周期）再收口。
    let saved_ok = match save_atomically(store, index) {
        Ok(()) => true,
        Err(e) => {
            warn!("自动压缩落盘失败（内存已回收，文件下次启动再试）: {e}");
            false
        }
    };

    // trigram 边车的 posting 是旧下标，内存必须置空；调用方既有的
    // 「trigram.is_none() → 后台补建」路径会重建并热替换，期间搜索走全表扫描兜底。
    // （tri_pending / cjk_names 已由 compact_tombstones 按新下标重映射。）
    store.trigram = None;

    let elapsed_secs = t0.elapsed().as_secs_f64();
    info!(
        "自动压缩完成：移除 {removed} 条墓碑 → {} 条目，耗时 {elapsed_secs:.1}s，落盘 {}",
        store.entry_count(),
        if saved_ok { "成功" } else { "失败（下次启动再试）" }
    );
    Ok(AutoCompactOutcome::Compacted { removed, saved_ok, elapsed_secs })
}

/// 原子落盘：写同目录 `.compact.tmp` 再 rename 覆盖（与 CLI `compact` 一致），
/// 避免压缩中途崩溃留下半截索引。
fn save_atomically(store: &IndexStore, index: &Path) -> anyhow::Result<()> {
    let dir = index.parent().unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(
        "{}.compact.tmp",
        index
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "index.bin".into())
    ));
    findx2_core::save_index_bin(&tmp, store)?;
    std::fs::rename(&tmp, index)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use findx2_core::index::IndexBuilder;
    use findx2_core::platform::RawEntry;
    use std::path::PathBuf;

    /// 1 个目录 + `n_files` 个文件，名字形如 `f{idx}.log`。
    fn mk_store(n_files: usize, tombstones: usize) -> IndexStore {
        assert!(tombstones <= n_files);
        let dirs = vec![RawEntry {
            file_id: 10,
            file_id_128: None,
            parent_id: 0,
            name: "data".into(),
            size: 0,
            mtime: 0,
            ctime: 0,
            attrs: 0x10,
            is_dir: true,
        }];
        let files: Vec<RawEntry> = (0..n_files)
            .map(|i| RawEntry {
                file_id: 100 + i as u64,
                file_id_128: None,
                parent_id: 10,
                name: format!("f{i}.log"),
                size: 100,
                mtime: 1,
                ctime: 1,
                attrs: 0,
                is_dir: false,
            })
            .collect();
        let mut store = IndexBuilder::new(b'C', 1, 2, 3)
            .build_from_raw(files, dirs, true)
            .unwrap();
        // 按名字定位墓碑目标，避免依赖建库顺序。
        for i in 0..tombstones {
            let name = format!("f{i}.log");
            let idx = (0..store.entries.len())
                .find(|&j| store.name_bytes(&store.entries[j]) == name.as_bytes())
                .expect("找不到待删条目");
            store.delete_entry(idx as u32);
        }
        store
    }

    fn temp_index_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "findx2-autocompact-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir.join("index.bin")
    }

    fn cleanup(p: &Path) {
        let _ = std::fs::remove_file(p);
        let _ = std::fs::remove_file(p.with_extension("bin.tri"));
        let _ = std::fs::remove_file(p.with_extension("bin.tri.pending"));
        let _ = std::fs::remove_file(p.parent().unwrap());
    }

    #[test]
    fn skips_when_below_threshold() {
        // 5 条目 1 墓碑（20%）：远低于「≥10 万且 >25%」。
        let mut store = mk_store(5, 1);
        let idx = temp_index_path("skip");
        findx2_core::save_index_bin(&idx, &store).unwrap();
        let out = auto_compact_if_needed(&mut store, &idx).unwrap();
        assert_eq!(out, AutoCompactOutcome::Skipped);
        cleanup(&idx);
    }

    #[test]
    fn compacts_and_persists_when_threshold_hit() {
        // 14 万条目打 5 万墓碑（35.7%）：命中阈值。
        const N: usize = 140_000;
        const TOMBS: usize = 50_000;
        let store = mk_store(N, TOMBS);
        let idx = temp_index_path("hit");
        findx2_core::save_index_bin(&idx, &store).unwrap();

        let n_before = store.entry_count();
        assert_eq!(n_before, N + 1, "文件条目 + 1 个目录");
        let mut store = findx2_core::load_index_bin(&idx).unwrap();
        assert!(store.should_compact(), "35.7% 墓碑必须命中体检");

        let out = auto_compact_if_needed(&mut store, &idx).unwrap();
        let AutoCompactOutcome::Compacted { removed, saved_ok, .. } = out else {
            panic!("命中阈值必须压缩，实际 {out:?}");
        };
        assert_eq!(removed, TOMBS, "被移除的墓碑数");
        assert!(saved_ok, "落盘应当成功");
        assert_eq!(store.entry_count(), n_before - TOMBS);
        assert_eq!(store.deleted.len(), 0);
        assert!(store.trigram.is_none(), "压缩后 tri 必须置空交后台重建");
        assert!(!store.should_compact());

        // 落盘验证：从磁盘重新加载，墓碑已物理消失、条目数正确。
        let reloaded = findx2_core::load_index_bin(&idx).unwrap();
        assert_eq!(reloaded.deleted.len(), 0, "文件里的墓碑必须已物理移除");
        assert_eq!(reloaded.entry_count(), n_before - TOMBS);
        // 存活条目仍可按名字搜到（下标重映射后搜索自洽）。
        let eng = findx2_core::SearchEngine::new(reloaded);
        let pq = findx2_core::QueryParser::parse("f60000.log").unwrap();
        let (hits, _) = eng
            .search(&pq, &findx2_core::SearchOptions::default())
            .unwrap();
        assert_eq!(hits.len(), 1, "存活条目压缩后必须仍可命中");
        cleanup(&idx);
    }
}
