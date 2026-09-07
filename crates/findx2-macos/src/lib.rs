//! macOS 后端：`getattrlistbulk` 全量扫描 + FSEvents 增量。

use findx2_core::{ChangeEvent, ChangeWatcher, RawEntry, Result, VolumeScanner, WatchCursor};

#[cfg(target_os = "macos")]
mod scan;
#[cfg(target_os = "macos")]
mod watch;

#[cfg(target_os = "macos")]
pub use scan::{
    default_scan_root, display_root_prefix, take_scan_note, volume_id_for_path, MacosVolumeScanner,
};
#[cfg(target_os = "macos")]
pub use watch::{current_event_id, watch_loop, MacosChangeWatcher};

#[cfg(not(target_os = "macos"))]
pub struct MacosVolumeScanner;

#[cfg(not(target_os = "macos"))]
impl VolumeScanner for MacosVolumeScanner {
    fn scan_into(
        &self,
        _volume: &str,
        _out: &mut dyn FnMut(RawEntry) -> Result<()>,
    ) -> Result<WatchCursor> {
        Err(findx2_core::Error::Platform(
            "findx2-macos 仅在 macOS 上可用".into(),
        ))
    }
}

#[cfg(not(target_os = "macos"))]
pub struct MacosChangeWatcher;

#[cfg(not(target_os = "macos"))]
impl ChangeWatcher for MacosChangeWatcher {
    fn watch(&self, _tx: std::sync::mpsc::Sender<ChangeEvent>) -> Result<()> {
        Err(findx2_core::Error::Platform(
            "findx2-macos 仅在 macOS 上可用".into(),
        ))
    }
}

#[cfg(not(target_os = "macos"))]
pub fn default_scan_root() -> String {
    "/".into()
}

#[cfg(not(target_os = "macos"))]
pub fn display_root_prefix(scan_root: &str) -> String {
    scan_root.to_string()
}

#[cfg(not(target_os = "macos"))]
pub fn volume_id_for_path(path: &str) -> String {
    path.to_string()
}

#[cfg(not(target_os = "macos"))]
pub fn take_scan_note() -> Option<String> {
    None
}

/// 扫描若干根路径并写成 `index.bin`。
pub fn build_full_disk_index(
    output: &std::path::Path,
    roots: Vec<String>,
    exclude_dir: Vec<String>,
    full_stat: bool,
    max_scan_threads: usize,
) -> Result<findx2_core::IndexStore> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (output, roots, exclude_dir, full_stat, max_scan_threads);
        return Err(findx2_core::Error::Platform(
            "findx2-macos 仅在 macOS 上可用".into(),
        ));
    }
    #[cfg(target_os = "macos")]
    {
        use findx2_core::{
            merge_index_stores, normalize_excluded_dir, save_exclude_sidecar, save_index_bin,
            IndexBuilder,
        };

        let roots = if roots.is_empty() {
            vec![default_scan_root()]
        } else {
            roots
        };
        let mut stores = Vec::new();
        for root in &roots {
            findx2_core::progress!("macOS 建库：{}", root);
            let mut files = Vec::new();
            let mut dirs = Vec::new();
            let scanner = MacosVolumeScanner {
                full_stat,
                max_threads: max_scan_threads,
            };
            let cursor = scanner.scan_into(root, &mut |e| {
                if e.is_dir {
                    dirs.push(e);
                } else {
                    files.push(e);
                }
                Ok(())
            })?;
            let store = IndexBuilder::new(0, 0, cursor.watch_gen, cursor.watch_cursor)
                .with_unix_volume(volume_id_for_path(root), display_root_prefix(root))
                .build_from_raw(files, dirs, full_stat)?;
            stores.push(store);
        }
        let mut store = if stores.len() == 1 {
            stores.pop().unwrap()
        } else {
            merge_index_stores(stores)?
        };
        let excluded: Vec<String> = exclude_dir
            .iter()
            .filter_map(|s| normalize_excluded_dir(s))
            .collect();
        if !excluded.is_empty() {
            store.excluded_dirs = excluded.clone();
            store.mark_excluded_entries(&excluded);
            let _ = save_exclude_sidecar(output, &excluded);
        }
        if let Some(parent) = output.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        save_index_bin(output, &store)?;
        if let Err(e) = findx2_core::build_trigram_sidecar(&store, output) {
            findx2_core::progress!("trigram 边车构建失败（搜索将回退全表扫描）: {e}");
        }
        Ok(store)
    }
}
