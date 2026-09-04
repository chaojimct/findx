//! Linux 增量：有特权走 fanotify（整挂载），否则 `notify`/inotify 盯用户选定根。
//! 无持久 journal；进程重启会漏事件，启动时应做一次短窗口追赶或接受短暂不一致。

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

use findx2_core::index::unix_secs_to_filetime;
use findx2_core::{ChangeEvent, ChangeWatcher, Result, WatchCursor};
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

pub struct LinuxChangeWatcher {
    pub roots: Vec<String>,
    pub cursor: WatchCursor,
}

impl LinuxChangeWatcher {
    pub fn new(roots: Vec<String>, cursor: WatchCursor) -> Self {
        Self { roots, cursor }
    }
}

impl ChangeWatcher for LinuxChangeWatcher {
    fn watch(&self, tx: Sender<ChangeEvent>) -> Result<()> {
        self.watch_from(
            self.roots.first().map(|s| s.as_str()).unwrap_or("/"),
            self.cursor,
            tx,
        )
        .map(|_| ())
    }

    fn watch_from(
        &self,
        volume: &str,
        cursor: WatchCursor,
        tx: Sender<ChangeEvent>,
    ) -> Result<WatchCursor> {
        let roots = if self.roots.is_empty() {
            vec![volume.to_string()]
        } else {
            self.roots.clone()
        };
        watch_loop(&roots, cursor, tx)
    }
}

pub fn watch_loop(roots: &[String], cursor: WatchCursor, tx: Sender<ChangeEvent>) -> Result<WatchCursor> {
    if try_fanotify(roots, &tx)? {
        return Ok(WatchCursor {
            watch_gen: cursor.watch_gen.saturating_add(1).max(1),
            watch_cursor: 0,
        });
    }
    findx2_core::progress!("Linux：fanotify 不可用（无 CAP_SYS_ADMIN），降级 inotify（非整盘实时）");
    watch_inotify(roots, tx)?;
    Ok(WatchCursor {
        watch_gen: cursor.watch_gen.saturating_add(1).max(1),
        watch_cursor: 0,
    })
}

fn try_fanotify(roots: &[String], tx: &Sender<ChangeEvent>) -> Result<bool> {
    let fd = unsafe {
        libc::fanotify_init(
            libc::FAN_CLOEXEC | libc::FAN_CLASS_NOTIF,
            libc::O_RDONLY as u32,
        )
    };
    if fd < 0 {
        return Ok(false);
    }
    let mut marked = 0usize;
    for root in roots {
        let c = match std::ffi::CString::new(root.as_bytes()) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let rc = unsafe {
            libc::fanotify_mark(
                fd,
                libc::FAN_MARK_ADD | libc::FAN_MARK_MOUNT,
                libc::FAN_CREATE
                    | libc::FAN_DELETE
                    | libc::FAN_MODIFY
                    | libc::FAN_MOVED_FROM
                    | libc::FAN_MOVED_TO
                    | libc::FAN_ONDIR
                    | libc::FAN_EVENT_ON_CHILD,
                libc::AT_FDCWD,
                c.as_ptr(),
            )
        };
        if rc == 0 {
            marked += 1;
        }
    }
    if marked == 0 {
        unsafe { libc::close(fd) };
        return Ok(false);
    }
    findx2_core::progress!("Linux：fanotify 已标记 {} 个挂载", marked);
    let mut buf = vec![0u8; 4096];
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            unsafe { libc::close(fd) };
            return Err(findx2_core::Error::JournalGap(format!(
                "fanotify 读取失败: {err}"
            )));
        }
        if n == 0 {
            continue;
        }
        parse_fanotify_buf(&buf[..n as usize], tx);
    }
}

fn parse_fanotify_buf(buf: &[u8], tx: &Sender<ChangeEvent>) {
    let mut off = 0usize;
    let meta_size = std::mem::size_of::<libc::fanotify_event_metadata>();
    while off + meta_size <= buf.len() {
        let meta: libc::fanotify_event_metadata =
            unsafe { std::ptr::read_unaligned(buf[off..].as_ptr() as *const _) };
        let elen = meta.event_len as usize;
        if elen < meta_size || off + elen > buf.len() {
            break;
        }
        if meta.fd >= 0 {
            if let Some(ev) = event_from_fd(meta.fd, meta.mask) {
                let _ = tx.send(ev);
            }
            unsafe { libc::close(meta.fd) };
        }
        off += elen;
    }
}

fn event_from_fd(fd: i32, mask: u64) -> Option<ChangeEvent> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        return None;
    }
    let ino = st.st_ino;
    let is_dir = (st.st_mode & libc::S_IFMT) == libc::S_IFDIR;
    let path = readlink_fd(fd);
    let name = path
        .as_deref()
        .and_then(|p| Path::new(p).file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent = path
        .as_deref()
        .and_then(|p| Path::new(p).parent())
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.ino())
        .unwrap_or(0);
    if mask & libc::FAN_DELETE != 0 || mask & libc::FAN_MOVED_FROM != 0 {
        return Some(ChangeEvent::Delete { file_id: ino });
    }
    if mask & libc::FAN_CREATE != 0 || mask & libc::FAN_MOVED_TO != 0 {
        return Some(ChangeEvent::CreatePending {
            file_id: ino,
            file_id_128: None,
            parent_id: parent,
            name,
            attrs: if is_dir { 0x10 } else { 0 },
            is_dir,
        });
    }
    Some(ChangeEvent::DataOrMeta {
        file_id: ino,
        size: Some(st.st_size as u64),
        mtime: Some(unix_secs_to_filetime(st.st_mtime.max(0) as u32)),
        ctime: Some(unix_secs_to_filetime(st.st_ctime.max(0) as u32)),
    })
}

fn readlink_fd(fd: i32) -> Option<String> {
    let proc = format!("/proc/self/fd/{fd}");
    std::fs::read_link(proc)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

fn watch_inotify(roots: &[String], tx: Sender<ChangeEvent>) -> Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let tx2 = tx.clone();
    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<notify::Event>| {
            if stop2.load(Ordering::Relaxed) {
                return;
            }
            let Ok(ev) = res else {
                return;
            };
            for path in ev.paths {
                if let Some(ce) = notify_to_change(&path, ev.kind) {
                    let _ = tx2.send(ce);
                }
            }
        },
        Config::default().with_poll_interval(Duration::from_secs(2)),
    )
    .map_err(|e| findx2_core::Error::Platform(format!("inotify: {e}")))?;
    for r in roots {
        let _ = watcher.watch(Path::new(r), RecursiveMode::Recursive);
    }
    // 阻塞：把 watcher 钉在本线程直到通道断开。
    loop {
        std::thread::park();
        if stop.load(Ordering::Relaxed) {
            break;
        }
    }
    drop(watcher);
    Ok(())
}

fn notify_to_change(path: &PathBuf, kind: EventKind) -> Option<ChangeEvent> {
use notify::event::{ModifyKind, RemoveKind, RenameMode};
    let name = path.file_name()?.to_string_lossy().into_owned();
    if crate::scan::should_skip_name(&name) {
        return None;
    }
    match kind {
        EventKind::Remove(RemoveKind::Any | RemoveKind::File | RemoveKind::Folder) => {
            let ino = path_ino(path)?;
            Some(ChangeEvent::Delete { file_id: ino })
        }
        EventKind::Create(_) => meta_create(path, name),
        EventKind::Modify(ModifyKind::Name(RenameMode::To | RenameMode::Both | RenameMode::Any)) => {
            meta_rename(path, name)
        }
        EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Metadata(_)) => {
            let meta = std::fs::symlink_metadata(path).ok()?;
            Some(ChangeEvent::DataOrMeta {
                file_id: meta.ino(),
                size: Some(meta.len()),
                mtime: Some(unix_secs_to_filetime(meta.mtime().max(0) as u32)),
                ctime: Some(unix_secs_to_filetime(meta.ctime().max(0) as u32)),
            })
        }
        _ => meta_create(path, name),
    }
}

fn path_ino(path: &Path) -> Option<u64> {
    std::fs::symlink_metadata(path).ok().map(|m| m.ino())
}

fn meta_create(path: &Path, name: String) -> Option<ChangeEvent> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path).ok()?;
    let parent = path
        .parent()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.ino())
        .unwrap_or(0);
    Some(ChangeEvent::CreatePending {
        file_id: meta.ino(),
        file_id_128: None,
        parent_id: parent,
        name,
        attrs: if meta.is_dir() { 0x10 } else { 0 },
        is_dir: meta.is_dir(),
    })
}

fn meta_rename(path: &Path, name: String) -> Option<ChangeEvent> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path).ok()?;
    let parent = path
        .parent()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.ino())
        .unwrap_or(0);
    Some(ChangeEvent::Rename {
        file_id: meta.ino(),
        new_parent_id: parent,
        new_name: name,
    })
}
