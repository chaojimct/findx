//! macOS 占位：`getattrlistbulk` / FSEvents 接入骨架。

use findx2_core::{ChangeEvent, ChangeWatcher, RawEntry, Result, VolumeScanner};

pub struct MacosVolumeScanner;

impl VolumeScanner for MacosVolumeScanner {
    fn scan(&self, _volume: &str) -> Result<Vec<RawEntry>> {
        Ok(Vec::new())
    }
}

pub struct MacosChangeWatcher;

impl ChangeWatcher for MacosChangeWatcher {
    fn watch(&self, _tx: std::sync::mpsc::Sender<ChangeEvent>) -> Result<()> {
        Err(findx2_core::Error::Platform(
            "macOS ChangeWatcher 尚未实现".into(),
        ))
    }
}
