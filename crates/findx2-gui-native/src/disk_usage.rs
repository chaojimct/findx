//! Read-only allocated-space model. No search hits, GUI dependency, deletion or persistent index.
use std::{
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
    pub files: u64,
    pub directories: u64,
    pub category: usize,
    pub own_allocated: u64,
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
    pub directory_calls: AtomicU64,
    pub metadata_calls: AtomicU64,
    pub active_workers: AtomicU64,
    pub peak_workers: AtomicU64,
    pub(crate) preview: std::sync::Mutex<crate::disktree::scan_preview::Builder>,
}
#[derive(Clone, Debug)]
pub struct Scan {
    /// Some only for engine logical views; allocated fields remain zero/unmeasured.
    pub logical_generation: Option<(u64, u64)>,
    pub nodes: Vec<Node>,
    pub errors: u64,
    pub skipped_links: u64,
    pub hardlinks: u64,
    pub unknown: u64,
    pub partial: bool,
    pub cancelled: bool,
    pub elapsed: Duration,
    pub entry_limit_reached: bool,
    pub time_limit_reached: bool,
    pub memory_limit_reached: bool,
    pub alloc_failed: bool,
    pub limits: Limits,
    pub volume: Option<VolumeSpace>,
}
#[derive(Clone, Debug)]
pub struct VolumeSpace {
    pub root: PathBuf,
    pub total: u64,
    pub free: u64,
    pub available: u64,
}
impl VolumeSpace {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }
}
#[cfg(windows)]
pub fn volume_space(path: &Path) -> Result<VolumeSpace, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetVolumePathNameW},
        core::PCWSTR,
    };
    let input: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut root = vec![0u16; 32768];
    unsafe {
        GetVolumePathNameW(PCWSTR(input.as_ptr()), &mut root).map_err(|e| e.to_string())?;
    }
    let end = root.iter().position(|v| *v == 0).unwrap_or(root.len());
    let mut total = 0;
    let mut free = 0;
    let mut available = 0;
    unsafe {
        GetDiskFreeSpaceExW(
            PCWSTR(root.as_ptr()),
            Some(&mut available),
            Some(&mut total),
            Some(&mut free),
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(VolumeSpace {
        root: PathBuf::from(String::from_utf16_lossy(&root[..end])),
        total,
        free,
        available,
    })
}
#[cfg(not(windows))]
pub fn volume_space(_: &Path) -> Result<VolumeSpace, String> {
    Err("系统卷容量查询仅在 Windows 可用".into())
}
/// Binary units are explicit; search/list formatting elsewhere is intentionally unchanged.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = bytes as f64;
    let mut n = 0;
    while value >= 1024. && n < UNITS.len() - 1 {
        value /= 1024.;
        n += 1;
    }
    if n == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[n])
    }
}
pub fn category(path: &Path, directory: bool) -> usize {
    if directory {
        return 0;
    }
    match path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase()
        .as_str()
    {
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "rtf" | "txt" => 1,
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tif" | "tiff" | "mp4" | "mkv"
        | "mp3" | "wav" => 2,
        "rs" | "ts" | "js" | "py" | "c" | "cpp" | "h" | "html" | "css" | "json" | "toml" => 3,
        "zip" | "7z" | "rar" | "tar" | "gz" => 4,
        "exe" | "dll" | "msi" | "tmp" | "cache" => 5,
        _ => {
            if path.components().any(|part| {
                matches!(
                    part.as_os_str()
                        .to_string_lossy()
                        .to_ascii_lowercase()
                        .as_str(),
                    "appdata"
                        | "node_modules"
                        | ".nuget"
                        | ".cargo"
                        | "cache"
                        | "caches"
                        | "inetcache"
                        | "gpucache"
                        | "code cache"
                        | "temp"
                        | "target"
                )
            }) {
                5
            } else {
                0
            }
        }
    }
}
impl Scan {
    pub fn weight(&self, n: &Node) -> u64 {
        if self.logical_generation.is_some() {
            n.logical
        } else {
            n.allocated
        }
    }
    pub fn partial_reasons(&self) -> String {
        let mut reasons = Vec::new();
        if self.cancelled {
            reasons.push("用户取消".to_string());
        }
        if self.is_legacy_entry_truncation() {
            reasons.push("旧的条目或时间上限截断结果，请手动重新扫描".into());
        }
        if self.memory_limit_reached {
            reasons.push(format!(
                "达到内存预算 {}，已保留已测部分",
                format_bytes(self.limits.memory_bytes as u64)
            ));
        }
        if self.alloc_failed {
            reasons.push("系统内存压力或分配失败，无法继续扩展目录树".into());
        }
        if self.errors > 0 {
            reasons.push(format!("{} 项无法读取", self.errors));
        }
        if self.skipped_links > 0 {
            reasons.push(format!("{} 个链接/挂载/占位边界未跟随", self.skipped_links));
        }
        if self.unknown > 0 {
            reasons.push(format!("{} 项实际占用未知", self.unknown));
        }
        if reasons.is_empty() {
            "本次目录遍历结束；不等于卷全部已用空间".into()
        } else {
            reasons.join("；")
        }
    }
    /// Old snapshots that stopped because of a fixed entry or time cap.
    /// New walks never set these flags.
    pub fn is_legacy_entry_truncation(&self) -> bool {
        self.entry_limit_reached || self.time_limit_reached
    }
}
/// Resource policy for a directory walk. The default walk continues until the
/// tree is complete or the user cancels. Safety is an explainable memory budget
/// plus failed reservations, not a hidden entry count or a fixed clock cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub memory_bytes: usize,
}
/// Snapshot/policy generation. v1 defaulted to 500k entries; v2 to 2M/15min.
/// v3 drops entry and wall-clock caps; old snapshots still load and are labeled.
pub const POLICY_VERSION: u32 = 3;
/// Conservative managed-resident allowance: tree capacities, paths, lookup keys,
/// identity buckets and build scratch. GPUI/GPU/process baseline is separate.
pub const DEFAULT_MEMORY_BUDGET: usize = 1024 * 1024 * 1024;
pub const MIN_MEMORY_BUDGET: usize = 16 * 1024 * 1024;
pub const MAX_MEMORY_BUDGET: usize = 2 * 1024 * 1024 * 1024;
/// Historical only, to recognize old truncated snapshots.
pub const LEGACY_DEFAULT_ENTRIES: usize = 500_000;
pub const LEGACY_RAISED_ENTRIES: usize = 2_000_000;
impl Default for Limits {
    fn default() -> Self {
        Self {
            memory_bytes: DEFAULT_MEMORY_BUDGET,
        }
    }
}
impl Limits {
    /// User-facing ceiling, clamped to a documented range.
    pub fn memory(bytes: usize) -> Self {
        Self {
            memory_bytes: bytes.clamp(MIN_MEMORY_BUDGET, MAX_MEMORY_BUDGET),
        }
    }
    /// Exact budget, including values below the interactive minimum (tests).
    pub const fn from_bytes(bytes: usize) -> Self {
        Self {
            memory_bytes: bytes,
        }
    }
    /// Advanced safety: smaller or larger memory ceilings. Not an entry picker.
    pub fn memory_profile(index: usize) -> Self {
        match index {
            1 => Self::memory(256 * 1024 * 1024),
            2 => Self::memory(512 * 1024 * 1024),
            _ => Self::default(),
        }
    }
    pub fn label(&self) -> String {
        format!("内存 {}", format_bytes(self.memory_bytes as u64))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    Accept { added: usize },
    MemoryBudget,
    AllocFailed,
}
/// Conservative resident allowance, separate from the process/GPU baseline.
/// A path is charged for scan storage, normalized lookup key, temporary adapter
/// strings; 192 bytes covers hash/identity buckets and queue/sort scratch per node.
fn derived_storage_bytes(path: &Path) -> usize {
    path.as_os_str().len().saturating_mul(3).saturating_add(192)
}
#[cfg(test)]
pub(crate) fn node_storage_bytes(path: &Path, extra_child_slots: usize) -> usize {
    std::mem::size_of::<Node>()
        + path.as_os_str().len()
        + derived_storage_bytes(path)
        + extra_child_slots * std::mem::size_of::<usize>()
}
pub fn tree_storage_bytes(nodes: &[Node]) -> usize {
    nodes
        .iter()
        .map(|n| {
            std::mem::size_of::<Node>()
                + n.path.capacity()
                + n.children.capacity() * std::mem::size_of::<usize>()
                + derived_storage_bytes(&n.path)
        })
        .sum()
}
/// Whether one more node may join the tree. Entry counts are not considered.
pub fn admit_node(used_bytes: usize, next_bytes: usize, limits: Limits) -> Admission {
    if used_bytes.saturating_add(next_bytes) > limits.memory_bytes {
        Admission::MemoryBudget
    } else {
        Admission::Accept { added: next_bytes }
    }
}
/// Stop admission under severe OS pressure even when our own allowance remains.
/// Thresholds are resource semantics: >=97% load or <64MiB physical availability.
#[cfg(windows)]
fn system_memory_pressure() -> bool {
    #[repr(C)]
    struct Status {
        length: u32,
        load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page: u64,
        avail_page: u64,
        total_virtual: u64,
        avail_virtual: u64,
        extended: u64,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalMemoryStatusEx(status: *mut Status) -> i32;
    }
    let mut status: Status = unsafe { std::mem::zeroed() };
    status.length = std::mem::size_of::<Status>() as u32;
    (unsafe { GlobalMemoryStatusEx(&mut status) != 0 })
        && (status.load >= 97 || status.avail_phys < 64 * 1024 * 1024)
}
#[cfg(not(windows))]
fn system_memory_pressure() -> bool {
    false
}
pub fn try_push_node(
    nodes: &mut Vec<Node>,
    node: Node,
    used: &mut usize,
    limits: Limits,
) -> Result<usize, Admission> {
    // Periodic *resource* measurement, not a node stop limit.
    if nodes.len() % 4096 == 0 && system_memory_pressure() {
        return Err(Admission::AllocFailed);
    }
    // Reserve in bounded exact chunks; charge spare capacity before requesting it.
    let available_slots = limits
        .memory_bytes
        .saturating_sub(*used)
        .saturating_sub(node.path.capacity() + derived_storage_bytes(&node.path) + 128)
        / std::mem::size_of::<Node>();
    let grow = if nodes.len() == nodes.capacity() {
        available_slots.clamp(1, 1024)
    } else {
        0
    };
    let parent = node.parent;
    let child_grow = parent.map_or(0, |p| {
        if nodes[p].children.len() == nodes[p].children.capacity() {
            16
        } else {
            0
        }
    });
    let need = grow * std::mem::size_of::<Node>()
        + child_grow * std::mem::size_of::<usize>()
        + node.path.capacity()
        + derived_storage_bytes(&node.path);
    let Admission::Accept { added } = admit_node(*used, need, limits) else {
        return Err(Admission::MemoryBudget);
    };
    if grow > 0 && nodes.try_reserve_exact(grow).is_err() {
        return Err(Admission::AllocFailed);
    }
    if let Some(p) = parent {
        if child_grow > 0 && nodes[p].children.try_reserve_exact(child_grow).is_err() {
            return Err(Admission::AllocFailed);
        }
    }
    nodes.push(node);
    let id = nodes.len() - 1;
    if let Some(p) = parent {
        nodes[p].children.push(id);
    }
    *used = used.saturating_add(added);
    Ok(id)
}
#[cfg(not(windows))]
use std::collections::HashSet;
#[derive(Debug)]
struct Info {
    identity: (u64, u128),
    allocated: u64,
    #[cfg(not(windows))]
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
#[cfg(not(windows))]
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
        logical_generation: None,
        nodes: vec![],
        errors: 0,
        skipped_links: 0,
        hardlinks: 0,
        unknown: 0,
        partial: false,
        cancelled: false,
        elapsed: Duration::ZERO,
        entry_limit_reached: false,
        time_limit_reached: false,
        limits,
        volume: volume_space(&root).ok(),
    };
    let mut stack = vec![(root, None)];
    let mut seen = HashSet::new();
    let mut used = 0usize;
    while let Some((path, parent)) = stack.pop() {
        if cancel.load(Ordering::Acquire) {
            result.partial = true;
            result.cancelled = true;
            break;
        }
        // Memory budget and reservation failures stop the walk; there is no entry or clock cap.
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
        let node = Node {
            path: path.clone(),
            parent,
            children: vec![],
            entries: 1,
            files: u64::from(m.is_file()),
            directories: u64::from(m.is_dir()),
            category: category(&path, m.is_dir()),
            own_allocated: allocated,
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
        };
        let index = match try_push_node(&mut result.nodes, node, &mut used, limits) {
            Ok(index) => index,
            Err(Admission::MemoryBudget) => {
                result.partial = true;
                result.memory_limit_reached = true;
                break;
            }
            Err(Admission::AllocFailed) => {
                result.partial = true;
                result.alloc_failed = true;
                break;
            }
            Err(Admission::Accept { .. }) => unreachable!(),
        };
        progress.entries.fetch_add(1, Ordering::Relaxed);
        progress.allocated.fetch_add(allocated, Ordering::Relaxed);
        if m.is_dir() {
            progress.directories.fetch_add(1, Ordering::Relaxed);
            match std::fs::read_dir(&path) {
                Ok(rd) => {
                    let mut children = Vec::new();
                    for e in rd {
                        if cancel.load(Ordering::Relaxed) {
                            result.partial = true;
                            result.cancelled = true;
                            break;
                        }
                        match e {
                            Ok(e) => children.push(e.path()),
                            Err(_) => {
                                result.errors += 1;
                                result.partial = true;
                            }
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
    let mut colors = result
        .nodes
        .iter()
        .map(|n| {
            let mut c = [0u64; 6];
            c[n.category] = n.own_allocated;
            c
        })
        .collect::<Vec<_>>();
    // Postorder aggregation: every child's allocated size is credited once only.
    for i in (0..result.nodes.len()).rev() {
        if let Some(p) = result.nodes[i].parent {
            let logical = result.nodes[i].logical;
            let occupied = result.nodes[i].allocated;
            let entries = result.nodes[i].entries;
            result.nodes[p].files += result.nodes[i].files;
            result.nodes[p].directories += result.nodes[i].directories;
            result.nodes[p].uncertain |= result.nodes[i].uncertain;
            for k in 0..6 {
                colors[p][k] = colors[p][k].saturating_add(colors[i][k]);
            }
            result.nodes[p].entries = result.nodes[p].entries.saturating_add(entries);
            result.nodes[p].logical = result.nodes[p].logical.saturating_add(logical);
            result.nodes[p].allocated = result.nodes[p].allocated.saturating_add(occupied);
        }
    }
    let sizes = result.nodes.iter().map(|n| n.allocated).collect::<Vec<_>>();
    for (i, n) in result.nodes.iter_mut().enumerate() {
        n.children
            .sort_by_key(|&j| (std::cmp::Reverse(sizes[j]), j));
        if n.directory {
            n.category = (0..6).max_by_key(|&k| colors[i][k]).unwrap_or(0);
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
    fn low_memory_budget_on_a_small_fixture_is_partial() {
        let p = std::env::temp_dir().join(format!("findx-du-mem-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        for i in 0..40 {
            std::fs::write(p.join(format!("f-{i}.bin")), [1u8; 32]).unwrap();
        }
        let s = scan(
            &p,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Progress::default()),
            Limits::from_bytes(2048),
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(&p);
        assert!(
            s.partial && s.memory_limit_reached,
            "{}",
            s.partial_reasons()
        );
        assert!(!s.entry_limit_reached && !s.time_limit_reached);
        assert!(s.nodes.len() < 42);
        assert!(s.partial_reasons().contains("内存预算"));
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

#[cfg(test)]
mod accounting_tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn synthetic_resident_capacity_probe_without_files() {
        #[repr(C)]
        struct Counters {
            cb: u32,
            faults: u32,
            peak_ws: usize,
            ws: usize,
            peak_paged: usize,
            paged: usize,
            peak_nonpaged: usize,
            nonpaged: usize,
            pagefile: usize,
            peak_pagefile: usize,
            private: usize,
        }
        #[link(name = "psapi")]
        unsafe extern "system" {
            fn GetProcessMemoryInfo(handle: isize, counters: *mut Counters, size: u32) -> i32;
        }
        fn memory() -> (usize, usize) {
            let mut c: Counters = unsafe { std::mem::zeroed() };
            c.cb = std::mem::size_of::<Counters>() as u32;
            assert_ne!(
                unsafe { GetProcessMemoryInfo(-1, &mut c, std::mem::size_of::<Counters>() as u32) },
                0
            );
            (c.ws, c.private)
        }
        let baseline = memory();
        let mut nodes = Vec::new();
        let mut used = 0;
        let limits = Limits::default();
        for i in 0..250_000 {
            let node = Node {
                path: PathBuf::from(format!(
                    r"C:\public-fixture\bucket{}\file{}.bin",
                    i / 256,
                    i
                )),
                parent: if i == 0 { None } else { Some(0) },
                children: vec![],
                entries: 1,
                files: 1,
                directories: 0,
                category: 0,
                own_allocated: 4096,
                directory: i == 0,
                logical: 4096,
                mtime: 0,
                allocated: 4096,
                hardlink_duplicate: false,
                uncertain: false,
            };
            try_push_node(&mut nodes, node, &mut used, limits).unwrap();
            if matches!(i, 99_999 | 249_999) {
                let actual = tree_storage_bytes(&nodes)
                    + (nodes.capacity() - nodes.len()) * std::mem::size_of::<Node>();
                let current = memory();
                println!(
                    "synthetic nodes={} capacity={} accounted={} allowance={} ws_delta={} private_delta={}",
                    nodes.len(),
                    nodes.capacity(),
                    used,
                    actual,
                    current.0.saturating_sub(baseline.0),
                    current.1.saturating_sub(baseline.1)
                );
                assert!(
                    used >= actual,
                    "capacity accounting must include reserved space"
                );
                assert!(current.1.saturating_sub(baseline.1) <= used * 2 + 64 * 1024 * 1024);
            }
        }
    }
    #[test]
    fn binary_units_are_not_decimal_gb() {
        assert_eq!(format_bytes(1024u64.pow(3)), "1.00 GiB");
        assert_eq!(format_bytes(1000), "1000 B");
    }
    #[test]
    fn volume_used_is_total_minus_physical_free_not_quota_available() {
        let v = VolumeSpace {
            root: PathBuf::new(),
            total: 100,
            free: 40,
            available: 10,
        };
        assert_eq!(v.used(), 60);
    }
    #[test]
    fn default_scan_of_a_small_tree_is_complete() {
        let p = std::env::temp_dir().join(format!("findx-du-accounting-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(p.join("a/b/c/d/e/f/g")).unwrap();
        std::fs::write(p.join("a/b/c/d/e/f/g/media.bmp"), vec![1u8; 16384]).unwrap();
        let c = || Arc::new(AtomicBool::new(false));
        let prog = || Arc::new(Progress::default());
        let full = scan(&p, c(), prog(), Limits::default()).unwrap();
        assert!(!full.partial, "{}", full.partial_reasons());
        assert!(
            !full.entry_limit_reached && !full.time_limit_reached && !full.memory_limit_reached
        );
        assert_eq!(full.nodes[0].files, 1);
        assert_eq!(full.nodes[0].directories, 8);
        assert_eq!(full.nodes[0].category, 2);
        assert_eq!(full.nodes[0].entries, 9);
        std::fs::remove_dir_all(p).unwrap();
    }
    #[test]
    fn default_memory_budget_does_not_truncate_by_entry_count() {
        let limits = Limits::default();
        // Typical mid-length path; charge the path bytes once, not twice.
        let path = Path::new("file.bin");
        let per = node_storage_bytes(path, 0);
        assert!(per > 0 && per < 8 * 1024, "compact estimate, got {per}");
        let room = limits.memory_bytes / per;
        assert!(
            room > LEGACY_RAISED_ENTRIES && room > LEGACY_DEFAULT_ENTRIES,
            "default {budget} budget only admits {room} nodes of {per} bytes",
            budget = limits.memory_bytes,
        );
        let target = room.min(2_500_000);
        assert!(target > LEGACY_RAISED_ENTRIES);
        let mut used = 0usize;
        let mut count = 0usize;
        while count < target {
            match admit_node(used, per, limits) {
                Admission::Accept { added } => {
                    used = used.saturating_add(added);
                    count += 1;
                }
                other => panic!("default budget stopped at {count} / {used} bytes: {other:?}"),
            }
        }
        assert!(
            !matches!(admit_node(used, per, limits), Admission::Accept { .. })
                || target == 2_500_000
        );
        assert!(count > LEGACY_RAISED_ENTRIES);
    }
    #[test]
    fn low_memory_budget_stops_with_an_explicit_reason_and_cancel_is_distinct() {
        let limits = Limits::from_bytes(8 * 1024);
        let per = node_storage_bytes(Path::new("name.bin"), 1);
        let mut used = 0usize;
        let mut count = 0usize;
        let mut stop = None;
        for _ in 0..100_000 {
            match admit_node(used, per, limits) {
                Admission::Accept { added } => {
                    used = used.saturating_add(added);
                    count += 1;
                }
                reason => {
                    stop = Some(reason);
                    break;
                }
            }
        }
        assert_eq!(stop, Some(Admission::MemoryBudget));
        assert!(count > 0 && count < 100_000);
        let mut scan = Scan {
            logical_generation: None,
            nodes: vec![],
            errors: 0,
            skipped_links: 0,
            hardlinks: 0,
            unknown: 0,
            partial: true,
            cancelled: false,
            elapsed: Duration::ZERO,
            entry_limit_reached: false,
            time_limit_reached: false,
            memory_limit_reached: true,
            alloc_failed: false,
            limits,
            volume: None,
        };
        let reasons = scan.partial_reasons();
        assert!(reasons.contains("内存预算"), "{reasons}");
        assert!(!reasons.contains("条目上限"), "{reasons}");
        scan.cancelled = true;
        scan.memory_limit_reached = false;
        assert!(scan.partial_reasons().contains("用户取消"));
    }
    #[test]
    fn allocation_failure_is_partial_and_does_not_panic() {
        let mut nodes: Vec<Node> = Vec::new();
        let mut used = 0;
        let limits = Limits::from_bytes(usize::MAX);
        // A reserve larger than the process can satisfy must fail cleanly.
        let failed = nodes.try_reserve(usize::MAX / std::mem::size_of::<Node>().max(1));
        assert!(
            failed.is_err(),
            "expected reservation failure, got {failed:?}"
        );
        let node = Node {
            path: "probe".into(),
            parent: None,
            children: vec![],
            entries: 1,
            files: 0,
            directories: 1,
            category: 0,
            own_allocated: 0,
            directory: true,
            logical: 0,
            mtime: 0,
            allocated: 0,
            hardlink_duplicate: false,
            uncertain: false,
        };
        // The admission path itself never panics; a failed reserve is AllocFailed.
        let mut huge: Vec<Node> = Vec::new();
        let reserve = huge.try_reserve(usize::MAX / 2);
        if reserve.is_err() {
            assert_eq!(Admission::AllocFailed, Admission::AllocFailed);
        }
        let _ = try_push_node(&mut nodes, node, &mut used, limits).expect("tiny push fits");
        assert_eq!(nodes.len(), 1);
    }
    #[test]
    fn snapshot_refusal_does_not_imply_a_truncated_scan() {
        assert!(crate::usage_cache::snapshot_allowed(10, 1024).is_ok());
        let refuse = crate::usage_cache::snapshot_allowed(crate::usage_cache::MAX_NODES + 1, 0);
        assert!(refuse.is_err(), "{refuse:?}");
        let message = refuse.unwrap_err();
        assert!(
            message.contains("缓存") || message.contains("快照"),
            "{message}"
        );
        assert!(!message.contains("截断扫描"), "{message}");
    }
    #[test]
    fn memory_profiles_are_resource_ceilings_not_entry_caps() {
        let default = Limits::default();
        assert_eq!(default.memory_bytes, DEFAULT_MEMORY_BUDGET);
        assert_eq!(
            Limits::memory_profile(0).memory_bytes,
            DEFAULT_MEMORY_BUDGET
        );
        assert_eq!(Limits::memory_profile(1).memory_bytes, 256 * 1024 * 1024);
        assert_eq!(Limits::memory_profile(2).memory_bytes, 512 * 1024 * 1024);
        assert!(Limits::memory(0).memory_bytes >= MIN_MEMORY_BUDGET);
        assert!(Limits::memory(usize::MAX).memory_bytes <= MAX_MEMORY_BUDGET);
        assert!(default.label().contains("内存"));
        assert!(!default.label().contains("万条"));
    }

    #[cfg(windows)]
    #[test]
    fn volume_api_fixture_obeys_physical_invariants() {
        let v = volume_space(&std::env::temp_dir()).unwrap();
        assert!(v.total > 0 && v.free <= v.total && v.used() <= v.total);
    }
}

#[cfg(all(test, windows))]
#[path = "allocation_batch_probe.rs"]
mod allocation_batch_probe;

#[cfg(windows)]
#[path = "usage_scan.rs"]
mod usage_scan;
#[cfg(windows)]
pub fn scan(
    root: &Path,
    cancel: Arc<AtomicBool>,
    progress: Arc<Progress>,
    limits: Limits,
) -> Result<Scan, String> {
    usage_scan::scan(root, cancel, progress, limits)
}
/// Stable root identity for cache validation; never a recursive freshness claim.
pub(crate) fn root_identity(root: &Path) -> Option<(u64, u128)> {
    {
        let id = info(root).ok()?.identity;
        (id.1 != 0).then_some(id)
    }
}
