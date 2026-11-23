use thiserror::Error as ThisError;

#[derive(Debug, ThisError)]
pub enum Error {
    #[error("文本抽取失败 {0}: {1}")]
    Extract(String, String),
    #[error("内容索引失败: {0}")]
    Index(String),
    #[error("IO 失败 {0}: {1}")]
    Io(String, #[source] std::io::Error),
}

impl From<tantivy::TantivyError> for Error {
    fn from(e: tantivy::TantivyError) -> Self {
        Error::Index(e.to_string())
    }
}

impl From<tantivy::query::QueryParserError> for Error {
    fn from(e: tantivy::query::QueryParserError) -> Self {
        Error::Index(e.to_string())
    }
}
