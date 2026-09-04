//! Linux：本机挂载点上 `openat`/`read_dir` + inode/`st_dev` 建库。不跨挂载点，跳过伪文件系统。

use std::fs::{self, File};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use findx2_core::index::unix_secs_to_filetime;
use findx2_core::{progress, RawEntry, Result, VolumeScanner, WatchCursor};

const SKIP_FS: &[&str] = &[
    "proc", "sysfs", "devtmpfs", "devpts", "cgroup", "cgroup2", "tmpfs", "overlay",
    "squashfs", "nsfs", "autofs", "nfs", "nfs4", "fuse.portal", "rpc_pipefs", "debugfs",
    "tracefs", "securityfs", "pstore", "bpf", "configfs",
];

const SKIP_NAMES: &[&str] = &[".", "..", "lost+found", ".Trash", ".Trash-1000"];

pub fn should_skip_name(name: &str) -> bool {
    SKIP_NAMES.iter().any(|s| *s == name)
}

pub struct LinuxVolumeScanner;

impl VolumeScanner for LinuxVolumeScanner {
    fn scan_into(
        &self,
        volume: &str,
        out: &mut dyn FnMut(RawEntry) -> Result<()>,
    ) -> Result<WatchCursor> {
        let root = if volume.is_empty() {
            default_scan_root()
        } else {
            volume.to_string()
        };
        progress!("Linux 扫描：{} …", root);
        let mut n = 0u64;
        let meta = fs::metadata(&root).map_err(|e| {
            findx2_core::Error::Platform(format!("无法访问 {}: {e}", root))
        })?;
        let root_dev = meta.dev();
        walk(Path::new(&root), root_dev, None, out, &mut n)?;
        progress!("Linux 扫描完成：{} 条", n);
        Ok(WatchCursor {
            watch_gen: 1,
            watch_cursor: 0,
        })
    }
}

pub fn default_scan_root() -> String {
    if Path::new("/").is_dir() {
        "/".into()
    } else {
        std::env::var("HOME").unwrap_or_else(|_| "/".into())
    }
}

/// 本地可索引挂载点（跳过 /proc /sys /dev 等）。无特权时也可列出，扫描时会跳过无权目录。
pub fn default_scan_roots() -> Vec<String> {
    let mounts = parse_local_mounts();
    if mounts.is_empty() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home".into());
        return vec![home];
    }
    mounts
}

fn parse_local_mounts() -> Vec<String> {
    let Ok(text) = fs::read_to_string("/proc/self/mounts") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let _spec = it.next();
        let Some(dir) = it.next() else { continue };
        let Some(fstype) = it.next() else { continue };
        if SKIP_FS.iter().any(|s| *s == fstype) {
            continue;
        }
        if dir == "/proc" || dir == "/sys" || dir == "/dev" || dir == "/run" || dir.starts_with("/snap")
        {
            continue;
        }
        let path = unescape_mount(dir);
        if Path::new(&path).is_dir() {
            out.push(path);
        }
    }
    out.sort();
    out.dedup();
    out
}

fn unescape_mount(s: &str) -> String {
    s.replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

fn walk(
    path: &Path,
    root_dev: u64,
    parent_ino: Option<u64>,
    out: &mut dyn FnMut(RawEntry) -> Result<()>,
    n: &mut u64,
) -> Result<()> {
    let entries = match fs::read_dir(path) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };
    for ent in entries.flatten() {
        let name = ent.file_name();
        let name_s = name.to_string_lossy();
        if should_skip_name(&name_s) {
            continue;
        }
        let child = ent.path();
        let meta = match fs::symlink_metadata(&child) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.dev() != root_dev {
            continue;
        }
        let is_dir = meta.is_dir();
        let is_link = meta.file_type().is_symlink();
        let (mtime, ctime, size) = meta_times(&child, &meta);
        let file_id = meta.ino();
        let parent_id = parent_ino.unwrap_or(0);
        out(RawEntry {
            file_id,
            file_id_128: None,
            parent_id,
            name: name_s.into_owned(),
            size,
            mtime,
            ctime,
            attrs: if is_dir { 0x10 } else { 0 },
            is_dir,
        })?;
        *n += 1;
        if *n % 100_000 == 0 {
            progress!("Linux 扫描：已枚举 {} 条 …", n);
        }
        if is_dir && !is_link {
            walk(&child, root_dev, Some(file_id), out, n)?;
        }
    }
    Ok(())
}

fn meta_times(path: &Path, meta: &std::fs::Metadata) -> (u64, u64, u64) {
    if let Some(t) = statx_times(path) {
        return t;
    }
    (
        unix_secs_to_filetime(meta.mtime().max(0) as u32),
        unix_secs_to_filetime(meta.ctime().max(0) as u32),
        meta.size(),
    )
}

fn statx_times(path: &Path) -> Option<(u64, u64, u64)> {
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let c = CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut stx: libc::statx = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::statx(
                libc::AT_FDCWD,
                c.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
                libc::STATX_TYPE | libc::STATX_MODE | libc::STATX_SIZE | libc::STATX_MTIME | libc::STATX_BTIME | libc::STATX_INO,
                &mut stx,
            )
        };
        if rc != 0 {
            return None;
        }
        let mtime = unix_secs_to_filetime(stx.stx_mtime.tv_sec.max(0) as u32);
        let ctime = if stx.stx_mask & libc::STATX_BTIME != 0 {
            unix_secs_to_filetime(stx.stx_btime.tv_sec.max(0) as u32)
        } else {
            mtime
        };
        Some((mtime, ctime, stx.stx_size))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        None
    }
}

pub fn volume_id_for_path(path: &str) -> String {
    fs::metadata(path)
        .map(|m| format!("dev:{}", m.dev()))
        .unwrap_or_else(|_| path.to_string())
}

pub fn display_root_prefix(scan_root: &str) -> String {
    if scan_root.is_empty() || scan_root == "/" {
        "/".into()
    } else {
        scan_root.trim_end_matches('/').to_string()
    }
}

/// 给 fanotify 打开挂载点用。
#[allow(dead_code)]
pub fn open_dir_raw(path: &Path) -> std::io::Result<File> {
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY)
        .open(path)
}

#[allow(dead_code)]
pub fn raw_fd(f: &File) -> i32 {
    f.as_raw_fd()
}

#[allow(dead_code)]
pub fn path_buf(p: &str) -> PathBuf {
    PathBuf::from(p)
}
