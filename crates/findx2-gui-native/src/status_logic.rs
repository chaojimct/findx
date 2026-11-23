//! 底栏短句，语义对齐 `gui/src/findx/indexStatus.ts`（本批只用 IPC Status 字段）。

use findx2_ipc::IpcResponse;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexStatusView {
    pub summary: String,
    pub detail: Option<String>,
}

pub fn count_label(n: u64) -> String {
    format!("索引 {} 项", format_zh_int(n))
}

/// Diagnostics already titles the row. Do not prefix another 索引 when the
/// summary itself starts with that word.
pub fn diagnostic_index_line(summary: &str) -> String {
    if summary.starts_with("索引") {
        summary.to_string()
    } else {
        format!("索引：{summary}")
    }
}

fn format_zh_int(n: u64) -> String {
    // 与 toLocaleString("zh-CN") 一样用逗号分组即可。
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out.chars().rev().collect()
}

pub fn is_watch_degraded(error: &str) -> bool {
    error.contains("CAP_SYS_ADMIN") && error.contains("inotify")
}

pub fn summarize_status(resp: &IpcResponse, enable_metadata_backfill: bool) -> IndexStatusView {
    match resp {
        IpcResponse::StatusResult {
            entry_count,
            metadata_ready,
            backfill_done,
            backfill_total,
            loading,
            loading_stage,
            loading_elapsed_secs,
            watch_error,
            backfill_error,
            ..
        } => {
            if *loading {
                let stage = loading_stage.as_deref().unwrap_or("").trim();
                let summary = "索引加载中".to_string();
                let detail = if !stage.is_empty() {
                    Some(format!("索引加载中：{stage}"))
                } else if let Some(secs) = loading_elapsed_secs {
                    Some(format!("索引加载中（已 {secs}s），完成后自动可用"))
                } else {
                    None
                };
                return IndexStatusView { summary, detail };
            }
            let base = count_label(*entry_count);
            if let Some(err) = watch_error
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                if is_watch_degraded(err) {
                    return IndexStatusView {
                        summary: format!("{base}，实时监控已降级"),
                        detail: Some(err.to_string()),
                    };
                }
                return IndexStatusView {
                    summary: format!("{base}，有警告"),
                    detail: Some(err.to_string()),
                };
            }
            if let Some(err) = backfill_error
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                return IndexStatusView {
                    summary: format!("{base}，有警告"),
                    detail: Some(err.to_string()),
                };
            }
            if !metadata_ready {
                if !enable_metadata_backfill {
                    return IndexStatusView {
                        summary: format!("{base}，元数据回填已关闭"),
                        detail: Some("元数据回填已关闭，可在设置中开启。".into()),
                    };
                }
                if *backfill_total > 0 {
                    let pct = ((*backfill_done as f64 / *backfill_total as f64) * 100.0)
                        .round()
                        .clamp(0.0, 100.0) as u32;
                    return IndexStatusView {
                        summary: format!("{base}，元数据回填 {pct}%"),
                        detail: Some(format!(
                            "已回填 {} / {}。",
                            format_zh_int(*backfill_done),
                            format_zh_int(*backfill_total)
                        )),
                    };
                }
                return IndexStatusView {
                    summary: format!("{base}，元数据回填中"),
                    detail: None,
                };
            }
            IndexStatusView {
                summary: base,
                detail: None,
            }
        }
        IpcResponse::Error { message } => IndexStatusView {
            summary: "索引不可用".into(),
            detail: Some(message.clone()),
        },
        IpcResponse::Pong => IndexStatusView {
            summary: "服务在线".into(),
            detail: None,
        },
        _ => IndexStatusView {
            summary: "索引 …".into(),
            detail: None,
        },
    }
}


/// try_index_store lock contention returns zeros with loading=true and no stage.
/// That is not a real reload and must not flip the status bar or Usage layout.
/// 已索引位置表示：按位掩码 + 旧版 `volume_letter` 合并（bit 0 = A:）。
/// 老 service 未上报时返回空数组，由调用方决定如何降级。
pub fn drive_letters(mask: u32, legacy_letter: Option<char>) -> Vec<char> {
    let mut out: Vec<char> = (0..26u32)
        .filter(|bit| mask & (1 << bit) != 0)
        .map(|bit| (b'A' + bit as u8) as char)
        .collect();
    if let Some(c) = legacy_letter {
        let up = c.to_ascii_uppercase();
        if up.is_ascii_uppercase() && !out.contains(&up) {
            out.push(up);
        }
    }
    out
}

/// 从 service 返回的状态 JSON 里提取已索引位置；解析失败返回空。
pub fn indexed_letters_from_status_json(raw: &str) -> Vec<char> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    let mask = v.get("indexed_drive_mask").and_then(|m| m.as_u64()).unwrap_or(0) as u32;
    let legacy = v
        .get("volume_letter")
        .and_then(|l| l.as_str())
        .and_then(|s| s.chars().next());
    drive_letters(mask, legacy)
}

/// 「C: · D:」形式的位置摘要；没有任何位置时返回 `None`。
pub fn indexed_locations_label(raw: &str) -> Option<String> {
    let letters = indexed_letters_from_status_json(raw);
    if letters.is_empty() {
        return None;
    }
    Some(
        letters
            .iter()
            .map(|c| format!("{c}:"))
            .collect::<Vec<_>>()
            .join(" · "),
    )
}

pub fn is_non_authoritative_status(resp: &IpcResponse) -> bool {
    match resp {
        IpcResponse::StatusResult {
            entry_count,
            last_usn,
            journal_id,
            volume_letter,
            loading,
            loading_stage,
            tombstone_count,
            ..
        } => {
            *loading
                && *entry_count == 0
                && *last_usn == 0
                && *journal_id == 0
                && volume_letter.is_none()
                && tombstone_count.is_none()
                && loading_stage
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or("")
                    .is_empty()
        }
        _ => false,
    }
}

pub fn apply_status(
    previous: Option<&IndexStatusView>,
    resp: &IpcResponse,
    enable_metadata_backfill: bool,
) -> IndexStatusView {
    if is_non_authoritative_status(resp) {
        if let Some(prev) = previous {
            if prev.summary.starts_with("索引") && !prev.summary.contains("加载中") {
                return prev.clone();
            }
        }
    }
    summarize_status(resp, enable_metadata_backfill)
}

/// Pipe missing, socket missing, or connection refused: the official service is
/// not listening. Other failures stay a generic disconnect.
pub fn is_service_not_listening(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    message.contains("服务未运行")
        || message.contains("管道不存在")
        || message.contains("服务未监听")
        || message.contains("端点不存在")
        || lower.contains("connection refused")
        || lower.contains("no such file")
        || lower.contains("os error 2")
}

pub fn disconnected_status(message: &str) -> IndexStatusView {
    if is_service_not_listening(message) {
        IndexStatusView {
            summary: "服务未运行".into(),
            detail: Some("启动后台索引服务后即可开始搜索。".into()),
        }
    } else {
        IndexStatusView {
            summary: "未连接".into(),
            detail: Some("暂时无法连接索引服务。".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use findx2_ipc::IpcResponse;

    #[test]
    fn drive_letters_merges_mask_and_legacy_letter_without_dups() {
        assert_eq!(drive_letters(0b11, None), vec!['A', 'B']);
        // A: bit plus legacy volume_letter must not duplicate.
        assert_eq!(drive_letters(1, Some('A')), vec!['A']);
        assert_eq!(drive_letters(0, Some('e')), vec!['E']);
        assert_eq!(drive_letters(0, Some('-')), Vec::<char>::new());
        // 26 bits is the Windows drive space; extra bits are ignored.
        assert_eq!(drive_letters(1 << 27, None), Vec::<char>::new());
    }

    #[test]
    fn indexed_letters_from_status_json_degrades_on_old_services() {
        assert!(indexed_letters_from_status_json("not json").is_empty());
        assert!(indexed_letters_from_status_json("{}").is_empty());
        // 33 = bits 0 (A) and 5 (F); legacy 'C' adds a third, dedup-free.
        let new = r#"{"type":"status_result","indexed_drive_mask":33,"volume_letter":"C"}"#;
        assert_eq!(indexed_letters_from_status_json(new), vec!['A', 'F', 'C']);
        assert_eq!(
            indexed_locations_label(new).as_deref(),
            Some("A: · F: · C:")
        );
        assert!(indexed_locations_label("{}").is_none());
    }

    #[test]
    fn stopped_service_is_distinct_from_a_generic_disconnect() {
        assert!(is_service_not_listening("无法连接索引服务：服务未运行。"));
        assert!(is_service_not_listening(
            "无法连接 `\\\\.\\pipe\\findx2`：管道不存在或服务未监听。"
        ));
        assert!(!is_service_not_listening("无法连接索引服务：拒绝访问。"));
        let down = disconnected_status("无法连接索引服务：服务未运行。");
        assert_eq!(down.summary, "服务未运行");
        assert!(!down.summary.contains('·'));
        assert_eq!(disconnected_status("超时").summary, "未连接");
    }

    #[test]
    fn ready_index_uses_count_label() {
        let resp = IpcResponse::StatusResult {
            entry_count: 1_234_567,
            dir_count: 0,
            last_usn: 0,
            journal_id: 0,
            volume_letter: None,
            healthy: true,
            metadata_ready: true,
            backfill_done: 0,
            backfill_total: 0,
            loading: false,
            loading_stage: None,
            loading_elapsed_secs: None,
            loading_phase_done: None,
            loading_phase_total: None,
            watch_error: None,
            backfill_error: None,
            tombstone_count: None,
            indexed_drive_mask: 0,
            wal_bytes: 0,
            last_fold_secs_ago: None,
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
        };
        let v = summarize_status(&resp, true);
        assert_eq!(v.summary, "索引 1,234,567 项");
        assert!(v.detail.is_none());
    }

    #[test]
    fn loading_prefers_stage() {
        let resp = IpcResponse::StatusResult {
            entry_count: 0,
            dir_count: 0,
            last_usn: 0,
            journal_id: 0,
            volume_letter: None,
            healthy: true,
            metadata_ready: false,
            backfill_done: 0,
            backfill_total: 0,
            loading: true,
            loading_stage: Some("解析条目（3/8 阶段）".into()),
            loading_elapsed_secs: Some(42),
            loading_phase_done: Some(3),
            loading_phase_total: Some(8),
            watch_error: None,
            backfill_error: None,
            tombstone_count: None,
            indexed_drive_mask: 0,
            wal_bytes: 0,
            last_fold_secs_ago: None,
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
        };
        let v = summarize_status(&resp, true);
        assert_eq!(v.summary, "索引加载中");
        assert_eq!(
            v.detail.as_deref(),
            Some("索引加载中：解析条目（3/8 阶段）")
        );
        assert_eq!(diagnostic_index_line(&v.summary), "索引加载中");
        assert!(!diagnostic_index_line("索引 4,270,379 项，有警告").contains('·'));
        assert_eq!(
            diagnostic_index_line("索引 4,270,379 项，有警告"),
            "索引 4,270,379 项，有警告"
        );
    }

    #[test]
    fn lock_contention_zero_loading_does_not_replace_ready_status() {
        let ready = IpcResponse::StatusResult {
            entry_count: 1_234_567,
            dir_count: 1,
            last_usn: 9,
            journal_id: 1,
            volume_letter: Some('C'),
            healthy: true,
            metadata_ready: false,
            backfill_done: 10,
            backfill_total: 100,
            loading: false,
            loading_stage: None,
            loading_elapsed_secs: None,
            loading_phase_done: None,
            loading_phase_total: None,
            watch_error: None,
            backfill_error: None,
            tombstone_count: Some(1),
            indexed_drive_mask: 0,
            wal_bytes: 0,
            last_fold_secs_ago: None,
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
        };
        let prev = summarize_status(&ready, true);
        let contention = IpcResponse::StatusResult {
            entry_count: 0,
            dir_count: 0,
            last_usn: 0,
            journal_id: 0,
            volume_letter: None,
            healthy: true,
            metadata_ready: false,
            backfill_done: 10,
            backfill_total: 100,
            loading: true,
            loading_stage: None,
            loading_elapsed_secs: None,
            loading_phase_done: None,
            loading_phase_total: None,
            watch_error: None,
            backfill_error: None,
            tombstone_count: None,
            indexed_drive_mask: 0,
            wal_bytes: 0,
            last_fold_secs_ago: None,
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
        };
        let kept = apply_status(Some(&prev), &contention, true);
        assert_eq!(kept, prev);
        assert!(!kept.summary.contains("加载中"));
        let first = apply_status(None, &contention, true);
        assert_eq!(first.summary, "索引加载中");
    }

}
