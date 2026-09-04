//! macOS FSEvents：`kFSEventStreamCreateFlagFileEvents` + `sinceWhen` 游标。
//! 丢事件 / MustScanSubDirs / RootChanged → `Error::JournalGap`，由服务侧重扫该卷。

use std::ffi::{c_void, CStr, CString};
use std::os::raw::{c_char, c_uint};
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use findx2_core::index::unix_secs_to_filetime;
use findx2_core::{ChangeEvent, ChangeWatcher, RawEntry, Result, WatchCursor};
use libc::{lstat, stat, S_IFDIR, S_IFMT, S_IFREG};

const FILE_EVENTS: u32 = 0x0000_0010;
const NO_DEFER: u32 = 0x0000_0002;
const WATCH_ROOT: u32 = 0x0000_0004;
const SINCE_NOW: u64 = u64::MAX;

const MUST_SCAN_SUBDIRS: u32 = 0x0000_0001;
const USER_DROPPED: u32 = 0x0000_0002;
const KERNEL_DROPPED: u32 = 0x0000_0004;
const ROOT_CHANGED: u32 = 0x0000_0020;
const ITEM_CREATED: u32 = 0x0000_0100;
const ITEM_REMOVED: u32 = 0x0000_0200;
const ITEM_RENAMED: u32 = 0x0000_0800;
const ITEM_MODIFIED: u32 = 0x0000_1000;
const ITEM_INODE_META: u32 = 0x0000_0400;
const ITEM_IS_DIR: u32 = 0x0002_0000;

#[repr(C)]
struct FSEventStreamContext {
    version: isize,
    info: *mut c_void,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
}

type FSEventStreamRef = *mut c_void;
type CFStringRef = *const c_void;
type CFArrayRef = *const c_void;
type CFAllocatorRef = *const c_void;
type CFRunLoopRef = *mut c_void;
type CFRunLoopMode = CFStringRef;

#[repr(C)]
struct CFArrayCallBacks {
    version: isize,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
    equal: *const c_void,
}

#[link(name = "CoreServices", kind = "framework")]
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn FSEventStreamCreate(
        allocator: CFAllocatorRef,
        callback: extern "C" fn(
            FSEventStreamRef,
            *mut c_void,
            usize,
            *mut c_void,
            *const c_uint,
            *const u64,
        ),
        context: *const FSEventStreamContext,
        pathsToWatch: CFArrayRef,
        sinceWhen: u64,
        latency: f64,
        flags: u32,
    ) -> FSEventStreamRef;
    fn FSEventStreamScheduleWithRunLoop(
        stream: FSEventStreamRef,
        runLoop: CFRunLoopRef,
        runLoopMode: CFRunLoopMode,
    );
    fn FSEventStreamStart(stream: FSEventStreamRef) -> u8;
    fn FSEventStreamStop(stream: FSEventStreamRef);
    fn FSEventStreamInvalidate(stream: FSEventStreamRef);
    fn FSEventStreamRelease(stream: FSEventStreamRef);
    fn FSEventsGetCurrentEventId() -> u64;
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopRun();
    fn CFRunLoopStop(rl: CFRunLoopRef);
    static kCFRunLoopDefaultMode: CFRunLoopMode;
    fn CFStringCreateWithCString(
        alloc: CFAllocatorRef,
        cStr: *const c_char,
        encoding: u32,
    ) -> CFStringRef;
    fn CFArrayCreate(
        alloc: CFAllocatorRef,
        values: *const *const c_void,
        numValues: isize,
        callbacks: *const CFArrayCallBacks,
    ) -> CFArrayRef;
    fn CFRelease(cf: *const c_void);
    static kCFTypeArrayCallBacks: CFArrayCallBacks;
}

const KCF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

struct WatchState {
    tx: Sender<ChangeEvent>,
    gap: Arc<AtomicBool>,
    last_id: Arc<AtomicU64>,
    runloop: std::sync::Mutex<Option<usize>>,
}

pub fn current_event_id() -> u64 {
    unsafe { FSEventsGetCurrentEventId() }
}

pub struct MacosChangeWatcher {
    pub root: String,
    pub cursor: WatchCursor,
}

impl MacosChangeWatcher {
    pub fn new(root: impl Into<String>, cursor: WatchCursor) -> Self {
        Self {
            root: root.into(),
            cursor,
        }
    }
}

impl ChangeWatcher for MacosChangeWatcher {
    fn watch(&self, tx: Sender<ChangeEvent>) -> Result<()> {
        self.watch_from(&self.root, self.cursor, tx).map(|_| ())
    }

    fn watch_from(
        &self,
        volume: &str,
        cursor: WatchCursor,
        tx: Sender<ChangeEvent>,
    ) -> Result<WatchCursor> {
        watch_loop(volume, cursor, tx)
    }
}

pub fn watch_loop(root: &str, cursor: WatchCursor, tx: Sender<ChangeEvent>) -> Result<WatchCursor> {
    let gap = Arc::new(AtomicBool::new(false));
    let last_id = Arc::new(AtomicU64::new(cursor.watch_cursor));
    let state = Box::new(WatchState {
        tx,
        gap: gap.clone(),
        last_id: last_id.clone(),
        runloop: std::sync::Mutex::new(None),
    });
    let info = Box::into_raw(state);

    let c_root = CString::new(root)
        .map_err(|_| findx2_core::Error::Platform("监听根路径含 NUL".into()))?;
    unsafe {
        let cf_path = CFStringCreateWithCString(ptr::null(), c_root.as_ptr(), KCF_STRING_ENCODING_UTF8);
        if cf_path.is_null() {
            let _ = Box::from_raw(info);
            return Err(findx2_core::Error::Platform("CFString 创建失败".into()));
        }
        let values = [cf_path as *const c_void];
        // kCFTypeArrayCallBacks 会 retain CFString；callbacks=NULL 时 CFRelease(cf_path)
        // 会立刻释放，FSEventStreamCreate 读到悬空指针。
        let arr = CFArrayCreate(ptr::null(), values.as_ptr(), 1, &kCFTypeArrayCallBacks);
        CFRelease(cf_path);
        if arr.is_null() {
            let _ = Box::from_raw(info);
            return Err(findx2_core::Error::Platform("CFArray 创建失败".into()));
        }

        let ctx = FSEventStreamContext {
            version: 0,
            info: info as *mut c_void,
            retain: ptr::null(),
            release: ptr::null(),
            copy_description: ptr::null(),
        };
        let since = if cursor.watch_cursor == 0 || cursor.watch_cursor == SINCE_NOW {
            SINCE_NOW
        } else {
            cursor.watch_cursor
        };
        let stream = FSEventStreamCreate(
            ptr::null(),
            fsevents_callback,
            &ctx,
            arr,
            since,
            0.5,
            FILE_EVENTS | NO_DEFER | WATCH_ROOT,
        );
        CFRelease(arr as *const c_void);
        if stream.is_null() {
            let _ = Box::from_raw(info);
            return Err(findx2_core::Error::Platform("FSEventStreamCreate 失败".into()));
        }

        let rl = CFRunLoopGetCurrent();
        (*info).runloop.lock().ok().map(|mut g| *g = Some(rl as usize));
        FSEventStreamScheduleWithRunLoop(stream, rl, kCFRunLoopDefaultMode);
        if FSEventStreamStart(stream) == 0 {
            FSEventStreamInvalidate(stream);
            FSEventStreamRelease(stream);
            let _ = Box::from_raw(info);
            return Err(findx2_core::Error::Platform("FSEventStreamStart 失败".into()));
        }
        CFRunLoopRun();
        FSEventStreamStop(stream);
        FSEventStreamInvalidate(stream);
        FSEventStreamRelease(stream);
        let _ = Box::from_raw(info);
    }

    if gap.load(Ordering::SeqCst) {
        return Err(findx2_core::Error::JournalGap(
            "FSEvents MustScanSubDirs / 丢事件，需子树或整卷重建".into(),
        ));
    }
    Ok(WatchCursor {
        watch_gen: cursor.watch_gen.max(1),
        watch_cursor: last_id.load(Ordering::SeqCst),
    })
}

extern "C" fn fsevents_callback(
    _stream: FSEventStreamRef,
    info: *mut c_void,
    num: usize,
    event_paths: *mut c_void,
    flags: *const c_uint,
    ids: *const u64,
) {
    if info.is_null() {
        return;
    }
    let state = unsafe { &*(info as *const WatchState) };
    let paths = event_paths as *const *const c_char;
    for i in 0..num {
        let fl = unsafe { *flags.add(i) };
        let id = unsafe { *ids.add(i) };
        state.last_id.store(id, Ordering::SeqCst);
        if fl & (MUST_SCAN_SUBDIRS | USER_DROPPED | KERNEL_DROPPED | ROOT_CHANGED) != 0 {
            state.gap.store(true, Ordering::SeqCst);
            if let Ok(g) = state.runloop.lock() {
                if let Some(rl) = *g {
                    unsafe { CFRunLoopStop(rl as CFRunLoopRef) };
                }
            }
            return;
        }
        let p = unsafe { *paths.add(i) };
        if p.is_null() {
            continue;
        }
        let path = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
        if let Some(ev) = path_to_event(&path, fl) {
            let _ = state.tx.send(ev);
        }
    }
}

fn path_to_event(path: &str, flags: u32) -> Option<ChangeEvent> {
    if crate::scan::should_skip_name(Path::new(path).file_name()?.to_str()?) {
        return None;
    }
    if flags & ITEM_REMOVED != 0 {
        if let Some(st) = lstat_ino(path) {
            return Some(ChangeEvent::Delete { file_id: st.0 });
        }
        return None;
    }
    let st = lstat_ino(path)?;
    let (ino, parent, name, size, mtime, ctime, is_dir) = st;
    if flags & ITEM_CREATED != 0 {
        return Some(ChangeEvent::CreatePending {
            file_id: ino,
            file_id_128: None,
            parent_id: parent,
            name,
            attrs: if is_dir { 0x10 } else { 0 },
            is_dir,
        });
    }
    if flags & ITEM_RENAMED != 0 {
        return Some(ChangeEvent::Rename {
            file_id: ino,
            new_parent_id: parent,
            new_name: name,
        });
    }
    if flags & (ITEM_MODIFIED | ITEM_INODE_META) != 0 {
        return Some(ChangeEvent::DataOrMeta {
            file_id: ino,
            size: Some(size),
            mtime: Some(mtime),
            ctime: Some(ctime),
        });
    }
    let _ = ITEM_IS_DIR;
    Some(ChangeEvent::Create {
        entry: RawEntry {
            file_id: ino,
            file_id_128: None,
            parent_id: parent,
            name,
            size,
            mtime,
            ctime,
            attrs: if is_dir { 0x10 } else { 0 },
            is_dir,
        },
    })
}

fn lstat_ino(path: &str) -> Option<(u64, u64, String, u64, u64, u64, bool)> {
    let c = CString::new(path).ok()?;
    let mut st: stat = unsafe { std::mem::zeroed() };
    if unsafe { lstat(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let mode = st.st_mode;
    let is_dir = (mode & S_IFMT) == S_IFDIR;
    if !is_dir && (mode & S_IFMT) != S_IFREG {
        // 仍索引 symlink 为条目但不跟随
    }
    let name = Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent = parent_ino(path).unwrap_or(0);
    Some((
        st.st_ino,
        parent,
        name,
        st.st_size as u64,
        unix_secs_to_filetime(st.st_mtime.max(0) as u32),
        unix_secs_to_filetime(st.st_ctime.max(0) as u32),
        is_dir,
    ))
}

fn parent_ino(path: &str) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let parent = Path::new(path).parent()?;
    let c = CString::new(parent.as_os_str().as_bytes()).ok()?;
    let mut st: stat = unsafe { std::mem::zeroed() };
    if unsafe { lstat(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some(st.st_ino)
}
