//! findx2 核心库：索引结构、查询解析、搜索引擎、持久化格式（平台无关）。

/// 统一进度日志：写 stderr，自动加 `[HH:MM:SS.mmm]` 本地时间戳前缀。
///
/// 之前各处用裸 `eprintln!`，没有时间，不利于排查「索引创建很慢、写盘耗时多少」的情况。
/// 注意：本宏走 stderr 而不是 tracing，是为了即便用户没有初始化 `tracing-subscriber`
/// 也能在控制台直接看到进度（CLI 默认就是这样）。
#[macro_export]
macro_rules! progress {
    ($($arg:tt)*) => {{
        ::std::eprintln!(
            "[{}] {}",
            $crate::log_now_local_short(),
            ::std::format_args!($($arg)*)
        );
    }};
}

/// 给 `progress!` 宏使用的本地时间字符串。也可用于其它地方的日志拼接。
pub fn log_now_local_short() -> impl std::fmt::Display {
    chrono::Local::now().format("%H:%M:%S%.3f")
}

pub mod error;
pub mod index;
pub mod meta_overlay;
pub mod persist;

pub use persist::{
    exclude_sidecar_path, load_exclude_sidecar, load_index_bin, save_exclude_sidecar,
    save_index_bin, save_index_zst, write_index_bin,
};
pub mod platform;
pub mod query;
pub mod search;

pub use error::Error;
pub use index::{
    merge_index_stores, normalize_excluded_dir, FileEntry, IndexBuilder, IndexStore, VolumeState,
};
pub use platform::{ChangeEvent, ChangeWatcher, RawEntry, VolumeScanner};
pub use query::{ParsedQuery, QueryParser, SortField};
pub use search::{BackfillProgress, SearchEngine, SearchHit, SearchOptions};

pub type Result<T> = std::result::Result<T, Error>;
