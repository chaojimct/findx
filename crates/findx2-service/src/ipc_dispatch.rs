//! JSON IPC 请求处理（命名管道与 Unix socket 共用）。

use std::sync::{Arc, Mutex, OnceLock, RwLock};

use findx2_core::{QueryParser, SearchEngine, SearchOptions};
use findx2_ipc::{IpcRequest, IpcResponse};

pub(crate) type EngineSlot = Arc<RwLock<Option<Arc<SearchEngine>>>>;

fn resident_index() -> &'static Mutex<Option<crate::everything_status::ResidentIndex>> {
    static SLOT: OnceLock<Mutex<Option<crate::everything_status::ResidentIndex>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

pub(crate) fn checkpoint_error_snapshot() -> Option<String> {
    #[cfg(windows)]
    { crate::run::last_persistence_error() }
    #[cfg(not(windows))]
    { None } // No receipt capability on this platform either; None does not imply durable.
}

pub(crate) fn req_kind_label(r: &IpcRequest) -> &'static str {
    match r {
        IpcRequest::PrepareStop{..}|IpcRequest::CommitStop{..}|IpcRequest::CancelStop{..}|IpcRequest::CheckpointIdentity{..}=>"StopManagement",
        IpcRequest::UpgradeCapabilities { .. } => "UpgradeCapabilities",
        IpcRequest::UsageCapabilities => "UsageCapabilities",
        IpcRequest::UsageChildren { .. } => "UsageChildren",
        IpcRequest::Ping => "Ping",
        IpcRequest::Status => "Status",
        IpcRequest::Search { .. } => "Search",
    }
}

pub(crate) fn process_request(slot: &EngineSlot, req: IpcRequest) -> IpcResponse {
    let engine = slot.read().ok().and_then(|g| g.clone());
    match (req, engine) {
        (IpcRequest::PrepareStop{..}|IpcRequest::CommitStop{..}|IpcRequest::CancelStop{..}|IpcRequest::CheckpointIdentity{..},_)=>IpcResponse::Error{message:"requires authenticated elevated Windows pipe transport".into()},
        (IpcRequest::UpgradeCapabilities { protocol_version: 1 }, engine) => IpcResponse::UpgradeCapabilitiesResult {
            protocol_version: 1,
            durable_prepare: false,
            runtime_error: engine.as_deref().and_then(crate::runtime_lifecycle::error_snapshot),
            last_checkpoint_error: checkpoint_error_snapshot(),
            reason: "protocol1 never authorizes service stop; protocol2 requires authenticated elevated Windows transport and a live prepare actor".into(),
        },
        (IpcRequest::UpgradeCapabilities { .. }, _) => IpcResponse::Error {
            message: "unsupported durable-upgrade protocol version; refuse before service stop".into(),
        },
        (IpcRequest::UsageCapabilities, Some(eng)) if eng.usage_available() => {
            IpcResponse::UsageCapabilitiesResult { logical_tree: 1, allocated: false }
        }
        (IpcRequest::UsageCapabilities, _) => IpcResponse::UsageCapabilitiesResult {
            logical_tree: 0,
            allocated: false,
        },
        (
            IpcRequest::UsageChildren {
                path,
                expected,
                after,
                max_bytes,
            },
            Some(eng),
        ) => match eng.usage_page(&path, expected, after, max_bytes) {
            Ok(p) => {
                let item = |n: findx2_core::usage::UsageItem| findx2_ipc::UsageItemDto {
                    id: n.id,
                    name: n.name,
                    directory: n.directory,
                    logical: n.logical,
                    files: n.files,
                    directories: n.directories,
                    unknown: n.unknown,
                };
                IpcResponse::UsageChildrenResult {
                    page: findx2_ipc::UsagePageDto {
                        epoch: p.epoch,
                        generation: p.generation,
                        root: item(p.root),
                        children: p.children.into_iter().map(item).collect(),
                        next: p.next,
                        resident_bytes: p.resident_bytes,
                        watch_error: crate::watch_health::watch_error_summary(),
                        metadata_error: eng.backfill_error_snapshot(),
                        state: format!(
                            "{}; watch={}",
                            p.state,
                            crate::watch_health::watch_error_summary().unwrap_or_else(|| {
                                "no reported error (not proof of metadata freshness)".into()
                            })
                        ),
                    },
                }
            }
            Err(message) => IpcResponse::Error { message },
        },
        (IpcRequest::UsageChildren { .. }, None) => IpcResponse::Error {
            message: "index loading; usage initializing".into(),
        },
        (IpcRequest::Ping, _) => IpcResponse::Pong,
        (IpcRequest::Status, None) => {
            // 只取一次快照：分三次取可能在两帧之间跨越阶段切换，导致同一响应里的
            // 阶段名与阶段序号对不上。
            let l = crate::load_state::snapshot();
            IpcResponse::StatusResult {
                entry_count: 0,
                dir_count: 0,
                last_usn: 0,
                journal_id: 0,
                volume_letter: None,
                healthy: false,
                metadata_ready: false,
                backfill_done: 0,
                backfill_total: 0,
                loading: true,
                loading_stage: l.as_ref().map(|s| crate::load_state::progress_line_of(s)),
                loading_elapsed_secs: l.as_ref().map(|s| s.started.elapsed().as_secs()),
                loading_phase_done: l.as_ref().map(|s| s.phases_done),
                loading_phase_total: l.as_ref().map(|s| s.phases_total),
                watch_error: crate::watch_health::watch_error_summary(),
                backfill_error: None,
                tombstone_count: None,
                indexed_drive_mask: 0,
                wal_bytes: 0,
                last_fold_secs_ago: None,
            }
        }
        (IpcRequest::Search { .. }, None) => IpcResponse::Error {
            message: "索引加载中，请稍候…".into(),
        },
        (
            IpcRequest::Search {
                query,
                pinyin,
                limit,
                offset,
            },
            Some(eng),
        ) => match search_ipc(&eng, &query, pinyin, limit, offset) {
            Ok((hits, total, elapsed_ms)) => IpcResponse::SearchResult {
                hits,
                total,
                elapsed_ms,
            },
            Err(message) => IpcResponse::Error { message },
        },
        (IpcRequest::Status, Some(eng)) => {
            let backfill = eng.backfill_progress_snapshot();
            let backfill_error = eng.backfill_error_snapshot();
            match eng.try_index_store() {
                Some(g) => {
                    let vol = g.volumes.first();
                    let (backfill_done, backfill_total) = if g.metadata_ready {
                        (0u64, 0u64)
                    } else {
                        backfill
                    };
                    let volume_letter = vol
                        .filter(|v| v.volume_letter != 0)
                        .map(|v| v.volume_letter as char);
                    let drive_mask = crate::everything_status::drive_mask_from_letters(
                        g.volumes.iter().map(|v| v.volume_letter),
                    );
                    *resident_index().lock().unwrap_or_else(|e| e.into_inner()) = Some(crate::everything_status::ResidentIndex {
                        entry_count: g.entry_count() as u64,
                        dir_count: g.dirs.len() as u64,
                        last_usn: vol.map(|v| v.last_usn).unwrap_or(0),
                        journal_id: vol.map(|v| v.usn_journal_id).unwrap_or(0),
                        volume_letter,
                        metadata_ready: g.metadata_ready,
                        tombstone_count: g.deleted.len() as u64,
                        drive_mask,
                    });
                    let (wal_bytes, last_fold_secs_ago) = crate::run::persistence_health_snapshot();
                    IpcResponse::StatusResult {
                        entry_count: g.entry_count() as u64,
                        dir_count: g.dirs.len() as u64,
                        last_usn: vol.map(|v| v.last_usn).unwrap_or(0),
                        journal_id: vol.map(|v| v.usn_journal_id).unwrap_or(0),
                        volume_letter,
                        healthy: crate::watch_health::watch_error_summary().is_none(),
                        metadata_ready: g.metadata_ready,
                        backfill_done,
                        backfill_total,
                        loading: false,
                        // 索引已可用，加载阶段信息不再有意义。
                        loading_stage: None,
                        loading_elapsed_secs: None,
                        loading_phase_done: None,
                        loading_phase_total: None,
                        watch_error: crate::watch_health::watch_error_summary(),
                        backfill_error: if g.metadata_ready {
                            None
                        } else {
                            backfill_error
                        },
                        tombstone_count: Some(g.deleted.len() as u64),
                        indexed_drive_mask: drive_mask,
                        wal_bytes,
                        last_fold_secs_ago,
                    }
                }
                None => {
                    let watch_error = crate::watch_health::watch_error_summary();
                    let locked = crate::everything_status::status_when_index_locked(
                        resident_index().lock().unwrap_or_else(|e| e.into_inner()).as_ref(),
                        backfill.0,
                        backfill.1,
                        watch_error.is_none(),
                    );
                    let l = crate::load_state::snapshot();
                    // WAL depth doesn't need the store lock; report it even on the fallback path.
                    let (wal_bytes, last_fold_secs_ago) = crate::run::persistence_health_snapshot();
                    IpcResponse::StatusResult {
                        entry_count: locked.entry_count,
                        dir_count: locked.dir_count,
                        last_usn: locked.last_usn,
                        journal_id: locked.journal_id,
                        volume_letter: locked.volume_letter,
                        healthy: locked.healthy,
                        metadata_ready: locked.metadata_ready,
                        backfill_done: locked.backfill_done,
                        backfill_total: locked.backfill_total,
                        loading: locked.loading,
                        loading_stage: if locked.loading { l.as_ref().map(|s| crate::load_state::progress_line_of(s)) } else { None },
                        loading_elapsed_secs: if locked.loading { l.as_ref().map(|s| s.started.elapsed().as_secs()) } else { None },
                        loading_phase_done: if locked.loading { l.as_ref().map(|s| s.phases_done) } else { None },
                        loading_phase_total: if locked.loading { l.as_ref().map(|s| s.phases_total) } else { None },
                        watch_error,
                        backfill_error,
                        tombstone_count: locked.tombstone_count,
                        indexed_drive_mask: locked.drive_mask,
                        wal_bytes,
                        last_fold_secs_ago,
                    }
                }
            }
        }
    }
}

pub(crate) fn search_ipc(
    engine: &SearchEngine,
    query: &str,
    pinyin: bool,
    limit_override: usize,
    offset: usize,
) -> std::result::Result<(Vec<findx2_ipc::SearchHitDto>, u32, u32), String> {
    let started = std::time::Instant::now();
    let t_parse = std::time::Instant::now();
    let pq = QueryParser::parse(query).map_err(|e| e.to_string())?;
    let lim = if limit_override == 0 {
        pq.limit as usize
    } else {
        limit_override
    };
    let parse_ms = t_parse.elapsed().as_micros() as u64;
    let t_search = std::time::Instant::now();
    let (hits, total) = engine
        .search_paged(
            query,
            &pq,
            &SearchOptions {
                allow_pinyin: pinyin,
                content: crate::content_index::content_opt(),
                ..Default::default()
            },
            offset,
            lim,
        )
        .map_err(|e| e.to_string())?;
    let search_us = t_search.elapsed().as_micros() as u64;
    let t_format = std::time::Instant::now();
    let dtos: Vec<findx2_ipc::SearchHitDto> = hits
        .into_iter()
        .map(|h| {
            findx2_ipc::SearchHitDto {
                entry_idx: h.entry_idx,
                name: h.name,
                path: h.path,
                size: h.size,
                mtime: h.mtime,
                is_directory: h.is_directory,
                name_highlight: h.name_highlight,
            }
        })
        .collect();
    let format_us = t_format.elapsed().as_micros() as u64;
    let elapsed_ms = started.elapsed().as_millis().min(u32::MAX as u128) as u32;
    findx2_core::progress!(
        "search [{}] -> {} hits / total {} : parse {}μs · core {}μs ({:.2}ms) · format {}μs · sum {}ms",
        query,
        dtos.len(),
        total,
        parse_ms,
        search_us,
        (search_us as f64) / 1000.0,
        format_us,
        elapsed_ms
    );
    Ok((dtos, total, elapsed_ms))
}
#[cfg(test)]
mod usage_tests {
    use super::*;
    #[test]
    fn independent_slot_pages_and_rejects_mixed_generations() {
        let root = findx2_core::RawEntry {
            file_id: 1,
            file_id_128: None,
            parent_id: 0,
            name: "fixture".into(),
            size: 0,
            mtime: 0,
            ctime: 0,
            attrs: 0x10,
            is_dir: true,
        };
        let g = findx2_core::IndexBuilder::new(b'F', 1, 1, 0)
            .build_from_raw(vec![], vec![root], true)
            .unwrap();
        let e = Arc::new(SearchEngine::new(g));
        e.enable_usage(1024 * 1024).unwrap();
        let slot: EngineSlot = Arc::new(RwLock::new(Some(e.clone())));
        assert!(matches!(
            process_request(&slot, IpcRequest::UsageCapabilities),
            IpcResponse::UsageCapabilitiesResult {
                logical_tree: 1,
                allocated: false
            }
        ));
        let req = |expected| IpcRequest::UsageChildren {
            path: "F:\\".into(),
            expected,
            after: None,
            max_bytes: 4096,
        };
        let (epoch, generation) = match process_request(&slot, req(None)) {
            IpcResponse::UsageChildrenResult { page } => (page.epoch, page.generation),
            _ => panic!("missing actual usage page"),
        };
        e.apply_volume_change(
            b'F',
            &findx2_core::ChangeEvent::CreatePending {
                file_id: 2,
                file_id_128: None,
                parent_id: 1,
                name: "new".into(),
                attrs: 0,
                is_dir: false,
            },
        )
        .unwrap();
        // A data delta advances the generation; paging within the same epoch
        // must survive it (live volumes advance the generation thousands of
        // times per second). The page reports the new generation.
        match process_request(&slot, req(Some((epoch, generation)))) {
            IpcResponse::UsageChildrenResult { page } => {
                assert!(page.generation > generation, "generation must advance with the delta");
            }
            _ => panic!("paging with a pre-delta stamp must survive generation churn"),
        }
        match process_request(&slot, req(None)) {
            IpcResponse::UsageChildrenResult { page } => {
                assert_eq!(page.root.files, 1);
                assert_eq!(page.root.unknown, 1);
                assert_eq!(page.children[0].name, "new");
            }
            _ => panic!("missing updated page"),
        }
        // An epoch change (runtime rebuild) is the real invalidation boundary.
        e.enable_usage(1024 * 1024).unwrap();
        assert!(matches!(
            process_request(&slot, req(Some((epoch, generation)))),
            IpcResponse::Error { .. }
        ));
    }

    #[test]
    fn watch_error_makes_status_unhealthy() {
        let _health = crate::watch_health::test_health_guard();
        let root = findx2_core::RawEntry {
            file_id: 1,
            file_id_128: None,
            parent_id: 0,
            name: "fixture".into(),
            size: 0,
            mtime: 0,
            ctime: 0,
            attrs: 0x10,
            is_dir: true,
        };
        let g = findx2_core::IndexBuilder::new(b'F', 1, 1, 0)
            .build_from_raw(vec![], vec![root], true)
            .unwrap();
        let e = Arc::new(SearchEngine::new(g));
        let slot: EngineSlot = Arc::new(RwLock::new(Some(Arc::clone(&e))));
        crate::watch_health::set_watch_error('Z', Some("parent identity missing; batch not committed".into()));
        match process_request(&slot, IpcRequest::Status) {
            IpcResponse::StatusResult { healthy, watch_error, .. } => {
                assert!(!healthy, "Status.healthy must reflect a visible watch error");
                assert!(watch_error.as_ref().is_some_and(|s| s.contains("parent identity missing")));
            }
            other => panic!("unexpected {other:?}"),
        }
        crate::watch_health::set_watch_error('Z', None);
        let published = match process_request(&slot, IpcRequest::Status) {
            IpcResponse::StatusResult { healthy, watch_error, entry_count, last_usn, journal_id, volume_letter, metadata_ready, .. } => {
                assert!(healthy, "Status.healthy must clear when watch errors are cleared");
                assert!(watch_error.is_none());
                (entry_count, last_usn, journal_id, volume_letter, metadata_ready)
            }
            other => panic!("unexpected {other:?}"),
        };
        let _write = e.index_store_mut();
        match process_request(&slot, IpcRequest::Status) {
            IpcResponse::StatusResult { healthy, loading, entry_count, last_usn, journal_id, volume_letter, metadata_ready, .. } => {
                assert!(healthy, "a held writer is not a watch failure");
                assert!(!loading, "a resident index stays published while its writer is busy");
                assert_eq!((entry_count, last_usn, journal_id, volume_letter, metadata_ready), published);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    #[test]
    fn fake_watcher_fences_failed_prefix_and_recovers_only_after_restart() {
        let _health = crate::watch_health::test_health_guard();
        use findx2_core::{ChangeEvent, RawEntry};
        let raw=|id,parent,dir| RawEntry {file_id:id,file_id_128:None,parent_id:parent,name:"synthetic".into(),size:1,mtime:0,ctime:0,attrs:if dir {0x10}else{0},is_dir:dir};
        let g=findx2_core::IndexBuilder::new(b'F',1,7,10).build_from_raw(vec![],vec![raw(1,0,true)],true).unwrap();
        let e=Arc::new(SearchEngine::new(g)); e.enable_usage(1024*1024).unwrap();
        let slot:EngineSlot=Arc::new(RwLock::new(Some(e.clone())));
        let (tx,rx)=std::sync::mpsc::channel::<(Vec<ChangeEvent>, u64)>();
        let engine=e.clone();
        let worker=std::thread::spawn(move || {
            let mut gate=crate::watch_health::NamespaceGate::default();
            for (events,cursor) in rx {
                let _=gate.apply(&engine,b'F',&events,7,cursor);
            }
            gate.blocked
        });
        tx.send((vec![ChangeEvent::Create{entry:raw(2,1,false)},ChangeEvent::CreatePending{file_id:3,file_id_128:Some([1;16]),parent_id:1,name:"bad".into(),attrs:0,is_dir:false}],20)).unwrap();
        tx.send((vec![ChangeEvent::Create{entry:raw(4,1,false)}],21)).unwrap();
        tx.send((vec![],22)).unwrap(); drop(tx);
        assert!(worker.join().unwrap());
        assert_eq!(e.index_store().volumes[0].last_usn,10);
        assert!(e.index_store().entry_on_volume(b'F',2).is_none());
        assert!(e.index_store().entry_on_volume(b'F',4).is_none());
        assert!(matches!(process_request(&slot,IpcRequest::Status),IpcResponse::StatusResult{healthy:false,watch_error:Some(_),..}));
        assert!(matches!(process_request(&slot,IpcRequest::UsageChildren{path:"F:\\".into(),expected:None,after:None,max_bytes:4096}),IpcResponse::UsageChildrenResult{..}));
        let mut recovered=crate::watch_health::NamespaceGate::default();
        recovered.apply(&e,b'F',&[ChangeEvent::Create{entry:raw(2,1,false)}],7,20).unwrap();
        assert_eq!(e.index_store().volumes[0].last_usn,20);
        assert!(matches!(process_request(&slot,IpcRequest::Status),IpcResponse::StatusResult{healthy:true,watch_error:None,..}));
    }

}

#[cfg(test)] mod search_snapshot_tests {
    #[test] fn directory_kind_is_carried_by_search_snapshot() {
        let raw=|id,parent,name:&str,dir|findx2_core::RawEntry { file_id:id,file_id_128:None,parent_id:parent,name:name.into(),size:if dir {0}else{123},mtime:0,ctime:0,attrs:if dir{16}else{0},is_dir:dir };
        let store=findx2_core::IndexBuilder::new(b'F',1,7,0).build_from_raw(vec![raw(10,2,"public-file.txt",false)],vec![raw(1,0,"root",true),raw(2,1,"public-folder",true)],true).unwrap();
        let engine=findx2_core::SearchEngine::new(store);
        let (hits,_,_)=super::search_ipc(&engine,"public",false,50,0).unwrap();
        assert!(hits.iter().any(|h| h.name=="public-folder" && h.is_directory));
        assert!(hits.iter().any(|h| h.name=="public-file.txt" && !h.is_directory));
    }
}

#[cfg(test)]
mod upgrade_negotiation_tests {
    use super::*;
    #[test]
    fn protocol1_capability_and_unauthenticated_protocol2_never_authorize_stop() {
        let slot: EngineSlot = Arc::new(RwLock::new(None));
        assert!(matches!(process_request(&slot, IpcRequest::UpgradeCapabilities { protocol_version: 1 }), IpcResponse::UpgradeCapabilitiesResult { protocol_version: 1, durable_prepare: false, .. }));
        assert!(matches!(process_request(&slot, IpcRequest::UpgradeCapabilities { protocol_version: 2 }), IpcResponse::Error { .. }));
        assert!(matches!(process_request(&slot, IpcRequest::UpgradeCapabilities { protocol_version: 252 }), IpcResponse::Error { .. }));
    }
}
