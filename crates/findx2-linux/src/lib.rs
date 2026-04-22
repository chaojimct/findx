//! Linux 占位：通过 `statx` + 目录遍历的扫描器骨架（当前返回空/错误）。

use findx2_core::{ChangeEvent, ChangeWatcher, RawEntry, Result, VolumeScanner};

pub struct LinuxVolumeScanner;

impl VolumeScanner for LinuxVolumeScanner {
    fn scan(&self, _volume: &str) -> Result<Vec<RawEntry>> {
        Ok(Vec::new())
    }
}

pub struct LinuxChangeWatcher;

impl ChangeWatcher for LinuxChangeWatcher {
    fn watch(&self, _tx: std::sync::mpsc::Sender<ChangeEvent>) -> Result<()> {
        Err(findx2_core::Error::Platform(
            "Linux ChangeWatcher 尚未实现".into(),
        ))
    }
}
