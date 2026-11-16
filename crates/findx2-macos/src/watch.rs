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
const USE_CF_TYPES: u32 = 0x0000_0001;
const USE_EXTENDED_DATA: u32 = 0x0000_0040;
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
    fn CFGetTypeID(cf: *const c_void) -> usize;
    fn CFArrayGetTypeID() -> usize;
    fn CFArrayGetCount(array: CFArrayRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CFArrayRef, index: isize) -> *const c_void;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFDictionaryGetValue(dictionary: *const c_void, key: *const c_void) -> *const c_void;
    fn CFStringGetTypeID() -> usize;
    fn CFStringGetCString(
        string: CFStringRef,
        buffer: *mut c_char,
        size: isize,
        encoding: u32,
    ) -> u8;
    fn CFNumberGetTypeID() -> usize;
    fn CFNumberGetValue(number: *const c_void, number_type: isize, value: *mut c_void) -> u8;
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

    let c_root =
        CString::new(root).map_err(|_| findx2_core::Error::Platform("监听根路径含 NUL".into()))?;
    unsafe {
        let cf_path =
            CFStringCreateWithCString(ptr::null(), c_root.as_ptr(), KCF_STRING_ENCODING_UTF8);
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
            FILE_EVENTS | NO_DEFER | WATCH_ROOT | USE_CF_TYPES | USE_EXTENDED_DATA,
        );
        CFRelease(arr as *const c_void);
        if stream.is_null() {
            let _ = Box::from_raw(info);
            return Err(findx2_core::Error::Platform(
                "FSEventStreamCreate 失败".into(),
            ));
        }

        let rl = CFRunLoopGetCurrent();
        (*info)
            .runloop
            .lock()
            .ok()
            .map(|mut g| *g = Some(rl as usize));
        FSEventStreamScheduleWithRunLoop(stream, rl, kCFRunLoopDefaultMode);
        if FSEventStreamStart(stream) == 0 {
            FSEventStreamInvalidate(stream);
            FSEventStreamRelease(stream);
            let _ = Box::from_raw(info);
            return Err(findx2_core::Error::Platform(
                "FSEventStreamStart 失败".into(),
            ));
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

// Extended-data keys are CFSTR macros in Apple's header, NOT exported symbols.
// Create local equal CFStrings rather than trying to link nonexistent constants.
struct OwnedCF(*const c_void);
impl Drop for OwnedCF {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}
fn stop_for_gap(state: &WatchState) {
    state.gap.store(true, Ordering::SeqCst);
    if let Ok(g) = state.runloop.lock() {
        if let Some(rl) = *g {
            unsafe { CFRunLoopStop(rl as CFRunLoopRef) };
        }
    }
}

// The stream uses UseCFTypes | UseExtendedData: a CFArray of dictionaries,
// each with CFString "path" and optional CFNumber "fileID". No index-sized cache
// or second filesystem walk is needed to identify a file after it was removed.
unsafe fn extended_event(
    paths: CFArrayRef,
    index: isize,
    path_key: *const c_void,
    inode_key: *const c_void,
) -> Option<(String, Option<u64>)> {
    let dictionary = CFArrayGetValueAtIndex(paths, index);
    if dictionary.is_null() || CFGetTypeID(dictionary) != CFDictionaryGetTypeID() {
        return None;
    }
    let path = CFDictionaryGetValue(dictionary, path_key);
    if path.is_null() || CFGetTypeID(path) != CFStringGetTypeID() {
        return None;
    }
    // Darwin PATH_MAX is 1024 bytes; leave headroom and fail visibly on overflow.
    let mut bytes = [0 as c_char; 4096];
    if CFStringGetCString(
        path,
        bytes.as_mut_ptr(),
        bytes.len() as isize,
        KCF_STRING_ENCODING_UTF8,
    ) == 0
    {
        return None;
    }
    let path = CStr::from_ptr(bytes.as_ptr())
        .to_string_lossy()
        .into_owned();
    let number = CFDictionaryGetValue(dictionary, inode_key);
    let mut inode = 0i64;
    let inode = if !number.is_null()
        && CFGetTypeID(number) == CFNumberGetTypeID()
        && CFNumberGetValue(
            number,
            4, /* kCFNumberSInt64Type */
            &mut inode as *mut _ as *mut c_void,
        ) != 0
        && inode != 0
    {
        Some(inode as u64)
    } else {
        None
    };
    Some((path, inode))
}

extern "C" fn fsevents_callback(
    _stream: FSEventStreamRef,
    info: *mut c_void,
    num: usize,
    event_paths: *mut c_void,
    flags: *const c_uint,
    ids: *const u64,
) {
    if info.is_null() || num == 0 {
        return;
    }
    let state = unsafe { &*(info as *const WatchState) };
    if event_paths.is_null()
        || flags.is_null()
        || ids.is_null()
        || unsafe {
            CFGetTypeID(event_paths) != CFArrayGetTypeID()
                || CFArrayGetCount(event_paths) < 0
                || (CFArrayGetCount(event_paths) as usize) < num
        }
    {
        stop_for_gap(state);
        return;
    }
    let path_key = OwnedCF(unsafe {
        CFStringCreateWithCString(
            ptr::null(),
            b"path\0".as_ptr().cast(),
            KCF_STRING_ENCODING_UTF8,
        )
    });
    let inode_key = OwnedCF(unsafe {
        CFStringCreateWithCString(
            ptr::null(),
            b"fileID\0".as_ptr().cast(),
            KCF_STRING_ENCODING_UTF8,
        )
    });
    if path_key.0.is_null() || inode_key.0.is_null() {
        stop_for_gap(state);
        return;
    }
    // Buffer only this delivered batch. Apply old-name removals before live-path
    // upserts so a within-batch rename cannot finish as a tombstoned new name.
    let mut events = Vec::new();
    for i in 0..num {
        let fl = unsafe { *flags.add(i) };
        let id = unsafe { *ids.add(i) };
        state.last_id.store(id, Ordering::SeqCst);
        if fl & (MUST_SCAN_SUBDIRS | USER_DROPPED | KERNEL_DROPPED | ROOT_CHANGED) != 0 {
            stop_for_gap(state);
            return;
        }
        let Some((path, inode)) =
            (unsafe { extended_event(event_paths, i as isize, path_key.0, inode_key.0) })
        else {
            stop_for_gap(state);
            return;
        };
        if let Some(event) = path_to_event(&path, fl, inode) {
            events.push(event);
        } else if fl & (ITEM_REMOVED | ITEM_RENAMED) != 0 && inode.is_none() {
            // Never advertise healthy live deletion tracking after losing identity.
            stop_for_gap(state);
            return;
        }
    }
    events.sort_by_key(|event| !matches!(event, ChangeEvent::Delete { .. }));
    for event in events {
        let _ = state.tx.send(event);
    }
}

fn path_to_event(path: &str, flags: u32, event_inode: Option<u64>) -> Option<ChangeEvent> {
    if crate::scan::should_skip_name(Path::new(path).file_name()?.to_str()?) {
        return None;
    }
    let current = lstat_ino(path);
    if flags & ITEM_REMOVED != 0 {
        if let Some(inode) = event_inode {
            if current.as_ref().is_none_or(|st| st.0 != inode) {
                return Some(ChangeEvent::Delete { file_id: inode });
            }
        }
        if current.is_none() {
            return None;
        }
        // Coalesced remove/create of the same inode: the current live path wins.
    }
    let Some(st) = current else {
        return if flags & ITEM_RENAMED != 0 {
            event_inode.map(|file_id| ChangeEvent::Delete { file_id })
        } else {
            None
        };
    };
    let (ino, parent, name, size, mtime, ctime, is_dir) = st;
    if flags & (ITEM_CREATED | ITEM_RENAMED | ITEM_REMOVED) != 0 {
        return Some(ChangeEvent::CreatePending {
            file_id: ino,
            file_id_128: None,
            parent_id: parent,
            name,
            attrs: if is_dir { 0x10 } else { 0 },
            is_dir,
        });
    }
    if flags & (ITEM_MODIFIED | ITEM_INODE_META) != 0 {
        return Some(ChangeEvent::DataOrMeta {
            file_id: ino,
            size: Some(size),
            mtime: Some(mtime),
            ctime: Some(ctime),
            create_if_missing: None,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removed_and_moved_out_paths_use_native_inode_after_unlink() {
        let missing =
            std::env::temp_dir().join(format!("findx-fsevents-missing-{}", std::process::id()));
        assert!(!missing.exists());
        let path = missing.to_str().unwrap();
        assert!(matches!(
            path_to_event(path, ITEM_REMOVED, Some(42)),
            Some(ChangeEvent::Delete { file_id: 42 })
        ));
        assert!(matches!(
            path_to_event(path, ITEM_RENAMED, Some(43)),
            Some(ChangeEvent::Delete { file_id: 43 })
        ));
        assert!(path_to_event(path, ITEM_REMOVED, None).is_none());
    }

    #[test]
    fn live_rename_and_coalesced_recreate_are_upserts_not_missing_row_renames() {
        use std::os::unix::fs::MetadataExt;
        let path = std::env::temp_dir().join(format!(
            "findx-fsevents-live-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .unwrap();
        let inode = std::fs::symlink_metadata(&path).unwrap().ino();
        for flags in [ITEM_RENAMED, ITEM_REMOVED | ITEM_CREATED] {
            assert!(
                matches!(path_to_event(path.to_str().unwrap(), flags, Some(inode)), Some(ChangeEvent::CreatePending { file_id, .. }) if file_id == inode)
            );
        }
        // A stale removal must not delete a new file that now occupies the path.
        assert!(
            matches!(path_to_event(path.to_str().unwrap(), ITEM_REMOVED, Some(inode + 1)), Some(ChangeEvent::Delete { file_id }) if file_id == inode + 1)
        );
        std::fs::remove_file(path).unwrap();
    }
}
