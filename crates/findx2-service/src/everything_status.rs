//! Everything SDK 1.4 readiness and WM_COPYDATA reply layout.
//! Pure functions so the status matrix and query bytes can be tested off Windows.
use std::io::Write;

pub(crate) const SDK_WINDOW_CLASS_IPC: &str = "EVERYTHING_TASKBAR_NOTIFICATION";
pub(crate) const SDK_WINDOW_CLASS_MAIN: &str = "EVERYTHING";
pub(crate) const COMPETING_PROCESS_POLICY: &str = "register the SDK 1.4 class names; do not close, hide, or restack a foreign Everything window; do not create a private class that current clients cannot find";

const ITEM_FOLDER: u32 = 0x01;
const REQUEST_NAME: u32 = 0x0000_0001;
const REQUEST_PATH: u32 = 0x0000_0002;
const REQUEST_FULL_PATH_AND_NAME: u32 = 0x0000_0004;
const REQUEST_EXTENSION: u32 = 0x0000_0008;
const REQUEST_SIZE: u32 = 0x0000_0010;
const REQUEST_DATE_CREATED: u32 = 0x0000_0020;
const REQUEST_DATE_MODIFIED: u32 = 0x0000_0040;
const REQUEST_DATE_ACCESSED: u32 = 0x0000_0080;
const REQUEST_ATTRIBUTES: u32 = 0x0000_0100;
const REQUEST_FILE_LIST_FILE_NAME: u32 = 0x0000_0200;
const REQUEST_RUN_COUNT: u32 = 0x0000_0400;
const REQUEST_DATE_RUN: u32 = 0x0000_0800;
const REQUEST_DATE_RECENTLY_CHANGED: u32 = 0x0000_1000;
const REQUEST_HIGHLIGHTED_NAME: u32 = 0x0000_2000;
const REQUEST_HIGHLIGHTED_PATH: u32 = 0x0000_4000;
const REQUEST_HIGHLIGHTED_FULL_PATH_AND_NAME: u32 = 0x0000_8000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StatusParts {
    pub loading: bool,
    pub metadata_ready: bool,
    pub backfill_done: u64,
    pub backfill_total: u64,
    pub drive_mask: u32,
    pub volume_letter: Option<char>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EngineSnapshot {
    pub reachable: bool,
    pub loading: bool,
    pub searchable: bool,
    pub updating: bool,
    pub metadata_complete: bool,
    pub drive_mask: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SdkFlags {
    pub loaded: isize,
    pub busy: isize,
    pub file_info_indexed: isize,
}

/// Last index view that was actually read. A contended writer must not replace it
/// with an empty loading snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ResidentIndex {
    pub entry_count: u64,
    pub dir_count: u64,
    pub last_usn: u64,
    pub journal_id: u64,
    pub volume_letter: Option<char>,
    pub metadata_ready: bool,
    pub tombstone_count: u64,
    pub drive_mask: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LockedStatus {
    pub loading: bool,
    pub entry_count: u64,
    pub dir_count: u64,
    pub last_usn: u64,
    pub journal_id: u64,
    pub volume_letter: Option<char>,
    pub metadata_ready: bool,
    pub backfill_done: u64,
    pub backfill_total: u64,
    pub healthy: bool,
    pub tombstone_count: Option<u64>,
    pub drive_mask: u32,
}

/// No resident snapshot yet: the process is still loading, and zeros mean that.
/// After a real snapshot exists, lock contention keeps those counts, stays
/// searchable, and still reports the current watch health. It does not mark
/// metadata complete.
pub(crate) fn status_when_index_locked(
    previous: Option<&ResidentIndex>,
    backfill_done: u64,
    backfill_total: u64,
    watch_ok: bool,
) -> LockedStatus {
    match previous {
        None => LockedStatus {
            loading: true,
            entry_count: 0,
            dir_count: 0,
            last_usn: 0,
            journal_id: 0,
            volume_letter: None,
            metadata_ready: false,
            backfill_done,
            backfill_total,
            healthy: watch_ok,
            tombstone_count: None,
            drive_mask: 0,
        },
        Some(prev) => LockedStatus {
            loading: false,
            entry_count: prev.entry_count,
            dir_count: prev.dir_count,
            last_usn: prev.last_usn,
            journal_id: prev.journal_id,
            volume_letter: prev.volume_letter,
            metadata_ready: prev.metadata_ready,
            backfill_done,
            backfill_total,
            healthy: watch_ok,
            tombstone_count: Some(prev.tombstone_count),
            drive_mask: prev.drive_mask,
        },
    }
}

pub(crate) fn drive_mask_from_letters(letters: impl IntoIterator<Item = u8>) -> u32 {
    let mut mask = 0u32;
    for letter in letters {
        let c = letter.to_ascii_uppercase();
        if (b'A'..=b'Z').contains(&c) {
            mask |= 1 << (c - b'A');
        }
    }
    mask
}

fn mask_of(parts: StatusParts) -> u32 {
    if parts.drive_mask != 0 {
        parts.drive_mask
    } else {
        parts.volume_letter.map(|ch| drive_mask_from_letters([ch as u8])).unwrap_or(0)
    }
}

/// `None` is offline or a timed-out pipe. A resident engine is searchable even
/// when metadata is only partial or the index is empty. Busy means a load or
/// an actual backfill is in progress, not merely `metadata_ready == false`.
pub(crate) fn snapshot_from_status(parts: Option<StatusParts>) -> EngineSnapshot {
    let Some(parts) = parts else {
        return EngineSnapshot { reachable: false, loading: false, searchable: false, updating: false, metadata_complete: false, drive_mask: 0 };
    };
    let updating = parts.loading || (!parts.metadata_ready && parts.backfill_total > 0 && parts.backfill_done < parts.backfill_total);
    EngineSnapshot {
        reachable: true,
        loading: parts.loading,
        searchable: !parts.loading,
        updating,
        metadata_complete: parts.metadata_ready,
        drive_mask: mask_of(parts),
    }
}

pub(crate) fn sdk_flags(snapshot: EngineSnapshot, external_busy: bool) -> SdkFlags {
    let loaded = snapshot.reachable && snapshot.searchable && !snapshot.loading;
    let busy = snapshot.reachable && (external_busy || snapshot.loading || snapshot.updating);
    SdkFlags {
        loaded: if loaded { 1 } else { 0 },
        busy: if busy { 1 } else { 0 },
        file_info_indexed: if loaded && snapshot.metadata_complete { 1 } else { 0 },
    }
}

pub(crate) fn drive_slot(raw: u32) -> Option<u32> {
    if raw <= 25 {
        return Some(raw);
    }
    let c = char::from_u32(raw)?;
    if c.is_ascii_alphabetic() {
        Some(u32::from(c.to_ascii_uppercase() as u8 - b'A'))
    } else {
        None
    }
}

pub(crate) fn drive_indexed(mask: u32, raw: u32) -> u8 {
    match drive_slot(raw) {
        Some(slot) if mask & (1 << slot) != 0 => 1,
        _ => 0,
    }
}

/// Sorts the engine actually applies. Degraded sorts and an unloaded database are 0.
pub(crate) fn fast_sort_supported(loaded: bool, sort_type: u32) -> u8 {
    if !loaded {
        return 0;
    }
    match sort_type {
        1 | 2 | 3 | 4 | 5 | 6 | 11 | 12 | 13 | 14 | 21 | 22 => 1,
        _ => 0,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Query1 {
    pub max_results: u32,
    pub offset: u32,
    pub reply_msg: u32,
    pub search_flags: u32,
    pub search: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Query2 {
    pub reply_msg: u32,
    pub search_flags: u32,
    pub offset: u32,
    pub max_results: u32,
    pub request_flags: u32,
    pub sort_type: u32,
    pub search: String,
}

fn read_u32_le(data: &[u8], off: usize) -> Option<u32> {
    data.get(off..off + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn extract_wstring(data: &[u8]) -> String {
    if data.len() < 2 { return String::new(); }
    let mut u16s = Vec::with_capacity(data.len() / 2);
    for chunk in data.chunks_exact(2) {
        let v = u16::from_le_bytes([chunk[0], chunk[1]]);
        if v == 0 { break; }
        u16s.push(v);
    }
    String::from_utf16_lossy(&u16s)
}

fn extract_astring(data: &[u8]) -> String {
    let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    String::from_utf8_lossy(&data[..end]).into_owned()
}

pub(crate) fn parse_query1(data: &[u8], wide: bool) -> Option<Query1> {
    if data.len() < 20 { return None; }
    let reply_msg = read_u32_le(data, 8).unwrap_or(0);
    if reply_msg == 0 { return None; }
    Some(Query1 {
        max_results: read_u32_le(data, 0).unwrap_or(100),
        offset: read_u32_le(data, 4).unwrap_or(0),
        reply_msg,
        search_flags: read_u32_le(data, 12).unwrap_or(0),
        search: if wide { extract_wstring(&data[20..]) } else { extract_astring(&data[20..]) },
    })
}

pub(crate) fn parse_query2(data: &[u8], wide: bool) -> Option<Query2> {
    if data.len() < 28 { return None; }
    let reply_msg = read_u32_le(data, 4)?;
    if reply_msg == 0 { return None; }
    Some(Query2 {
        reply_msg,
        search_flags: read_u32_le(data, 8).unwrap_or(0),
        offset: read_u32_le(data, 12).unwrap_or(0),
        max_results: read_u32_le(data, 16).unwrap_or(100),
        request_flags: read_u32_le(data, 20).unwrap_or(0),
        sort_type: read_u32_le(data, 24).unwrap_or(0),
        search: if wide { extract_wstring(&data[28..]) } else { extract_astring(&data[28..]) },
    })
}

pub(crate) struct IpcHit {
    pub(crate) full_path: String,
    pub(crate) name: String,
    pub(crate) parent: String,
    pub(crate) is_dir: bool,
    pub(crate) size: u64,
    pub(crate) mtime: u64,
    pub(crate) ctime: u64,
    pub(crate) attrs: u32,
}

pub(crate) fn build_list_w(results: &[IpcHit], offset: u32, start: usize, count: usize) -> Vec<u8> {
    let mut tot_folders = 0u32;
    let mut tot_files = 0u32;
    for r in results {
        if r.is_dir {
            tot_folders += 1;
        } else {
            tot_files += 1;
        }
    }
    let mut strings: Vec<u8> = Vec::new();
    let mut items: Vec<(u32, usize, usize)> = Vec::new();
    let mut num_folders = 0u32;
    let mut num_files = 0u32;
    for i in start..start + count {
        let r = &results[i];
        let fn_off = strings.len() / 2;
        for u in r.name.encode_utf16() {
            strings.extend_from_slice(&u.to_le_bytes());
        }
        strings.extend_from_slice(&[0, 0]);
        let path_off = strings.len() / 2;
        for u in r.parent.encode_utf16() {
            strings.extend_from_slice(&u.to_le_bytes());
        }
        strings.extend_from_slice(&[0, 0]);
        let fl = if r.is_dir {
            num_folders += 1;
            ITEM_FOLDER
        } else {
            num_files += 1;
            0
        };
        items.push((fl, fn_off, path_off));
    }

    let header_size = 28usize;
    let str_base = header_size + count * 12;
    let total = str_base + strings.len();
    let mut buf = vec![0u8; total];

    buf[0..4].copy_from_slice(&tot_folders.to_le_bytes());
    buf[4..8].copy_from_slice(&tot_files.to_le_bytes());
    buf[8..12].copy_from_slice(&(results.len() as u32).to_le_bytes());
    buf[12..16].copy_from_slice(&num_folders.to_le_bytes());
    buf[16..20].copy_from_slice(&num_files.to_le_bytes());
    buf[20..24].copy_from_slice(&(count as u32).to_le_bytes());
    buf[24..28].copy_from_slice(&offset.to_le_bytes());

    for (i, (fl, fn_off, pa_off)) in items.iter().enumerate() {
        let pos = header_size + i * 12;
        buf[pos..pos + 4].copy_from_slice(&fl.to_le_bytes());
        buf[pos + 4..pos + 8].copy_from_slice(&((str_base + fn_off * 2) as u32).to_le_bytes());
        buf[pos + 8..pos + 12].copy_from_slice(&((str_base + pa_off * 2) as u32).to_le_bytes());
    }
    buf[str_base..str_base + strings.len()].copy_from_slice(&strings);
    buf
}

/// QUERYA 回复：与 QUERYW 同 header（28B）+ 同 item 表（12B），但字符串区按 ANSI 字节、偏移以字节计算。
pub(crate) fn build_list_a(results: &[IpcHit], offset: u32, start: usize, count: usize) -> Vec<u8> {
    let mut tot_folders = 0u32;
    let mut tot_files = 0u32;
    for r in results {
        if r.is_dir {
            tot_folders += 1;
        } else {
            tot_files += 1;
        }
    }
    let mut strings: Vec<u8> = Vec::new();
    let mut items: Vec<(u32, usize, usize)> = Vec::new();
    let mut num_folders = 0u32;
    let mut num_files = 0u32;
    for i in start..start + count {
        let r = &results[i];
        let fn_off = strings.len();
        strings.extend_from_slice(r.name.as_bytes());
        strings.push(0);
        let path_off = strings.len();
        strings.extend_from_slice(r.parent.as_bytes());
        strings.push(0);
        let fl = if r.is_dir {
            num_folders += 1;
            ITEM_FOLDER
        } else {
            num_files += 1;
            0
        };
        items.push((fl, fn_off, path_off));
    }

    let header_size = 28usize;
    let str_base = header_size + count * 12;
    let total = str_base + strings.len();
    let mut buf = vec![0u8; total];

    buf[0..4].copy_from_slice(&tot_folders.to_le_bytes());
    buf[4..8].copy_from_slice(&tot_files.to_le_bytes());
    buf[8..12].copy_from_slice(&(results.len() as u32).to_le_bytes());
    buf[12..16].copy_from_slice(&num_folders.to_le_bytes());
    buf[16..20].copy_from_slice(&num_files.to_le_bytes());
    buf[20..24].copy_from_slice(&(count as u32).to_le_bytes());
    buf[24..28].copy_from_slice(&offset.to_le_bytes());

    for (i, (fl, fn_off, pa_off)) in items.iter().enumerate() {
        let pos = header_size + i * 12;
        buf[pos..pos + 4].copy_from_slice(&fl.to_le_bytes());
        buf[pos + 4..pos + 8].copy_from_slice(&((str_base + fn_off) as u32).to_le_bytes());
        buf[pos + 8..pos + 12].copy_from_slice(&((str_base + pa_off) as u32).to_le_bytes());
    }
    buf[str_base..str_base + strings.len()].copy_from_slice(&strings);
    buf
}

pub(crate) fn build_list2(
    results: &[IpcHit],
    offset: u32,
    start: usize,
    count: usize,
    request_flags: u32,
    wide: bool,
) -> Vec<u8> {
    let header_size = 20usize;
    let items_size = count * 8;
    let data_start = header_size + items_size;

    let mut payload: Vec<u8> = Vec::new();
    let mut data_offsets: Vec<u32> = Vec::with_capacity(count);

    for i in start..start + count {
        let r = &results[i];
        data_offsets.push((data_start + payload.len()) as u32);
        write_item2_payload(&mut payload, r, request_flags, wide);
    }

    let mut buf = vec![0u8; data_start + payload.len()];
    buf[data_start..].copy_from_slice(&payload);

    buf[0..4].copy_from_slice(&(results.len() as u32).to_le_bytes());
    buf[4..8].copy_from_slice(&(count as u32).to_le_bytes());
    buf[8..12].copy_from_slice(&offset.to_le_bytes());
    buf[12..16].copy_from_slice(&request_flags.to_le_bytes());
    buf[16..20].copy_from_slice(&1u32.to_le_bytes());

    for i in 0..count {
        let pos = header_size + i * 8;
        let r = &results[start + i];
        let flags = if r.is_dir { ITEM_FOLDER } else { 0 };
        buf[pos..pos + 4].copy_from_slice(&flags.to_le_bytes());
        buf[pos + 4..pos + 8].copy_from_slice(&data_offsets[i].to_le_bytes());
    }

    buf
}

fn write_ipc_string(b: &mut Vec<u8>, s: &str, wide: bool) {
    if wide {
        let n = s.encode_utf16().count() as u32;
        b.write_all(&n.to_le_bytes()).ok();
        for u in s.encode_utf16() {
            b.write_all(&u.to_le_bytes()).ok();
        }
        b.write_all(&0u16.to_le_bytes()).ok();
    } else {
        let bytes = s.as_bytes();
        let n = bytes.len() as u32;
        b.write_all(&n.to_le_bytes()).ok();
        b.write_all(bytes).ok();
        b.write_all(&[0u8]).ok();
    }
}

fn write_item2_payload(ms: &mut Vec<u8>, r: &IpcHit, rf: u32, wide: bool) {
    if rf & REQUEST_NAME != 0 {
        write_ipc_string(ms, &r.name, wide);
    }
    if rf & REQUEST_PATH != 0 {
        write_ipc_string(ms, &r.parent, wide);
    }
    if rf & REQUEST_FULL_PATH_AND_NAME != 0 {
        write_ipc_string(ms, &r.full_path, wide);
    }
    if rf & REQUEST_EXTENSION != 0 {
        let ext = if r.is_dir {
            String::new()
        } else {
            r.name
                .rfind('.')
                .map(|i| r.name[i + 1..].to_string())
                .unwrap_or_default()
        };
        write_ipc_string(ms, &ext, wide);
    }
    if rf & REQUEST_SIZE != 0 {
        ms.write_all(&r.size.to_le_bytes()).ok();
    }
    if rf & REQUEST_DATE_CREATED != 0 {
        ms.write_all(&(r.ctime as i64).to_le_bytes()).ok();
    }
    if rf & REQUEST_DATE_MODIFIED != 0 {
        ms.write_all(&(r.mtime as i64).to_le_bytes()).ok();
    }
    if rf & REQUEST_DATE_ACCESSED != 0 {
        ms.write_all(&0i64.to_le_bytes()).ok();
    }
    if rf & REQUEST_ATTRIBUTES != 0 {
        // Win32 文件属性宏：低 8 位由 USN/MFT 直接给出（READONLY=0x1, HIDDEN=0x2, SYSTEM=0x4, DIRECTORY=0x10, ARCHIVE=0x20, …）
        ms.write_all(&r.attrs.to_le_bytes()).ok();
    }
    if rf & REQUEST_FILE_LIST_FILE_NAME != 0 {
        write_ipc_string(ms, "", wide);
    }
    if rf & REQUEST_RUN_COUNT != 0 {
        ms.write_all(&0u32.to_le_bytes()).ok();
    }
    if rf & REQUEST_DATE_RUN != 0 {
        ms.write_all(&0i64.to_le_bytes()).ok();
    }
    if rf & REQUEST_DATE_RECENTLY_CHANGED != 0 {
        ms.write_all(&0i64.to_le_bytes()).ok();
    }
    if rf & REQUEST_HIGHLIGHTED_NAME != 0 {
        write_ipc_string(ms, &r.name, wide);
    }
    if rf & REQUEST_HIGHLIGHTED_PATH != 0 {
        write_ipc_string(ms, &r.parent, wide);
    }
    if rf & REQUEST_HIGHLIGHTED_FULL_PATH_AND_NAME != 0 {
        write_ipc_string(ms, &r.full_path, wide);
    }
}

fn u32_at(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}

fn utf16_at(buf: &[u8], off: usize) -> String {
    let mut units = Vec::new();
    let mut i = off;
    while i + 1 < buf.len() {
        let v = u16::from_le_bytes([buf[i], buf[i + 1]]);
        if v == 0 { break; }
        units.push(v);
        i += 2;
    }
    String::from_utf16_lossy(&units)
}

fn sample(dir: bool) -> IpcHit {
    IpcHit {
        full_path: if dir { "C:\\Windows".into() } else { "C:\\Windows\\notepad.exe".into() },
        name: if dir { "Windows".into() } else { "notepad.exe".into() },
        parent: if dir { "C:".into() } else { "C:\\Windows".into() },
        is_dir: dir,
        size: if dir { 0 } else { 42 },
        mtime: 100,
        ctime: 80,
        attrs: if dir { 0x10 } else { 0x20 },
    }
}

fn parts(loading: bool, ready: bool, done: u64, total: u64, mask: u32) -> StatusParts {
    StatusParts { loading, metadata_ready: ready, backfill_done: done, backfill_total: total, drive_mask: mask, volume_letter: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searchable_partial_metadata_is_loaded_and_not_busy() {
        let live = snapshot_from_status(Some(parts(false, false, 0, 0, 1 << 2)));
        let flags = sdk_flags(live, false);
        assert_eq!(flags, SdkFlags { loaded: 1, busy: 0, file_info_indexed: 0 });
        assert_eq!(drive_indexed(live.drive_mask, 0), 0);
        assert_eq!(drive_indexed(live.drive_mask, 2), 1);
        assert_eq!(drive_indexed(live.drive_mask, u32::from(b'C')), 1);
        assert_eq!(drive_indexed(live.drive_mask, u32::from(b'A')), 0);
    }

    #[test]
    fn contended_lock_keeps_the_searchable_snapshot_and_watch_health() {
        let previous = ResidentIndex {
            entry_count: 4_337_248,
            dir_count: 585_701,
            last_usn: 97_173_513_536,
            journal_id: 134_249_285_381_003_141,
            volume_letter: Some('C'),
            metadata_ready: false,
            tombstone_count: 167_713,
            drive_mask: 1 << 2,
        };
        let locked = status_when_index_locked(Some(&previous), 2_390_813, 3_575_219, false);
        assert!(!locked.loading);
        assert_eq!(locked.entry_count, previous.entry_count);
        assert_eq!(locked.last_usn, previous.last_usn);
        assert_eq!(locked.journal_id, previous.journal_id);
        assert_eq!(locked.volume_letter, Some('C'));
        assert!(!locked.metadata_ready);
        assert!(!locked.healthy);
        assert_eq!(locked.backfill_done, 2_390_813);
        let snap = snapshot_from_status(Some(StatusParts {
            loading: locked.loading,
            metadata_ready: locked.metadata_ready,
            backfill_done: locked.backfill_done,
            backfill_total: locked.backfill_total,
            drive_mask: locked.drive_mask,
            volume_letter: locked.volume_letter,
        }));
        assert_eq!(sdk_flags(snap, false), SdkFlags { loaded: 1, busy: 1, file_info_indexed: 0 });
        let cold = status_when_index_locked(None, 0, 0, true);
        assert!(cold.loading && cold.entry_count == 0 && cold.last_usn == 0 && cold.journal_id == 0 && cold.volume_letter.is_none());
        assert_eq!(sdk_flags(snapshot_from_status(Some(parts(cold.loading, cold.metadata_ready, cold.backfill_done, cold.backfill_total, cold.drive_mask))), false).loaded, 0);
    }

    fn empty_ready_loading_backfill_and_offline_stay_distinct() {
        let empty = sdk_flags(snapshot_from_status(Some(parts(false, true, 0, 0, 0))), false);
        assert_eq!(empty, SdkFlags { loaded: 1, busy: 0, file_info_indexed: 1 });
        let loading = sdk_flags(snapshot_from_status(Some(parts(true, false, 0, 10, 0))), false);
        assert_eq!(loading, SdkFlags { loaded: 0, busy: 1, file_info_indexed: 0 });
        let backfill = sdk_flags(snapshot_from_status(Some(parts(false, false, 3, 10, 0))), false);
        assert_eq!(backfill, SdkFlags { loaded: 1, busy: 1, file_info_indexed: 0 });
        let offline = sdk_flags(snapshot_from_status(None), false);
        assert_eq!(offline, SdkFlags { loaded: 0, busy: 0, file_info_indexed: 0 });
        let forced = sdk_flags(snapshot_from_status(Some(parts(false, true, 0, 0, 0))), true);
        assert_eq!(forced.busy, 1);
        assert_eq!(forced.loaded, 1);
        assert_eq!(fast_sort_supported(false, 1), 0);
        assert_eq!(fast_sort_supported(true, 1), 1);
        assert_eq!(fast_sort_supported(true, 7), 0);
    }

    #[test]
    fn volume_letter_fallback_does_not_mark_every_drive() {
        let snap = snapshot_from_status(Some(StatusParts {
            loading: false,
            metadata_ready: false,
            backfill_done: 0,
            backfill_total: 0,
            drive_mask: 0,
            volume_letter: Some('C'),
        }));
        assert_eq!(drive_indexed(snap.drive_mask, 2), 1);
        assert_eq!(drive_indexed(snap.drive_mask, 0), 0);
    }

    #[test]
    fn sdk_query_w_reply_uses_offset_8_and_exact_strings() {
        let mut req = vec![0u8; 20];
        req[0..4].copy_from_slice(&5u32.to_le_bytes());
        req[8..12].copy_from_slice(&0x8000u32.to_le_bytes());
        for u in "windows".encode_utf16().chain(Some(0)) {
            req.extend_from_slice(&u.to_le_bytes());
        }
        let q = parse_query1(&req, true).unwrap();
        assert_eq!(q.reply_msg, 0x8000);
        assert_eq!(q.search, "windows");
        assert!(parse_query1(&req[..19], true).is_none());
        let mut missing = req.clone();
        missing[8..12].fill(0);
        assert!(parse_query1(&missing, true).is_none());

        let hits = [sample(true), sample(false)];
        let buf = build_list_w(&hits, 0, 0, 2);
        assert_eq!(u32_at(&buf, 0), 1);
        assert_eq!(u32_at(&buf, 4), 1);
        assert_eq!(u32_at(&buf, 20), 2);
        assert_eq!(u32_at(&buf, 28), 1);
        assert_eq!(u32_at(&buf, 40), 0);
        let name_off = u32_at(&buf, 32) as usize;
        let path_off = u32_at(&buf, 36) as usize;
        assert!(name_off < buf.len() && path_off < buf.len());
        assert_eq!(utf16_at(&buf, name_off), "Windows");
        assert_eq!(utf16_at(&buf, path_off), "C:");
        let file_name = u32_at(&buf, 28 + 12 + 4) as usize;
        assert_eq!(utf16_at(&buf, file_name), "notepad.exe");
        let ansi = build_list_a(&hits, 0, 0, 2);
        assert_eq!(u32_at(&ansi, 28), 1);
        let ansi_name = u32_at(&ansi, 32) as usize;
        assert_eq!(&ansi[ansi_name..ansi_name + 7], b"Windows");
    }

    #[test]
    fn sdk_query2_carries_requested_name_path_and_size() {
        let mut req = vec![0u8; 28];
        req[4..8].copy_from_slice(&7u32.to_le_bytes());
        req[16..20].copy_from_slice(&1u32.to_le_bytes());
        req[20..24].copy_from_slice(&(REQUEST_NAME | REQUEST_PATH | REQUEST_SIZE).to_le_bytes());
        req[24..28].copy_from_slice(&5u32.to_le_bytes());
        req.extend_from_slice(b"notepad\0");
        let q = parse_query2(&req, false).unwrap();
        assert_eq!(q.reply_msg, 7);
        assert_eq!(q.max_results, 1);
        assert_eq!(q.sort_type, 5);
        assert_eq!(q.search, "notepad");
        let buf = build_list2(&[sample(false)], 0, 0, 1, q.request_flags, false);
        assert_eq!(u32_at(&buf, 0), 1);
        assert_eq!(u32_at(&buf, 4), 1);
        assert_eq!(u32_at(&buf, 12), REQUEST_NAME | REQUEST_PATH | REQUEST_SIZE);
        assert_eq!(u32_at(&buf, 20), 0);
        let data = u32_at(&buf, 24) as usize;
        let name_len = u32_at(&buf, data) as usize;
        assert_eq!(&buf[data + 4..data + 4 + name_len], b"notepad.exe");
        let path_at = data + 4 + name_len + 1;
        let path_len = u32_at(&buf, path_at) as usize;
        assert_eq!(&buf[path_at + 4..path_at + 4 + path_len], b"C:\\Windows");
        let size_at = path_at + 4 + path_len + 1;
        assert_eq!(u64::from_le_bytes(buf[size_at..size_at + 8].try_into().unwrap()), 42);
    }

    #[test]
    fn competing_window_policy_keeps_the_sdk_class_names() {
        assert_eq!(SDK_WINDOW_CLASS_IPC, "EVERYTHING_TASKBAR_NOTIFICATION");
        assert_eq!(SDK_WINDOW_CLASS_MAIN, "EVERYTHING");
        assert!(COMPETING_PROCESS_POLICY.contains("do not close"));
        assert!(COMPETING_PROCESS_POLICY.contains("do not create a private class"));
    }
}
