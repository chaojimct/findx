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

/// 防止启动时与 JournalGap 重建后重复拉起两条回填线程。
static BACKFILL_RUNNING: AtomicBool = AtomicBool::new(false);

struct BackfillRunningGuard;
impl Drop for BackfillRunningGuard {
    fn drop(&mut self) {
        BACKFILL_RUNNING.store(false, Ordering::SeqCst);
    }
}

use findx2_core::SearchEngine;
#[cfg(unix)]
use tracing::warn;
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
        engine.set_backfill_error(Some(
            "元数据回填已关闭（FINDX2_DISABLE_BACKFILL）；时间与大小筛选可能不准".into(),
        ));
        return;
    }
    if engine.metadata_ready() {
        info!("索引 metadata_ready 已为 true，跳过后台回填");
        return;
    }
    if BACKFILL_RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        info!("元数据回填已在运行，跳过重复启动");
        return;
    }
    let engine_err = engine.clone();
    let lifecycle=match crate::runtime_lifecycle::reserve(&engine,"backfill") {Ok(v)=>v.map(|p|p.require_result()),Err(_)=>{BACKFILL_RUNNING.store(false,Ordering::SeqCst);return}};
    if let Err(e) = std::thread::Builder::new()
        .name("findx2-backfill".into())
        .spawn(move || {
            let mut lifecycle=lifecycle.map(|p|p.activate());
            let _guard = BackfillRunningGuard;
            let result=backfill_loop(engine.clone(),index_path);
            match &result{Ok(())=>crate::runtime_lifecycle::success_optional(&mut lifecycle),Err(e)=>crate::runtime_lifecycle::error_optional(&mut lifecycle,e.to_string())}
            if let Err(e)=result {
                error!("元数据回填线程退出: {e}");
                engine.set_backfill_error(Some(format!("元数据回填异常：{e}")));
            }
        })
    {
        BACKFILL_RUNNING.store(false, Ordering::SeqCst);
        error!("无法启动回填线程: {e}");
        engine_err.set_backfill_error(Some(format!("无法启动回填线程：{e}")));
    }
}

/// 终态落盘：把 overlay 一次性 flush 进主索引并写盘（v6）。
/// **只在回填彻底跑完时调用一次**——回填中途绝不落盘，避免持写锁阻塞 search。
///
/// overlay flush 在 `bulk_apply` 里记一条 `FullResync`，`persist_index` 见到后走
/// v6 全量分段重写（与旧 v5 一次 549MB 同量级，但一生一次）。独立线程跑，不阻塞调用者。
fn spawn_final_persist(engine: Arc<SearchEngine>, path: std::path::PathBuf) {
    spawn_final_persist_for_epoch(engine,path,None)
}
fn spawn_final_persist_for_epoch(engine:Arc<SearchEngine>,path:std::path::PathBuf,epoch:Option<u64>) {
    let lifecycle=match crate::runtime_lifecycle::reserve(&engine,"metadata-finalizer") {Ok(v)=>v.map(|p|p.require_result()),Err(_)=>return};
    std::thread::Builder::new()
        .name("findx2-final-persist".into())
        .spawn(move || {
            let mut lifecycle=lifecycle.map(|p|p.activate());
            let t0 = Instant::now();
            // 1) flush overlay 进主索引（写锁 ~2-5s @ 8.5M 全填）。
            //    这步发生时回填线程已退出，不会再有新 put；search 会被这次写锁阻塞数秒。
            //    虽然不完美，但只发生一次，可接受。
            let ready=if let Some(epoch)=epoch {
                engine.complete_metadata_if_covered(epoch).map_err(anyhow::Error::msg).and_then(|complete|if complete{Ok(())}else{Err(anyhow::anyhow!("metadata coverage changed before finalization"))})
            }else{engine.set_metadata_ready(true).map_err(anyhow::Error::from)};
            match ready {
                Ok(_) => info!(
                    "终态落盘：metadata_ready=true 已写入索引（耗时 {:?}）",
                    t0.elapsed()
                ),
                Err(e) => {
                    crate::runtime_lifecycle::error_optional(&mut lifecycle,e.to_string());
                    error!("终态落盘：set_metadata_ready 失败: {e}");
                    engine.set_backfill_error(Some(format!("元数据已补全但写入索引失败：{e}")));
                    return;
                }
            }
            // 2) 落盘（持读锁，HDD 上数秒；parking_lot 下 reader 共存，search 不受影响）。
            //    Windows 走 v6：FullResync → 全量分段重写。
            //    非 Windows（Unix 回填桩）走旧 v5 直写。
            let t1 = Instant::now();
            if epoch.is_some_and(|e| engine.metadata_epoch() != e) {
                crate::runtime_lifecycle::error_optional(&mut lifecycle,"obsolete metadata finalizer cannot prove current coverage".into());
                info!("obsolete metadata finalizer skipped");
                return;
            }
            #[cfg(windows)]
            match crate::run::persist_metadata_final(&engine, &path, epoch) {
                Ok(true) => {},
                Ok(false) => {
                    // Epoch replacement is cancellation, NOT successful durable
                    // completion or permission to remove the new epoch sidecar.
                    crate::runtime_lifecycle::success_optional(&mut lifecycle);
                    info!("obsolete metadata finalizer cancelled after writer wait");
                    return;
                },
                Err(e) => {
                    crate::runtime_lifecycle::error_optional(&mut lifecycle,format!("{e:#}"));
                    error!("terminal metadata persistence failed: {e:#}");
                    engine.set_backfill_error(Some(format!("metadata covered but terminal persistence failed: {e:#}")));
                    return;
                }
            }
            #[cfg(not(windows))]
            {
                let store = engine.index_store();
                if let Err(e) = findx2_core::save_index_bin(&path, &store) {
                    crate::runtime_lifecycle::error_optional(&mut lifecycle,e.to_string());
                    error!("终态落盘：save_index_bin 失败: {e}");
                    return;
                }
            }
            // 终态已落盘：断点边车使命完成，删除（下次回填从头也不怕，因为 ready=true 不会再回填）。
            if epoch.is_some_and(|e| engine.metadata_epoch() != e) { return; }
            delete_overlay_sidecar(&path);
            crate::runtime_lifecycle::success_optional(&mut lifecycle);
            let msg = format!(
                "终态落盘：v6 已写盘（写盘耗时 {:?}，全程 {:?}）",
                t1.elapsed(),
                t0.elapsed()
            );
            info!("{msg}");
            crate::win_service::append_service_log(&msg);
        })
        .ok();
}

#[cfg(unix)]
fn backfill_loop(engine: Arc<SearchEngine>, index_path: std::path::PathBuf) -> anyhow::Result<()> {
    use findx2_core::index::unix_secs_to_filetime;
    use std::os::unix::fs::MetadataExt;

    let total_entries = engine.index_store().entry_count();
    engine.set_backfill_error(None);
    engine.set_backfill_total(total_entries as u64);
    info!("Unix 后台回填启动：索引共 {total_entries} 条，收集待补文件 …");

    let mut pending: Vec<(usize, String)> = Vec::new();
    const SCAN_CHUNK: usize = 65_536;
    let mut start = 0usize;
    while start < total_entries {
        let end = (start + SCAN_CHUNK).min(total_entries);
        {
            let g = engine.index_store();
            for i in start..end {
                let Some(e) = g.entries.get(i) else { break };
                if e.is_dir_entry() || e.size != 0 {
                    continue;
                }
                if let Some(path) = unix_entry_fs_path(&g, i) {
                    pending.push((i, path));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(1));
        start = end;
    }

    if load_overlay_sidecar(&engine, &index_path).unwrap_or(0) > 0 {
        pending.retain(|(idx, _)| !engine.metadata_overlay_has(*idx));
    }

    if pending.is_empty() {
        info!("Unix 回填：无需补元数据，触发终态落盘");
        delete_overlay_sidecar(&index_path);
        spawn_final_persist(engine, index_path);
        return Ok(());
    }

    let total_pending = pending.len();
    engine.set_backfill_total(total_pending as u64);
    info!("Unix 回填：待补 {total_pending} 条，按目录批量 stat");

    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .saturating_sub(1)
        .clamp(2, 8);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("findx2-unix-backfill-{i}"))
        .build()
        .map_err(|e| anyhow::anyhow!("创建 Unix 回填线程池失败：{e}"))?;

    let done = Arc::new(AtomicUsize::new(0));
    const BATCH: usize = 512;
    pool.install(|| {
        use rayon::prelude::*;
        pending.par_chunks(BATCH).for_each(|chunk| {
            let mut batch: Vec<(usize, u64, u64, u64)> = Vec::with_capacity(chunk.len());
            for &(idx, ref path) in chunk {
                let Ok(meta) = std::fs::symlink_metadata(path) else {
                    continue;
                };
                if meta.file_type().is_dir() {
                    continue;
                }
                batch.push((
                    idx,
                    meta.len(),
                    unix_secs_to_filetime(meta.mtime().max(0) as u32),
                    unix_secs_to_filetime(meta.ctime().max(0) as u32),
                ));
            }
            if !batch.is_empty() {
                let n = batch.len();
                engine.extend_metadata_overlay_batch(&batch);
                let d = done.fetch_add(n, Ordering::Relaxed) + n;
                engine.reset_backfill_done_to(d as u64);
            }
        });
    });

    if let Err(e) = save_overlay_sidecar(&engine, &index_path) {
        warn!("Unix 回填断点边车保存失败: {e:#}");
    }
    info!(
        "Unix 回填完成：{} / {total_pending} 条已写入 overlay",
        done.load(Ordering::Relaxed)
    );
    spawn_final_persist(engine, index_path);
    Ok(())
}

/// 用原始大小写名字沿父链拼 Unix 路径（小写物化路径在大小写敏感盘上打不开）。
#[cfg(unix)]
fn unix_entry_fs_path(store: &findx2_core::IndexStore, idx: usize) -> Option<String> {
    let prefix = store.volume_for_entry(idx)?.root_prefix.clone();
    let e = store.entries.get(idx)?;
    let mut segs: Vec<String> = Vec::new();
    if e.is_dir_entry() {
        let fr = store.frns.get(idx).copied().unwrap_or(0);
        let mut di = *store.dir_index.get(&fr)?;
        loop {
            let d = store.dirs.get(di as usize)?;
            if let Some(n) = unix_dir_name(store, d) {
                if !n.is_empty() {
                    segs.push(n.to_string());
                }
            }
            if d.parent_idx == 0 {
                break;
            }
            di = d.parent_idx;
        }
    } else {
        segs.push(store.name_str(e).ok()?.to_string());
        let mut di = e.dir_idx;
        while (di as usize) < store.dirs.len() {
            let d = store.dirs.get(di as usize)?;
            if let Some(n) = unix_dir_name(store, d) {
                if !n.is_empty() {
                    segs.push(n.to_string());
                }
            }
            if d.parent_idx == 0 {
                break;
            }
            di = d.parent_idx;
        }
    }
    segs.reverse();
    let rel = segs.join("/");
    let prefix = prefix.trim_end_matches('/');
    if rel.is_empty() {
        Some(if prefix.is_empty() {
            "/".into()
        } else {
            prefix.to_string()
        })
    } else if prefix.is_empty() || prefix == "/" {
        Some(format!("/{rel}"))
    } else {
        Some(format!("{prefix}/{rel}"))
    }
}

#[cfg(unix)]
fn unix_dir_name<'a>(
    store: &'a findx2_core::IndexStore,
    d: &findx2_core::index::DirEntry,
) -> Option<&'a str> {
    let start = d.name_offset as usize;
    let end = start + d.name_len as usize;
    let raw = store.names_buf.get(start..end)?;
    let raw = raw.strip_suffix(&[0]).unwrap_or(raw);
    std::str::from_utf8(raw).ok()
}

/// 单卷待回填集合（P1-5 按卷画像独立建池时跨函数传递）。

/// overlay 断点边车 `<index>.overlay.bin`（P2 回填续跑）。
/// 格式（LE）：magic "FXOV" u32 + version u32(1) + entry_count u64 + count u64 +
/// 每条 24B（idx u64, size u64, mtime_secs u32, ctime_secs u32）。
/// 原子落盘（tmp + rename）；entry_count 对不上直接丢弃（索引换过）。
const OVERLAY_MAGIC: u32 = 0x4658_4F56;
const OVERLAY_VERSION: u32 = 1;

fn overlay_sidecar_path(index_path: &std::path::Path) -> std::path::PathBuf {
    let mut p = index_path.as_os_str().to_owned();
    p.push(".overlay.bin");
    std::path::PathBuf::from(p)
}

fn save_overlay_sidecar(
    engine: &Arc<SearchEngine>,
    index_path: &std::path::Path,
) -> anyhow::Result<()> {
    let entry_count = engine.index_store().entry_count();
    let snap = engine.metadata_overlay_snapshot();
    let path = overlay_sidecar_path(index_path);
    let mut buf = Vec::with_capacity(24 + snap.len() * 24);
    buf.extend_from_slice(&OVERLAY_MAGIC.to_le_bytes());
    buf.extend_from_slice(&OVERLAY_VERSION.to_le_bytes());
    buf.extend_from_slice(&(entry_count as u64).to_le_bytes());
    buf.extend_from_slice(&(snap.len() as u64).to_le_bytes());
    for (idx, size, mtime, ctime) in &snap {
        buf.extend_from_slice(&(*idx as u64).to_le_bytes());
        buf.extend_from_slice(&size.to_le_bytes());
        buf.extend_from_slice(&mtime.to_le_bytes());
        buf.extend_from_slice(&ctime.to_le_bytes());
    }
    let tmp = path.with_extension("overlay.tmp");
    std::fs::write(&tmp, &buf)?;
    std::fs::rename(&tmp, &path)?;
    info!(
        "回填断点已保存：{} 条 overlay（{}MB）",
        snap.len(),
        buf.len() / (1024 * 1024)
    );
    Ok(())
}

/// 加载断点边车（entry_count 对不上就丢弃）；返回载入条数。
fn load_overlay_sidecar(
    engine: &Arc<SearchEngine>,
    index_path: &std::path::Path,
) -> anyhow::Result<usize> {
    let path = overlay_sidecar_path(index_path);
    let body = match std::fs::read(&path) {
        Ok(b) => b,
        Err(_) => return Ok(0),
    };
    if body.len() < 24 {
        return Ok(0);
    }
    let magic = u32::from_le_bytes(body[0..4].try_into().unwrap());
    let ver = u32::from_le_bytes(body[4..8].try_into().unwrap());
    let entry_count = u64::from_le_bytes(body[8..16].try_into().unwrap()) as usize;
    let count = u64::from_le_bytes(body[16..24].try_into().unwrap()) as usize;
    let current = engine.index_store().entry_count();
    if magic != OVERLAY_MAGIC || ver != OVERLAY_VERSION || entry_count != current {
        info!("断点边车版本/规模不符（已删），从头回填");
        let _ = std::fs::remove_file(&path);
        return Ok(0);
    }
    if body.len() < 24 + count * 24 {
        return Ok(0);
    }
    let mut batch: Vec<(usize, u64, u64, u64)> = Vec::with_capacity(8192);
    let mut loaded = 0usize;
    for i in 0..count {
        let o = 24 + i * 24;
        let idx = u64::from_le_bytes(body[o..o + 8].try_into().unwrap()) as usize;
        let size = u64::from_le_bytes(body[o + 8..o + 16].try_into().unwrap());
        let mtime = u32::from_le_bytes(body[o + 16..o + 20].try_into().unwrap());
        let ctime = u32::from_le_bytes(body[o + 20..o + 24].try_into().unwrap());
        // overlay 存 unix 秒；extend 接口吃 FILETIME，转回去（秒级无损）。
        batch.push((
            idx,
            size,
            findx2_core::index::unix_secs_to_filetime(mtime),
            findx2_core::index::unix_secs_to_filetime(ctime),
        ));
        if batch.len() >= 8192 {
            engine.extend_metadata_overlay_batch(&batch);
            loaded += batch.len();
            batch.clear();
        }
    }
    if !batch.is_empty() {
        engine.extend_metadata_overlay_batch(&batch);
        loaded += batch.len();
    }
    info!("断点续跑：载入 overlay {} 条", loaded);
    Ok(loaded)
}

fn delete_overlay_sidecar(index_path: &std::path::Path) {
    let _ = std::fs::remove_file(overlay_sidecar_path(index_path));
}

#[cfg(windows)]
fn backfill_loop(engine:Arc<SearchEngine>,index_path:std::path::PathBuf)->anyhow::Result<()> {
    use crate::metadata_pipeline::{Limits,ticket_pass};
    use findx2_windows::{DirMetadataReader,fill_metadata_by_id_pooled};
    let epoch=engine.metadata_epoch();
    let coverage=engine.metadata_coverage(epoch).map_err(anyhow::Error::msg)?;
    engine.set_backfill_error(None);engine.set_backfill_total(coverage.unknown as u64);
    let workers=std::thread::available_parallelism().map(|n|n.get()/2).unwrap_or(2).clamp(1,4);
    let limits=Limits{workers,..Limits::default()};
    let cancel=Arc::new(AtomicBool::new(false));
    let dirs=engine.index_store().dirs.len();
    let mut reader:Option<(u8,DirMetadataReader)>=None;
    let mut selected_volume=None;let mut failed_volumes=[false;256];
    let mut directory_pages=0usize;let mut directory_committed=0usize;let mut directory_errors=0usize;
    // One serial streaming reader keeps observation ordering sound and reserves
    // bounded memory even for a giant directory. File-ID fallback uses a separate
    // bounded IO pool, never Rayon's search pool. No repeated directory rescans.
    for di in 0..dirs {
        let Some((volume,frn))=engine.pending_metadata_directory(epoch,di).map_err(anyhow::Error::msg)? else{continue};
        if failed_volumes[volume as usize]{directory_errors+=1;continue;}
        if selected_volume!=Some(volume){
            selected_volume=Some(volume);
            let path=format!("\\\\.\\{}:",volume as char);
            reader=DirMetadataReader::open_volume(&path,limits.directory_bytes).ok().map(|r|(volume,r));
            if reader.is_none(){failed_volumes[volume as usize]=true;}
        }
        let Some((_,r))=reader.as_mut() else{directory_errors+=1;continue};
        if r.start_directory(frn,None).is_err(){directory_errors+=1;continue;}
        loop {
            let stamp=engine.begin_metadata_observation(epoch).map_err(anyhow::Error::msg)?;
            match r.read_page(){
                Ok(Some(records))=>{
                    directory_pages+=1;
                    let committed=engine.commit_metadata_observation(volume,stamp,records,limits.directory_bytes).map_err(anyhow::Error::msg)?;
                    directory_committed+=committed;engine.add_backfill_done(committed as u64);
                }
                Ok(None)=>break,
                Err(_)=>{directory_errors+=1;break;}
            }
            std::thread::yield_now();
        }
    }
    let reader_bytes=reader.as_ref().map(|r|r.1.capacity_bytes()).unwrap_or(0);drop(reader);
    let pool=rayon::ThreadPoolBuilder::new().num_threads(limits.workers).thread_name(|i|format!("findx2-backfill-bounded-{i}")).build()?;
    let report=ticket_pass(&engine,epoch,&cancel,limits,|volume,items|{
        let path=format!("\\\\.\\{}:",volume as char);
        let frns:Vec<_>=items.iter().map(|i|i.frn).collect();
        let ids=vec![None;items.len()];let indices:Vec<_>=(0..items.len()).collect();
        Ok(pool.install(||fill_metadata_by_id_pooled(&path,&frns,&ids,&indices,None,Some(cancel.clone()))))
    }).map_err(anyhow::Error::msg)?;
    let coverage=engine.metadata_coverage(epoch).map_err(anyhow::Error::msg)?;
    info!(directory_pages,directory_committed,directory_errors,reader_bytes,?report,unknown=coverage.unknown,"bounded metadata pass complete");
    if coverage.unknown!=0 {
        engine.set_backfill_error(Some(format!("元数据回填遍历完成但仍有 {} 条未验证（失败/并发变更）；metadata_ready 保持 false，重启/重建后可重试",coverage.unknown)));
        return Ok(());
    }
    // No full overlay sidecar snapshot. The existing overlay is drained in bounded
    // pages at final persist; failure coverage never becomes metadata_ready=true.
    spawn_final_persist_for_epoch(engine,index_path,Some(epoch));
    Ok(())
}
