//! findx2 自有 IPC 协议：命名管道 / Unix socket 上的 JSON 行协议。
//!
//! 单条请求/响应为一行 UTF-8 JSON（便于 `BufRead::read_line`）。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Unix 域套接字路径。`name` 为设置里的管道名（默认 `findx2`），可含 `.sock` 后缀。
pub fn unix_socket_path(name: &str) -> PathBuf {
    let trimmed = name.trim();
    let file = if trimmed.is_empty() {
        "findx2.sock".to_string()
    } else if trimmed.ends_with(".sock") {
        trimmed.to_string()
    } else if trimmed.contains('/') {
        return PathBuf::from(trimmed);
    } else {
        format!("{trimmed}.sock")
    };
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let dir = PathBuf::from(home).join("Library/Application Support/FindX");
        let _ = std::fs::create_dir_all(&dir);
        dir.join(file)
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
            if !runtime.is_empty() {
                return PathBuf::from(runtime).join(file);
            }
        }
        std::env::temp_dir().join(file)
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(file)
    }
}

/// Protocol 2 terminal handoff identity. Equality is exact; no field has a default.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StopToken {
    pub protocol_version: u32,
    pub engine_instance: String,
    pub service_pid: u32,
    pub request_nonce: String,
    pub index_identity: String,
    pub mutation_sequence: u64,
    pub published_generation: u64,
    pub outcome_epoch: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DurableStopReceipt {
    pub token: StopToken,
    pub state: String,
    pub process_stopped: bool,
}

/// 客户端 → 服务端的请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcRequest {
    Search {
        query: String,
        /// 缺省 true：与 GUI「默认拼音匹配」一致。旧客户端漏传时不应静默关掉拼音。
        #[serde(default = "default_pinyin")]
        pinyin: bool,
        #[serde(default = "default_limit")]
        limit: usize,
        /// 分页偏移（排序后切片 `[offset, offset+limit)`）。缺省 0，老客户端照常工作。
        #[serde(default)]
        offset: usize,
    },
    /// Negotiation only. Missing/unknown versions must never imply prepare support.
    UpgradeCapabilities { protocol_version: u32 },
    PrepareStop { protocol_version: u32, expected_instance: String, request_nonce: String, index_identity: String, timeout_ms: u64 },
    CommitStop { token: StopToken },
    CancelStop { token: StopToken },
    CheckpointIdentity { protocol_version: u32 },
    UsageCapabilities,
    UsageChildren {
        path: String,
        expected: Option<(u64, u64)>,
        /// Opaque server continuation, never a directory/entry index.
        /// Scoped to path + index epoch; data-generation churn is tolerated,
        /// an expired lease (epoch changed) requires refresh.
        after: Option<u32>,
        max_bytes: usize,
    },
    Status,
    Ping,
}

fn default_limit() -> usize {
    500
}

fn default_pinyin() -> bool {
    true
}

fn default_metadata_ready() -> bool {
    true
}

/// 服务端 → 客户端的响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcResponse {
    SearchResult {
        hits: Vec<SearchHitDto>,
        #[serde(default)]
        total: u32,
        /// service 侧从 query 解析到生成 hits 的耗时（毫秒，整数）；
        /// `0` 表示旧版 service 未上报。GUI 状态栏显示 「查询 N 条 · 耗时 X ms」。
        #[serde(default)]
        elapsed_ms: u32,
    },
    StatusResult {
        entry_count: u64,
        #[serde(default)]
        dir_count: u64,
        #[serde(default)]
        last_usn: u64,
        #[serde(default)]
        journal_id: u64,
        #[serde(default)]
        volume_letter: Option<char>,
        #[serde(default)]
        healthy: bool,
        /// 与 `index.bin`：快速首遍未完成回填时为 `false`
        #[serde(default = "default_metadata_ready")]
        metadata_ready: bool,
        /// 元数据回填进度（仅 `metadata_ready == false` 时有效；完成后为 0）
        #[serde(default)]
        backfill_done: u64,
        #[serde(default)]
        backfill_total: u64,
        /// `true` 表示 service 进程已起、命名管道已监听，但 `index.bin` 还没加载完成。
        /// GUI 在该状态下应显示「索引加载中…」而不是「管道超时」。
        #[serde(default)]
        loading: bool,
        /// `loading == true` 时的加载阶段文案，例如 `解析条目（3/8 阶段，已 42s，共 3.1 亿条）`。
        /// `None` = 旧版 service 未上报，或已加载完成。
        ///
        /// 千万级库的 `load_index_bin` 要几十秒到几分钟；没有这个字段时 GUI 只能显示一个
        /// 静止的「加载中」，与「卡死」无法区分。
        #[serde(default)]
        loading_stage: Option<String>,
        /// 加载已耗时（秒）；`None` = 未上报。
        #[serde(default)]
        loading_elapsed_secs: Option<u64>,
        /// 加载进度百分比的分子/分母（当前实现按"阶段"计：第几阶段 / 共几阶段）。
        /// 前端可用于画细进度条；`None` = 未上报。用 u32 而非 float，避免 JSON 浮点比较。
        #[serde(default)]
        loading_phase_done: Option<u32>,
        #[serde(default)]
        loading_phase_total: Option<u32>,
        /// USN 监听故障描述（`None` = 各卷增量正常）。watch 中断/重建时 service 侧设置，
        /// GUI 状态栏展示。`#[serde(default)]` 保持与旧版 service 的兼容。
        #[serde(default)]
        watch_error: Option<String>,
        /// 元数据回填被关闭或失败时的说明（`None` = 回填正常推进或已完成）。
        #[serde(default)]
        backfill_error: Option<String>,
        /// 墓碑条目数（已删除/排除/卷重建留下的「只标记不删除」残留）。
        /// `None` = 旧版 service 未上报；GUI 设置页据此显示「建议压缩」与一键压缩入口。
        /// 阈值约定见 `IndexStore::should_compact`（≥10 万条目且墓碑比 >25%）。
        #[serde(default)]
        tombstone_count: Option<u64>,
        /// WAL 未折叠字节数（0 = 无堆积或旧版 service）。GUI 健康条用它回答
        /// “后台在写什么”：心跳只 append KB 级；持续涨到 fold 阈值才触发整段拷贝。
        #[serde(default)]
        wal_bytes: u64,
        /// 距上次成功 fold 的秒数（`None` = 本进程尚未 fold 或旧版 service）。
        #[serde(default)]
        last_fold_secs_ago: Option<u64>,
        /// Bit 0 is A:. Old peers omit it; the host then falls back to `volume_letter`.
        #[serde(default)]
        indexed_drive_mask: u32,
    },
    UpgradeCapabilitiesResult {
        protocol_version: u32,
        durable_prepare: bool,
        /// Atomic managed-runtime source outcome; None is not a durability receipt.
        #[serde(default)]
        runtime_error: Option<String>,
        #[serde(default)]
        last_checkpoint_error: Option<String>,
        reason: String,
    },
    CheckpointIdentityResult { protocol_version: u32, engine_instance: String, service_pid: u32, index_identity: String },
    StopPrepared { token: StopToken },
    StopCommitted { receipt: DurableStopReceipt, receipt_path: String },
    StopCancelled,
    UsageCapabilitiesResult {
        logical_tree: u32,
        allocated: bool,
    },
    UsageChildrenResult {
        page: UsagePageDto,
    },
    Pong,
    Error {
        message: String,
    },
}

/// 搜索结果 DTO（与前端 / CLI 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHitDto {
    #[serde(default)]
    pub entry_idx: u32,
    pub name: String,
    pub path: String,
    pub size: u64,
    pub mtime: u64,
    #[serde(default)]
    pub is_directory: bool,
    /// 文件名高亮：Unicode 标量字符下标 [start, end)，与 `findx2-core` 搜索/拼音匹配一致
    #[serde(default)]
    pub name_highlight: Vec<[u32; 2]>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_search_request() {
        let j = serde_json::to_string(&IpcRequest::Search {
            query: "foo".into(),
            pinyin: false,
            limit: 100,
            offset: 0,
        })
        .unwrap();
        let r: IpcRequest = serde_json::from_str(&j).unwrap();
        match r {
            IpcRequest::Search {
                query,
                pinyin,
                limit,
                offset,
            } => {
                assert_eq!(query, "foo");
                assert!(!pinyin);
                assert_eq!(limit, 100);
                assert_eq!(offset, 0);
            }
            _ => panic!(),
        }
        // 老客户端不带 offset 也能解析（向后兼容）。
        let r2: IpcRequest = serde_json::from_str(r#"{"type":"search","query":"x"}"#).unwrap();
        match r2 {
            IpcRequest::Search { offset, limit, .. } => {
                assert_eq!(offset, 0);
                assert_eq!(limit, 500);
            }
            _ => panic!(),
        }
    }
}

/// Logical-only directory view. No physical allocation inference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageItemDto {
    pub id: u32,
    pub name: String,
    pub directory: bool,
    pub logical: u64,
    pub files: u64,
    pub directories: u64,
    pub unknown: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsagePageDto {
    pub epoch: u64,
    pub generation: u64,
    pub root: UsageItemDto,
    pub children: Vec<UsageItemDto>,
    pub next: Option<u32>,
    pub resident_bytes: usize,
    pub state: String,
    #[serde(default)]
    pub watch_error: Option<String>,
    #[serde(default)]
    pub metadata_error: Option<String>,
}

#[cfg(test)]
mod usage_health_tests {
    #[test]
    fn missing_optional_health_fields_decode_as_unknown_without_breaking_pages() {
        let page: super::UsagePageDto=serde_json::from_value(serde_json::json!({"epoch":1,"generation":2,"root":{"id":0,"name":"fixture","directory":true,"logical":0,"files":0,"directories":1,"unknown":0},"children":[],"next":null,"resident_bytes":0,"state":"metadata freshness not established"})).unwrap();
        assert!(page.watch_error.is_none());
        assert!(page.metadata_error.is_none());
    }
}

#[cfg(test)]
mod upgrade_protocol_tests {
    #[test]
    fn upgrade_negotiation_requires_explicit_version_and_has_no_implicit_prepare() {
        assert!(serde_json::from_str::<super::IpcRequest>(r#"{"type":"upgrade_capabilities"}"#).is_err());
        assert!(serde_json::from_str::<super::IpcRequest>(r#"{"type":"prepare_upgrade","protocol_version":1}"#).is_err());
        assert!(matches!(serde_json::from_str::<super::IpcRequest>(r#"{"type":"upgrade_capabilities","protocol_version":1}"#).unwrap(), super::IpcRequest::UpgradeCapabilities { protocol_version: 1 }));
    }

    #[test]
    fn older_negative_capabilities_without_runtime_error_remain_compatible(){
        let response:super::IpcResponse=serde_json::from_str(r#"{"type":"upgrade_capabilities_result","protocol_version":1,"durable_prepare":false,"reason":"old negative response"}"#).unwrap();
        assert!(matches!(response,super::IpcResponse::UpgradeCapabilitiesResult{durable_prepare:false,runtime_error:None,..}));
    }
}
