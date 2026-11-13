//! Presentation-only engine/footer state. Never owns a treemap or starts a scan.
use findx2_ipc::{IpcResponse, UsagePageDto};

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub entries: u64,
    pub directory_nodes: u64,
    pub volume: Option<char>,
    pub healthy: bool,
    pub metadata_ready: bool,
    pub loading: bool,
    pub watch_error: Option<String>,
    pub metadata_error: Option<String>,
    pub detail: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootCounts {
    pub volume: char,
    pub files: u64,
    pub directories: u64,
    pub unknown: u64,
    pub warning: Option<String>,
}
#[derive(Clone, Debug, Default)]
pub struct EngineFooter {
    pub source: Option<String>,
    pub connected: bool,
    pub last: Option<Snapshot>,
    pub counts: Option<RootCounts>,
    pub error: Option<String>,
    pub count_error: Option<String>,
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
            self.source = Some(endpoint.to_string());
        }
    }
    pub fn observe(&mut self, result: &Result<IpcResponse, String>) {
        match result {
            Err(e) => {
                self.connected = false;
                self.error = Some(e.clone());
                self.busy = false;
            }
            Ok(resp) if crate::status_logic::is_non_authoritative_status(resp) => {
                // A lock-contention sentinel must not erase authoritative warnings/counts.
                // If we were disconnected, a sentinel alone is not proof of healthy online state.
                self.busy = true;
            }
            Ok(IpcResponse::StatusResult {
                entry_count,
                dir_count,
                volume_letter,
                healthy,
                metadata_ready,
                loading,
                watch_error,
                backfill_error,
                last_usn,
                journal_id,
                backfill_done,
                backfill_total,
                loading_stage,
                ..
            }) => {
                if self.last.as_ref().and_then(|s| s.volume) != *volume_letter {
                    self.counts = None;
                    self.count_error = None;
                }
                self.connected = true;
                self.error = None;
                self.busy = false;
                self.last=Some(Snapshot{entries:*entry_count,directory_nodes:*dir_count,
                    volume:*volume_letter,healthy:*healthy,metadata_ready:*metadata_ready,
                    loading:*loading,watch_error:watch_error.clone(),metadata_error:backfill_error.clone(),
                    detail:format!("healthy={healthy}; metadata_ready={metadata_ready}\njournal {journal_id}; USN {last_usn}\nBackfill {backfill_done}/{backfill_total}{}{}{}",
                        loading_stage.as_ref().map(|s|format!("\n{s}")).unwrap_or_default(),
                        watch_error.as_ref().map(|s|format!("\nWatch: {s}")).unwrap_or_default(),
                        backfill_error.as_ref().map(|s|format!("\nMetadata: {s}")).unwrap_or_default())});
            }
            Ok(IpcResponse::Error { message }) => {
                self.connected = true;
                self.error = Some(message.clone());
                self.busy = false;
            }
            Ok(_) => {
                self.busy = true;
            }
        }
    }
    pub fn scope(&self) -> Option<char> {
        if !self.connected || self.error.is_some() {
            return None;
        }
        self.last
            .as_ref()
            .filter(|s| !s.loading)
            .and_then(|s| s.volume)
    }
    pub fn accept_counts(&mut self, volume: char, page: &UsagePageDto) {
        if self.scope() != Some(volume) {
            return;
        }
        self.counts = Some(RootCounts {
            volume,
            files: page.root.files,
            directories: page.root.directories,
            unknown: page.root.unknown,
            warning: page
                .watch_error
                .iter()
                .chain(page.metadata_error.iter())
                .cloned()
                .reduce(|a, b| format!("{a}\n{b}")),
        });
        self.count_error = None;
    }
    pub fn warning(&self) -> bool {
        !self.connected
            || self.error.is_some()
            || self.busy
            || self
                .counts
                .as_ref()
                .is_some_and(|c| c.unknown > 0 || c.warning.is_some())
            || self.last.as_ref().is_none_or(|s| {
                !s.healthy
                    || !s.metadata_ready
                    || s.watch_error.is_some()
                    || s.metadata_error.is_some()
            })
    }
    pub fn label(&self) -> String {
        if !self.connected {
            return if self.error.is_none() && self.last.is_none() {
                "引擎连接中".into()
            } else {
                "引擎离线".into()
            };
        }
        if self.error.is_some() {
            return "引擎不可用".into();
        }
        let Some(s) = &self.last else {
            return "引擎状态待确认".into();
        };
        if s.loading {
            return "引擎加载中".into();
        }
        if s.watch_error.is_some() || !s.healthy {
            return "引擎在线 · 监控有警告".into();
        }
        if !s.metadata_ready
            || s.metadata_error.is_some()
            || self.counts.as_ref().is_some_and(|c| c.unknown > 0)
        {
            return "引擎在线 · 元数据待验证".into();
        }
        if self.counts.as_ref().is_some_and(|c| c.warning.is_some()) {
            return "引擎在线 · 有警告".into();
        }
        if self.busy {
            return "引擎在线 · 刷新暂忙".into();
        }
        "引擎在线".into()
    }
    pub fn counts_label(&self) -> String {
        if let Some(c) = &self.counts {
            let old = !self.connected
                || self.error.is_some()
                || self.count_error.is_some()
                || self.last.as_ref().is_some_and(|s| s.loading);
            return format!(
                "{}{}: 文件 {} · 目录 {}",
                if old { "上次 " } else { "" },
                c.volume,
                grouped(c.files),
                grouped(c.directories)
            );
        }
        self.last
            .as_ref()
            .filter(|s| !s.loading)
            .map(|s| format!("索引 {} 项 · 分项待获取", grouped(s.entries)))
            .unwrap_or_else(|| "文件/目录统计待获取".into())
    }
    pub fn tooltip(&self) -> String {
        let mut out = vec![self.label(), self.counts_label()];
        if let Some(c) = &self.counts {
            out.push(format!("范围 {}:\\：索引逻辑树可见路径统计；硬链接按路径计数，目录含根。不是磁盘实时枚举；不代表其他卷。待验证 {} 项。约每 10 秒异步刷新。",c.volume,grouped(c.unknown)));
        }
        if let Some(c) = &self.counts {
            if let Some(e) = &c.warning {
                out.push(e.clone())
            }
        }
        if let Some(e) = &self.error {
            out.push(e.clone())
        }
        if let Some(e) = &self.count_error {
            out.push(format!("分项统计刷新失败，保留上次结果：{e}"))
        }
        if self.busy {
            out.push("状态刷新暂忙；保留上次有效状态，不把零值当作新统计。".into())
        }
        if let Some(s) = &self.last {
            out.push(format!(
                "存储条目 {}，目录节点 {}（包含内部节点/墓碑口径，不能把条目总数当文件数）。",
                grouped(s.entries),
                grouped(s.directory_nodes)
            ));
            out.push(s.detail.clone());
        }
        out.join("\n")
    }
}
/// Keep the entire original diagnostic in the hover text, not in the single-line footer.
pub fn compact_usage(detail: &str, unknown: Option<u64>, scanning: bool, stale: bool) -> String {
    if detail.contains("超时") {
        return "索引请求超时 · 可重试".into();
    }
    if detail.contains("失败")
        || detail.contains("错误")
        || detail.contains("异常")
        || detail.contains("不支持")
        || detail.contains("不可用")
    {
        return "索引读取异常 · 查看详情".into();
    }
    if detail.contains("取消") {
        return "已取消 · 保留已读结果".into();
    }
    if scanning {
        return "索引读取中…".into();
    }
    let mut s = "索引逻辑".to_string();
    if let Some(n) = unknown {
        if n > 0 {
            s.push_str(&format!(" · 待验证 {}", grouped(n)))
        }
    }
    if stale {
        s.push_str(" · 有警告")
    }
    s
}
#[cfg(test)]
mod tests {
    use super::*;
    fn status() -> IpcResponse {
        serde_json::from_str(r#"{"type":"status_result","entry_count":500,"dir_count":90,"volume_letter":"C","healthy":true,"metadata_ready":false,"backfill_error":"80 条未验证"}"#).unwrap()
    }
    fn page() -> UsagePageDto {
        serde_json::from_str(r#"{"epoch":1,"generation":9,"root":{"id":1,"name":"","directory":true,"logical":1,"files":311,"directories":71,"unknown":80},"children":[],"next":null,"resident_bytes":1,"state":"canonical indexed paths","watch_error":null,"metadata_error":null}"#).unwrap()
    }
    #[test]
    fn counts_are_real_scoped_root_counts_not_entries_or_subtraction() {
        let mut f = EngineFooter::default();
        f.observe(&Ok(status()));
        assert!(f.counts_label().contains("索引 500 项"));
        f.accept_counts('C', &page());
        assert_eq!(f.counts.as_ref().unwrap().files, 311);
        assert!(f.counts_label().contains("文件 311 · 目录 71"));
        assert!(f.tooltip().contains("不代表其他卷"));
    }
    #[test]
    fn metadata_warning_is_visible_and_full_detail_is_retained() {
        let mut f = EngineFooter::default();
        f.observe(&Ok(status()));
        assert!(f.warning());
        assert!(f.label().contains("元数据待验证"));
        assert!(f.tooltip().contains("80 条未验证"));
    }
    #[test]
    fn zero_loading_does_not_erase_last_authoritative_warning_or_counts() {
        let mut f = EngineFooter::default();
        f.observe(&Ok(status()));
        f.accept_counts('C', &page());
        let z = serde_json::from_str(r#"{"type":"status_result","entry_count":0,"loading":true}"#)
            .unwrap();
        f.observe(&Ok(z));
        assert!(f.label().contains("元数据待验证"));
        assert!(f.counts_label().contains("文件 311"));
        assert!(f.busy);
    }
    #[test]
    fn true_disconnect_stays_offline_even_after_a_busy_sentinel() {
        let mut f = EngineFooter::default();
        f.observe(&Ok(status()));
        f.accept_counts('C', &page());
        f.observe(&Err("pipe closed".into()));
        assert_eq!(f.label(), "引擎离线");
        assert!(f.counts_label().starts_with("上次"));
        f.observe(&Ok(serde_json::from_str(
            r#"{"type":"status_result","entry_count":0,"loading":true}"#,
        )
        .unwrap()));
        assert_eq!(f.label(), "引擎离线");
        assert!(f.tooltip().contains("pipe closed"));
    }
    #[test]
    fn root_scope_change_clears_old_counts_and_late_results_are_ignored() {
        let mut f = EngineFooter::default();
        f.observe(&Ok(status()));
        f.accept_counts('C', &page());
        let mut s = status();
        if let IpcResponse::StatusResult { volume_letter, .. } = &mut s {
            *volume_letter = Some('D')
        };
        f.observe(&Ok(s));
        assert!(f.counts.is_none());
        f.accept_counts('C', &page());
        assert!(f.counts.is_none());
    }
    #[test]
    fn counts_error_marks_cached_counts_as_old() {
        let mut f = EngineFooter::default();
        f.observe(&Ok(status()));
        f.accept_counts('C', &page());
        f.count_error = Some("unsupported".into());
        assert!(f.counts_label().starts_with("上次"));
    }
    #[test]
    fn concise_usage_hides_generation_but_not_unknown_or_warning() {
        let d =
            "索引逻辑 · g123 · canonical indexed paths allocation not measured journal metadata";
        assert_eq!(
            compact_usage(d, Some(80), false, true),
            "索引逻辑 · 待验证 80 · 有警告"
        );
        assert!(!compact_usage(d, Some(0), false, false).contains("g123"));
        assert!(compact_usage("索引目录读取失败：offline", None, false, false).contains("异常"));
    }
    #[test]
    fn endpoint_change_clears_cached_status_and_counts() {
        let mut f = EngineFooter::default();
        f.set_source("a");
        f.observe(&Ok(status()));
        f.accept_counts('C', &page());
        f.set_source("b");
        assert!(!f.connected);
        assert!(f.last.is_none());
        assert!(f.counts.is_none());
    }
    #[test]
    fn root_unknown_and_root_error_are_not_hidden_by_healthy_status() {
        let mut f = EngineFooter::default();
        let mut s = status();
        if let IpcResponse::StatusResult {
            metadata_ready,
            backfill_error,
            ..
        } = &mut s
        {
            *metadata_ready = true;
            *backfill_error = None
        };
        f.observe(&Ok(s));
        f.accept_counts('C', &page());
        assert!(f.label().contains("元数据待验证"));
        let mut p = page();
        p.root.unknown = 0;
        p.watch_error = Some("journal failed".into());
        f.accept_counts('C', &p);
        assert!(f.warning());
        assert!(f.label().contains("警告"));
        assert!(f.tooltip().contains("journal failed"));
    }
    #[test]
    fn stale_prefixed_failure_and_timeout_remain_visible() {
        assert!(compact_usage("索引可能滞后 IPC 请求失败", None, false, true).contains("异常"));
        assert!(compact_usage("请求超时", None, false, false).contains("超时"));
        assert!(compact_usage("已取消", None, false, false).contains("取消"));
    }
}
