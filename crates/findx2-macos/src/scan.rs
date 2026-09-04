//! macOS：`getattrlistbulk` 按目录批量拉 inode / 名字 / 时间 / 大小。
//! 只扫 Data 卷（或用户指定根），不读文件内容（避免把 iCloud 占位拉回本地）。

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;

use findx2_core::index::unix_secs_to_filetime;
use findx2_core::{progress, RawEntry, Result, VolumeScanner, WatchCursor};
use libc::{
    attrlist, getattrlistbulk, open, timespec, ATTR_BIT_MAP_COUNT, ATTR_CMN_CRTIME,
    ATTR_CMN_FILEID, ATTR_CMN_MODTIME, ATTR_CMN_NAME, ATTR_CMN_OBJTYPE, ATTR_CMN_PARENTID,
    ATTR_CMN_RETURNED_ATTRS, ATTR_FILE_DATALENGTH, O_DIRECTORY, O_RDONLY,
};

/// `sys/attr.h`：libc 未导出 `ATTR_CMN_ERROR`。
const ATTR_CMN_ERROR: u32 = 0x2000_0000;
/// `sys/vnode.h`：`fsobj_type_t` / vnode 类型，libc 未导出。
const VREG: u32 = 1;
const VDIR: u32 = 2;
const VLNK: u32 = 5;

const SKIP_NAMES: &[&str] = &[
    ".",
    "..",
    ".Spotlight-V100",
    ".fseventsd",
    ".TemporaryItems",
    ".Trashes",
    ".DocumentRevisions-V100",
    ".MobileBackups",
    "Backups.backupdb",
    ".PKInstallSandboxManager",
    ".vol",
    "TheVolumeSettingsFolder",
];

#[repr(C)]
struct AttrReference {
    attr_dataoffset: i32,
    attr_length: u32,
}

#[repr(C)]
struct AttributeSet {
    commonattr: u32,
    volattr: u32,
    dirattr: u32,
    fileattr: u32,
    forkattr: u32,
}

pub fn should_skip_name(name: &str) -> bool {
    SKIP_NAMES.iter().any(|s| *s == name)
}

pub fn current_fsevents_id() -> u64 {
    crate::watch::current_event_id()
}

pub struct MacosVolumeScanner;

impl VolumeScanner for MacosVolumeScanner {
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
        progress!("macOS 扫描：打开 {} …", root);
        let mut n = 0u64;
        scan_tree(Path::new(&root), out, &mut n)?;
        progress!("macOS 扫描完成：{} 条", n);
        Ok(WatchCursor {
            watch_gen: 1,
            watch_cursor: current_fsevents_id(),
        })
    }
}

pub fn default_scan_root() -> String {
    let data = "/System/Volumes/Data";
    if Path::new(data).is_dir() {
        data.into()
    } else {
        "/".into()
    }
}

fn scan_tree(root: &Path, out: &mut dyn FnMut(RawEntry) -> Result<()>, n: &mut u64) -> Result<()> {
    let c_path = CString::new(root.as_os_str().as_bytes())
        .map_err(|_| findx2_core::Error::Platform("扫描根路径含 NUL".into()))?;
    let fd = unsafe { open(c_path.as_ptr(), O_RDONLY | O_DIRECTORY) };
    if fd < 0 {
        return Err(findx2_core::Error::Platform(format!(
            "无法打开目录 {}: {}",
            root.display(),
            std::io::Error::last_os_error()
        )));
    }
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    scan_dirfd(owned.as_raw_fd(), out, n)
}

fn scan_dirfd(dirfd: i32, out: &mut dyn FnMut(RawEntry) -> Result<()>, n: &mut u64) -> Result<()> {
    let mut attrs: attrlist = unsafe { std::mem::zeroed() };
    attrs.bitmapcount = ATTR_BIT_MAP_COUNT as u16;
    attrs.commonattr = ATTR_CMN_RETURNED_ATTRS
        | ATTR_CMN_NAME
        | ATTR_CMN_ERROR
        | ATTR_CMN_OBJTYPE
        | ATTR_CMN_CRTIME
        | ATTR_CMN_MODTIME
        | ATTR_CMN_FILEID
        | ATTR_CMN_PARENTID;
    attrs.fileattr = ATTR_FILE_DATALENGTH;

    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let count = unsafe {
            getattrlistbulk(
                dirfd,
                &mut attrs as *mut attrlist as *mut libc::c_void,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
                0,
            )
        };
        if count < 0 {
            return Err(findx2_core::Error::Platform(format!(
                "getattrlistbulk: {}",
                std::io::Error::last_os_error()
            )));
        }
        if count == 0 {
            break;
        }
        let mut kids: Vec<(i32, bool)> = Vec::new();
        let mut off = 0usize;
        for _ in 0..count {
            if off + 4 > buf.len() {
                break;
            }
            let rec_len = u32::from_ne_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
            if rec_len < 8 || off + rec_len > buf.len() {
                break;
            }
            let rec = &buf[off..off + rec_len];
            if let Some((entry, is_dir, is_link)) = parse_record(rec) {
                if !should_skip_name(&entry.name) {
                    let name_c = CString::new(entry.name.as_bytes()).ok();
                    out(entry)?;
                    *n += 1;
                    if *n % 100_000 == 0 {
                        progress!("macOS 扫描：已枚举 {} 条 …", n);
                    }
                    if is_dir && !is_link {
                        if let Some(c) = name_c {
                            let child = unsafe {
                                libc::openat(dirfd, c.as_ptr(), O_RDONLY | O_DIRECTORY)
                            };
                            if child >= 0 {
                                kids.push((child, true));
                            }
                        }
                    }
                }
            }
        off += rec_len;
        }
        for (child_fd, _) in kids {
            let owned = unsafe { OwnedFd::from_raw_fd(child_fd) };
            let _ = scan_dirfd(owned.as_raw_fd(), out, n);
        }
    }
    Ok(())
}

fn align4(p: usize) -> usize {
    (p + 3) & !3
}

fn parse_record(rec: &[u8]) -> Option<(RawEntry, bool, bool)> {
    // RETURNED_ATTRS 固定在最前；其余 common 按比特从低到高。
    let mut p = 4usize;
    if p + std::mem::size_of::<AttributeSet>() > rec.len() {
        return None;
    }
    let returned: AttributeSet = unsafe { std::ptr::read_unaligned(rec[p..].as_ptr() as *const _) };
    p += std::mem::size_of::<AttributeSet>();
    p = align4(p);

    let mut name = String::new();
    let mut objtype: u32 = VREG;
    let mut cr: timespec = unsafe { std::mem::zeroed() };
    let mut md: timespec = unsafe { std::mem::zeroed() };
    let mut file_id = 0u64;
    let mut parent_id = 0u64;

    // NAME = 0x1
    if returned.commonattr & ATTR_CMN_NAME != 0 {
        if p + std::mem::size_of::<AttrReference>() > rec.len() {
            return None;
        }
        let aref: AttrReference =
            unsafe { std::ptr::read_unaligned(rec[p..].as_ptr() as *const _) };
        let name_off = (p as i32).saturating_add(aref.attr_dataoffset) as usize;
        let name_len = aref.attr_length as usize;
        if name_off < rec.len() && name_len > 0 {
            let end = (name_off + name_len).min(rec.len());
            let raw = &rec[name_off..end];
            let bytes = if raw.last() == Some(&0) {
                &raw[..raw.len() - 1]
            } else {
                raw
            };
            name = String::from_utf8_lossy(bytes).into_owned();
        }
        p += std::mem::size_of::<AttrReference>();
        p = align4(p);
    }
    // OBJTYPE = 0x8
    if returned.commonattr & ATTR_CMN_OBJTYPE != 0 {
        if p + 4 > rec.len() {
            return None;
        }
        objtype = u32::from_ne_bytes(rec[p..p + 4].try_into().ok()?);
        p += 4;
        p = align4(p);
    }
    // CRTIME = 0x200
    if returned.commonattr & ATTR_CMN_CRTIME != 0 {
        if p + std::mem::size_of::<timespec>() > rec.len() {
            return None;
        }
        cr = unsafe { std::ptr::read_unaligned(rec[p..].as_ptr() as *const timespec) };
        p += std::mem::size_of::<timespec>();
        p = align4(p);
    }
    // MODTIME = 0x400
    if returned.commonattr & ATTR_CMN_MODTIME != 0 {
        if p + std::mem::size_of::<timespec>() > rec.len() {
            return None;
        }
        md = unsafe { std::ptr::read_unaligned(rec[p..].as_ptr() as *const timespec) };
        p += std::mem::size_of::<timespec>();
        p = align4(p);
    }
    // FILEID = 0x02000000
    if returned.commonattr & ATTR_CMN_FILEID != 0 {
        if p + 8 > rec.len() {
            return None;
        }
        file_id = u64::from_ne_bytes(rec[p..p + 8].try_into().ok()?);
        p += 8;
        p = align4(p);
    }
    // PARENTID = 0x04000000
    if returned.commonattr & ATTR_CMN_PARENTID != 0 {
        if p + 8 > rec.len() {
            return None;
        }
        parent_id = u64::from_ne_bytes(rec[p..p + 8].try_into().ok()?);
        p += 8;
        p = align4(p);
    }
    // ERROR = 0x20000000（在 FILEID 之后）
    if returned.commonattr & ATTR_CMN_ERROR != 0 {
        if p + 4 > rec.len() {
            return None;
        }
        let err = i32::from_ne_bytes(rec[p..p + 4].try_into().ok()?);
        p += 4;
        p = align4(p);
        if err != 0 {
            return None;
        }
    }

    let mut size = 0u64;
    if returned.fileattr & ATTR_FILE_DATALENGTH != 0 && p + 8 <= rec.len() {
        size = i64::from_ne_bytes(rec[p..p + 8].try_into().ok()?).max(0) as u64;
    }

    let is_dir = objtype == VDIR;
    let is_link = objtype == VLNK;
    if name.is_empty() {
        return None;
    }
    Some((
        RawEntry {
            file_id,
            file_id_128: None,
            parent_id,
            name,
            size,
            mtime: unix_secs_to_filetime(md.tv_sec.max(0) as u32),
            ctime: unix_secs_to_filetime(cr.tv_sec.max(0) as u32),
            attrs: if is_dir { 0x10 } else { 0 },
            is_dir,
        },
        is_dir,
        is_link,
    ))
}

/// 显示路径前缀：Data 卷当作 Unix 根，这样 `/Users` 对用户可见。
pub fn display_root_prefix(scan_root: &str) -> String {
    let t = scan_root.trim_end_matches('/');
    if t.is_empty() || t == "/System/Volumes/Data" {
        "/".into()
    } else {
        t.to_string()
    }
}

pub fn volume_id_for_path(path: &str) -> String {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path)
        .map(|m| format!("dev:{}", m.dev()))
        .unwrap_or_else(|_| path.to_string())
}
