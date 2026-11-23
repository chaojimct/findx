//! service 常驻内容索引：打开 + 增量跟随 + 查询装配。
//!
//! - `init(index_bin, engine)`：`<index>.content/` 存在则 mmap 打开，并起后台线程；
//!   不存在则保持关闭（`content:` 查询自动回退逐文件慢路径，行为正确只是慢）。
//! - `notify_applied(volume, events)`：watch 热路径调，**只做事件→job 的轻量提取**
//!   （不碰 store 读锁、不读文件），job 经有界 channel 扔给后台线程。
//! - `content_opt()`：`search_ipc` / Everything `do_search` 共用，装入
//!   `SearchOptions.content`，使 `content:` 走 tantivy 快路径。
//! - 后台线程：resolve 路径（读锁）→ 抽取 → upsert/delete → 周期 commit。
//!   抽取是 IO 密集，线程优先级默认即可（M2 再做闲时/限速治理）。

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use findx2_core::{ChangeEvent, SearchEngine};

#[derive(Clone, Copy)]
enum JobKind {
    Upsert,
    Delete,
}

struct Job {
    volume: u8,
    frn: u64,
    kind: JobKind,
}

struct Hub {
    index: Arc<findx2_content::ContentIndex>,
    tx: std::sync::mpsc::SyncSender<Vec<Job>>,
}

fn hub() -> &'static Mutex<Option<Hub>> {
    static H: OnceLock<Mutex<Option<Hub>>> = OnceLock::new();
    H.get_or_init(|| Mutex::new(None))
}

/// service 启动后调（engine 已挂 EngineSlot 时机均可，早调早跟随）。
/// 内容索引不存在 → 返回 false，保持关闭，不报错。
pub(crate) fn init(index_bin: &std::path::Path, engine: Arc<SearchEngine>) -> bool {
    let Some(cidx) = findx2_content::open_if_exists(index_bin) else {
        tracing::info!(
            "内容索引未建（{} 不存在）：`content:` 查询走慢路径；`findx2 content-index` 构建后加速",
            findx2_content::content_dir_for(index_bin).display()
        );
        return false;
    };
    let cidx = Arc::new(cidx);
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<Job>>(256);
    {
        let mut g = hub().lock().expect("content hub 锁中毒");
        if g.is_some() {
            return true;
        }
        *g = Some(Hub {
            index: cidx.clone(),
            tx,
        });
    }
    let docs = cidx.num_docs();
    std::thread::Builder::new()
        .name("findx2-content-index".into())
        .spawn(move || worker_loop(engine, cidx, rx))
        .ok();
    tracing::info!(
        "内容索引已挂载：{}（{} docs），增量跟随启动",
        findx2_content::content_dir_for(index_bin).display(),
        docs
    );
    true
}

/// 查询装配：search_ipc / do_search 共用。
pub(crate) fn content_opt() -> Option<Arc<dyn findx2_core::ContentSearch>> {
    let g = hub().lock().ok()?;
    g.as_ref()
        .map(|h| h.index.clone() as Arc<dyn findx2_core::ContentSearch>)
}

/// watch 热路径调：apply 成功后把事件翻译成 job（不阻塞，不碰 store）。
/// channel 满则整批丢弃并计数（回放/USN 风暴时保搜索优先，下次全量重建可自愈）。
pub(crate) fn notify_applied(volume: u8, events: &[ChangeEvent]) {
    let jobs: Vec<Job> = events
        .iter()
        .filter_map(|ev| match ev {
            ChangeEvent::Create { entry } if !entry.is_dir => Some(Job {
                volume,
                frn: entry.file_id,
                kind: JobKind::Upsert,
            }),
            ChangeEvent::CreatePending { file_id, is_dir, .. } if !is_dir => Some(Job {
                volume,
                frn: *file_id,
                kind: JobKind::Upsert,
            }),
            ChangeEvent::Delete { file_id } => Some(Job {
                volume,
                frn: *file_id,
                kind: JobKind::Delete,
            }),
            ChangeEvent::Rename { file_id, .. } => Some(Job {
                volume,
                frn: *file_id,
                kind: JobKind::Upsert,
            }),
            ChangeEvent::DataOrMeta { file_id, .. } => Some(Job {
                volume,
                frn: *file_id,
                kind: JobKind::Upsert,
            }),
            _ => None,
        })
        .collect();
    if jobs.is_empty() {
        return;
    }
    let dropped = {
        let g = match hub().lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        match g.as_ref() {
            Some(h) => h.tx.try_send(jobs).is_err(),
            None => return,
        }
    };
    if dropped {
        tracing::warn!("内容索引 channel 满：丢弃一批增量（搜索优先；重建可自愈）");
    }
}

fn worker_loop(
    engine: Arc<SearchEngine>,
    index: Arc<findx2_content::ContentIndex>,
    rx: std::sync::mpsc::Receiver<Vec<Job>>,
) {
    let mut since_commit = 0u64;
    let mut last_commit = Instant::now();
    let mut upserts = 0u64;
    let mut deletes = 0u64;
    let mut failed = 0u64;
    loop {
        // 5 秒一批：忙时吞吐，闲时及时可见。
        let mut batch: Vec<Job> = match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(j) => j,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Vec::new(),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        while let Ok(mut more) = rx.try_recv() {
            batch.append(&mut more);
            if batch.len() >= 5000 {
                break;
            }
        }
        for job in &batch {
            match apply_job(&engine, &index, job) {
                Ok(true) => {
                    since_commit += 1;
                    match job.kind {
                        JobKind::Upsert => upserts += 1,
                        JobKind::Delete => deletes += 1,
                    }
                }
                Ok(false) => {}
                Err(_) => failed += 1,
            }
        }
        if since_commit >= 500 || (since_commit > 0 && last_commit.elapsed() >= Duration::from_secs(30)) {
            if index.commit().is_ok() {
                since_commit = 0;
                last_commit = Instant::now();
            }
        }
    }
    let _ = index.commit();
    tracing::info!(
        "内容索引线程退出：upsert {upserts} / delete {deletes} / 失败 {failed}"
    );
}

/// 单 job：resolve → 抽取 → 入库。返回 Ok(true)=产生变更。
fn apply_job(
    engine: &Arc<SearchEngine>,
    index: &Arc<findx2_content::ContentIndex>,
    job: &Job,
) -> Result<bool, ()> {
    // 路径解析：FRN 在多卷下可能碰撞，先按 volume_letter 命中卷，兜底扫全卷。
    let (slot, vol_key, path, mtime, size) = {
        let g = engine.index_store();
        let mut hit: Option<(u32, String, String, u64, u64)> = None;
        for (i, v) in g.volumes.iter().enumerate() {
            if v.volume_letter != job.volume && job.volume != 0 {
                continue;
            }
            let idx = match g.frn_to_entry.get_scoped(job.frn, i as u32) {
                Some(idx) => idx as usize,
                None => continue,
            };
            let e = match g.entries.get(idx) {
                Some(e) => e,
                None => continue,
            };
            if e.is_dir_entry() || e.is_deleted() {
                // 目录/墓碑：Delete 事件走删除；Upsert 遇到说明条目已变，直接跳过。
                if matches!(job.kind, JobKind::Delete) {
                    hit = Some((
                        i as u32,
                        findx2_core::volume_key_for(&v.volume_id, i),
                        String::new(),
                        0,
                        0,
                    ));
                    break;
                }
                continue;
            }
            let p = g.entry_display_path(idx).map_err(|_| ())?;
            hit = Some((
                i as u32,
                findx2_core::volume_key_for(&v.volume_id, i),
                p,
                e.mtime as u64,
                e.size,
            ));
            break;
        }
        // volume_letter==0（unix 多卷）或字母对不上：全卷扫 FRN。
        if hit.is_none() {
            for (i, v) in g.volumes.iter().enumerate() {
                let idx = match g.frn_to_entry.get_scoped(job.frn, i as u32) {
                    Some(idx) => idx as usize,
                    None => continue,
                };
                let e = match g.entries.get(idx) {
                    Some(e) => e,
                    None => continue,
                };
                if e.is_dir_entry() || e.is_deleted() {
                    continue;
                }
                let p = g.entry_display_path(idx).map_err(|_| ())?;
                hit = Some((
                    i as u32,
                    findx2_core::volume_key_for(&v.volume_id, i),
                    p,
                    e.mtime as u64,
                    e.size,
                ));
                break;
            }
        }
        hit.ok_or(())?
    };
    let _ = slot;
    match job.kind {
        JobKind::Delete => {
            index.delete_by_frn(&vol_key, job.frn).map_err(|_| ())?;
            Ok(true)
        }
        JobKind::Upsert => {
            let p = std::path::Path::new(&path);
            if findx2_content::should_extract(p).is_none() {
                return Ok(false);
            }
            let t = findx2_content::extract_text(p).map_err(|_| ())?;
            index
                .index_doc(
                    &findx2_content::DocMeta {
                        frn: job.frn,
                        volume_id: vol_key,
                        path,
                        mtime,
                        size,
                    },
                    &t.text,
                )
                .map_err(|_| ())?;
            Ok(true)
        }
    }
}
