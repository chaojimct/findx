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
    /// 快速建条目（USN watch 热路径）：只带名字/父链/属性，不带 size/mtime/ctime。
    /// 元数据由后台 stat worker 补（`UsnWatchMsg::StatRefresh`），watch 线程永不阻塞在
    /// `OpenFileById` 上。已存在条目只更新名字/父链/属性，**不**清零已有元数据。
    CreatePending {
        file_id: u64,
        file_id_128: Option<[u8; 16]>,
        parent_id: u64,
        name: String,
        attrs: u32,
        is_dir: bool,
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

/// 增量游标：Windows 为 JournalId + NextUsn；macOS 为 FSEvents 世代 + eventId；
/// Linux fanotify 无持久 journal，世代在进程启动时递增，游标可为 0。
#[derive(Debug, Clone, Copy, Default)]
pub struct WatchCursor {
    pub watch_gen: u64,
    pub watch_cursor: u64,
}

/// 初始全量扫描。热路径应走 `scan_into`，避免百万级 `Vec` 峰值。
pub trait VolumeScanner: Send + Sync {
    fn scan_into(
        &self,
        volume: &str,
        out: &mut dyn FnMut(RawEntry) -> Result<()>,
    ) -> Result<WatchCursor>;

    fn scan(&self, volume: &str) -> Result<Vec<RawEntry>> {
        let mut out = Vec::new();
        self.scan_into(volume, &mut |e| {
            out.push(e);
            Ok(())
        })?;
        Ok(out)
    }
}

/// 增量实时监控（可选，MVP 可先返回不支持）
pub trait ChangeWatcher: Send + Sync {
    fn watch(&self, tx: Sender<ChangeEvent>) -> Result<()>;

    /// 带断点续跑的监听。断档（FSEvents MustScan / USN JournalGap）应返回 `Error::JournalGap`。
    fn watch_from(
        &self,
        volume: &str,
        cursor: WatchCursor,
        tx: Sender<ChangeEvent>,
    ) -> Result<WatchCursor> {
        let _ = (volume, cursor);
        self.watch(tx)?;
        Ok(WatchCursor::default())
    }
}
