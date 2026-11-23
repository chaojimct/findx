use thiserror::Error as ThisError;

#[derive(Debug, ThisError)]
pub enum Error {
    #[error("文本抽取失败 {0}: {1}")]
    Extract(String, String),
    #[error("内容索引失败: {0}")]
    Index(String),
    #[error("IO 失败 {0}: {1}")]
    Io(String, #[source] std::io::Error),
    /// 构建被取消（progress 回调返回 false）。调用方应清理临时目录。
    #[error("内容索引构建已取消")]
    Cancelled,
}

impl From<tantivy::TantivyError> for Error {
    fn from(e: tantivy::TantivyError) -> Self {
        Error::Index(e.to_string())
    }
}

impl From<Error> for findx2_core::Error {
    fn from(e: Error) -> Self {
        findx2_core::Error::Persist(format!("内容索引: {e}"))
    }
}

impl From<tantivy::query::QueryParserError> for Error {
    fn from(e: tantivy::query::QueryParserError) -> Self {
        Error::Index(e.to_string())
    }
}
