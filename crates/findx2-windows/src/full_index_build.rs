//! 多卷全盘建库（快速首遍或 full-stat），供 `findx2` CLI 与 `findx2-service` 共用。

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use findx2_core::{
    merge_index_stores, normalize_excluded_dir, save_exclude_sidecar, save_index_bin, IndexBuilder,
    IndexStore,
};
use rayon::prelude::*;

/// 与 CLI `index` 子命令一致的卷列表解析。
pub fn resolve_volume_list(
    volume: Option<String>,
    volumes: Option<Vec<String>>,
) -> Vec<String> {
    if let Some(ref vs) = volumes {
        if vs.is_empty() {
            vec![volume.unwrap_or_else(|| "C:".into())]
        } else {
            vs.clone()
        }
    } else if let Some(v) = volume {
        vec![v]
    } else {
        let mut v = crate::enumerate_local_drive_letters();
        if v.is_empty() {
            v.push("C:".into());
        }
        v
    }
}

fn write_index_progress(path: &Path, v: &serde_json::Value) -> std::io::Result<()> {
    let s = serde_json::to_string(v)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, s.as_bytes())
}

fn index_one_volume(
    volume_trim: &str,
    full_stat: bool,
    metadata_ready: bool,
) -> findx2_core::Result<IndexStore> {
    let (files, dirs) = if full_stat {
        crate::scan_volume(volume_trim)?
    } else {
        crate::scan_volume_fast(volume_trim)?
    };
    let letter = volume_trim
        .chars()
        .find(|c| c.is_ascii_alphabetic())
        .unwrap_or('C')
        .to_ascii_uppercase() as u8;
    let serial = crate::get_volume_serial_number(volume_trim)?;
    let usn = crate::UsnJournalWatcher::new(volume_trim).probe()?;
    let builder = IndexBuilder::new(letter, serial, usn.journal_id, usn.next_usn);
    findx2_core::progress!(
        "正在构建卷 {} 的内存索引结构（百万级时排序可能需数分钟，请见下方进度）…",
        volume_trim
    );
    tracing::info!(
        "正在构建卷 {} 的内存索引结构（百万级时排序可能需数分钟）…",
        volume_trim
    );
    let _ = std::io::stderr().flush();
    builder.build_from_raw(files, dirs, metadata_ready)
}

/// 并行扫描、合并、写入 `output`；`progress_file` 与 GUI 轮询格式一致（`.indexing.json`）。
///
/// `excluded_dirs`：用户配置的排除目录（任意书写形式，内部会归一为小写反斜杠不带尾 `\`）。
/// 扫描后会按前缀剔除 `entries`（含目录），并把规范化结果写入 `<output>.exclude.json` 边车，
/// service 启动时与运行时增量都会复用同一份。
pub fn build_full_disk_index(
    output: &Path,
    full_stat: bool,
    max_scan_threads: usize,
    progress_file: Option<&Path>,
    volume: Option<String>,
    volumes: Option<Vec<String>>,
    excluded_dirs: Vec<String>,
) -> findx2_core::Result<()> {
    let vol_list = resolve_volume_list(volume, volumes);
    let metadata_ready = full_stat;
    let normalized_excludes: Vec<String> = excluded_dirs
        .iter()
        .filter_map(|s| normalize_excluded_dir(s))
        .collect();
    let threads = max_scan_threads.max(1).min(vol_list.len().max(1));
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(|e| findx2_core::Error::Platform(e.to_string()))?;

    let progress_path: std::path::PathBuf =
        progress_file.map(Path::to_path_buf).unwrap_or_else(|| {
            output.with_extension("indexing.json")
        });

    let total_vols = vol_list.len();
    let _ = write_index_progress(
        &progress_path,
        &serde_json::json!({
            "phase": "starting",
            "volumes_total": total_vols,
            "volumes_completed": 0,
            "entries_indexed": 0u64,
            "message": format!("准备扫描 {} 个卷…", total_vols),
        }),
    );

    let progress_path = Arc::new(progress_path);
    let completed = Arc::new(AtomicUsize::new(0));
    let entries_accumulated = Arc::new(AtomicU64::new(0));

    // 启动一个进度采样线程：每 500ms 把 mft::SCAN_LIVE_ENTRIES 的实时值写入 .indexing.json，
    // 让 GUI 在卷扫描中途也能看到"已索引 X 条"持续递增。
    // SCAN_LIVE_ENTRIES 是单进程全局，本函数对一次建库唯一调用，重置后再起跑安全。
    crate::SCAN_LIVE_ENTRIES.store(0, Ordering::Relaxed);
    let ticker_stop = Arc::new(AtomicBool::new(false));
    let ticker = {
        let path_for_tick = progress_path.clone();
        let stop = ticker_stop.clone();
        let completed = completed.clone();
        let total_vols = total_vols;
        std::thread::Builder::new()
            .name("findx2-index-progress".into())
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(500));
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let live = crate::SCAN_LIVE_ENTRIES.load(Ordering::Relaxed);
                    let c = completed.load(Ordering::Relaxed);
                    let _ = write_index_progress(
                        path_for_tick.as_ref(),
                        &serde_json::json!({
                            "phase": "scanning",
                            "volumes_total": total_vols,
                            "volumes_completed": c,
                            "entries_indexed": live,
                            "message": format!(
                                "MFT 枚举中…已收录 {} 条（{}/{} 卷已完成）",
                                live, c, total_vols
                            ),
                        }),
                    );
                }
            })
            .ok()
    };

    let part: Vec<findx2_core::Result<IndexStore>> = pool.install(|| {
        vol_list
            .par_iter()
            .map(|v| {
                let vol_s = v.trim();
                let r = index_one_volume(vol_s, full_stat, metadata_ready);
                if let Ok(ref store) = r {
                    let added = store.entry_count() as u64;
                    let entries_so_far =
                        entries_accumulated.fetch_add(added, Ordering::Relaxed) + added;
                    let c = completed.fetch_add(1, Ordering::Relaxed) + 1;
                    let _ = write_index_progress(
                        progress_path.as_ref(),
                        &serde_json::json!({
                            "phase": "scanning",
                            "volumes_total": total_vols,
                            "volumes_completed": c,
                            "current_volume": vol_s,
                            "entries_indexed": entries_so_far,
                            "message": format!(
                                "卷 {} 已入索引（{}/{} 卷，累计约 {} 条）",
                                vol_s, c, total_vols, entries_so_far
                            ),
                        }),
                    );
                }
                r
            })
            .collect()
    });

    // 扫描全部完成，停 ticker；后续 merging/writing 阶段进度由各自显式 write_index_progress 推送。
    ticker_stop.store(true, Ordering::Relaxed);
    if let Some(handle) = ticker {
        let _ = handle.join();
    }

    let mut stores: Vec<IndexStore> = Vec::with_capacity(part.len());
    for r in part {
        stores.push(r?);
    }

    let pre_merge_entries: u64 = stores.iter().map(|s| s.entry_count() as u64).sum();
    let _ = write_index_progress(
        progress_path.as_ref(),
        &serde_json::json!({
            "phase": "merging",
            "volumes_total": total_vols,
            "volumes_completed": total_vols,
            "entries_indexed": pre_merge_entries,
            "message": if stores.len() > 1 {
                "合并多卷索引…"
            } else {
                "整理内存索引…"
            },
        }),
    );

    let mut store = if stores.len() == 1 {
        stores.into_iter().next().expect("one volume")
    } else {
        merge_index_stores(stores)?
    };
    // 把规范化后的排除目录注入 store（持续给后续 USN 增量过滤用），并对已扫到的 entries 打墓碑。
    if !normalized_excludes.is_empty() {
        store.excluded_dirs = normalized_excludes.clone();
        let marked = store.mark_excluded_entries(&normalized_excludes);
        if marked > 0 {
            findx2_core::progress!(
                "排除目录命中：{} 条已标记为已删除（共 {} 条排除规则）",
                marked,
                normalized_excludes.len()
            );
        }
    }
    let entry_count = store.entry_count();
    let _ = write_index_progress(
        progress_path.as_ref(),
        &serde_json::json!({
            "phase": "writing",
            "volumes_total": total_vols,
            "volumes_completed": total_vols,
            "entries_indexed": entry_count,
            "message": format!("写入 {}（{} 条）…", output.display(), entry_count),
        }),
    );
    findx2_core::progress!("正在写入 {} …", output.display());
    tracing::info!("正在写入 {} …", output.display());
    save_index_bin(output, &store)?;
    // 把规范化的排除目录写边车（即便为空也写一份；下次 GUI 改设置可以读到旧值做 diff）。
    save_exclude_sidecar(output, &normalized_excludes)?;
    let _ = std::fs::remove_file(progress_path.as_ref());
    tracing::info!("已写入 {}，条目 {}", output.display(), entry_count);
    println!("已写入 {}，条目 {}", output.display(), entry_count);
    Ok(())
}
