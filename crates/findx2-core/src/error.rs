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

    #[error("UTF-8 无效")]
    Utf8(#[from] std::str::Utf8Error),

    #[error("UTF-8 无效（字符串）")]
    Utf8String(#[from] std::string::FromUtf8Error),

    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
}
