//! Read-only allocated-space model. No search hits, GUI dependency, deletion or persistent index.
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
#[derive(Clone, Debug)]
pub struct Node {
    pub path: PathBuf,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    /// Measured paths in this subtree, including this node; drilling never reuses the global count.
    pub entries: u64,
    pub directory: bool,
    pub logical: u64,
    pub mtime: u64,
    pub allocated: u64,
    pub hardlink_duplicate: bool,
    pub uncertain: bool,
}
#[derive(Default)]
pub struct Progress {
    pub entries: AtomicU64,
    pub allocated: AtomicU64,
    pub directories: AtomicU64,
}
#[derive(Debug)]
pub struct Scan {
    pub nodes: Vec<Node>,
    pub errors: u64,
    pub skipped_links: u64,
    pub hardlinks: u64,
    pub unknown: u64,
    pub partial: bool,
    pub cancelled: bool,
    pub elapsed: Duration,
}
#[derive(Clone, Copy)]
pub struct Limits {
    pub entries: usize,
    pub timeout: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            entries: 500_000,
            timeout: Duration::from_secs(300),
        }
    }
}
#[derive(Debug)]
struct Info {
    identity: (u64, u128),
    allocated: u64,
    links: u32,
}
#[cfg(windows)]
fn info(path: &Path) -> Result<Info, String> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{
            FILE_ID_INFO, FILE_STANDARD_INFO, FileIdInfo, FileStandardInfo,
            GetFileInformationByHandleEx,
        },
    };
    let file = std::fs::OpenOptions::new()
        .access_mode(0x80)
        .share_mode(7)
        .custom_flags(0x02000000)
        .open(path)
        .map_err(|e| e.to_string())?;
    let mut standard = FILE_STANDARD_INFO::default();
    let mut id = FILE_ID_INFO::default();
    let h = HANDLE(file.as_raw_handle());
    unsafe {
        GetFileInformationByHandleEx(
            h,
            FileStandardInfo,
            &mut standard as *mut _ as *mut _,
            std::mem::size_of_val(&standard) as u32,
        )
        .map_err(|e| e.to_string())?;
        GetFileInformationByHandleEx(
            h,
            FileIdInfo,
            &mut id as *mut _ as *mut _,
            std::mem::size_of_val(&id) as u32,
        )
        .map_err(|e| e.to_string())?;
    }
    // AllocationSize is the reservation size for sparse/compressed NTFS files, not
    // necessarily their disk cost. Query the filesystem's actual storage bytes.
    let allocated = if std::fs::symlink_metadata(path)
        .map_err(|e| e.to_string())?
        .is_dir()
    {
        standard.AllocationSize.max(0) as u64
    } else {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::{
            Foundation::{GetLastError, SetLastError, WIN32_ERROR},
            Storage::FileSystem::GetCompressedFileSizeW,
        };
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut high = 0;
        unsafe {
            SetLastError(WIN32_ERROR(0));
            let low = GetCompressedFileSizeW(windows::core::PCWSTR(wide.as_ptr()), Some(&mut high));
            if low == u32::MAX {
                let e = GetLastError();
                if e.0 != 0 {
                    return Err(format!("读取实际占用失败：{}", e.0));
                }
            }
            ((high as u64) << 32) | low as u64
        }
    };
    Ok(Info {
        identity: (
            id.VolumeSerialNumber,
            u128::from_le_bytes(id.FileId.Identifier),
        ),
        allocated,
        links: standard.NumberOfLinks,
    })
}
#[cfg(unix)]
fn info(path: &Path) -> Result<Info, String> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    Ok(Info {
        identity: (m.dev(), m.ino() as u128),
        allocated: m.blocks() * 512,
        links: m.nlink() as u32,
    })
}
#[cfg(not(any(unix, windows)))]
fn info(_: &Path) -> Result<Info, String> {
    Err("平台未实现实际占用读取".into())
}
fn boundary(m: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        return m.file_attributes() & (0x400 | 0x1000 | 0x40000 | 0x400000) != 0;
    }
    #[cfg(not(windows))]
    {
        m.is_symlink()
    }
}
pub fn scan(
    root: &Path,
    cancel: Arc<AtomicBool>,
    progress: Arc<Progress>,
    limits: Limits,
) -> Result<Scan, String> {
    let start = Instant::now();
    if boundary(&std::fs::symlink_metadata(root).map_err(|e| e.to_string())?) {
        return Err("不跟随扫描根的重解析点".into());
    }
    let root = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
    let meta = std::fs::symlink_metadata(&root).map_err(|e| e.to_string())?;
    if !meta.is_dir() || boundary(&meta) {
        return Err("扫描根必须是本地普通目录；不跟随重解析/占位文件或挂载入口".into());
    }
    let volume = info(&root)?.identity.0;
    let mut result = Scan {
        nodes: vec![],
        errors: 0,
        skipped_links: 0,
        hardlinks: 0,
        unknown: 0,
        partial: false,
        cancelled: false,
        elapsed: Duration::ZERO,
    };
    let mut stack = vec![(root, None)];
    let mut seen = HashSet::new();
    while let Some((path, parent)) = stack.pop() {
        if cancel.load(Ordering::Acquire) {
            result.partial = true;
            result.cancelled = true;
            break;
        }
        if result.nodes.len() >= limits.entries || start.elapsed() > limits.timeout {
            result.partial = true;
            break;
        }
        let m = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => {
                result.errors += 1;
                result.partial = true;
                continue;
            }
        };
        if boundary(&m) {
            result.skipped_links += 1;
            result.partial = true;
            continue;
        }
        let mut allocated = 0;
        let mut duplicate = false;
        let mut uncertain = false;
        match info(&path) {
            Ok(i) => {
                if i.identity.0 != volume {
                    result.skipped_links += 1;
                    result.partial = true;
                    continue;
                }
                if !m.is_dir() && i.links > 1 && !seen.insert(i.identity) {
                    duplicate = true;
                    result.hardlinks += 1;
                } else {
                    allocated = i.allocated;
                }
            }
            Err(_) => {
                result.unknown += 1;
                result.partial = true;
                uncertain = true;
            }
        }
        let index = result.nodes.len();
        result.nodes.push(Node {
            path: path.clone(),
            parent,
            children: vec![],
            entries: 1,
            directory: m.is_dir(),
            logical: if m.is_file() { m.len() } else { 0 },
            mtime: m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|t| t.as_secs())
                .unwrap_or(0),
            allocated,
            hardlink_duplicate: duplicate,
            uncertain,
        });
        if let Some(p) = parent {
            result.nodes[p].children.push(index);
        }
        progress.entries.fetch_add(1, Ordering::Relaxed);
        progress.allocated.fetch_add(allocated, Ordering::Relaxed);
        if m.is_dir() {
            progress.directories.fetch_add(1, Ordering::Relaxed);
            match std::fs::read_dir(&path) {
                Ok(rd) => {
                    let mut children = Vec::new();
                    for e in rd {
                        if cancel.load(Ordering::Relaxed) || start.elapsed() > limits.timeout {
                            result.partial = true;
                            result.cancelled = cancel.load(Ordering::Relaxed);
                            break;
                        }
                        match e {
                            Ok(e) => children.push(e.path()),
                            Err(_) => {
                                result.errors += 1;
                                result.partial = true;
                            }
                        }
                        if children.len() > limits.entries {
                            result.partial = true;
                            break;
                        }
                    }
                    children.sort();
                    for p in children.into_iter().rev() {
                        stack.push((p, Some(index)));
                    }
                }
                Err(_) => {
                    result.errors += 1;
                    result.partial = true;
                }
            }
        }
    }
    // Postorder aggregation: every child's allocated size is credited once only.
    for i in (0..result.nodes.len()).rev() {
        if let Some(p) = result.nodes[i].parent {
            let logical = result.nodes[i].logical;
            let occupied = result.nodes[i].allocated;
            let entries = result.nodes[i].entries;
            result.nodes[p].entries = result.nodes[p].entries.saturating_add(entries);
            result.nodes[p].logical = result.nodes[p].logical.saturating_add(logical);
            result.nodes[p].allocated = result.nodes[p].allocated.saturating_add(occupied);
        }
    }
    result.elapsed = start.elapsed();
    if result.nodes.is_empty() {
        return Err("扫描未读取任何目录（可能已取消）".into());
    }
    Ok(result)
}
#[derive(Clone, Copy, Debug)]
pub struct Tile {
    pub index: usize,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}
/// Balanced slice-and-dice layout, independent of GUI crates. Tile areas equal byte shares.
pub fn tiles(weights: &[(usize, u64)]) -> Vec<Tile> {
    fn split(v: &[(usize, u64)], x: f32, y: f32, w: f32, h: f32, out: &mut Vec<Tile>) {
        if v.is_empty() {
            return;
        }
        if v.len() == 1 {
            out.push(Tile {
                index: v[0].0,
                x,
                y,
                w,
                h,
            });
            return;
        }
        let total = v.iter().map(|v| v.1 as f64).sum::<f64>();
        let mut left = 0.;
        let mut cut = 1;
        for (i, v) in v.iter().enumerate().take(v.len() - 1) {
            left += v.1 as f64;
            cut = i + 1;
            if left >= total / 2. {
                break;
            }
        }
        let ratio = (left / total) as f32;
        if w >= h {
            split(&v[..cut], x, y, w * ratio, h, out);
            split(&v[cut..], x + w * ratio, y, w * (1. - ratio), h, out);
        } else {
            split(&v[..cut], x, y, w, h * ratio, out);
            split(&v[cut..], x, y + h * ratio, w, h * (1. - ratio), out);
        }
    }
    let mut v = weights
        .iter()
        .copied()
        .filter(|(_, w)| *w > 0)
        .collect::<Vec<_>>();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    let mut out = vec![];
    split(&v, 0., 0., 1., 1., &mut out);
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn treemap_area_matches_share() {
        let t = tiles(&[(0, 10), (1, 30), (2, 60)]);
        for tile in t {
            let expected = [0.1, 0.3, 0.6][tile.index];
            assert!((tile.w * tile.h - expected).abs() < 0.0001);
            assert!(tile.x + tile.w <= 1.0001 && tile.y + tile.h <= 1.0001);
        }
    }
    #[test]
    fn real_hardlinks_are_counted_once() {
        let p = std::env::temp_dir().join(format!("findx-du-fixture-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir(&p).unwrap();
        std::fs::write(p.join("one"), vec![7u8; 8192]).unwrap();
        std::fs::hard_link(p.join("one"), p.join("two")).unwrap();
        let s = scan(
            &p,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Progress::default()),
            Limits::default(),
        )
        .unwrap();
        assert_eq!(s.hardlinks, 1);
        assert_eq!(s.nodes[0].logical, 16384);
        assert_eq!(s.nodes[0].entries, 3);
        let f: Vec<_> = s.nodes.iter().filter(|n| !n.directory).collect();
        assert_eq!(f.iter().filter(|n| n.hardlink_duplicate).count(), 1);
        assert!(f.iter().map(|n| n.allocated).sum::<u64>() >= 8192);
        std::fs::remove_dir_all(p).unwrap();
    }
    #[test]
    fn limit_is_reported_as_partial() {
        let s = scan(
            &std::env::temp_dir(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(Progress::default()),
            Limits {
                entries: 1,
                timeout: Duration::from_secs(1),
            },
        )
        .unwrap();
        assert!(s.partial);
        assert_eq!(s.nodes.len(), 1);
    }
    #[cfg(windows)]
    #[test]
    fn sparse_storage_is_not_reserved_logical_allocation() {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{Foundation::HANDLE, System::IO::DeviceIoControl};
        let dir = std::env::temp_dir().join(format!("findx-native-sparse-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sparse.bin");
        let f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        let mut returned = 0;
        unsafe {
            DeviceIoControl(
                HANDLE(f.as_raw_handle()),
                0x900C4,
                None,
                0,
                None,
                0,
                Some(&mut returned),
                None,
            )
            .unwrap();
        }
        f.set_len(16 * 1024 * 1024).unwrap();
        drop(f);
        let i = info(&path).unwrap();
        assert!(
            i.allocated < 16 * 1024 * 1024,
            "sparse reservation was wrongly counted as disk cost"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn inflight_cancel_keeps_a_real_partial_result() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("findx-du-midcancel-{}-{stamp}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        for i in 0..1000 {
            std::fs::write(root.join(format!("entry-{i:04}.bin")), [7u8; 4096]).unwrap();
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(Progress::default());
        let (c, p, worker_root) = (cancel.clone(), progress.clone(), root.clone());
        let worker = std::thread::spawn(move || scan(&worker_root, c, p, Limits::default()));
        while progress.entries.load(Ordering::Relaxed) < 8 && !worker.is_finished() {
            std::thread::yield_now();
        }
        cancel.store(true, Ordering::Relaxed);
        let result = worker.join().unwrap().unwrap();
        std::fs::remove_dir_all(root).unwrap();
        assert!(result.cancelled && result.partial);
        assert!(result.nodes.len() > 1 && result.nodes.len() < 1001);
        assert!(result.nodes[0].allocated > 0);
        assert_eq!(result.nodes[0].entries, result.nodes.len() as u64);
    }
    #[test]
    fn cancelled_scan_does_not_claim_complete() {
        let c = Arc::new(AtomicBool::new(true));
        let result = scan(
            &std::env::temp_dir(),
            c,
            Arc::new(Progress::default()),
            Limits::default(),
        );
        assert!(result.is_err() || result.as_ref().is_ok_and(|s| s.partial && s.cancelled));
    }
}
