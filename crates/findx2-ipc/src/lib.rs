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
        /// USN 监听故障描述（`None` = 各卷增量正常）。watch 中断/重建时 service 侧设置，
        /// GUI 状态栏展示。`#[serde(default)]` 保持与旧版 service 的兼容。
        #[serde(default)]
        watch_error: Option<String>,
        /// 元数据回填被关闭或失败时的说明（`None` = 回填正常推进或已完成）。
        #[serde(default)]
        backfill_error: Option<String>,
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
