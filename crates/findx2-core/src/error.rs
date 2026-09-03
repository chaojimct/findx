use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("IO: {0}")]
    Io(#[from] std::io::Error),

    #[error("索引持久化: {0}")]
    Persist(String),

    #[error("查询解析: {0}")]
    Query(String),

    #[error("平台扫描: {0}")]
    Platform(String),

    /// USN Journal 断档：游标已被覆写（`StartUsn < FirstUsn` / `ERROR_JOURNAL_ENTRY_DELETED`）
    /// 或 Journal 重建导致 ID 变化。调用方应触发全量重建，而非继续增量。
    #[error("USN 日志断档，需全量重建: {0}")]
    JournalGap(String),

    #[error("UTF-8 无效")]
    Utf8(#[from] std::str::Utf8Error),

    #[error("UTF-8 无效（字符串）")]
    Utf8String(#[from] std::string::FromUtf8Error),

    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
}
