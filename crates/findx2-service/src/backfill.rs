//! 快速首遍索引的后台元数据回填（size / mtime / ctime）。
//!
//! ## 核心思路（v3 紧凑 overlay 版）
//! - service 启动后如果索引 `metadata_ready=false`（首遍 fast 建库），就在后台开一个线程慢慢补；
//! - 元数据来源用 `findx2_windows::fetch_dir_meta_batched` (NtQueryDirectoryFile 快路径)
//!   + `fill_metadata_by_id_pooled` (OpenFileById 兜底)，按卷分组并行；
//! - **写入路径只改 overlay**：`SearchEngine::extend_metadata_overlay_batch` 直接落到紧凑
//!   `MetaOverlay`（无锁、O(1) 平铺数组）。**回填线程从此不再持 IndexStore 写锁**——
//!   search 永远不会被 backfill 卡住；
//! - 持久化：每 `PERSIST_INTERVAL` 触发一次"flush overlay 进主索引 + 写盘 index.bin"。
//!   flush 是唯一一次写锁，~2-3 秒，但发生在低频时机；
//! - 完成后置 `metadata_ready=true` 并最终落盘 + 清空 overlay 释放内存。
//!
//! ## 与旧实现 (v2) 的关键区别
//! - 删除了 `MERGE_CHUNK / MERGE_BREATHE / try_write_for 礼让`等所有"和 search 抢锁"的代码；
//! - overlay 不再是 DashMap，是平铺 `Vec<AtomicU64>`，内存固定 16B × entry_count。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use findx2_core::{save_index_bin, SearchEngine};
use tracing::{error, info};

/// 是否启用后台元数据回填。默认 **on**；可用 `FINDX2_DISABLE_BACKFILL=1` 关掉
/// （diagnostic 场景或想完全保留 fast 索引语义时）。
fn backfill_enabled() -> bool {
    !matches!(
        std::env::var("FINDX2_DISABLE_BACKFILL").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE")
    )
}

/// 单次往 overlay 灌入的批大小——纯内存操作，给个稍大点的值减少调度开销。
/// overlay.put 是 atomic store，无锁；这里大小只影响日志/进度更新粒度。
const OVERLAY_BATCH: usize = 8_192;

pub(crate) fn spawn_backfill(engine: Arc<SearchEngine>, index_path: std::path::PathBuf) {
    if !backfill_enabled() {
        info!("后台元数据回填已禁用（FINDX2_DISABLE_BACKFILL=1）");
        return;
    }
    if engine.metadata_ready() {
        info!("索引 metadata_ready 已为 true，跳过后台回填");
        return;
    }
    std::thread::Builder::new()
        .name("findx2-backfill".into())
        .spawn(move || {
            if let Err(e) = backfill_loop(engine, index_path) {
                error!("元数据回填线程退出: {e}");
            }
        })
        .ok();
}

/// 终态落盘：把 overlay 一次性 flush 进主索引并写盘 index.bin。
/// **只在回填彻底跑完时调用一次**——回填中途绝不落盘，避免持写锁阻塞 search。
///
/// 写盘 549MB 用独立线程跑，不阻塞调用者；调用者立刻返回，service 后续的 search 不受影响。
/// 写盘失败也不致命：下次启动 metadata_ready=false 会重新回填，无副作用。
fn spawn_final_persist(engine: Arc<SearchEngine>, path: std::path::PathBuf) {
    std::thread::Builder::new()
        .name("findx2-final-persist".into())
        .spawn(move || {
            let t0 = Instant::now();
            // 1) flush overlay 进主索引（写锁 ~2-5s @ 8.5M 全填）。
            //    这步发生时回填线程已退出，不会再有新 put；search 会被这次写锁阻塞数秒。
            //    虽然不完美，但只发生一次，可接受。
            match engine.set_metadata_ready(true) {
                Ok(_) => info!("终态落盘：metadata_ready=true 已写入索引（耗时 {:?}）", t0.elapsed()),
                Err(e) => {
                    error!("终态落盘：set_metadata_ready 失败: {e}");
                    return;
                }
            }
            // 2) 写盘 index.bin（持读锁，HDD 上 549MB 可能 5-15 秒）。
            //    parking_lot 下 reader 共存，search 不受影响。
            let t1 = Instant::now();
            let store = engine.index_store();
            if let Err(e) = save_index_bin(&path, &store) {
                error!("终态落盘：save_index_bin 失败: {e}");
                return;
            }
            drop(store);
            info!(
                "终态落盘：index.bin 已写盘（写盘耗时 {:?}，全程 {:?}）",
                t1.elapsed(),
                t0.elapsed()
            );
        })
        .ok();
}

#[cfg(not(windows))]
fn backfill_loop(_engine: Arc<SearchEngine>, _index_path: std::path::PathBuf) -> anyhow::Result<()> {
    warn!("非 Windows 平台不支持后台元数据回填");
    Ok(())
}

#[cfg(windows)]
fn backfill_loop(engine: Arc<SearchEngine>, index_path: std::path::PathBuf) -> anyhow::Result<()> {
    use findx2_windows::{fetch_dir_meta_batched, fill_metadata_by_id_pooled};

    // ⚠️ 关键修复：rayon thread-pool starvation。
    //
    // `fetch_dir_meta_batched` / `fill_metadata_by_id_pooled` 内部用 rayon 全局池
    // 并发跑 NtQueryDirectoryFile / OpenFileById，**每个 worker 阻塞在内核 syscall
    // 上数秒**（HDD 上更久）。8.5M 文件的卷会把全局池所有 worker 占满 10+ 秒。
    //
    // 而 search 路径里的 par_iter / par_chunks 也排队走全局池——结果表现就是：
    //   "回填启动后 search 一律卡 10 秒，回填进度也不走（因为 syscall 还在跑）"。
    //
    // 解决：给 backfill 单开一个专属池，限制为 cpu_count/2 个线程（足够打满磁盘 IO，
    // 不会饿死 search）。整个 backfill 主流程包在 install() 里，子函数里的 par_iter
    // 自动走专属池——search 永远走全局池，互不抢占。
    let total_threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8);
    // CPU-2：留 2 个核给 search/USN/IPC 已经够用。
    // 之前 CPU/2 太保守——回填的瓶颈在磁盘 IO 而不是 CPU，多放线程能让磁盘队列更深，
    // HDD 上 NtQueryDirectoryFile 的吞吐能提一档。
    let backfill_threads = total_threads.saturating_sub(2).max(2).min(total_threads);
    let backfill_pool = rayon::ThreadPoolBuilder::new()
        .num_threads(backfill_threads)
        .thread_name(|i| format!("findx2-backfill-{i}"))
        .build()
        .map_err(|e| anyhow::anyhow!("创建回填专属线程池失败：{e}"))?;
    info!(
        "回填专属线程池已建：threads={}/{}（剩余 {} 留给 search）",
        backfill_threads,
        total_threads,
        total_threads - backfill_threads
    );

    let total_entries = engine.index_store().entry_count();
    info!(
        "后台回填启动：索引共 {total_entries} 条，扫描待补元数据条目 …"
    );
    let scan_start = Instant::now();

    // 1) 扫描待补：按 (盘符 -> {files: [(entry_idx, frn, dir_idx)], dirs: [frn]}) 分组。
    //    - files：真正要回填的目标（size==0 的文件），同时记下 dir_idx 供后续 parent 反查；
    //    - dirs：用来驱动 NtQueryDirectoryFile 快路径（一次 syscall 拿一目录的所有子项 meta）。
    //
    // ⚠️ **关键并发设计**：read lock 一次性持有时间必须**毫秒级**，绝对不能是秒级。
    // 8.5M 条目的循环在 release 上 ~1s，但服务里同时跑 USN watcher 的 write 需求，
    // SRW Lock 一旦让 writer 排队，**所有后续 reader（包括 search）也会排队**——结果就是
    // "回填一启动 GUI 完全卡死"。因此把扫描拆成 N 个分段，每段读锁拿出来 ~64K 条 entry 就立刻
    // 放锁、给 search/USN write 一个调度窗口、再续。
    struct VolPending {
        files: Vec<(usize, u64)>,
        dirs: Vec<u64>,
    }
    let mut by_volume: std::collections::BTreeMap<char, VolPending> =
        std::collections::BTreeMap::new();
    let mut total_pending = 0usize;

    let total_entries_count = total_entries;
    const SCAN_CHUNK: usize = 65_536;
    let mut start = 0usize;
    while start < total_entries_count {
        let end = (start + SCAN_CHUNK).min(total_entries_count);
        // 单段最多握 ~1ms read lock；释放后 sleep 让出调度。
        {
            let g = engine.index_store();
            for i in start..end {
                let e = match g.entries.get(i) {
                    Some(e) => e,
                    None => break,
                };
                let frn = match g.frns.get(i) {
                    Some(&f) if f != 0 => f,
                    _ => continue,
                };
                let letter = g.volume_letter_for_entry(i);
                let slot = by_volume
                    .entry(letter)
                    .or_insert_with(|| VolPending { files: Vec::new(), dirs: Vec::new() });
                if e.is_dir_entry() {
                    slot.dirs.push(frn);
                } else if e.size == 0 {
                    // 判据：`size==0 && !is_dir`。fast 首遍 mtime 已是 USN TimeStamp 非 0，
                    // size 才是"尚未回填"的真正信号。空文件（size==0）会被多跑一次，可忽略。
                    slot.files.push((i, frn));
                    total_pending += 1;
                }
            }
        }
        // 让 search / USN write 插队。1ms 看似短，但足以让排队的 writer 跑一轮 batch。
        std::thread::sleep(Duration::from_millis(1));
        start = end;
    }
    info!(
        "回填扫描完成：耗时 {:?}，待补文件 {} 条（分布在 {} 个卷）",
        scan_start.elapsed(),
        total_pending,
        by_volume.len()
    );

    if total_pending == 0 {
        info!("无需回填，触发终态落盘（异步线程）");
        spawn_final_persist(engine, index_path);
        return Ok(());
    }

    engine.set_backfill_total(total_pending as u64);
    let cancel = Arc::new(AtomicBool::new(false));
    let global_done = Arc::new(AtomicUsize::new(0));

    // 2) 逐卷：先走 NtQueryDirectoryFile 批量快路径，剩余未命中的走 OpenFileById 兜底。
    //    **整个循环包在专属线程池 install 里**，让内部所有 rayon par_iter 都走专属池，
    //    不污染全局池（search 走全局池）。
    backfill_pool.install(|| -> anyhow::Result<()> {
    for (letter, pending) in &by_volume {
        if pending.files.is_empty() {
            continue;
        }
        let vol_path = format!("\\\\.\\{}:", letter);
        info!(
            "回填 卷 {}: 待补文件 {} 条，驱动目录 {} 个 ({})",
            letter,
            pending.files.len(),
            pending.dirs.len(),
            vol_path,
        );

        // 建"待回填 FRN 集合"，用于在 NtQueryDirectoryFile 阶段筛选只要的子项。
        let needed: std::collections::HashSet<u64> =
            pending.files.iter().map(|(_, f)| *f).collect();

        // 2.1 NtQueryDirectoryFile 批量。优化：
        //  - 只扫描"含有待补文件"的目录（needed 集合的 parent 集），其它目录开了也白开；
        //  - 按 FRN 升序排，让 MFT 物理读连续，Windows 内核 Mcb cache 命中率↑。
        //
        // 待补文件 frn 集合 = needed；其 parent 集合需要从 entries 反查。
        // 同样按 chunk 拿读锁——避免一次握几秒锁阻死 search/USN write。
        let mut parent_set: std::collections::HashSet<u64> =
            std::collections::HashSet::with_capacity(pending.files.len());
        const PARENT_CHUNK: usize = 32_768;
        for chunk in pending.files.chunks(PARENT_CHUNK) {
            {
                let g = engine.index_store();
                for &(idx, _frn) in chunk {
                    let dir_idx = g.entries[idx].dir_idx as usize;
                    if let Some(d) = g.dirs.get(dir_idx) {
                        parent_set.insert(d.frn);
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let mut dir_frns: Vec<(u64, Option<[u8; 16]>)> = pending
            .dirs
            .iter()
            .filter(|&&f| parent_set.contains(&f))
            .map(|&f| (f, None))
            .collect();
        dir_frns.sort_unstable_by_key(|(frn, _)| *frn);
        info!(
            "回填 卷 {}: 实际扫描目录 {}/{}（按 FRN 排序，已剔除无待补子文件的目录）",
            letter,
            dir_frns.len(),
            pending.dirs.len(),
        );
        // 接 progress 回调：每完成一批目录就按比例推进 backfill_done，
        // 让 GUI 能实时看到 D 盘那种"扫几分钟才返回"的卷在动而不是僵住。
        // 估算系数：该卷待补文件 / 实际扫描目录数（每目录平均 ~N 个待补文件）。
        let vol_files = pending.files.len() as u64;
        let _vol_dirs = dir_frns.len().max(1) as u64;
        let last_reported = std::sync::atomic::AtomicU64::new(0);
        let engine_cb = engine.clone();
        let progress_cb = move |cur: usize, total: usize| {
            // 把 [0..vol_dirs] 映射到 [0..vol_files]，单调推进。
            let projected = (cur as u64).saturating_mul(vol_files) / (total.max(1) as u64);
            let prev = last_reported.swap(projected, Ordering::Relaxed);
            if projected > prev {
                engine_cb.add_backfill_done(projected - prev);
            }
        };
        let dir_recs = fetch_dir_meta_batched(
            &vol_path,
            &dir_frns,
            Some(&progress_cb),
            Some(cancel.clone()),
        );
        // 进度回调里推过的"估算量"在真正写 overlay 前归零，避免双重计数。
        // 真正的命中量在下面循环里按 hit_frns 重新累加。
        engine.reset_backfill_done_to(global_done.load(Ordering::Relaxed) as u64);

        // 从 entry 视角建 frn→entry_idx（仅在待回填集合里），用于把 NtQuery 的返回映射回 entry。
        let mut frn_to_entry: std::collections::HashMap<u64, usize> =
            std::collections::HashMap::with_capacity(pending.files.len() * 2);
        for &(idx, frn) in &pending.files {
            frn_to_entry.insert(frn, idx);
        }

        // 记录 NtQuery 命中的 frn，避免后续兜底重复跑。
        let mut hit_frns: std::collections::HashSet<u64> =
            std::collections::HashSet::with_capacity(needed.len());
        let mut buf: Vec<(usize, u64, u64, u64)> = Vec::with_capacity(OVERLAY_BATCH);
        for (frn, sz, mt, ct) in dir_recs {
            if !needed.contains(&frn) {
                continue;
            }
            let Some(&entry_idx) = frn_to_entry.get(&frn) else {
                continue;
            };
            if !hit_frns.insert(frn) {
                continue;
            }
            buf.push((entry_idx, sz, mt, ct));
            if buf.len() >= OVERLAY_BATCH {
                let n = buf.len();
                engine.extend_metadata_overlay_batch(&buf);
                engine.add_backfill_done(n as u64);
                global_done.fetch_add(n, Ordering::Relaxed);
                buf.clear();
            }
        }
        if !buf.is_empty() {
            let n = buf.len();
            engine.extend_metadata_overlay_batch(&buf);
            engine.add_backfill_done(n as u64);
            global_done.fetch_add(n, Ordering::Relaxed);
        }
        info!(
            "回填 卷 {}: NtQueryDirectoryFile 命中 {}/{}",
            letter,
            hit_frns.len(),
            pending.files.len(),
        );

        // 2.2 剩余未命中 → OpenFileById 兜底（极少数孤儿 / reparse 目录扫不到）。
        let remain: Vec<(usize, u64)> = pending
            .files
            .iter()
            .filter(|(_, f)| !hit_frns.contains(f))
            .copied()
            .collect();
        if !remain.is_empty() {
            info!("回填 卷 {}: OpenFileById 兜底 {} 条", letter, remain.len());
            let frns: Vec<u64> = remain.iter().map(|(_, f)| *f).collect();
            let id128s: Vec<Option<[u8; 16]>> = vec![None; frns.len()];
            let local_indices: Vec<usize> = (0..frns.len()).collect();
            let updates = fill_metadata_by_id_pooled(
                &vol_path,
                &frns,
                &id128s,
                &local_indices,
                None,
                Some(cancel.clone()),
            );
            let mut buf2: Vec<(usize, u64, u64, u64)> = Vec::with_capacity(OVERLAY_BATCH);
            for (local_idx, sz, mt, ct) in updates {
                let (entry_idx, _frn) = remain[local_idx];
                buf2.push((entry_idx, sz, mt, ct));
                if buf2.len() >= OVERLAY_BATCH {
                    let n = buf2.len();
                    engine.extend_metadata_overlay_batch(&buf2);
                    engine.add_backfill_done(n as u64);
                    global_done.fetch_add(n, Ordering::Relaxed);
                    buf2.clear();
                }
            }
            if !buf2.is_empty() {
                let n = buf2.len();
                engine.extend_metadata_overlay_batch(&buf2);
                engine.add_backfill_done(n as u64);
                global_done.fetch_add(n, Ordering::Relaxed);
            }
        }
        info!("回填 卷 {} 完成", letter);
    }
    Ok(())
    })?;

    // 3) 终态落盘异步化。
    //    回填扫描循环到此结束，所有元数据都在 overlay 里。
    //    flush + 写盘 549MB 是耗时操作（HDD 上可能 10+ 秒），扔到独立线程跑，
    //    本回填线程可以立刻退出，不阻塞任何东西。
    info!(
        "后台元数据回填完成：done={}/{}，触发终态落盘（异步线程）",
        global_done.load(Ordering::Relaxed),
        total_pending
    );
    spawn_final_persist(engine, index_path);
    Ok(())
}

