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
                let summary = "索引 · 加载中".to_string();
                let detail = if !stage.is_empty() {
                    Some(format!("索引: 加载中 · {stage}"))
                } else if let Some(secs) = loading_elapsed_secs {
                    Some(format!("索引: 加载中（已 {secs}s）· 完成后自动可用"))
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
                        summary: format!("{base} · 实时监控已降级"),
                        detail: Some(err.to_string()),
                    };
                }
                return IndexStatusView {
                    summary: format!("{base} · 有警告"),
                    detail: Some(err.to_string()),
                };
            }
            if let Some(err) = backfill_error
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                return IndexStatusView {
                    summary: format!("{base} · 有警告"),
                    detail: Some(err.to_string()),
                };
            }
            if !metadata_ready {
                if !enable_metadata_backfill {
                    return IndexStatusView {
                        summary: format!("{base} · 元数据回填已关闭"),
                        detail: Some("元数据回填已关闭，可在设置中开启。".into()),
                    };
                }
                if *backfill_total > 0 {
                    let pct = ((*backfill_done as f64 / *backfill_total as f64) * 100.0)
                        .round()
                        .clamp(0.0, 100.0) as u32;
                    return IndexStatusView {
                        summary: format!("{base} · 元数据回填 {pct}%"),
                        detail: Some(format!(
                            "已回填 {} / {}。",
                            format_zh_int(*backfill_done),
                            format_zh_int(*backfill_total)
                        )),
                    };
                }
                return IndexStatusView {
                    summary: format!("{base} · 元数据回填中"),
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

pub fn disconnected_status(message: &str) -> IndexStatusView {
    IndexStatusView {
        summary: "未连接隔离后端".into(),
        detail: Some(message.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use findx2_ipc::IpcResponse;

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
        };
        let v = summarize_status(&resp, true);
        assert_eq!(v.summary, "索引 · 加载中");
        assert_eq!(
            v.detail.as_deref(),
            Some("索引: 加载中 · 解析条目（3/8 阶段）")
        );
    }
}
