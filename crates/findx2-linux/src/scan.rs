//! Linux：`getdents64` + `openat` 并行建库。
//!
//! 默认 **fast**：只收 inode / 名字 / 是否目录，不 `statx`（size/mtime=0，后台回填）。
//! `--full-stat` 时在同一 `dirfd` 上 `statx`/`fstatat`，不再对完整路径双重 stat。

use std::collections::VecDeque;
use std::ffi::CString;
use std::fs::{self, File};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use findx2_core::index::unix_secs_to_filetime;
use findx2_core::{progress, RawEntry, Result, VolumeScanner, WatchCursor};

const SKIP_FS: &[&str] = &[
    "proc", "sysfs", "devtmpfs", "devpts", "cgroup", "cgroup2", "tmpfs", "overlay",
    "squashfs", "nsfs", "autofs", "nfs", "nfs4", "fuse.portal", "rpc_pipefs", "debugfs",
    "tracefs", "securityfs", "pstore", "bpf", "configfs",
];

const SKIP_NAMES: &[&str] = &[".", "..", "lost+found", ".Trash", ".Trash-1000"];

const DT_UNKNOWN: u8 = 0;
const DT_DIR: u8 = 4;
const DT_LNK: u8 = 10;

const GETDENTS_BUF: usize = 256 * 1024;

pub fn should_skip_name(name: &str) -> bool {
    SKIP_NAMES.iter().any(|s| *s == name)
}

pub struct LinuxVolumeScanner {
    pub full_stat: bool,
    pub max_threads: usize,
}

impl Default for LinuxVolumeScanner {
    fn default() -> Self {
        Self {
            full_stat: false,
            max_threads: 0,
        }
    }
}

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
        let n = scan_parallel(&root, self.full_stat, self.max_threads, out)?;
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

struct DirJob {
    fd: i32,
    dir_ino: u64,
    root_dev: u64,
}

struct Queue {
    jobs: Mutex<VecDeque<DirJob>>,
    wait: Condvar,
    inflight: AtomicUsize,
}

fn scan_parallel(
    root: &str,
    full_stat: bool,
    max_threads: usize,
    out: &mut dyn FnMut(RawEntry) -> Result<()>,
) -> Result<u64> {
    let c_root = CString::new(root.as_bytes())
        .map_err(|_| findx2_core::Error::Platform("扫描根路径含 NUL".into()))?;
    let fd = unsafe { libc::open(c_root.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(findx2_core::Error::Platform(format!(
            "无法打开目录 {root}: {}",
            std::io::Error::last_os_error()
        )));
    }
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        unsafe { libc::close(fd) };
        return Err(findx2_core::Error::Platform(format!(
            "fstat {root}: {}",
            std::io::Error::last_os_error()
        )));
    }
    let root_dev = st.st_dev;
    let root_ino = st.st_ino;

    let threads = resolve_threads(max_threads);
    progress!("Linux 扫描：{} 线程，{}元数据", threads, if full_stat { "同步" } else { "fast（稍后回填）" });

    let q = Arc::new(Queue {
        jobs: Mutex::new(VecDeque::from([DirJob {
            fd,
            dir_ino: root_ino,
            root_dev,
        }])),
        wait: Condvar::new(),
        inflight: AtomicUsize::new(1),
    });
    let counted = Arc::new(AtomicU64::new(0));
    let shards: Arc<Mutex<Vec<Vec<RawEntry>>>> = Arc::new(Mutex::new(Vec::new()));

    std::thread::scope(|scope| {
        for _ in 0..threads {
            let q = Arc::clone(&q);
            let shards = Arc::clone(&shards);
            let counted = Arc::clone(&counted);
            scope.spawn(move || {
                let mut local = Vec::<RawEntry>::new();
                loop {
                    let job = {
                        let mut guard = q.jobs.lock().unwrap();
                        loop {
                            if let Some(j) = guard.pop_front() {
                                break j;
                            }
                            if q.inflight.load(Ordering::Acquire) == 0 {
                                q.wait.notify_all();
                                if let Ok(mut g) = shards.lock() {
                                    g.push(std::mem::take(&mut local));
                                }
                                return;
                            }
                            guard = q.wait.wait(guard).unwrap();
                        }
                    };
                    enumerate_dir(job, full_stat, &q, &mut local, &counted);
                    let left = q.inflight.fetch_sub(1, Ordering::AcqRel) - 1;
                    if left == 0 {
                        q.wait.notify_all();
                    }
                }
            });
        }
    });

    // 扫描根本身不在 getdents 结果里；入库后子项 parent_id 才能对上，回填才能拼出路径。
    out(RawEntry {
        file_id: root_ino,
        file_id_128: None,
        parent_id: 0,
        name: String::new(),
        size: 0,
        mtime: 0,
        ctime: 0,
        attrs: 0x10,
        is_dir: true,
    })?;
    let mut n = 1u64;
    let mut acc = shards.lock().unwrap();
    for batch in acc.drain(..) {
        for e in batch {
            out(e)?;
            n += 1;
        }
    }
    let _ = counted;
    Ok(n)
}

fn resolve_threads(max_threads: usize) -> usize {
    let auto = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 16);
    if max_threads == 0 {
        auto
    } else {
        max_threads.clamp(1, 16)
    }
}

fn enumerate_dir(
    job: DirJob,
    full_stat: bool,
    q: &Queue,
    local: &mut Vec<RawEntry>,
    counted: &AtomicU64,
) {
    let owned = unsafe { OwnedFd::from_raw_fd(job.fd) };
    let fd = owned.as_raw_fd();
    let mut buf = vec![0u8; GETDENTS_BUF];
    loop {
        let nread = unsafe {
            libc::syscall(
                libc::SYS_getdents64,
                fd,
                buf.as_mut_ptr(),
                buf.len(),
            )
        };
        if nread < 0 {
            break;
        }
        if nread == 0 {
            break;
        }
        let nread = nread as usize;
        let mut off = 0usize;
        while off + 19 <= nread {
            let d_reclen = u16::from_ne_bytes(buf[off + 16..off + 18].try_into().unwrap()) as usize;
            if d_reclen < 20 || off + d_reclen > nread {
                break;
            }
            let d_ino = u64::from_ne_bytes(buf[off..off + 8].try_into().unwrap());
            let d_type = buf[off + 18];
            let name_bytes = &buf[off + 19..off + d_reclen];
            let name_end = name_bytes.iter().position(|&b| b == 0).unwrap_or(name_bytes.len());
            let name = String::from_utf8_lossy(&name_bytes[..name_end]).into_owned();
            off += d_reclen;
            if should_skip_name(&name) {
                continue;
            }
            let mut is_dir = d_type == DT_DIR;
            let mut is_link = d_type == DT_LNK;
            let mut size = 0u64;
            let mut mtime = 0u64;
            let mut ctime = 0u64;
            let need_stat = full_stat || d_type == DT_UNKNOWN;
            if need_stat {
                if let Some(meta) = meta_at(fd, &name) {
                    is_dir = meta.is_dir;
                    is_link = meta.is_link;
                    if full_stat {
                        size = meta.size;
                        mtime = meta.mtime;
                        ctime = meta.ctime;
                    }
                } else {
                    continue;
                }
            }
            local.push(RawEntry {
                file_id: d_ino,
                file_id_128: None,
                parent_id: job.dir_ino,
                name: name.clone(),
                size,
                mtime,
                ctime,
                attrs: if is_dir { 0x10 } else { 0 },
                is_dir,
            });
            let n = counted.fetch_add(1, Ordering::Relaxed) + 1;
            if n % 100_000 == 0 {
                progress!("Linux 扫描：已枚举 {n} 条 …");
            }
            if is_dir && !is_link {
                if let Some(child) = open_child_dir(fd, &name, job.root_dev) {
                    q.inflight.fetch_add(1, Ordering::AcqRel);
                    q.jobs.lock().unwrap().push_back(child);
                    q.wait.notify_one();
                }
            }
        }
    }
}

struct MetaAt {
    is_dir: bool,
    is_link: bool,
    size: u64,
    mtime: u64,
    ctime: u64,
}

fn meta_at(dirfd: i32, name: &str) -> Option<MetaAt> {
    let c = CString::new(name.as_bytes()).ok()?;
    let mut stx: libc::statx = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::statx(
            dirfd,
            c.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW | libc::AT_NO_AUTOMOUNT,
            libc::STATX_TYPE
                | libc::STATX_MODE
                | libc::STATX_SIZE
                | libc::STATX_MTIME
                | libc::STATX_BTIME
                | libc::STATX_INO,
            &mut stx,
        )
    };
    if rc != 0 {
        return fstatat_fallback(dirfd, &c);
    }
    let mode = stx.stx_mode as u32;
    let is_dir = (mode & libc::S_IFMT) == libc::S_IFDIR;
    let is_link = (mode & libc::S_IFMT) == libc::S_IFLNK;
    let mtime = unix_secs_to_filetime(stx.stx_mtime.tv_sec.max(0) as u32);
    let ctime = if stx.stx_mask & libc::STATX_BTIME != 0 {
        unix_secs_to_filetime(stx.stx_btime.tv_sec.max(0) as u32)
    } else {
        mtime
    };
    Some(MetaAt {
        is_dir,
        is_link,
        size: stx.stx_size,
        mtime,
        ctime,
    })
}

fn fstatat_fallback(dirfd: i32, name: &CString) -> Option<MetaAt> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::fstatat(dirfd, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) };
    if rc != 0 {
        return None;
    }
    Some(MetaAt {
        is_dir: (st.st_mode & libc::S_IFMT) == libc::S_IFDIR,
        is_link: (st.st_mode & libc::S_IFMT) == libc::S_IFLNK,
        size: st.st_size as u64,
        mtime: unix_secs_to_filetime(st.st_mtime.max(0) as u32),
        ctime: unix_secs_to_filetime(st.st_ctime.max(0) as u32),
    })
}

fn open_child_dir(dirfd: i32, name: &str, root_dev: u64) -> Option<DirJob> {
    let c = CString::new(name.as_bytes()).ok()?;
    let fd = unsafe {
        libc::openat(
            dirfd,
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return None;
    }
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } != 0 || st.st_dev != root_dev {
        unsafe { libc::close(fd) };
        return None;
    }
    Some(DirJob {
        fd,
        dir_ino: st.st_ino,
        root_dev,
    })
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
