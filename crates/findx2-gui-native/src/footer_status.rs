//! Footer presentation only. Counts are global index entries, not a disk-root estimate.
use findx2_ipc::IpcResponse;

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub live_entries: Option<u64>,
    pub healthy: bool,
    pub metadata_ready: bool,
    pub loading: bool,
    pub watching: bool,
    pub watch_error: Option<String>,
    pub metadata_error: Option<String>,
}
#[derive(Clone, Debug, Default)]
pub struct EngineFooter {
    pub source: Option<String>,
    pub connected: bool,
    pub last: Option<Snapshot>,
    pub error: Option<String>,
    pub busy: bool,
}
pub fn grouped(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',')
        }
        out.push(c)
    }
    out
}
impl EngineFooter {
    pub fn set_source(&mut self, endpoint: &str) {
        if self.source.as_deref() != Some(endpoint) {
            *self = Self::default();
            self.source = Some(endpoint.into());
        }
    }
    pub fn observe(&mut self, result: &Result<IpcResponse, String>) {
        match result {
            Err(e) => {
                self.connected = false;
                self.error = Some(e.clone());
                self.busy = false;
            }
            Ok(resp) if crate::status_logic::is_non_authoritative_status(resp) => self.busy = true,
            Ok(IpcResponse::StatusResult {
                entry_count,
                tombstone_count,
                healthy,
                metadata_ready,
                loading,
                last_usn,
                journal_id,
                watch_error,
                backfill_error,
                ..
            }) => {
                self.connected = true;
                self.error = None;
                self.busy = false;
                self.last = Some(Snapshot {
                    // entries includes both file and searchable directory rows; dirs is a
                    // separate topology arena. Only the tombstone subtraction is valid.
                    live_entries: tombstone_count.and_then(|n| entry_count.checked_sub(n)),
                    healthy: *healthy,
                    metadata_ready: *metadata_ready,
                    loading: *loading,
                    watching: *last_usn > 0 && *journal_id > 0,
                    watch_error: watch_error.clone(),
                    metadata_error: backfill_error.clone(),
                });
            }
            Ok(IpcResponse::Error { message }) => {
                self.connected = true;
                self.error = Some(message.clone());
                self.busy = false;
            }
            Ok(_) => self.busy = true,
        }
    }
    pub fn warning(&self) -> bool {
        !self.connected
            || self.error.is_some()
            || self.busy
            || self
                .last
                .as_ref()
                .is_none_or(|s| s.loading || !s.healthy || s.watch_error.is_some())
    }
    pub fn needs_details(&self) -> bool {
        self.last
            .as_ref()
            .is_some_and(|s| !s.metadata_ready || s.metadata_error.is_some())
    }
    pub fn label(&self) -> String {
        if !self.connected {
            return if self.error.is_none() && self.last.is_none() {
                "连接中…"
            } else {
                "连接已断开"
            }
            .into();
        }
        if self.error.is_some() {
            return "暂不可用".into();
        }
        let Some(s) = &self.last else {
            return "状态待确认".into();
        };
        if s.loading {
            return "正在加载…".into();
        }
        if s.watch_error.is_some() || !s.healthy {
            return "更新异常".into();
        }
        if self.busy {
            return "刷新稍候".into();
        }
        if s.watching {
            "实时更新中"
        } else {
            "已连接"
        }
        .into()
    }
    pub fn counts_label(&self) -> String {
        let old = !self.connected
            || self.error.is_some()
            || self.busy
            || self.last.as_ref().is_some_and(|s| s.loading);
        match self.last.as_ref().and_then(|s| s.live_entries) {
            Some(n) => format!(
                "{}已索引 {} 项",
                if old { "上次：" } else { "" },
                grouped(n)
            ),
            None => "索引统计待获取".into(),
        }
    }
    pub fn counts_tooltip(&self) -> String {
        let freshness = if !self.connected || self.error.is_some() {
            "连接断开，显示上次统计。"
        } else if self.busy {
            "本次刷新暂忙，保留上次统计。"
        } else {
            "约每 2 秒刷新，统计全部已索引位置，不代表已扫描所有磁盘。"
        };
        // Older engines do not report live file/directory counts. Do not relabel
        // a root aggregate or the raw topology length as global file/dir counts.
        format!(
            "{freshness}总项数包含文件和目录，已扣除删除项；当前服务尚不能准确拆分文件数和目录数。"
        )
    }
    pub fn tooltip(&self) -> String {
        if !self.connected {
            return "连接尚未建立或已断开，列表和统计暂不更新；重连后会继续刷新。".into();
        }
        if self.error.is_some() {
            return "服务暂时无法响应，保留上次统计；请稍后重试。".into();
        }
        let Some(s) = &self.last else {
            return "正在获取状态，请稍候。".into();
        };
        if s.loading {
            return "正在加载文件列表，完成后即可搜索。".into();
        }
        if let Some(e) = &s.watch_error {
            let reason = if e.contains("拒绝访问")
                || e.contains("Access is denied")
                || e.contains("os error 5")
            {
                "读取权限不足"
            } else if e.contains("journal") || e.contains("日志") {
                "文件变更记录读取失败"
            } else {
                "文件变更监听失败"
            };
            return format!("{reason}，列表可能不再及时更新。请检查服务状态后重试。");
        }
        if !s.healthy {
            return "文件变更监听异常，列表可能不再及时更新；请检查服务状态后重试。".into();
        }
        if !s.metadata_ready {
            let detail = match s.metadata_error.as_deref() {
                Some(e) if e.contains("遍历完成但仍有") => {
                    let n = e
                        .split("遍历完成但仍有")
                        .nth(1)
                        .and_then(|s| s.split_whitespace().next())
                        .and_then(|s| s.parse::<u64>().ok());
                    n.map(|n| {
                        format!(
                            "最近一次检查有 {} 项大小或时间未读全，名称搜索可用，相关筛选可能不全",
                            grouped(n)
                        )
                    })
                    .unwrap_or_else(|| {
                        "部分文件大小或时间未读全，名称搜索可用，相关筛选可能不全".into()
                    })
                }
                Some(e) if e.contains("写入") || e.contains("persist") => {
                    "文件信息保存未完成".into()
                }
                Some(e) if e.contains("关闭") => "大小和时间补全已关闭，相关筛选可能不全".into(),
                Some(_) => "文件大小和时间读取遇到问题，名称搜索可用，相关筛选可能不全".into(),
                None => "正在补全文件大小和时间，名称搜索可用，相关筛选暂可能不全".into(),
            };
            return format!(
                "{detail}。更改尚未定期保存，请正常退出；异常退出可能需要恢复最近更新。"
            );
        }
        if self.busy {
            return "刷新暂忙，保留上次有效状态和统计；稍后自动重试。".into();
        }
        "已连接，文件列表随磁盘变化自动更新。".into()
    }
}
/// Original details stay available in the Usage hover, not in the single-line footer.
pub fn compact_usage(detail: &str, unknown: Option<u64>, scanning: bool, stale: bool) -> String {
    if detail.contains("超时") {
        return "读取超时 · 可重试".into();
    }
    let has_result=detail.contains("索引逻辑 ·");
    if !has_result && (detail.contains("失败") || detail.contains("错误") || detail.contains("异常") || detail.contains("不支持") || detail.contains("不可用")) {
        return "读取异常 · 查看详情".into();
    }
    if detail.contains("已取消") || detail.starts_with("取消") {
        return "已取消 · 保留已读结果".into();
    }
    if scanning || detail.contains("index loading") || detail.starts_with("正在读取") {
        return "正在读取…".into();
    }
    let mut s = "索引大小".to_string();
    if let Some(n) = unknown {
        if n > 0 {
            s.push_str(&format!(" · {} 项信息不全", grouped(n)))
        }
    }
    if stale {
        s.push_str(" · 详情有提示")
    }
    s
}
#[cfg(test)]
mod tests {
    use super::*;
    fn status(entries: u64, deleted: u64) -> IpcResponse {
        serde_json::from_value(serde_json::json!({"type":"status_result","entry_count":entries,"dir_count":999999,"tombstone_count":deleted,"healthy":true,"metadata_ready":false,"last_usn":7,"journal_id":9,"backfill_error":"元数据回填遍历完成但仍有 80 条未验证（失败/并发变更）；metadata_ready 保持 false，重启/重建后可重试"})).unwrap()
    }
    #[test]
    fn global_entries_are_not_files_or_topology_nodes() {
        let mut f = EngineFooter::default();
        f.observe(&Ok(status(500, 80)));
        assert_eq!(f.counts_label(), "已索引 420 项");
        assert!(!f.counts_label().contains("C:"));
        assert!(f.counts_tooltip().contains("文件和目录"));
    }
    #[test]
    fn natural_change_fixture_create_delete_rename_is_visible() {
        let mut f = EngineFooter::default();
        for (entries, deleted, total) in [(5, 1, 4), (6, 1, 5), (6, 2, 4), (6, 2, 4)] {
            f.observe(&Ok(status(entries, deleted)));
            assert!(f.counts_label().contains(&format!("{total} 项")));
        }
    }
    #[test]
    fn partial_completion_is_not_perpetual_work() {
        let mut f = EngineFooter::default();
        f.observe(&Ok(status(500, 80)));
        assert_eq!(f.label(), "实时更新中");
        assert!(!f.warning());
        assert!(f.needs_details());
        let t = f.tooltip();
        assert!(t.contains("最近一次检查有 80 项"));
        assert!(t.contains("尚未定期保存"));
        assert!(!t.contains("元数据"));
        assert!(!t.contains("USN"));
        assert!(!t.contains("metadata_ready"));
        assert!(!t.contains("正在补全"));
    }
    #[test]
    fn actual_work_loading_and_watch_failure_are_distinct() {
        let mut f = EngineFooter::default();
        let mut s = status(500, 80);
        if let IpcResponse::StatusResult { backfill_error, .. } = &mut s {
            *backfill_error = None
        };
        f.observe(&Ok(s.clone()));
        assert!(f.tooltip().contains("正在补全"));
        if let IpcResponse::StatusResult { watch_error, .. } = &mut s {
            *watch_error = Some("Access is denied (os error 5)".into())
        };
        f.observe(&Ok(s.clone()));
        assert_eq!(f.label(), "更新异常");
        assert!(f.tooltip().contains("权限不足"));
        if let IpcResponse::StatusResult { loading, .. } = &mut s {
            *loading = true
        };
        f.observe(&Ok(s));
        assert_eq!(f.label(), "正在加载…");
    }
    #[test]
    fn zero_loading_preserves_counts_without_faking_zero_or_offline() {
        let mut f = EngineFooter::default();
        f.observe(&Ok(status(500, 80)));
        f.observe(&Ok(serde_json::from_str(
            r#"{"type":"status_result","entry_count":0,"loading":true}"#,
        )
        .unwrap()));
        assert_eq!(f.counts_label(), "上次：已索引 420 项");
        assert_eq!(f.label(), "刷新稍候");
        assert!(f.connected);
    }
    #[test]
    fn true_disconnect_stays_disconnected_after_sentinel() {
        let mut f = EngineFooter::default();
        f.observe(&Ok(status(500, 80)));
        f.observe(&Err("pipe closed".into()));
        f.observe(&Ok(serde_json::from_str(
            r#"{"type":"status_result","entry_count":0,"loading":true}"#,
        )
        .unwrap()));
        assert_eq!(f.label(), "连接已断开");
        assert!(f.counts_label().starts_with("上次"));
    }
    #[test]
    fn endpoint_change_drops_old_stats() {
        let mut f = EngineFooter::default();
        f.set_source("a");
        f.observe(&Ok(status(500, 80)));
        f.set_source("b");
        assert!(f.last.is_none());
        assert_eq!(f.label(), "连接中…");
    }
    #[test]
    fn unavailable_or_invalid_tombstones_never_invent_file_totals() {
        let mut f = EngineFooter::default();
        let mut s = status(500, 80);
        if let IpcResponse::StatusResult {
            tombstone_count, ..
        } = &mut s
        {
            *tombstone_count = None
        };
        f.observe(&Ok(s));
        assert_eq!(f.counts_label(), "索引统计待获取");
        f.observe(&Ok(status(5, 10)));
        assert_eq!(f.counts_label(), "索引统计待获取");
    }
    #[test]
    fn no_journal_does_not_claim_live_watching() {
        let mut f = EngineFooter::default();
        let mut s = status(5, 1);
        if let IpcResponse::StatusResult { journal_id, .. } = &mut s {
            *journal_id = 0
        };
        f.observe(&Ok(s));
        assert_eq!(f.label(), "已连接");
    }
    #[test]
    fn compact_usage_keeps_unknown_and_errors() {
        assert_eq!(
            compact_usage("g123 canonical indexed paths", Some(80), false, true),
            "索引大小 · 80 项信息不全 · 详情有提示"
        );
        assert!(compact_usage("索引请求失败", None, false, true).contains("异常"));
    }
    #[test]fn cancellable_pending_is_not_reported_as_already_cancelled(){assert_eq!(compact_usage("正在读取索引目录（最长 45 秒，可取消）",None,false,false),"正在读取…");}

    #[test]fn no_reported_watch_error_is_not_a_read_failure(){assert_eq!(compact_usage("索引逻辑 · 当前层完整 · 监控未报告错误（不代表全量新鲜）",Some(0),false,false),"索引大小");}

}
