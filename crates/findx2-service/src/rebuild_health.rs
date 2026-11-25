//! 自愈计数器（P1-3）：跨进程持久的重建失败计数，对标 Listary「错误超阈值下次启动强制重建」。
//!
//! 语义：
//! - 每次触发全卷重建的失败（USN 断档达进程内上限被迫放弃 / `rebuild_volume` 失败）记一次；
//! - 收到健康 Checkpoint（增量链真正跑通）清零；
//! - 启动时若计数 ≥ 阈值，跳过增量 resume，直接全卷重建，并消费一次（清零，避免每次启动都重建）。
//!
//! 文件：与 `index.bin` 同目录的 `rebuild_health.json`（小文件，原子替换写，损坏按 0 处理）。
//! 阈值：连续 3 次（与 `run.rs` 进程内 `gap_rebuilds < 3` 对齐）。
//! 计数按卷独立（多卷服务不互相污染）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 连续失败几次后，下次启动强制全卷重建。
pub const FORCE_REBUILD_THRESHOLD: u32 = 3;
/// 与 `index.bin` 同目录的健康文件。
pub const HEALTH_FILE_NAME: &str = "rebuild_health.json";

fn health_path_for(index_path: &Path) -> PathBuf {
    index_path.with_file_name(HEALTH_FILE_NAME)
}

fn load_map(path: &Path) -> HashMap<String, u32> {
    let Ok(bytes) = std::fs::read(path) else {
        return HashMap::new();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

fn save_map(path: &Path, map: &HashMap<String, u32>) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(bytes) = serde_json::to_vec(map) else {
        return;
    };
    // 原子替换：先写临时再 rename，避免崩溃留半截文件。
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, &bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

fn vol_key(letter: char) -> String {
    letter.to_ascii_uppercase().to_string()
}

/// 记录一次重建类失败，返回累计值（饱和防溢出）。低频路径，失败即写盘。
pub fn record_failure(index_path: &Path, letter: char) -> u32 {
    let path = health_path_for(index_path);
    let mut map = load_map(&path);
    let n = map.get(&vol_key(letter)).copied().unwrap_or(0);
    let n = n.saturating_add(1);
    map.insert(vol_key(letter), n);
    save_map(&path, &map);
    n
}

/// 健康 Checkpoint 到达即清零（增量链跑通的证明）。
/// 已为 0 时只读不写，避免高频 checkpoint 刷盘。
pub fn record_success(index_path: &Path, letter: char) {
    let path = health_path_for(index_path);
    let mut map = load_map(&path);
    if map.remove(&vol_key(letter)).is_some() {
        save_map(&path, &map);
    }
}

/// 当前累计失败数（Status 展示用；文件缺失/损坏按 0）。
pub fn failures_for(index_path: &Path, letter: char) -> u32 {
    load_map(&health_path_for(index_path))
        .get(&vol_key(letter))
        .copied()
        .unwrap_or(0)
}

/// 启动时检查：超阈值则返回 true 并消费一次（清零落盘，下次启动不再强制）。
pub fn take_force_rebuild(index_path: &Path, letter: char) -> bool {
    let path = health_path_for(index_path);
    let mut map = load_map(&path);
    let n = map.get(&vol_key(letter)).copied().unwrap_or(0);
    if n < FORCE_REBUILD_THRESHOLD {
        return false;
    }
    map.remove(&vol_key(letter));
    save_map(&path, &map);
    true
}

/// service 全局 index 位置（`run_foreground` 启动时登记一次；Status 查询计数用）。
/// 未登记（单测/fixture）时计数查询一律按 0 处理。
static INDEX_PATH: std::sync::OnceLock<std::sync::Mutex<Option<PathBuf>>> =
    std::sync::OnceLock::new();

fn index_path_slot() -> &'static std::sync::Mutex<Option<PathBuf>> {
    INDEX_PATH.get_or_init(|| std::sync::Mutex::new(None))
}

/// 启动时登记 `index.bin` 位置（供 Status 展示计数值；只记首次，多卷共用首个）。
pub fn note_index_path(index_path: &Path) {
    let mut g = index_path_slot().lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        *g = Some(index_path.to_path_buf());
    }
}

/// Status 展示用：该卷累计失败数（未登记/文件缺失按 0）。
pub fn failures_for_letter(letter: Option<char>) -> u32 {
    let g = index_path_slot().lock().unwrap_or_else(|e| e.into_inner());
    let (Some(p), Some(l)) = (g.as_ref(), letter) else {
        return 0;
    };
    failures_for(p, l)
}

#[cfg(test)]
mod rebuild_health_tests {
    use super::*;

    fn tmp_index() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("index.bin");
        (d, p)
    }

    #[test]
    fn missing_file_means_zero_and_no_force() {
        let (_d, p) = tmp_index();
        assert_eq!(failures_for(&p, 'C'), 0);
        assert!(!take_force_rebuild(&p, 'C'));
    }

    #[test]
    fn three_failures_trigger_one_force_rebuild_then_reset() {
        let (_d, p) = tmp_index();
        assert_eq!(record_failure(&p, 'C'), 1);
        assert_eq!(record_failure(&p, 'C'), 2);
        assert!(!take_force_rebuild(&p, 'C'));
        assert_eq!(record_failure(&p, 'C'), 3);
        // 超阈值：消费一次。
        assert!(take_force_rebuild(&p, 'C'));
        // 已消费：不再强制。
        assert!(!take_force_rebuild(&p, 'C'));
        assert_eq!(failures_for(&p, 'C'), 0);
    }

    #[test]
    fn success_resets_and_volumes_are_independent() {
        let (_d, p) = tmp_index();
        record_failure(&p, 'C');
        record_failure(&p, 'C');
        record_failure(&p, 'D');
        record_success(&p, 'C');
        assert_eq!(failures_for(&p, 'C'), 0);
        assert_eq!(failures_for(&p, 'D'), 1);
        assert!(!take_force_rebuild(&p, 'C'));
    }

    #[test]
    fn corrupt_file_is_treated_as_zero() {
        let (d, p) = tmp_index();
        std::fs::write(d.path().join(HEALTH_FILE_NAME), b"{not json").unwrap();
        assert_eq!(failures_for(&p, 'C'), 0);
        assert_eq!(record_failure(&p, 'c'), 1); // 小写归一化
        assert_eq!(failures_for(&p, 'C'), 1);
    }
}
