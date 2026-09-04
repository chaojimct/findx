//! JSON IPC 请求处理（命名管道与 Unix socket 共用）。

use std::sync::{Arc, RwLock};

use findx2_core::{QueryParser, SearchEngine, SearchOptions};
use findx2_ipc::{IpcRequest, IpcResponse};

pub(crate) type EngineSlot = Arc<RwLock<Option<Arc<SearchEngine>>>>;

pub(crate) fn req_kind_label(r: &IpcRequest) -> &'static str {
    match r {
        IpcRequest::Ping => "Ping",
        IpcRequest::Status => "Status",
        IpcRequest::Search { .. } => "Search",
    }
}

pub(crate) fn process_request(slot: &EngineSlot, req: IpcRequest) -> IpcResponse {
    let engine = slot.read().ok().and_then(|g| g.clone());
    match (req, engine) {
        (IpcRequest::Ping, _) => IpcResponse::Pong,
        (IpcRequest::Status, None) => IpcResponse::StatusResult {
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
            watch_error: crate::watch_health::watch_error_summary(),
            backfill_error: None,
        },
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
                    IpcResponse::StatusResult {
                        entry_count: g.entry_count() as u64,
                        dir_count: g.dirs.len() as u64,
                        last_usn: vol.map(|v| v.last_usn).unwrap_or(0),
                        journal_id: vol.map(|v| v.usn_journal_id).unwrap_or(0),
                        volume_letter: vol.filter(|v| v.volume_letter != 0).map(|v| v.volume_letter as char),
                        healthy: true,
                        metadata_ready: g.metadata_ready,
                        backfill_done,
                        backfill_total,
                        loading: false,
                        watch_error: crate::watch_health::watch_error_summary(),
                        backfill_error: if g.metadata_ready {
                            None
                        } else {
                            backfill_error
                        },
                    }
                }
                None => IpcResponse::StatusResult {
                    entry_count: 0,
                    dir_count: 0,
                    last_usn: 0,
                    journal_id: 0,
                    volume_letter: None,
                    healthy: true,
                    metadata_ready: false,
                    backfill_done: backfill.0,
                    backfill_total: backfill.1,
                    loading: true,
                    watch_error: crate::watch_health::watch_error_summary(),
                    backfill_error,
                },
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
                ..Default::default()
            },
            offset,
            lim,
        )
        .map_err(|e| e.to_string())?;
    let search_us = t_search.elapsed().as_micros() as u64;
    let t_format = std::time::Instant::now();
    let store = engine.index_store();
    let dtos: Vec<findx2_ipc::SearchHitDto> = hits
        .into_iter()
        .map(|h| {
            let is_directory = store
                .entries
                .get(h.entry_idx as usize)
                .map(|e| e.is_dir_entry())
                .unwrap_or(false);
            findx2_ipc::SearchHitDto {
                entry_idx: h.entry_idx,
                name: h.name,
                path: h.path,
                size: h.size,
                mtime: h.mtime,
                is_directory,
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
