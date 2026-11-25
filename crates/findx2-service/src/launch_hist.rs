//! 启动历史（P1-D）：`{path → 打开次数, last_open}`。
//!
//! - 纯记录层：GUI 上报打开成功，service 累计并落盘；**不参与排序**
//!   （`CombinedScore` 压进 u64 + `sort_top_k` 接线是 T123 P1-D 的第二步，
//!   归排序 owner；此处只提供 `score_for(path)` 查询口径，保持 API 稳定）。
//! - 存储：与 `index.bin` 同目录的 `index.bin.launch.json`（小文件，原子替换写，
//!   损坏按空表处理）。`usage.rs`（磁盘用量树）无关，不复用。
//! - 上限 20_000 条，超限淘汰最久未用；每次上报即落盘（用户paced，IO 可忽略）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// 单条启动记录。
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct LaunchRec {
    /// 累计打开次数（饱和截断，不回绕）。
    pub count: u32,
    /// 最近打开时刻（unix 秒）。
    pub last_open: u64,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct LaunchFile {
    #[serde(default)]
    entries: HashMap<String, LaunchRec>,
}

pub struct LaunchHist {
    index_bin: PathBuf,
    entries: HashMap<String, LaunchRec>,
}

impl LaunchHist {
    fn sidecar(index_bin: &Path) -> PathBuf {
        let mut p = index_bin.as_os_str().to_owned();
        p.push(".launch.json");
        PathBuf::from(p)
    }

    /// 路径归一化键：Windows 不区分大小写，统一小写 + 斜杠。
    fn key_of(path: &str) -> String {
        path.replace('/', "\\").to_ascii_lowercase()
    }

    pub fn open(index_bin: &Path) -> Self {
        let mut entries = HashMap::new();
        if let Ok(raw) = std::fs::read(Self::sidecar(index_bin)) {
            if let Ok(f) = serde_json::from_slice::<LaunchFile>(&raw) {
                entries = f.entries;
            }
        }
        Self {
            index_bin: index_bin.to_path_buf(),
            entries,
        }
    }

    /// 上报一次打开；返回该 path 当前记录（调用方可直接用 `score_for` 换算）。
    pub fn record(&mut self, path: &str, now_unix: u64) -> LaunchRec {
        const CAP: usize = 20_000;
        if self.entries.len() >= CAP && !self.entries.contains_key(&Self::key_of(path)) {
            // 淘汰最久未用（线性扫；CAP 小且上报低频，可接受）。
            if let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, r)| r.last_open)
                .map(|(k, _)| k.clone())
            {
                self.entries.remove(&oldest);
            }
        }
        let e = self
            .entries
            .entry(Self::key_of(path))
            .or_insert(LaunchRec {
                count: 0,
                last_open: 0,
            });
        e.count = e.count.saturating_add(1);
        e.last_open = e.last_open.max(now_unix);
        *e
    }

    pub fn get(&self, path: &str) -> Option<LaunchRec> {
        self.entries.get(&Self::key_of(path)).copied()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 原子落盘（tmp + rename；Windows 下旧文件被占则尽力而为，失败不抛）。
    pub fn save(&self) {
        let sidecar = Self::sidecar(&self.index_bin);
        let tmp = sidecar.with_extension("launch.json.tmp");
        let Ok(body) = serde_json::to_vec(&LaunchFile {
            entries: self.entries.clone(),
        }) else {
            return;
        };
        if std::fs::write(&tmp, body).is_err() {
            return;
        }
        let _ = std::fs::rename(&tmp, &sidecar);
    }
}

/// 评分口径（排序 owner 接线用；当前未接入排序，仅供查询/诊断）。
/// 高频且新 → 大；未知路径 → 0。
pub fn score_for(rec: Option<LaunchRec>, now_unix: u64) -> u64 {
    let Some(r) = rec else { return 0 };
    let age_days = now_unix.saturating_sub(r.last_open) / 86_400;
    // 新近 30 天内线性加权 + 频次对数档，压进 u64 高位为将来 CombinedScore 预留。
    let recency: u64 = 30u64.saturating_sub(age_days.min(30));
    let freq: u64 = (r.count as u64 + 1).ilog2() as u64;
    (recency << 32) | (freq & 0xffff_ffff)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// 进程级单例（随 service 常驻；`note_launch_index` 在 run_foreground 报备路径）。
fn hub() -> &'static Mutex<Option<LaunchHist>> {
    static HUB: OnceLock<Mutex<Option<LaunchHist>>> = OnceLock::new();
    HUB.get_or_init(|| Mutex::new(None))
}

/// run_foreground 启动时报备 index 路径（与 rebuild_health::note_index_path 并列）。
pub fn note_launch_index(index_bin: &std::path::Path) {
    if let Ok(mut g) = hub().lock() {
        *g = Some(LaunchHist::open(index_bin));
    }
}

/// GUI 上报打开成功（fire-and-forget，best-effort；失败静默）。
pub fn record_open(path: &str) {
    let Ok(mut g) = hub().lock() else {
        return;
    };
    let Some(h) = g.as_mut() else {
        return;
    };
    h.record(path, now_unix());
    h.save();
}

/// 查询某路径记录（诊断/将来打分用）。
pub fn lookup(path: &str) -> Option<LaunchRec> {
    hub().lock().ok()?.as_ref()?.get(path)
}

#[cfg(test)]
mod launch_hist_tests {
    use super::*;

    #[test]
    fn record_counts_and_updates_recency() {
        let mut h = LaunchHist {
            index_bin: PathBuf::from("x"),
            entries: HashMap::new(),
        };
        let r1 = h.record(r"C:\A\b.txt", 100);
        assert_eq!((r1.count, r1.last_open), (1, 100));
        // 大小写/斜杠归一到同一键。
        let r2 = h.record("c:/a/B.TXT", 200);
        assert_eq!((r2.count, r2.last_open), (2, 200));
        assert_eq!(h.len(), 1);
        assert_eq!(h.get(r"C:\a\b.txt"), Some(r2));
    }

    #[test]
    fn evicts_oldest_beyond_cap_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let idx = dir.path().join("index.bin");
        let mut h = LaunchHist::open(&idx);
        // 小 cap 行为用大表验证太慢：直接塞满后确认淘汰最老。
        for i in 0..20_005u32 {
            h.record(&format!("C:\\f{i}.txt"), i as u64);
        }
        assert!(h.len() <= 20_000);
        assert!(h.get("C:\\f0.txt").is_none(), "最老的应被淘汰");
        assert!(h.get("C:\\f20004.txt").is_some());
        h.save();
        let h2 = LaunchHist::open(&idx);
        assert_eq!(h2.len(), h.len());
        assert!(h2.get("C:\\f20004.txt").is_some(), "落盘应可重载");
    }

    #[test]
    fn score_prefers_frequent_and_recent() {
        let now = 1_000_000;
        let hot = score_for(
            Some(LaunchRec {
                count: 100,
                last_open: now,
            }),
            now,
        );
        let cold = score_for(
            Some(LaunchRec {
                count: 1,
                last_open: now - 60 * 86_400,
            }),
            now,
        );
        assert!(hot > cold);
        assert_eq!(score_for(None, now), 0);
    }
}
