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
        /// 拼音辅库：首字母查询。缺省开（有库才生效）；GUI 全文页可关。
        #[serde(default = "default_true")]
        pinyin_initials: bool,
        /// 拼音辅库：全拼查询。缺省开（有库才生效）；GUI 全文页可关。
        #[serde(default = "default_true")]
        pinyin_full: bool,
        /// 查询 ID（P0-1）：GUI 每次击键自增；`SearchCancel{request_id}` 取消在飞的同 ID 查询；
        /// `SearchResult`/`Cancelled` 原样回显，GUI 凭此丢弃过期响应（防"结果闪回旧查询"）。
        /// 缺省 0 = 不跟踪（老客户端行为：不可取消、无回显）。
        #[serde(default)]
        request_id: u64,
        /// 会话 ID（P0-2）：同输入框的连续击键共用一 ID，service 复用该会话的结果集做增量过滤；
        /// 0 = 无会话（现状行为，每次全量）。换输入框/新搜索意图请换 ID。
        #[serde(default)]
        session_id: u64,
    },
    /// 取消在飞的 `Search{request_id}`（P0-1）。未知/已完成的 ID → 回 `Cancelled`（幂等，无错误）。
    SearchCancel { request_id: u64 },
    /// 全文内容索引构建（`<index>.content/`）：service 后台线程执行，不停服。
    /// 已在构建 → Error；`rebuild=true` 删旧全量重建，否则复用追加。
    ContentBuild {
        #[serde(default)]
        rebuild: bool,
    },
    /// 取消正在进行的 `ContentBuild`（无构建在跑 → Pong 空操作）。
    ContentCancel,
    /// 按 entry_idx 补取命中摘要（列表首屏只填前 N 行，其余选中/可见时按需取；
    /// 避免首屏为几百行回读文件卡住 service）。
    /// query 取 content: 取值原文（与 Search 一致，供分词用）。
    /// window：第几个匹配点（上一个/下一个翻页），缺省 0。
    ContentSnippets {
        query: String,
        entry_indices: Vec<u32>,
        #[serde(default)]
        window: usize,
    },
    /// 取 PDF 单页命中框（预览高亮用；与 `ContentSnippets` 同源解析 query）。
    /// query 取 content: 取值原文；entry_index 为结果行号；page 为预览侧当前页。
    /// 失败/无命中 → 空 rects（预览降级为无高亮，不报错）。
    ContentPdfBoxes {
        query: String,
        entry_index: u32,
        page: u32,
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

fn default_true() -> bool {
    true
}

fn default_metadata_ready() -> bool {
    true
}

/// 全文内容索引状态（`StatusResult.content_state` 取值）。
/// - `missing`：未建，`content:` 走慢速逐文件；
/// - `building`：后台构建中（进度见 content_scanned 等）；
/// - `ready`：可用 + 增量跟随中；
/// - `stale`：过期（schema/文件名索引/范围变化），仍可搜，建议重建；
/// - `failed`：构建失败（保留旧索引，`content_error` 看原因）；
/// - `disabled`：增量跟随被关闭且未建。
pub mod content_state {
    pub const MISSING: &str = "missing";
    pub const BUILDING: &str = "building";
    pub const READY: &str = "ready";
    pub const STALE: &str = "stale";
    pub const FAILED: &str = "failed";
    pub const DISABLED: &str = "disabled";
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
        /// 请求 ID 回显（P0-1）：与发出 `Search` 时的 `request_id` 一致；
        /// 缺省 0 = 老版 service 未上报。GUI 凭此丢弃过期响应。
        #[serde(default)]
        request_id: u64,
    },
    /// 查询被取消的确认（P0-1）：`SearchCancel` / 新查询替代旧查询触发。
    /// 老客户端收不到（从不发 request_id != 0 的请求）；收到即丢弃该 ID 的等待。
    Cancelled { request_id: u64 },
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
        /// 全文内容索引状态（见 `content_state` 模块）。缺省 "" = 老版 service 未上报。
        #[serde(default)]
        content_state: String,
        /// 内容索引文档数（`ready`/`stale`/`failed` 时有效）。
        #[serde(default)]
        content_docs: u64,
        /// 内容索引目录体积（字节，打开/构建完成时刷新）。
        #[serde(default)]
        content_bytes: u64,
        /// 本次（或上次）构建进度：已扫描文件名索引条目数。
        #[serde(default)]
        content_scanned: u64,
        /// 本次（或上次）构建进度：已抽取入倒排的文件数。
        #[serde(default)]
        content_extracted: u64,
        /// 本次（或上次）构建进度：跳过（不支持类型/超限/空文件）数。
        #[serde(default)]
        content_skipped: u64,
        /// 本次（或上次）构建进度：抽取/入库失败数。
        #[serde(default)]
        content_failed: u64,
        /// 全文索引异常说明（构建失败原因 / 增量丢批提示 / 过期原因）。
        /// `None` = 无异常。`#[serde(default)]` 保持与旧版 service 的兼容。
        #[serde(default)]
        content_error: Option<String>,
        /// 全文索引状态细节（Stale 原因 / 构建备注，`ready` 时为空）。
        /// 缺省 "" = 老版 service 未上报。
        #[serde(default)]
        content_detail: String,
        /// 文件名索引总条目数（构建进度 `content_scanned / content_entry_total` 即百分比）。
        #[serde(default)]
        content_entry_total: u64,
        /// service 实际挂载的内容目录（GUI 不再从自身 indexPath 推导；缺省 "" = 老版 service）。
        #[serde(default)]
        content_dir: String,
        /// 拼音辅库（首字母）文档数；0 = 未建/未挂载，查询自动降级。
        #[serde(default)]
        content_aux_ini_docs: u64,
        /// 拼音辅库（首字母）目录体积（字节）。
        #[serde(default)]
        content_aux_ini_bytes: u64,
        /// 拼音辅库（全拼）文档数；0 = 未建/未挂载，查询自动降级。
        #[serde(default)]
        content_aux_full_docs: u64,
        /// 拼音辅库（全拼）目录体积（字节）。
        #[serde(default)]
        content_aux_full_bytes: u64,
        /// 自愈计数器（P1-3）：跨进程持久的连续重建失败数（`rebuild_health.json`）。
        /// ≥3 时 service 下次启动强制全卷重建。缺省 0 = 老版 service 未上报。
        /// GUI 状态栏可在 >0 时提示"索引曾多次重建失败"。
        #[serde(default)]
        rebuild_failures: u32,
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
    /// `ContentSnippets` 应答：按 entry_idx 的命中摘要窗口。
    ContentSnippets {
        snippets: Vec<ContentSnippetDto>,
    },
    /// `ContentPdfBoxes` 应答：单页命中框（点坐标，左上原点）+ 页尺寸（点）。
    /// 空 rects = 无命中/失败，调用方降级为无高亮。
    ContentPdfBoxes {
        #[serde(default)]
        rects: Vec<[f32; 4]>,
        #[serde(default)]
        page_w: f32,
        #[serde(default)]
        page_h: f32,
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
    /// 全文命中摘要（tantivy snippet HTML，`<b>` 标命中词；GUI 侧去标签展示）。
    /// 只在含 `content:` 谓词的搜索中对当页前 N 个命中填充，其余为空串。
    /// 缺省 "" = 老版 service 未上报。
    #[serde(default)]
    pub snippet: String,
    /// 拼音辅库命中标记（GUI 可显示“拼”徽标；老客户端忽略）。
    #[serde(default)]
    pub pinyin_hit: bool,
    /// 摘要对应窗口是全文第几个匹配点（0 起；上一个/下一个翻页用）。
    #[serde(default)]
    pub snippet_window: usize,
    /// 全文共多少个匹配点（上一个/下一个翻页与 `x/y` 展示用；0 = 未知）。
    #[serde(default)]
    pub snippet_total: u32,
}

/// 单条命中摘要（`ContentSnippets` 应答用；`window`/`total` 含义同上）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentSnippetDto {
    pub entry_idx: u32,
    pub html: String,
    pub window: usize,
    pub total: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_search_request() {
        let j = serde_json::to_string(&IpcRequest::Search {
            query: "foo".into(),
            pinyin: false,
            pinyin_initials: true,
            pinyin_full: true,
            limit: 100,
            offset: 0,
            request_id: 7,
            session_id: 3,
        })
        .unwrap();
        let r: IpcRequest = serde_json::from_str(&j).unwrap();
        match r {
            IpcRequest::Search {
                query,
                pinyin,
                pinyin_initials,
                pinyin_full,
                limit,
                offset,
                request_id,
                session_id,
            } => {
                assert_eq!(query, "foo");
                assert!(!pinyin);
                assert_eq!(limit, 100);
                assert!(pinyin_initials);
                assert!(pinyin_full);
                assert_eq!(offset, 0);
                assert_eq!(request_id, 7);
                assert_eq!(session_id, 3);
            }
            _ => panic!(),
        }
        // 老客户端不带 offset/request_id/session_id 也能解析（向后兼容）。
        let r2: IpcRequest = serde_json::from_str(r#"{"type":"search","query":"x"}"#).unwrap();
        match r2 {
            IpcRequest::Search { offset, limit, pinyin_initials, pinyin_full, request_id, session_id, .. } => {
                assert_eq!(offset, 0);
                assert_eq!(limit, 500);
                assert!(pinyin_initials);
                assert!(pinyin_full);
                assert_eq!(request_id, 0);
                assert_eq!(session_id, 0);
            }
            _ => panic!(),
        }
        // SearchCancel 往返 + 已取消确认。
        let c = IpcRequest::SearchCancel { request_id: 9 };
        let back: IpcRequest = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert!(matches!(back, IpcRequest::SearchCancel { request_id: 9 }));
        let cr = IpcResponse::Cancelled { request_id: 9 };
        let back_r: IpcResponse =
            serde_json::from_str(&serde_json::to_string(&cr).unwrap()).unwrap();
        assert!(matches!(back_r, IpcResponse::Cancelled { request_id: 9 }));
        // 老 service 的 SearchResult（无 request_id）按缺省解析。
        let sr: IpcResponse = serde_json::from_str(
            r#"{"type":"search_result","hits":[],"total":1,"elapsed_ms":5}"#,
        )
        .unwrap();
        assert!(matches!(sr, IpcResponse::SearchResult { request_id: 0, .. }));
    }

    #[test]
    fn content_protocol_roundtrip_and_old_peer_compat() {
        // 新请求往返。
        for req in [
            IpcRequest::ContentBuild { rebuild: false },
            IpcRequest::ContentBuild { rebuild: true },
            IpcRequest::ContentCancel,
        ] {
            let j = serde_json::to_string(&req).unwrap();
            let back: IpcRequest = serde_json::from_str(&j).unwrap();
            match (&req, &back) {
                (IpcRequest::ContentBuild { rebuild: a }, IpcRequest::ContentBuild { rebuild: b }) => {
                    assert_eq!(a, b);
                }
                (IpcRequest::ContentCancel, IpcRequest::ContentCancel) => {}
                _ => panic!("content request roundtrip mismatch: {j}"),
            }
        }
        // 老 service 的 Status（无 content_* 字段）按缺省值解析。
        let old: IpcResponse = serde_json::from_str(
            r#"{"type":"status_result","entry_count":7,"healthy":true}"#,
        )
        .unwrap();
        match old {
            IpcResponse::StatusResult {
                content_state,
                content_docs,
                content_error,
                content_detail,
                content_entry_total,
                ..
            } => {
                assert!(content_state.is_empty());
                assert_eq!(content_docs, 0);
                assert!(content_error.is_none());
                assert!(content_detail.is_empty());
                assert_eq!(content_entry_total, 0);
            }
            _ => panic!(),
        }
        // 老 service 的命中（无 snippet）按空串解析。
        let hit: SearchHitDto = serde_json::from_str(
            r#"{"entry_idx":1,"name":"a.txt","path":"C:\\a.txt","size":3,"mtime":4}"#,
        )
        .unwrap();
        assert!(hit.snippet.is_empty());
        assert!(hit.name_highlight.is_empty());
        // 状态常量互斥且稳定（GUI/脚本可直接比字符串）。
        let states = [
            content_state::MISSING,
            content_state::BUILDING,
            content_state::READY,
            content_state::STALE,
            content_state::FAILED,
            content_state::DISABLED,
        ];
        let mut seen = std::collections::HashSet::new();
        for s in states {
            assert!(!s.is_empty());
            assert!(seen.insert(s), "content_state 重复: {s}");
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
