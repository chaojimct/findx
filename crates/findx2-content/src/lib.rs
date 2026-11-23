//! findx2 全文内容索引（M0 PoC）。
//!
//! 设计原则（见全文搜索报告）：
//! - 文件名索引保持自研不动；本 crate 是**独立内容倒排**，只通过
//!   `volume_id + frn` 与 `IndexStore` 做关联（Rename 零重抽，Delete 删 doc）。
//! - schema v1 冻结字段，升级走版本号 + 重建，不做在线迁移。

pub mod error;
pub mod extract;
pub mod index;

pub use error::Error;
pub use extract::{extract_text, ExtractedText};
pub use index::{open_or_create, ContentIndex, ContentHit, DocMeta, SCHEMA_VERSION};

pub type Result<T> = std::result::Result<T, Error>;
