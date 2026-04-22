//! USN Journal：QUERY / READ 轮询，映射为 ChangeEvent。

use std::ffi::OsString;
use std::mem::size_of;
use std::os::windows::ffi::OsStringExt;
use std::ptr;
use std::sync::mpsc::Sender;
use std::time::Duration;

use crate::open_by_id::fetch_file_metadata_by_id;
use findx2_core::{ChangeEvent, ChangeWatcher, RawEntry, Result};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetVolumeInformationW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Ioctl::{
    FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL, READ_USN_JOURNAL_DATA_V0,
    USN_JOURNAL_DATA_V0, USN_REASON_BASIC_INFO_CHANGE, USN_REASON_DATA_EXTEND,
    USN_REASON_DATA_TRUNCATION, USN_REASON_FILE_CREATE, USN_REASON_FILE_DELETE,
    USN_REASON_RENAME_NEW_NAME, USN_REASON_RENAME_OLD_NAME,
};
use windows::Win32::System::IO::DeviceIoControl;

/// 从 `index.bin` 恢复的 USN 起点（须与当前 `journal_id` 一致）
#[derive(Debug, Clone, Copy)]
pub struct UsnResume {
    pub journal_id: u64,
    /// 上次成功 READ 后缓冲区中的下一 USN（与 `VolumeState.last_usn` 一致）
    pub start_usn: u64,
}

/// 传给监听循环的消息：`ChangeEvent` 或持久化检查点。
#[derive(Debug)]
pub enum UsnWatchMsg {
    Event(ChangeEvent),
    /// 每次 READ 成功后回写 `VolumeState.last_usn` / `usn_journal_id`
    Checkpoint {
        journal_id: u64,
        next_usn: u64,
    },
}

/// 从 Journal 查询到的状态（与 VolumeState 对齐字段）
#[derive(Debug, Clone)]
pub struct UsnState {
    pub journal_id: u64,
    pub next_usn: u64,
    pub first_usn: i64,
}

/// 取卷序列号（与 `GetVolumeInformationW`），`volume` 可为 `C:` / `C:\` / `\\.\C:`。
pub fn get_volume_serial_number(volume: &str) -> Result<u32> {
    let root = volume_root_for_info(volume);
    let wide: Vec<u16> = root.encode_utf16().chain(Some(0)).collect();
    let mut serial: u32 = 0;
    let mut max_component = 0u32;
    let mut flags = 0u32;
    let r = unsafe {
        GetVolumeInformationW(
            PCWSTR(wide.as_ptr()),
            None,
            Some(ptr::from_mut(&mut serial)),
            Some(ptr::from_mut(&mut max_component)),
            Some(ptr::from_mut(&mut flags)),
            None,
        )
    };
    if r.is_err() {
        return Err(findx2_core::Error::Platform(format!(
            "GetVolumeInformationW 失败: {root}"
        )));
    }
    Ok(serial)
}

/// `FSCTL_READ_USN_JOURNAL` 单条记录：`USN_RECORD_V2` 与 `USN_RECORD_V3` 字段偏移不同（v3 为 `FILE_ID_128`）。
fn parse_read_usn_record(
    rec: &[u8],
) -> Option<(u64, u64, u32, u32, usize, usize, Option<[u8; 16]>)> {
    if rec.len() < 8 {
        return None;
    }
    let major = u16::from_le_bytes(rec.get(4..6)?.try_into().ok()?);
    match major {
        2 => {
            if rec.len() < 60 {
                return None;
            }
            let fr = u64::from_le_bytes(rec[8..16].try_into().ok()?);
            let pr = u64::from_le_bytes(rec[16..24].try_into().ok()?);
            let reason = u32::from_le_bytes(rec[40..44].try_into().ok()?);
            let attrs = u32::from_le_bytes(rec[52..56].try_into().ok()?);
            let name_len = u16::from_le_bytes(rec[56..58].try_into().ok()?) as usize;
            let name_off = u16::from_le_bytes(rec[58..60].try_into().ok()?) as usize;
            Some((fr, pr, reason, attrs, name_len, name_off, None))
        }
        3 => {
            if rec.len() < 76 {
                return None;
            }
            let id128: [u8; 16] = rec[8..24].try_into().ok()?;
            let fr = u64::from_le_bytes(id128[0..8].try_into().ok()?);
            let pr = u64::from_le_bytes(rec[24..32].try_into().ok()?);
            let reason = u32::from_le_bytes(rec[56..60].try_into().ok()?);
            let attrs = u32::from_le_bytes(rec[68..72].try_into().ok()?);
            let name_len = u16::from_le_bytes(rec[72..74].try_into().ok()?) as usize;
            let name_off = u16::from_le_bytes(rec[74..76].try_into().ok()?) as usize;
            Some((fr, pr, reason, attrs, name_len, name_off, Some(id128)))
        }
        _ => None,
    }
}

fn volume_root_for_info(volume: &str) -> String {
    let v = volume.trim().trim_end_matches('\\');
    if v.len() == 1 && v.chars().next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false) {
        format!("{}:\\", v.to_ascii_uppercase())
    } else if v.len() == 2 && v.as_bytes()[1] == b':' {
        format!("{}\\", v.to_ascii_uppercase())
    } else {
        v.to_string()
    }
}

pub struct UsnJournalWatcher {
    pub volume_path: String,
}

impl UsnJournalWatcher {
    pub fn new(volume_path: impl Into<String>) -> Self {
        Self {
            volume_path: volume_path.into(),
        }
    }

    /// 查询当前 Journal ID 与游标（用于持久化 VolumeState）
    pub fn probe(&self) -> Result<UsnState> {
        let dev = volume_device_path(&self.volume_path);
        let h = open_volume_handle(&dev)?;
        let jd = unsafe { query_usn_journal(h)? };
        let _ = unsafe { CloseHandle(h) };
        Ok(UsnState {
            journal_id: jd.UsnJournalID,
            next_usn: jd.NextUsn as u64,
            first_usn: jd.FirstUsn,
        })
    }
}

/// 轮询 USN Journal：投递 `ChangeEvent`，并在每次成功 READ 后发送 `Checkpoint` 以便持久化 `last_usn`。
/// - `resume` 为 `None` 时从当前 `NextUsn` 起读（适合测试）；**增量续跑请传入上次落盘的 `UsnResume`。**
/// - 若磁盘上的 `UsnJournalID` 与 `resume.journal_id` 不一致（日志被重建），返回 Err，调用方应全量重建索引。
pub fn usn_watch_forever(
    volume_path: &str,
    resume: Option<UsnResume>,
    tx: Sender<UsnWatchMsg>,
) -> Result<()> {
    let dev = volume_device_path(volume_path);
    let h = open_volume_handle(&dev)?;
    let jd = unsafe { query_usn_journal(h)? };
    if let Some(r) = resume {
        if r.journal_id != jd.UsnJournalID {
            let _ = unsafe { CloseHandle(h) };
            return Err(findx2_core::Error::Platform(format!(
                "USN Journal ID 已变化（{} -> {}），请执行全量 index 重建",
                r.journal_id, jd.UsnJournalID
            )));
        }
    }
    let mut next_cursor = match resume {
        Some(r) => r.start_usn as i64,
        None => jd.NextUsn as i64,
    };
    let journal_id = jd.UsnJournalID;

    loop {
        let read_data = READ_USN_JOURNAL_DATA_V0 {
            StartUsn: next_cursor,
            ReasonMask: USN_REASON_FILE_CREATE
                | USN_REASON_FILE_DELETE
                | USN_REASON_RENAME_OLD_NAME
                | USN_REASON_RENAME_NEW_NAME
                | USN_REASON_DATA_EXTEND
                | USN_REASON_DATA_TRUNCATION
                | USN_REASON_BASIC_INFO_CHANGE,
            ReturnOnlyOnClose: 0,
            Timeout: 0,
            BytesToWaitFor: 0,
            UsnJournalID: journal_id,
        };

        let mut out = vec![0u8; 256 * 1024];
        let mut returned: u32 = 0;
        let ioctl_ok = unsafe {
            DeviceIoControl(
                h,
                FSCTL_READ_USN_JOURNAL,
                Some(&read_data as *const _ as *const _),
                size_of::<READ_USN_JOURNAL_DATA_V0>() as u32,
                Some(out.as_mut_ptr() as *mut _),
                out.len() as u32,
                Some(&mut returned),
                None,
            )
        };

        if ioctl_ok.is_err() {
            let _ = unsafe { CloseHandle(h) };
            return Err(findx2_core::Error::Platform(
                "FSCTL_READ_USN_JOURNAL 失败".into(),
            ));
        }

        if returned >= 8 {
            let slice = &out[..returned as usize];
            next_cursor = i64::from_le_bytes(slice[0..8].try_into().unwrap());
            let _ = tx.send(UsnWatchMsg::Checkpoint {
                journal_id,
                next_usn: next_cursor as u64,
            });

            let mut off = 8usize;
            while off + 8 <= slice.len() {
                let rec_full = &slice[off..];
                let record_len = u32::from_le_bytes(rec_full[0..4].try_into().unwrap()) as usize;
                if record_len < 8 || off + record_len > slice.len() {
                    break;
                }
                let rec = &slice[off..off + record_len];
                off += record_len;

                let Some((file_ref, parent_ref, reason, attrs, name_len, name_off, file_id_128)) =
                    parse_read_usn_record(rec)
                else {
                    continue;
                };
                if name_off + name_len > rec.len() {
                    continue;
                }
                let wide = u16_slice(&rec[name_off..name_off + name_len]);
                let name = OsString::from_wide(wide)
                    .to_string_lossy()
                    .into_owned();
                let is_dir = (attrs & 0x10) != 0;

                map_record_to_watch_msg(
                    h,
                    reason,
                    file_ref,
                    parent_ref,
                    &name,
                    attrs,
                    is_dir,
                    file_id_128,
                    &tx,
                );
            }
        }

        std::thread::sleep(Duration::from_millis(500));
    }
}

fn map_record_to_watch_msg(
    volume: windows::Win32::Foundation::HANDLE,
    reason: u32,
    file_ref: u64,
    parent_ref: u64,
    name: &str,
    attrs: u32,
    is_dir: bool,
    file_id_128: Option<[u8; 16]>,
    tx: &Sender<UsnWatchMsg>,
) {
    if (reason & USN_REASON_FILE_DELETE) != 0 {
        let _ = tx.send(UsnWatchMsg::Event(ChangeEvent::Delete { file_id: file_ref }));
        return;
    }
    if (reason & USN_REASON_FILE_CREATE) != 0 || (reason & USN_REASON_RENAME_NEW_NAME) != 0 {
        if let Some((size, mt, ct)) =
            unsafe { fetch_file_metadata_by_id(volume, file_ref, file_id_128) }
        {
            let _ = tx.send(UsnWatchMsg::Event(ChangeEvent::Create {
                entry: RawEntry {
                    file_id: file_ref,
                    file_id_128,
                    parent_id: parent_ref,
                    name: name.to_string(),
                    size,
                    mtime: mt,
                    ctime: ct,
                    attrs: attrs & 0xff,
                    is_dir,
                },
            }));
        }
        return;
    }
    if (reason & USN_REASON_RENAME_OLD_NAME) != 0 {
        let _ = tx.send(UsnWatchMsg::Event(ChangeEvent::Rename {
            file_id: file_ref,
            new_parent_id: parent_ref,
            new_name: name.to_string(),
        }));
        return;
    }
    if (reason & USN_REASON_DATA_EXTEND) != 0
        || (reason & USN_REASON_DATA_TRUNCATION) != 0
        || (reason & USN_REASON_BASIC_INFO_CHANGE) != 0
    {
        let mut sz = None;
        let mut mt = None;
        let mut ct = None;
        if let Some((size, mtime, ctime)) =
            unsafe { fetch_file_metadata_by_id(volume, file_ref, file_id_128) }
        {
            sz = Some(size);
            mt = Some(mtime);
            ct = Some(ctime);
        }
        let _ = tx.send(UsnWatchMsg::Event(ChangeEvent::DataOrMeta {
            file_id: file_ref,
            size: sz,
            mtime: mt,
            ctime: ct,
        }));
    }
}

impl ChangeWatcher for UsnJournalWatcher {
    fn watch(&self, tx: std::sync::mpsc::Sender<ChangeEvent>) -> Result<()> {
        let dev = volume_device_path(&self.volume_path);
        let h = open_volume_handle(&dev)?;
        let jd = unsafe { query_usn_journal(h)? };
        let mut next_cursor = jd.NextUsn as i64;

        loop {
            let read_data = READ_USN_JOURNAL_DATA_V0 {
                StartUsn: next_cursor,
                ReasonMask: USN_REASON_FILE_CREATE
                    | USN_REASON_FILE_DELETE
                    | USN_REASON_RENAME_OLD_NAME
                    | USN_REASON_RENAME_NEW_NAME
                    | USN_REASON_DATA_EXTEND
                    | USN_REASON_DATA_TRUNCATION
                    | USN_REASON_BASIC_INFO_CHANGE,
                ReturnOnlyOnClose: 0,
                Timeout: 0,
                BytesToWaitFor: 0,
                UsnJournalID: jd.UsnJournalID,
            };

            let mut out = vec![0u8; 256 * 1024];
            let mut returned: u32 = 0;
            let r = unsafe {
                DeviceIoControl(
                    h,
                    FSCTL_READ_USN_JOURNAL,
                    Some(&read_data as *const _ as *const _),
                    size_of::<READ_USN_JOURNAL_DATA_V0>() as u32,
                    Some(out.as_mut_ptr() as *mut _),
                    out.len() as u32,
                    Some(&mut returned),
                    None,
                )
            };

            if r.is_err() {
                let _ = unsafe { CloseHandle(h) };
                return Err(findx2_core::Error::Platform(
                    "FSCTL_READ_USN_JOURNAL 失败".into(),
                ));
            }

            if returned >= 8 {
                let slice = &out[..returned as usize];
                next_cursor = i64::from_le_bytes(slice[0..8].try_into().unwrap());
                let mut off = 8usize;
                while off + 8 <= slice.len() {
                    let rec_full = &slice[off..];
                    let record_len =
                        u32::from_le_bytes(rec_full[0..4].try_into().unwrap()) as usize;
                    if record_len < 8 || off + record_len > slice.len() {
                        break;
                    }
                    let rec = &slice[off..off + record_len];
                    off += record_len;

                    let Some((file_ref, parent_ref, reason, attrs, name_len, name_off, file_id_128)) =
                        parse_read_usn_record(rec)
                    else {
                        continue;
                    };
                    if name_off + name_len > rec.len() {
                        continue;
                    }
                    let wide = u16_slice(&rec[name_off..name_off + name_len]);
                    let name = OsString::from_wide(wide)
                        .to_string_lossy()
                        .into_owned();
                    let is_dir = (attrs & 0x10) != 0;

                    map_record_to_event(
                        h,
                        reason,
                        file_ref,
                        parent_ref,
                        &name,
                        attrs,
                        is_dir,
                        file_id_128,
                        &tx,
                    );
                }
            }

            std::thread::sleep(Duration::from_millis(500));
        }
    }
}

fn map_record_to_event(
    volume: windows::Win32::Foundation::HANDLE,
    reason: u32,
    file_ref: u64,
    parent_ref: u64,
    name: &str,
    attrs: u32,
    is_dir: bool,
    file_id_128: Option<[u8; 16]>,
    tx: &std::sync::mpsc::Sender<ChangeEvent>,
) {
    if (reason & USN_REASON_FILE_DELETE) != 0 {
        let _ = tx.send(ChangeEvent::Delete { file_id: file_ref });
        return;
    }
    if (reason & USN_REASON_FILE_CREATE) != 0 || (reason & USN_REASON_RENAME_NEW_NAME) != 0 {
        if let Some((size, mt, ct)) =
            unsafe { fetch_file_metadata_by_id(volume, file_ref, file_id_128) }
        {
            let _ = tx.send(ChangeEvent::Create {
                entry: RawEntry {
                    file_id: file_ref,
                    file_id_128,
                    parent_id: parent_ref,
                    name: name.to_string(),
                    size,
                    mtime: mt,
                    ctime: ct,
                    attrs: attrs & 0xff,
                    is_dir,
                },
            });
        }
        return;
    }
    if (reason & USN_REASON_RENAME_OLD_NAME) != 0 {
        let _ = tx.send(ChangeEvent::Rename {
            file_id: file_ref,
            new_parent_id: parent_ref,
            new_name: name.to_string(),
        });
        return;
    }
    if (reason & USN_REASON_DATA_EXTEND) != 0
        || (reason & USN_REASON_DATA_TRUNCATION) != 0
        || (reason & USN_REASON_BASIC_INFO_CHANGE) != 0
    {
        let mut sz = None;
        let mut mt = None;
        let mut ct = None;
        if let Some((size, mtime, ctime)) =
            unsafe { fetch_file_metadata_by_id(volume, file_ref, file_id_128) }
        {
            sz = Some(size);
            mt = Some(mtime);
            ct = Some(ctime);
        }
        let _ = tx.send(ChangeEvent::DataOrMeta {
            file_id: file_ref,
            size: sz,
            mtime: mt,
            ctime: ct,
        });
    }
}

/// 供 MFT 枚举与 watch 共用：`FSCTL_QUERY_USN_JOURNAL`。
pub(crate) unsafe fn query_usn_journal(
    vol: windows::Win32::Foundation::HANDLE,
) -> Result<USN_JOURNAL_DATA_V0> {
    let mut out = USN_JOURNAL_DATA_V0::default();
    let mut ret: u32 = 0;
    let ok = DeviceIoControl(
        vol,
        FSCTL_QUERY_USN_JOURNAL,
        None,
        0,
        Some(&mut out as *mut _ as *mut _),
        size_of::<USN_JOURNAL_DATA_V0>() as u32,
        Some(&mut ret),
        None,
    );
    if ok.is_err() {
        return Err(findx2_core::Error::Platform(
            "FSCTL_QUERY_USN_JOURNAL 失败".into(),
        ));
    }
    Ok(out)
}

fn open_volume_handle(path: &str) -> Result<windows::Win32::Foundation::HANDLE> {
    let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    let h = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    };
    match h {
        Ok(handle) if !handle.is_invalid() => Ok(handle),
        Ok(handle) => {
            let _ = unsafe { CloseHandle(handle) };
            Err(findx2_core::Error::Platform("卷句柄无效".into()))
        }
        Err(e) => Err(findx2_core::Error::Platform(format!("打开卷失败: {e}"))),
    }
}

fn volume_device_path(volume: &str) -> String {
    let v = volume.trim().trim_end_matches('\\');
    if v.len() == 1 && v.chars().next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false) {
        format!(r"\\.\{}:", v.to_ascii_uppercase())
    } else if v.ends_with(':') && v.len() == 2 {
        format!(r"\\.\{}", v.to_ascii_uppercase())
    } else if v.starts_with(r"\\.\") {
        v.to_string()
    } else {
        format!(r"\\.\{}", v)
    }
}

fn u16_slice(bytes: &[u8]) -> &[u16] {
    let len = bytes.len() / 2;
    unsafe { std::slice::from_raw_parts(bytes.as_ptr() as *const u16, len) }
}
