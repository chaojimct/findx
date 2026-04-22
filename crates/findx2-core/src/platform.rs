//! 平台抽象：`VolumeScanner` 全量扫描、`ChangeWatcher` 增量监控。

use std::sync::mpsc::Sender;

use crate::Result;

/// 原始目录/文件条目（来自 MFT 或 stat 遍历）
#[derive(Debug, Clone)]
pub struct RawEntry {
    /// 文件/目录在本卷内的文件引用号（Windows FRN；其他平台可填 inode 或合成 id）
    pub file_id: u64,
    /// `USN_RECORD_V3` 的 `FILE_ID_128` 前 16 字节（与 `file_id` 低 64 位同源）；用于 `OpenFileById` 扩展 ID 回退。
    pub file_id_128: Option<[u8; 16]>,
    pub parent_id: u64,
    pub name: String,
    pub size: u64,
    pub mtime: u64,
    pub ctime: u64,
    pub attrs: u32,
    pub is_dir: bool,
}

/// USN / inotify 等产生的变更事件
#[derive(Debug, Clone)]
pub enum ChangeEvent {
    Create {
        entry: RawEntry,
    },
    Delete {
        file_id: u64,
    },
    Rename {
        file_id: u64,
        new_parent_id: u64,
        new_name: String,
    },
    /// 文件大小或时间变化
    DataOrMeta {
        file_id: u64,
        size: Option<u64>,
        mtime: Option<u64>,
        ctime: Option<u64>,
    },
}

/// 初始全量扫描
pub trait VolumeScanner: Send + Sync {
    fn scan(&self, volume: &str) -> Result<Vec<RawEntry>>;
}

/// 增量实时监控（可选，MVP 可先返回不支持）
pub trait ChangeWatcher: Send + Sync {
    fn watch(&self, tx: Sender<ChangeEvent>) -> Result<()>;
}
