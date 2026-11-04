//! Linux 增量：有特权走 fanotify（整挂载），否则 `notify`/inotify 盯用户选定根。
//! 无持久 journal；进程重启会漏事件，启动时应做一次短窗口追赶或接受短暂不一致。

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use findx2_core::index::unix_secs_to_filetime;
use findx2_core::{ChangeEvent, ChangeWatcher, Result, WatchCursor};
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

static LAST_WATCH_NOTE: Mutex<Option<String>> = Mutex::new(None);

pub fn take_watch_note() -> Option<String> {
    LAST_WATCH_NOTE.lock().ok().and_then(|mut g| g.take())
}

fn set_watch_note(msg: impl Into<String>) {
    if let Ok(mut g) = LAST_WATCH_NOTE.lock() {
        *g = Some(msg.into());
    }
}

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
    set_watch_note(
        "无 CAP_SYS_ADMIN，增量已降级为 inotify（非整盘实时；重启可能漏事件）",
    );
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
        create_if_missing: Some(findx2_core::CreateIfMissing {
            parent_id: parent,
            name,
            attrs: if is_dir { 0x10 } else { 0 },
            is_dir,
        }),
    })
}

fn readlink_fd(fd: i32) -> Option<String> {
    let proc = format!("/proc/self/fd/{fd}");
    std::fs::read_link(proc)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

/// 路径到 inode 的缓存，用于在删除事件时查找已删除文件的 inode
type PathInodeCache = Arc<Mutex<HashMap<PathBuf, u64>>>;

fn watch_inotify(roots: &[String], tx: Sender<ChangeEvent>) -> Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let tx2 = tx.clone();
    let cache: PathInodeCache = Arc::new(Mutex::new(HashMap::new()));
    let cache2 = cache.clone();
    let roots_owned: Vec<String> = roots.to_vec();
    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<notify::Event>| {
            if stop2.load(Ordering::Relaxed) {
                return;
            }
            let Ok(ev) = res else {
                return;
            };
            // inotify 队列溢出：后台追赶，避免在 notify 回调里同步扫大树再次撑爆队列。
            if ev.need_rescan() {
                set_watch_note(
                    "inotify 队列溢出，已对监听根做追赶扫描（仍非 fanotify 整盘实时）",
                );
                let tx3 = tx2.clone();
                let cache3 = cache2.clone();
                let roots3 = roots_owned.clone();
                let _ = std::thread::Builder::new()
                    .name("findx2-inotify-catchup".into())
                    .spawn(move || {
                        for r in &roots3 {
                            catch_up_tree(Path::new(r), &tx3, &cache3);
                        }
                    });
                return;
            }
            for path in &ev.paths {
                for ce in notify_to_changes(path, ev.kind, &cache2) {
                    let _ = tx2.send(ce);
                }
                // 新目录：watch 注册存在经典竞态。立即追赶 + 短延迟再追赶一次，
                // 覆盖「mkdir 后立刻往里写文件」窗口；已存在条目走 CreatePending 快路径。
                if matches!(ev.kind, EventKind::Create(notify::event::CreateKind::Folder)) {
                    catch_up_tree(path, &tx2, &cache2);
                    let path_delayed = path.clone();
                    let tx_d = tx2.clone();
                    let cache_d = cache2.clone();
                    let _ = std::thread::Builder::new()
                        .name("findx2-dir-catchup".into())
                        .spawn(move || {
                            std::thread::sleep(Duration::from_millis(150));
                            catch_up_tree(&path_delayed, &tx_d, &cache_d);
                        });
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

fn cache_insert(cache: &PathInodeCache, path: &Path, ino: u64) {
    if let Ok(mut map) = cache.lock() {
        map.insert(path.to_path_buf(), ino);
        if map.len() > 100_000 {
            let keys: Vec<_> = map.keys().take(50_000).cloned().collect();
            for k in keys {
                map.remove(&k);
            }
        }
    }
}

fn notify_to_changes(path: &Path, kind: EventKind, cache: &PathInodeCache) -> Vec<ChangeEvent> {
    use notify::event::{AccessKind, AccessMode, ModifyKind, RemoveKind, RenameMode};
    let Some(name_os) = path.file_name() else {
        return Vec::new();
    };
    let name = name_os.to_string_lossy().into_owned();
    if crate::scan::should_skip_name(&name) {
        return Vec::new();
    }
    match kind {
        EventKind::Remove(RemoveKind::Any | RemoveKind::File | RemoveKind::Folder) => {
            let ino = if let Ok(mut map) = cache.lock() {
                map.remove(path).or_else(|| path_ino(path))
            } else {
                path_ino(path)
            };
            ino.map(|file_id| ChangeEvent::Delete { file_id })
                .into_iter()
                .collect()
        }
        // 移出监视树：路径已不存在，按删除处理。
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
            let ino = if let Ok(mut map) = cache.lock() {
                map.remove(path).or_else(|| path_ino(path))
            } else {
                path_ino(path)
            };
            ino.map(|file_id| ChangeEvent::Delete { file_id })
                .into_iter()
                .collect()
        }
        EventKind::Create(_) => {
            let mut out = Vec::new();
            if let Some(ev) = meta_create_retry(path, name) {
                if let ChangeEvent::CreatePending { file_id, .. } = &ev {
                    cache_insert(cache, path, *file_id);
                }
                out.push(ev);
            }
            out
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::To | RenameMode::Both | RenameMode::Any)) => {
            let mut out = Vec::new();
            if let Some(ev) = meta_rename(path, name.clone()) {
                if let ChangeEvent::Rename { file_id, .. } = &ev {
                    cache_insert(cache, path, *file_id);
                }
                out.push(ev);
            } else if let Some(ev) = meta_create_retry(path, name.clone()) {
                // rename-to 时若尚未在索引中，按创建补齐（跨监视树移入）。
                if let ChangeEvent::CreatePending { file_id, .. } = &ev {
                    cache_insert(cache, path, *file_id);
                }
                out.push(ev);
            }
            out
        }
        EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Metadata(_))
        | EventKind::Access(AccessKind::Close(AccessMode::Write)) => {
            // Modify / CloseWrite：带 create_if_missing，Create 漏事件时可回填。
            match data_or_meta_with_backfill(path, name) {
                Some(ev) => {
                    if let ChangeEvent::DataOrMeta { file_id, .. } = &ev {
                        cache_insert(cache, path, *file_id);
                    }
                    vec![ev]
                }
                None => Vec::new(),
            }
        }
        EventKind::Access(_) => Vec::new(), // OPEN / Close(Read) 不处理，避免噪音
        _ => {
            let mut out = Vec::new();
            if let Some(ev) = meta_create_retry(path, name) {
                if let ChangeEvent::CreatePending { file_id, .. } = &ev {
                    cache_insert(cache, path, *file_id);
                }
                out.push(ev);
            }
            out
        }
    }
}

fn data_or_meta_with_backfill(path: &Path, name: String) -> Option<ChangeEvent> {
    use findx2_core::CreateIfMissing;
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path).ok()?;
    let parent = path
        .parent()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.ino())
        .unwrap_or(0);
    let is_dir = meta.is_dir();
    Some(ChangeEvent::DataOrMeta {
        file_id: meta.ino(),
        size: Some(meta.len()),
        mtime: Some(unix_secs_to_filetime(meta.mtime().max(0) as u32)),
        ctime: Some(unix_secs_to_filetime(meta.ctime().max(0) as u32)),
        create_if_missing: Some(CreateIfMissing {
            parent_id: parent,
            name,
            attrs: if is_dir { 0x10 } else { 0 },
            is_dir,
        }),
    })
}

/// 新目录创建后、或 inotify 溢出时：遍历目录树，用 CreatePending 补漏（已存在条目走快路径）。
fn catch_up_tree(dir: &Path, tx: &Sender<ChangeEvent>, cache: &PathInodeCache) {
    let mut stack = vec![dir.to_path_buf()];
    let mut seen = 0usize;
    const MAX_CATCH_UP: usize = 50_000;
    while let Some(cur) = stack.pop() {
        let rd = match std::fs::read_dir(&cur) {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        for ent in rd.flatten() {
            if seen >= MAX_CATCH_UP {
                set_watch_note(format!(
                    "inotify 追赶扫描达到上限 {MAX_CATCH_UP}，可能仍有遗漏"
                ));
                return;
            }
            let path = ent.path();
            let name = ent.file_name().to_string_lossy().into_owned();
            if crate::scan::should_skip_name(&name) {
                continue;
            }
            let ft = match ent.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if ft.is_symlink() {
                continue;
            }
            if let Some(ev) = meta_create(&path, name) {
                if let ChangeEvent::CreatePending { file_id, .. } = &ev {
                    cache_insert(cache, &path, *file_id);
                }
                let _ = tx.send(ev);
                seen += 1;
            }
            if ft.is_dir() {
                stack.push(path);
            }
        }
    }
}

fn path_ino(path: &Path) -> Option<u64> {
    std::fs::symlink_metadata(path).ok().map(|m| m.ino())
}

fn meta_create_retry(path: &Path, name: String) -> Option<ChangeEvent> {
    // 短暂竞态：CREATE 到达时偶发 stat 失败，短重试。
    for attempt in 0..3u32 {
        if let Some(ev) = meta_create(path, name.clone()) {
            return Some(ev);
        }
        if attempt + 1 < 3 {
            std::thread::sleep(Duration::from_millis(1 << attempt));
        }
    }
    None
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
