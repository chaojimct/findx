//! 元数据队列。停止时不再把已经排队的文件逐批 `OpenFileById`。
//!
//! 已经进入内核的那一次打开不会被取消，也没有时间上限。取消只保证这次调用返回之后
//! 不再开始下一次打开。没打开的已有文件写成持久未验证，FRN、名字、父目录和 USN 游标不动。
//! 记不成未验证就不确认对应的 reservation。

#![cfg_attr(not(windows), allow(dead_code))]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::time::Duration;

use findx2_core::SearchEngine;

pub(crate) type StatUpdate = (usize, u64, u64, u64);
pub(crate) type StatWork = (Option<[u8; 16]>, Option<crate::runtime_lifecycle::Activity>, u64);
pub(crate) type StatMessage = (u64, Option<[u8; 16]>, Option<crate::runtime_lifecycle::Reservation>, u64);

/// `false`：这条提示没有进入打开队列。已存在的文件先写成持久未验证，然后才确认 reservation。
/// 索引里没有这个活文件时不删除、不改其它 FRN。未验证状态写失败则不确认。
pub(crate) fn offer_stat(
    tx: &SyncSender<StatMessage>,
    engine: &Arc<SearchEngine>,
    volume: u8,
    file_id: u64,
    file_id_128: Option<[u8; 16]>,
    stop: &AtomicBool,
) -> anyhow::Result<bool> {
    if file_id_128.is_some_and(|id| id[8..].iter().any(|&b| b != 0)) {
        anyhow::bail!("full 128-bit stat binding unresolved; refusing low64 alias");
    }
    engine.mark_metadata_dirty(volume, file_id);
    if stop.load(Ordering::SeqCst) {
        return engine
            .retain_unverified_metadata(volume, file_id)
            .map(|_| false)
            .map_err(anyhow::Error::msg);
    }
    let mut queued = crate::runtime_lifecycle::reserve(engine, "queued-stat")
        .map_err(anyhow::Error::msg)?
        .map(|p| p.require_result());
    if stop.load(Ordering::SeqCst) {
        return match engine.retain_unverified_metadata(volume, file_id) {
            Ok(_) => {
                if let Some(reservation) = queued.as_mut() {
                    reservation.success();
                }
                Ok(false)
            }
            Err(error) => Err(anyhow::anyhow!(error)),
        };
    }
    match tx.try_send((file_id, file_id_128, queued, engine.metadata_epoch())) {
        Ok(()) => Ok(true),
        Err(TrySendError::Full((id, _, mut work, _))) | Err(TrySendError::Disconnected((id, _, mut work, _))) => {
            match engine.retain_unverified_metadata(volume, id) {
                Ok(_) => {
                    if let Some(reservation) = work.as_mut() {
                        reservation.success();
                    }
                    Ok(false)
                }
                Err(error) => Err(anyhow::anyhow!(error)),
            }
        }
    }
}

pub(crate) fn run_stat_worker<F>(
    rx: Receiver<StatMessage>,
    engine: Arc<SearchEngine>,
    volume_device_path: String,
    stop: Arc<AtomicBool>,
    batch_limit: usize,
    mut fetch: F,
) where
    F: FnMut(&[u64], &[Option<[u8; 16]>], &[usize], Option<Arc<AtomicBool>>) -> Vec<StatUpdate>,
{
    let mut pending: HashMap<u64, StatWork> = HashMap::new();
    let mut logged_no_perm = false;
    let mut open_batch = |pending: &mut HashMap<u64, StatWork>| {
        let cancel = Some(stop.clone());
        drain_stat_batch_with(
            &engine,
            &volume_device_path,
            pending,
            &mut logged_no_perm,
            Some(stop.as_ref()),
            |frns, ids, indices| fetch(frns, ids, indices, cancel.clone()),
        );
    };
    loop {
        if stop.load(Ordering::Acquire) {
            match rx.try_recv() {
                Ok(msg) => {
                    accept(&mut pending, msg);
                    continue;
                }
                Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) => match rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(msg) => {
                        accept(&mut pending, msg);
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) => continue,
                },
            }
        }
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(msg) => {
                accept(&mut pending, msg);
                if stop.load(Ordering::Acquire) {
                    continue;
                }
                if pending.len() >= batch_limit {
                    open_batch(&mut pending);
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if stop.load(Ordering::Acquire) {
                    continue;
                }
                if !pending.is_empty() {
                    open_batch(&mut pending);
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if !stop.load(Ordering::Acquire) && pending.len() >= 50_000 {
            open_batch(&mut pending);
        }
    }
    while let Ok(msg) = rx.try_recv() {
        accept(&mut pending, msg);
    }
    if stop.load(Ordering::Acquire) {
        release_unopened_stat(&engine, &volume_device_path, &mut pending);
    } else if !pending.is_empty() {
        open_batch(&mut pending);
    }
}

pub(crate) fn drain_stat_batch_with(
    engine: &Arc<SearchEngine>,
    volume_device_path: &str,
    pending: &mut HashMap<u64, StatWork>,
    logged_no_perm: &mut bool,
    cancel: Option<&AtomicBool>,
    fetch: impl FnOnce(&[u64], &[Option<[u8; 16]>], &[usize]) -> Vec<StatUpdate>,
) {
    if pending.is_empty() {
        return;
    }
    if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        release_unopened_stat(engine, volume_device_path, pending);
        *logged_no_perm = false;
        return;
    }
    let volume = volume_letter(volume_device_path);
    let Some(source) = stat_source(volume) else {
        return;
    };
    // 队列里的 reservation 在收进 pending 时已经激活。这里的子 admission 可以在
    // prepare/STOP 期间结束，不会堵在自己的父工作后面。
    let mut batch = match crate::runtime_lifecycle::enter(engine, source) {
        Ok(activity) => activity.require_result(),
        Err(_) if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) => {
            release_unopened_stat(engine, volume_device_path, pending);
            return;
        }
        Err(_) => return,
    };
    let accepted = std::mem::take(pending);
    let mut accepted: HashMap<_, _> = accepted
        .into_iter()
        .filter_map(|(frn, (id, mut work, epoch))| {
            if stat_obsolete(engine, volume, frn, epoch) {
                if let Some(item) = work.as_mut() {
                    item.success();
                }
                None
            } else {
                Some((frn, (id, work, epoch)))
            }
        })
        .collect();
    let mut frns: Vec<_> = accepted.keys().copied().collect();
    frns.sort_unstable();
    let ids: Vec<_> = frns.iter().map(|frn| accepted[frn].0).collect();
    let tickets: Vec<_> = frns.iter().map(|frn| engine.metadata_ticket_by_id(volume, *frn)).collect();
    struct Lease<'a> {
        engine: &'a SearchEngine,
        tickets: &'a [Option<findx2_core::usage::MetadataTicket>],
    }
    impl Drop for Lease<'_> {
        fn drop(&mut self) {
            for ticket in self.tickets.iter().flatten() {
                self.engine.release_metadata_ticket(*ticket);
            }
        }
    }
    let _lease = Lease { engine, tickets: &tickets };
    let indices: Vec<_> = tickets.iter().enumerate().filter_map(|(i, ticket)| ticket.map(|_| i)).collect();
    // fetch 返回之前，frns/ids/indices 都还活着。取消不能在这次调用返回前释放它们。
    let updates = if indices.is_empty() { Vec::new() } else { fetch(&frns, &ids, &indices) };
    let cancelled = cancel.is_some_and(|flag| flag.load(Ordering::Acquire));
    let mut committed = vec![false; frns.len()];
    let mut error = None;
    for (local, size, mtime, ctime) in updates {
        match tickets.get(local).copied().flatten() {
            Some(ticket) => match engine.commit_metadata(ticket, size, mtime, ctime) {
                Ok(true) => committed[local] = true,
                Ok(false) => {}
                Err(cause) => error = Some(cause),
            },
            None => error = Some("stat producer returned an unadmitted local identity".into()),
        }
    }
    let mut retained_unknown = false;
    for (i, frn) in frns.into_iter().enumerate() {
        let (id, mut work, epoch) = accepted.remove(&frn).unwrap();
        if committed[i] || stat_obsolete(engine, volume, frn, epoch) {
            if let Some(item) = work.as_mut() {
                item.success();
            }
        } else if cancelled {
            match engine.retain_unverified_metadata(volume, frn) {
                Ok(_) => {
                    retained_unknown = true;
                    if let Some(item) = work.as_mut() {
                        item.success();
                    }
                }
                Err(cause) => {
                    if let Some(item) = work.as_mut() {
                        item.error(cause.clone());
                    }
                    error = Some(cause);
                }
            }
        } else {
            pending.insert(frn, (id, work, epoch));
        }
    }
    if !pending.is_empty() || error.is_some() {
        *logged_no_perm = true;
        batch.error(error.unwrap_or_else(|| {
            format!(
                "{} stat operations unresolved (permission/transient/stale); ownership retained for retry, no deletion inferred",
                pending.len()
            )
        }));
    } else if retained_unknown {
        *logged_no_perm = false;
        // 未验证是接受过的未知状态，不是把这一卷之前的失败修掉了。
        batch.success();
    } else {
        *logged_no_perm = false;
        batch.recovered_source();
    }
}

pub(crate) fn release_unopened_stat(engine: &SearchEngine, volume_device_path: &str, pending: &mut HashMap<u64, StatWork>) {
    let volume = volume_letter(volume_device_path);
    let accepted = std::mem::take(pending);
    for (frn, (_id, mut work, epoch)) in accepted {
        if stat_obsolete(engine, volume, frn, epoch) {
            if let Some(item) = work.as_mut() {
                item.success();
            }
            continue;
        }
        match engine.retain_unverified_metadata(volume, frn) {
            Ok(_) => {
                if let Some(item) = work.as_mut() {
                    item.success();
                }
            }
            Err(cause) => {
                if let Some(item) = work.as_mut() {
                    item.error(cause);
                }
            }
        }
    }
}

fn accept(pending: &mut HashMap<u64, StatWork>, message: StatMessage) {
    let (frn, id128, work, epoch) = message;
    if let Some((_, Some(mut superseded), _)) = pending.insert(frn, (id128, work.map(|item| item.activate()), epoch)) {
        superseded.success();
    }
}

fn volume_letter(volume_device_path: &str) -> u8 {
    volume_device_path.chars().rev().find(|c| c.is_ascii_alphabetic()).unwrap_or('?').to_ascii_uppercase() as u8
}

fn stat_obsolete(engine: &SearchEngine, volume: u8, frn: u64, epoch: u64) -> bool {
    let current = engine.metadata_epoch();
    if epoch != 0 && current != 0 && current != epoch && engine.metadata_coverage(current).is_ok() {
        return true;
    }
    let store = engine.index_store();
    store.entry_on_volume(volume, frn).is_some_and(|idx| {
        store.frns.get(idx as usize) == Some(&frn)
            && store.entries.get(idx as usize).is_some_and(|entry| entry.is_deleted() || entry.is_dir_entry())
    })
}

fn stat_source(volume: u8) -> Option<&'static str> {
    const SOURCES: [&str; 26] = [
        "stat-batch-A", "stat-batch-B", "stat-batch-C", "stat-batch-D", "stat-batch-E", "stat-batch-F", "stat-batch-G",
        "stat-batch-H", "stat-batch-I", "stat-batch-J", "stat-batch-K", "stat-batch-L", "stat-batch-M", "stat-batch-N",
        "stat-batch-O", "stat-batch-P", "stat-batch-Q", "stat-batch-R", "stat-batch-S", "stat-batch-T", "stat-batch-U",
        "stat-batch-V", "stat-batch-W", "stat-batch-X", "stat-batch-Y", "stat-batch-Z",
    ];
    SOURCES.get(usize::from(volume.checked_sub(b'A')?)).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use findx2_core::{IndexBuilder, RawEntry, SearchEngine};
    use std::sync::mpsc;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicUsize;
    use std::thread;
    use std::time::Instant;

    fn stop_flag(value: bool) -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(value))
    }

    fn queue_message(engine: &SearchEngine, frn: u64) -> StatMessage {
        let work = crate::runtime_lifecycle::reserve(engine, "queued-stat").unwrap().map(|item| item.require_result());
        (frn, None, work, engine.metadata_epoch())
    }

    fn stat_fixture() -> (PathBuf, Arc<SearchEngine>, crate::runtime_lifecycle::Owner) {
        let dir = std::env::temp_dir().join(format!(
            "findx-stat-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let raw = |id, dir| RawEntry {
            file_id: id,
            file_id_128: None,
            parent_id: if dir { 0 } else { 1 },
            name: if dir { "root".into() } else { "file".into() },
            size: 0,
            mtime: 0,
            ctime: 0,
            attrs: if dir { 0x10 } else { 0 },
            is_dir: dir,
        };
        let mut store = IndexBuilder::new(b'F', 1, 7, 10).build_from_raw(vec![raw(10, false)], vec![raw(1, true)], false).unwrap();
        store.set_track_mutations(true);
        let engine = Arc::new(SearchEngine::new(store));
        engine.enable_usage(4 * 1024 * 1024).unwrap();
        let path = dir.join("index.bin");
        findx2_core::v6_save_full(&findx2_core::V6Layout::for_index(&path), &engine.index_store()).unwrap();
        let owner = crate::runtime_lifecycle::Owner::attach(engine.clone(), path, stop_flag(false)).unwrap();
        (dir, engine, owner)
    }

    fn queued(engine: &SearchEngine) -> HashMap<u64, StatWork> {
        let work = crate::runtime_lifecycle::reserve(engine, "queued-stat").unwrap().map(|item| item.require_result().activate());
        [(10, (None, work, engine.metadata_epoch()))].into_iter().collect()
    }

    struct Identity {
        frn: u64,
        size: u64,
        mtime: u32,
        ctime: u32,
        dir_idx: u32,
        name: Vec<u8>,
        unverified: bool,
        deleted: bool,
        directory: bool,
    }

    fn dir_name(store: &findx2_core::IndexStore, volume: u8, frn: u64) -> Vec<u8> {
        let idx = store.dir_on_volume(volume, frn).unwrap() as usize;
        let dir = &store.dirs[idx];
        assert_eq!(dir.frn, frn);
        let start = dir.name_offset as usize;
        store.names_buf[start..start + dir.name_len as usize].to_vec()
    }

    fn identity(store: &findx2_core::IndexStore, volume: u8, frn: u64) -> Identity {
        let idx = store.entry_on_volume(volume, frn).unwrap() as usize;
        assert_eq!(store.frns[idx], frn);
        let entry = store.entries[idx];
        let start = entry.name_offset as usize;
        let name = store.names_buf[start..start + entry.n_len as usize].to_vec();
        Identity {
            frn,
            size: entry.size,
            mtime: entry.mtime,
            ctime: entry.ctime,
            dir_idx: entry.dir_idx,
            name,
            unverified: entry.metadata_unverified(),
            deleted: entry.is_deleted(),
            directory: entry.is_dir_entry(),
        }
    }

    fn publish(engine: &SearchEngine, path: &Path) {
        let layout = findx2_core::V6Layout::for_index(path);
        let batch = engine.take_persistence_batch().expect("停止后应该有可提交的元数据");
        match findx2_core::v6_try_commit(&layout, &batch).expect("提交关闭代") {
            findx2_core::CommitOutcome::Delta { .. } => {}
            findx2_core::CommitOutcome::NeedFull { .. } => {
                let store = engine.index_store();
                findx2_core::v6_save_full(&layout, &store).unwrap();
            }
        }
    }

    fn corpus() -> (PathBuf, PathBuf, Arc<SearchEngine>, crate::runtime_lifecycle::Owner) {
        let dir = std::env::temp_dir().join(format!(
            "findx-stat-stop-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let file = |id, name: &str, size, mtime| RawEntry {
            file_id: id,
            file_id_128: None,
            parent_id: 1,
            name: name.into(),
            size,
            mtime,
            ctime: mtime,
            attrs: 0,
            is_dir: false,
        };
        let mut store = IndexBuilder::new(b'F', 1, 7, 97_445_207_112)
            .build_from_raw(
                vec![
                    file(10, "kept.bin", 111, 1_700_000_011),
                    file(11, "opened.bin", 222, 1_700_000_021),
                    file(12, "queued.bin", 333, 1_700_000_031),
                    file(13, "tail.bin", 444, 1_700_000_041),
                ],
                vec![RawEntry {
                    file_id: 1,
                    file_id_128: None,
                    parent_id: 0,
                    name: "root".into(),
                    size: 0,
                    mtime: 0,
                    ctime: 0,
                    attrs: 0x10,
                    is_dir: true,
                }],
                true,
            )
            .unwrap();
        for (frn, mtime) in [(10, 1_700_000_011u32), (11, 1_700_000_021), (12, 1_700_000_031), (13, 1_700_000_041)] {
            let idx = store.entry_on_volume(b'F', frn).unwrap() as usize;
            store.entries[idx].mtime = mtime;
            store.entries[idx].ctime = mtime + 1;
        }
        let path = dir.join("index.bin");
        findx2_core::v6_save_full(&findx2_core::V6Layout::for_index(&path), &store).unwrap();
        store.set_track_mutations(true);
        let engine = Arc::new(SearchEngine::new(store));
        engine.enable_usage(4 * 1024 * 1024).unwrap();
        let owner = crate::runtime_lifecycle::Owner::attach(engine.clone(), path.clone(), stop_flag(false)).unwrap();
        (dir, path, engine, owner)
    }

    #[test]
    fn full_stat_queue_returns_without_waiting_or_recording_a_failure() {
        let (dir, engine, owner) = stat_fixture();
        let (tx, rx) = mpsc::sync_channel(1);
        let first = crate::runtime_lifecycle::reserve(&engine, "queued-stat").unwrap().map(|item| item.require_result());
        tx.try_send((10u64, None, first, engine.metadata_epoch())).unwrap();
        let started = Instant::now();
        assert!(!offer_stat(&tx, &engine, b'F', 10, None, &AtomicBool::new(false)).unwrap());
        assert!(started.elapsed() < Duration::from_millis(200));
        assert!(owner.retained_errors().is_empty());
        match rx.try_recv() {
            Ok((10, None, mut work, _)) => {
                if let Some(reservation) = work.as_mut() {
                    reservation.success();
                }
            }
            Ok((id, _, _, _)) => panic!("saturated queue returned a different file id {id}"),
            Err(_) => panic!("saturated queue dropped the first hint"),
        }
        assert!(rx.try_recv().is_err());
        drop(rx);
        drop(owner);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn full_stat_queue_keeps_nonzero_size_unknown_and_the_queued_hint() {
        let (dir, path, engine, owner) = corpus();
        let (tx, rx) = mpsc::sync_channel(1);
        tx.try_send(queue_message(&engine, 10)).unwrap();
        let started = Instant::now();
        assert!(!offer_stat(&tx, &engine, b'F', 11, None, &AtomicBool::new(false)).unwrap());
        assert!(started.elapsed() < Duration::from_millis(200));
        assert!(owner.retained_errors().is_empty());
        assert_eq!(engine.metadata_claim(b'F', 11), Some((222, 1_700_000_021, 1_700_000_022, false)));
        assert_eq!(engine.metadata_claim(b'F', 10).unwrap().0, 111);
        assert!(!engine.metadata_ready());
        assert_eq!(engine.metadata_coverage(engine.metadata_epoch()).unwrap().unknown, 1);
        match rx.try_recv() {
            Ok((10, None, mut work, _)) => {
                if let Some(reservation) = work.as_mut() {
                    reservation.success();
                }
            }
            Ok((id, _, _, _)) => panic!("queued hint changed id {id}"),
            Err(_) => panic!("full queue dropped the retained hint"),
        }
        assert!(rx.try_recv().is_err());
        drop(rx);
        drop(owner);
        std::fs::remove_dir_all(dir).unwrap();
        let _ = path;
    }

    #[test]
    fn stat_settlement_authoritative_delete_and_replaced_epoch_do_not_fake_failure() {
        for replace in [false, true] {
            let (dir, engine, owner) = stat_fixture();
            let mut pending = queued(&engine);
            if replace {
                engine.enable_usage(4 * 1024 * 1024).unwrap();
            } else {
                let idx = engine.index_store().entry_on_volume(b'F', 10).unwrap();
                engine.index_store_mut().delete_entry(idx);
            }
            drain_stat_batch_with(&engine, r"\\.\F:", &mut pending, &mut false, None, |_, _, _| {
                panic!("obsolete IO must not run")
            });
            assert!(pending.is_empty());
            assert!(owner.retained_errors().is_empty());
            drop(owner);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn stat_settlement_permission_or_missing_results_retain_work_and_real_retry_repairs() {
        let (dir, engine, owner) = stat_fixture();
        let mut pending = queued(&engine);
        drain_stat_batch_with(&engine, r"\\.\F:", &mut pending, &mut false, None, |_, _, _| vec![]);
        assert_eq!(pending.len(), 1);
        assert!(!owner.retained_errors().is_empty());
        assert!(!engine.index_store().entries[engine.index_store().entry_on_volume(b'F', 10).unwrap() as usize].is_deleted());
        drain_stat_batch_with(&engine, r"\\.\F:", &mut pending, &mut false, None, |_, _, indices| {
            indices.iter().map(|idx| (*idx, 77, 0, 0)).collect()
        });
        assert!(pending.is_empty());
        assert!(owner.retained_errors().is_empty());
        drop(owner);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stat_settlement_stale_ticket_retries_without_old_generation_write() {
        let (dir, engine, owner) = stat_fixture();
        let mut pending = queued(&engine);
        let change = engine.clone();
        drain_stat_batch_with(&engine, r"\\.\F:", &mut pending, &mut false, None, move |_, _, indices| {
            change.mark_metadata_dirty(b'F', 10);
            indices.iter().map(|idx| (*idx, 999, 0, 0)).collect()
        });
        assert_eq!(pending.len(), 1);
        assert!(!owner.retained_errors().is_empty());
        drain_stat_batch_with(&engine, r"\\.\F:", &mut pending, &mut false, None, |_, _, indices| {
            indices.iter().map(|idx| (*idx, 88, 0, 0)).collect()
        });
        assert!(pending.is_empty());
        assert!(owner.retained_errors().is_empty());
        let store = engine.index_store();
        let idx = store.entry_on_volume(b'F', 10).unwrap();
        assert_eq!(store.entries[idx as usize].size, 88);
        drop(store);
        drop(owner);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn shutdown_saturated_stat_queue_persists_unverified_without_opening_and_reloads() {
        let (dir, path, engine, owner) = corpus();
        let before = {
            let store = engine.index_store();
            (
                store.volumes[0].last_usn,
                store.volumes[0].usn_journal_id,
                store.entries.len(),
                dir_name(&store, b'F', 1),
                identity(&store, b'F', 10),
            )
        };
        let opened = Arc::new(AtomicUsize::new(0));
        let opened_fetch = opened.clone();
        let stop = stop_flag(true);
        let (tx, rx) = mpsc::sync_channel(8);
        let worker_engine = engine.clone();
        let worker_stop = stop.clone();
        let worker = thread::spawn(move || {
            run_stat_worker(rx, worker_engine, r"\\.\F:".into(), worker_stop, 5_000, move |_, _, _, _| {
                opened_fetch.fetch_add(1, Ordering::SeqCst);
                panic!("停止后的饱和队列不能再打开文件");
            });
        });
        for frn in [11u64, 12, 13] {
            tx.send(queue_message(&engine, frn)).unwrap();
        }
        let started = Instant::now();
        drop(tx);
        worker.join().expect("stat worker 必须返回，不能把线程留在队列里");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "这是测试里的死锁探测，不是产品停止的时间上限"
        );
        assert_eq!(opened.load(Ordering::SeqCst), 0);
        assert!(owner.retained_errors().is_empty());
        assert!(!engine.metadata_ready());
        assert!(engine.set_metadata_ready(true).is_err());
        {
            let store = engine.index_store();
            assert_eq!(store.volumes[0].last_usn, before.0);
            assert_eq!(store.volumes[0].usn_journal_id, before.1);
            assert_eq!(store.entries.len(), before.2);
            assert_eq!(dir_name(&store, b'F', 1), before.3);
            let kept = identity(&store, b'F', 10);
            assert_eq!(kept.size, 111);
            assert!(!kept.unverified);
            assert_eq!(kept.name, before.4.name);
            assert_eq!(kept.dir_idx, before.4.dir_idx);
            for (frn, size, mtime, ctime) in [
                (11, 222, 1_700_000_021, 1_700_000_022),
                (12, 333, 1_700_000_031, 1_700_000_032),
                (13, 444, 1_700_000_041, 1_700_000_042),
            ] {
                let item = identity(&store, b'F', frn);
                assert_eq!(item.frn, frn);
                assert_eq!(item.size, size);
                assert_eq!(item.mtime, mtime);
                assert_eq!(item.ctime, ctime);
                assert!(item.unverified);
                assert!(!item.deleted);
                assert!(!item.directory);
            }
        }
        publish(&engine, &path);
        let loaded = findx2_core::v6_load(&findx2_core::V6Layout::for_index(&path)).unwrap();
        assert!(!loaded.metadata_ready);
        assert_eq!(loaded.volumes[0].last_usn, before.0);
        assert_eq!(loaded.volumes[0].usn_journal_id, before.1);
        assert_eq!(loaded.entries.len(), before.2);
        assert!(!identity(&loaded, b'F', 10).unverified);
        assert_eq!(identity(&loaded, b'F', 10).size, 111);
        for frn in [11u64, 12, 13] {
            let item = identity(&loaded, b'F', frn);
            assert!(item.unverified, "frn {frn} 重新加载后仍应是未验证");
            assert!(!item.deleted);
        }
        assert_eq!(dir_name(&loaded, b'F', 1), b"root");
        drop(owner);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn shutdown_inflight_open_finishes_then_unopened_queue_is_persisted_unverified() {
        let (dir, path, engine, owner) = corpus();
        let cursor = engine.index_store().volumes[0].last_usn;
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let fetches = Arc::new(AtomicUsize::new(0));
        let fetches_worker = fetches.clone();
        let stop = stop_flag(false);
        let (tx, rx) = mpsc::sync_channel(8);
        tx.send(queue_message(&engine, 11)).unwrap();
        let worker_engine = engine.clone();
        let worker_stop = stop.clone();
        let worker = thread::spawn(move || {
            run_stat_worker(rx, worker_engine, r"\\.\F:".into(), worker_stop, 1, move |frns, ids, indices, _| {
                let n = fetches_worker.fetch_add(1, Ordering::SeqCst);
                assert_eq!(n, 0, "取消之后不能再进下一次打开");
                let local = indices[0];
                let frn = frns[local];
                started_tx.send(frn).unwrap();
                release_rx.recv().unwrap();
                // 这次原生调用已经返回，缓冲区在 fetch 整个返回前都必须可读。
                assert_eq!(frns[local], frn);
                assert!(ids[local].is_none());
                vec![(local, 555, 0, 0)]
            });
        });
        assert_eq!(started_rx.recv_timeout(Duration::from_secs(2)).unwrap(), 11);
        tx.send(queue_message(&engine, 12)).unwrap();
        tx.send(queue_message(&engine, 13)).unwrap();
        stop.store(true, Ordering::SeqCst);
        release_tx.send(()).unwrap();
        let started = Instant::now();
        drop(tx);
        worker.join().expect("正在打开的那一次返回后，worker 必须退出");
        assert!(started.elapsed() < Duration::from_secs(3), "这是测试里的死锁探测，不是产品停止的时间上限");
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        assert!(owner.retained_errors().is_empty());
        assert!(!engine.metadata_ready());
        {
            let store = engine.index_store();
            assert_eq!(store.volumes[0].last_usn, cursor);
            let opened = identity(&store, b'F', 11);
            assert_eq!(opened.size, 555);
            assert!(!opened.unverified);
            assert_eq!(opened.name, b"opened.bin");
            for (frn, size) in [(12, 333), (13, 444)] {
                let item = identity(&store, b'F', frn);
                assert_eq!(item.size, size);
                assert!(item.unverified);
                assert!(!item.deleted);
            }
            let kept = identity(&store, b'F', 10);
            assert_eq!(kept.size, 111);
            assert!(!kept.unverified);
            assert_eq!(dir_name(&store, b'F', 1), b"root");
        }
        publish(&engine, &path);
        let loaded = findx2_core::v6_load(&findx2_core::V6Layout::for_index(&path)).unwrap();
        assert_eq!(loaded.volumes[0].last_usn, cursor);
        assert_eq!(identity(&loaded, b'F', 11).size, 555);
        assert!(!identity(&loaded, b'F', 11).unverified);
        assert!(identity(&loaded, b'F', 12).unverified);
        assert_eq!(identity(&loaded, b'F', 12).size, 333);
        assert!(identity(&loaded, b'F', 13).unverified);
        assert_eq!(identity(&loaded, b'F', 10).size, 111);
        assert!(!identity(&loaded, b'F', 10).unverified);
        drop(owner);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stop_or_disconnected_offer_persists_unverified_without_a_false_ack() {
        let (dir, path, engine, owner) = corpus();
        let (tx, rx) = mpsc::sync_channel(1);
        assert!(!offer_stat(&tx, &engine, b'F', 12, None, &AtomicBool::new(true)).unwrap());
        assert!(rx.try_recv().is_err(), "已经停止时不能再把文件送进打开队列");
        assert!(identity(&engine.index_store(), b'F', 12).unverified);
        assert_eq!(identity(&engine.index_store(), b'F', 12).size, 333);
        drop(rx);
        assert!(!offer_stat(&tx, &engine, b'F', 13, None, &AtomicBool::new(false)).unwrap());
        assert!(identity(&engine.index_store(), b'F', 13).unverified);
        let mut wide = [0u8; 16];
        wide[8] = 1;
        let error = offer_stat(&tx, &engine, b'F', 10, Some(wide), &AtomicBool::new(true)).unwrap_err();
        assert!(error.to_string().contains("128-bit"));
        assert!(!identity(&engine.index_store(), b'F', 10).unverified);
        assert_eq!(identity(&engine.index_store(), b'F', 10).size, 111);
        assert!(owner.retained_errors().is_empty());
        assert_eq!(engine.index_store().volumes[0].last_usn, 97_445_207_112);
        publish(&engine, &path);
        let loaded = findx2_core::v6_load(&findx2_core::V6Layout::for_index(&path)).unwrap();
        assert!(identity(&loaded, b'F', 12).unverified);
        assert!(identity(&loaded, b'F', 13).unverified);
        assert!(!identity(&loaded, b'F', 10).unverified);
        assert_eq!(loaded.volumes[0].last_usn, 97_445_207_112);
        drop(owner);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
