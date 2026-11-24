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
        IpcRequest::ContentBuild { .. } => "ContentBuild",
        IpcRequest::ContentCancel => "ContentCancel",
        IpcRequest::ContentSnippets { .. } => "ContentSnippets",
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
                // 索引还没加载：内容状态未知（GUI 按缺省/未知处理）。
                content_state: String::new(),
                content_docs: 0,
                content_bytes: 0,
                content_scanned: 0,
                content_extracted: 0,
                content_skipped: 0,
                content_failed: 0,
                content_error: None,
                content_detail: String::new(),
                content_entry_total: 0,
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
                pinyin_initials,
                pinyin_full,
            },
            Some(eng),
        ) => match search_ipc(&eng, &query, pinyin, limit, offset, pinyin_initials, pinyin_full) {
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
                    let cs = crate::content_index::status_snapshot();
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
                        content_state: cs.state.to_string(),
                        content_docs: cs.docs,
                        content_bytes: cs.bytes,
                        content_scanned: cs.scanned,
                        content_extracted: cs.extracted,
                        content_skipped: cs.skipped,
                        content_failed: cs.failed,
                        content_error: cs.error,
                        content_detail: cs.detail,
                        content_entry_total: cs.entry_total,
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
                    let cs = crate::content_index::status_snapshot();
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
                        content_state: cs.state.to_string(),
                        content_docs: cs.docs,
                        content_bytes: cs.bytes,
                        content_scanned: cs.scanned,
                        content_extracted: cs.extracted,
                        content_skipped: cs.skipped,
                        content_failed: cs.failed,
                        content_error: cs.error,
                        content_detail: cs.detail,
                        content_entry_total: cs.entry_total,
                    }
                }
            }
        }
        (IpcRequest::ContentBuild { rebuild }, _) => {
            match crate::content_index::start_build(rebuild) {
                Ok(_) => IpcResponse::Pong,
                Err(message) => IpcResponse::Error { message },
            }
        }
        (IpcRequest::ContentCancel, _) => {
            // 幂等：无构建在跑也是 Pong，GUI 只管轮询 Status 看状态。
            crate::content_index::cancel_build();
            IpcResponse::Pong
        }
        (IpcRequest::ContentSnippets { query, entry_indices }, Some(eng)) => {
            // 按需补摘要（选中行场景一次 1 个）：截断防刷，无摘要行缺席。
            // query 取原文，service 侧自行提取 content: 取值（与 Search 一致）。
            const SNIPPET_FETCH_CAP: usize = 64;
            let idxs: Vec<u32> = entry_indices.into_iter().take(SNIPPET_FETCH_CAP).collect();
            match QueryParser::parse(&query) {
                Ok(pq) => {
                    let needles = content_needles(&pq);
                    if needles.is_empty() {
                        IpcResponse::ContentSnippets { snippets: vec![] }
                    } else {
                        let map = crate::content_index::snippets_for(
                            &eng,
                            &needles.join(" "),
                            &idxs,
                            SNIPPET_FETCH_CAP,
                        );
                        IpcResponse::ContentSnippets {
                            snippets: map.into_iter().collect(),
                        }
                    }
                }
                Err(e) => IpcResponse::Error {
                    message: e.to_string(),
                },
            }
        }
        (IpcRequest::ContentSnippets { .. }, None) => IpcResponse::Error {
            message: "索引加载中，请稍候…".into(),
        },
    }
}

/// 各分支的 `content:`/`utf8content:` 取值（主分支 + 顶层 `|` OR 分支）。
/// snippet 分词与拼音打标只看这些，不看整串 query（操作符名本身不是关键词）。
fn content_needles(pq: &findx2_core::ParsedQuery) -> Vec<&str> {
    let mut out = Vec::new();
    for q in std::iter::once(pq).chain(pq.or_branches.iter()) {
        if let Some(s) = q.content_substring.as_ref() {
            out.push(s.as_str());
        }
        if let Some(s) = q.utf8content_substring.as_ref() {
            out.push(s.as_str());
        }
    }
    out
}

pub(crate) fn search_ipc(
    engine: &SearchEngine,
    query: &str,
    pinyin: bool,
    limit_override: usize,
    offset: usize,
    pinyin_initials: bool,
    pinyin_full: bool,
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
                content: crate::content_index::content_opt_with(pinyin_initials, pinyin_full),
                ..Default::default()
            },
            offset,
            lim,
        )
        .map_err(|e| e.to_string())?;
    let search_us = t_search.elapsed().as_micros() as u64;
    let t_format = std::time::Instant::now();
    let mut dtos: Vec<findx2_ipc::SearchHitDto> = hits
        .iter()
        .map(|h| {
            findx2_ipc::SearchHitDto {
                entry_idx: h.entry_idx,
                name: h.name.clone(),
                path: h.path.clone(),
                size: h.size,
                mtime: h.mtime,
                is_directory: h.is_directory,
                name_highlight: h.name_highlight.clone(),
                snippet: String::new(),
                pinyin_hit: false,
            }
        })
        .collect();
    // 含 content: 谓词时给当页前 N 个填 snippet（有界成本，超出的行后面懒取）。
    // 拼音辅库命中同时打标（GUI 显示“拼”徽标；主库直命中不标）。
    // 注意：只取各分支的 content: 取值做分词/打标，不拿整串 query
    // （否则操作符名 `content` 本身会被当关键词高亮，见 content:machuntian 误标）。
    let needles = content_needles(&pq);
    if !needles.is_empty() {
        for needle in &needles {
            if needle.is_ascii() && (pinyin_initials || pinyin_full) {
                let aux_set = crate::content_index::aux_pinyin_keys(
                    needle,
                    pinyin_initials,
                    pinyin_full,
                    findx2_core::CONTENT_FAST_CAP,
                );
                if !aux_set.is_empty() {
                    // entry_idx → (volume_key, frn)，与 snippets_for 同一映射。
                    let g = engine.index_store();
                    for (h, dto) in hits.iter().zip(dtos.iter_mut()) {
                        let frn = match g.frns.get(h.entry_idx as usize).copied() {
                            Some(f) => f,
                            None => continue,
                        };
                        let mut key: Option<(String, u64)> = None;
                        for (slot, vol) in g.volumes.iter().enumerate() {
                            if g.frn_to_entry.get_scoped(frn, slot as u32) == Some(h.entry_idx) {
                                key = Some((
                                    findx2_core::volume_key_for(&vol.volume_id, slot),
                                    frn,
                                ));
                                break;
                            }
                        }
                        if key.is_some_and(|k| aux_set.contains(&k)) {
                            dto.pinyin_hit = true;
                        }
                    }
                }
            }
        }
        let needle_text = needles.join(" ");
        let idxs: Vec<u32> = hits.iter().map(|h| h.entry_idx).collect();
        let map = crate::content_index::snippets_for(
            &engine,
            &needle_text,
            &idxs,
            crate::content_index::SNIPPET_PER_PAGE,
        );
        if !map.is_empty() {
            for (h, dto) in hits.iter().zip(dtos.iter_mut()) {
                if let Some(s) = map.get(&h.entry_idx) {
                    dto.snippet = s.clone();
                }
            }
        }
    }
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
    #[test] fn content_needles_ignores_operator_name_and_covers_or_branches() {
        let pq = findx2_core::QueryParser::parse("machuntian | content:qw").unwrap();
        assert_eq!(super::content_needles(&pq), vec!["qw"]);
        let pq2 = findx2_core::QueryParser::parse("content:machuntian").unwrap();
        assert_eq!(super::content_needles(&pq2), vec!["machuntian"]);
        let pq3 = findx2_core::QueryParser::parse("报告").unwrap();
        assert!(super::content_needles(&pq3).is_empty());
    }

    #[test] fn directory_kind_is_carried_by_search_snapshot() {
        let raw=|id,parent,name:&str,dir|findx2_core::RawEntry { file_id:id,file_id_128:None,parent_id:parent,name:name.into(),size:if dir {0}else{123},mtime:0,ctime:0,attrs:if dir{16}else{0},is_dir:dir };
        let store=findx2_core::IndexBuilder::new(b'F',1,7,0).build_from_raw(vec![raw(10,2,"public-file.txt",false)],vec![raw(1,0,"root",true),raw(2,1,"public-folder",true)],true).unwrap();
        let engine=findx2_core::SearchEngine::new(store);
        let (hits,_,_)=super::search_ipc(&engine,"public",false,50,0,true,true).unwrap();
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
