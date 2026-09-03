//! USN Journal：QUERY / READ 轮询，映射为 ChangeEvent。

use std::ffi::OsString;
use std::mem::size_of;
use std::os::windows::ffi::OsStringExt;
use std::ptr;
use std::sync::mpsc::Sender;
use std::collections::HashMap;

use findx2_core::{ChangeEvent, Result};
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
    /// 元数据补拉请求（后台 stat worker 消费；CLI 可同步执行或忽略）。
    /// watch 线程只解析 journal、不做任何 `OpenFileById`，stat 全部走这里异步化——
    /// bulk 复制/解压时 journal 解析（顺序小 IO）与 stat（随机 IO）解耦，索引名秒级可见。
    StatRefresh {
        file_id: u64,
        file_id_128: Option<[u8; 16]>,
    },
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

/// 轻量 journal 状态查询（P2 覆写预警用，不持句柄）。
/// 返回 `(journal_id, first_usn, next_usn)`。
pub fn query_journal_state(volume_path: &str) -> Result<(u64, i64, i64)> {
    let dev = volume_device_path(volume_path);
    let h = open_volume_handle(&dev)?;
    let _close = scope_close(h);
    let jd = unsafe { query_usn_journal(h)? };
    Ok((jd.UsnJournalID, jd.FirstUsn, jd.NextUsn))
}

/// 轮询 USN Journal：投递 `ChangeEvent`，并在每次成功 READ 后发送 `Checkpoint` 以便持久化 `last_usn`。
/// - `resume` 为 `None` 时从当前 `NextUsn` 起读（适合测试）；**增量续跑请传入上次落盘的 `UsnResume`。**
/// - 若磁盘上的 `UsnJournalID` 与 `resume.journal_id` 不一致（日志被重建），返回
///   `Error::JournalGap`，调用方应全量重建索引。
/// - 若 `resume.start_usn` 已小于当前 `FirstUsn`（日志覆写过游标，变更已丢失），同样返回
///   `Error::JournalGap`——继续增量会静默丢数，必须重建。
/// - 等待模式：阻塞式 READ（`BytesToWaitFor` + `Timeout`），有数据立刻返回、无数据挂起
///   至多 `Timeout` 秒。相比 sleep 轮询：延迟 500ms→~实时，空闲零 syscall。
/// - 同一批 READ 内同 FRN 多记录合并（reason 按位或、取最新 name/parent），bulk 场景下
///   `OpenFileById` 次数从 N 降到 1；且 watch 线程本身永不 stat（见 `StatRefresh`）。
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
            return Err(findx2_core::Error::JournalGap(format!(
                "USN Journal ID 已变化（{} -> {}），请执行全量 index 重建",
                r.journal_id, jd.UsnJournalID
            )));
        }
        if (r.start_usn as i64) < jd.FirstUsn {
            let _ = unsafe { CloseHandle(h) };
            return Err(findx2_core::Error::JournalGap(format!(
                "USN Journal 已覆写过游标（start_usn={} < FirstUsn={}），中间变更已丢失，请执行全量 index 重建",
                r.start_usn, jd.FirstUsn
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
            // 阻塞等待：无数据时挂起至多 2 秒（有数据立刻返回）。同步句柄下有效；
            // 64KB 未过滤增量攒够或超时即返回，避免小批量高频唤醒。
            ReturnOnlyOnClose: 0,
            Timeout: 2,
            BytesToWaitFor: 64 * 1024,
            UsnJournalID: journal_id,
        };

        // 追赶期（停机后首次 catch-up）大批量一次吃完，减少 ioctl 往返。
        let mut out = vec![0u8; 1024 * 1024];
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

        if let Err(e) = ioctl_ok {
            let _ = unsafe { CloseHandle(h) };
            // 游标被覆写（停机期间变更太多）：静默继续=丢数，必须重建。
            if is_journal_entry_deleted(&e) {
                return Err(findx2_core::Error::JournalGap(format!(
                    "USN Journal 已覆写过游标（ERROR_JOURNAL_ENTRY_DELETED），请执行全量 index 重建"
                )));
            }
            return Err(findx2_core::Error::Platform(format!(
                "FSCTL_READ_USN_JOURNAL 失败: {e}"
            )));
        }

        if returned >= 8 {
            let slice = &out[..returned as usize];
            next_cursor = i64::from_le_bytes(slice[0..8].try_into().unwrap());
            let _ = tx.send(UsnWatchMsg::Checkpoint {
                journal_id,
                next_usn: next_cursor as u64,
            });

            // 整批先解析、按 FRN 合并，再统一投递（见 coalesce_watch_batch）。
            let mut batch: Vec<ParsedWatchRec> = Vec::new();
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
                batch.push(ParsedWatchRec {
                    file_ref,
                    parent_ref,
                    reason,
                    attrs,
                    name,
                    is_dir,
                    file_id_128,
                });
            }
            for rec in coalesce_watch_batch(batch) {
                emit_coalesced_watch_rec(&rec, &tx);
            }
        }
        // 无 sleep：阻塞式 READ 自带等待节拍；有数据时紧追，无数据时内核挂起。
    }
}

/// 单条已解析的 USN 监听记录（批合并的输入单元）。
struct ParsedWatchRec {
    file_ref: u64,
    parent_ref: u64,
    reason: u32,
    attrs: u32,
    name: String,
    is_dir: bool,
    file_id_128: Option<[u8; 16]>,
}

/// 同一批 READ 内同 FRN 多记录合并：reason 按位或，name/parent/attrs 取最后一条。
/// 保序：输出按 FRN 首次出现顺序（跨文件因果 preserved；单文件风暴压成一条）。
/// 纯函数，可单测（无需管理员/USN）。
fn coalesce_watch_batch(recs: Vec<ParsedWatchRec>) -> Vec<ParsedWatchRec> {
    let mut order: Vec<u64> = Vec::new();
    let mut map: HashMap<u64, ParsedWatchRec> = HashMap::new();
    for r in recs {
        if let Some(acc) = map.get_mut(&r.file_ref) {
            acc.reason |= r.reason;
            acc.parent_ref = r.parent_ref;
            acc.name = r.name;
            acc.attrs = r.attrs;
            acc.is_dir = r.is_dir;
            if r.file_id_128.is_some() {
                acc.file_id_128 = r.file_id_128;
            }
        } else {
            order.push(r.file_ref);
            map.insert(r.file_ref, r);
        }
    }
    order
        .into_iter()
        .filter_map(|k| map.remove(&k))
        .collect()
}

/// 合并后的单条投递（watch 线程内，永不做 `OpenFileById`）：
/// - Delete（含其它位时先删后走新建分支——FRN 复用场景）；
/// - OLD+NEW 配对 → 一条 Rename（NEW 侧 parent/name）；
/// - 仅 OLD → Rename（旧语义；下一批的 NEW_NAME 会以 CreatePending 补齐）；
/// - CREATE / 仅 NEW_NAME → CreatePending（0 元数据入库，名字立即可搜）+ StatRefresh；
/// - 纯 DATA/META → 不入库（名字未变），只 StatRefresh 排队异步补 meta。
fn emit_coalesced_watch_rec(rec: &ParsedWatchRec, tx: &Sender<UsnWatchMsg>) {
    if (rec.reason & USN_REASON_FILE_DELETE) != 0 {
        let _ = tx.send(UsnWatchMsg::Event(ChangeEvent::Delete {
            file_id: rec.file_ref,
        }));
        // 不 return：同批同 FRN 若还有 CREATE/NEW_NAME（FRN 复用），继续走新建分支。
    }
    let has_old = (rec.reason & USN_REASON_RENAME_OLD_NAME) != 0;
    let has_new = (rec.reason & USN_REASON_RENAME_NEW_NAME) != 0;
    if has_old || has_new {
        // 配对或单 OLD 都发 Rename（NEW 侧数据优先；单 OLD 时沿用旧语义）。
        let _ = tx.send(UsnWatchMsg::Event(ChangeEvent::Rename {
            file_id: rec.file_ref,
            new_parent_id: rec.parent_ref,
            new_name: rec.name.clone(),
        }));
        if has_old && !has_new {
            return;
        }
        // 配对/仅 NEW：名字已搬，内容 meta 可能同时变了，顺手排队 stat（目录跳过，size 恒 0）。
        if !rec.is_dir {
            let _ = tx.send(UsnWatchMsg::StatRefresh {
                file_id: rec.file_ref,
                file_id_128: rec.file_id_128,
            });
        }
        return;
    }
    if (rec.reason & USN_REASON_FILE_CREATE) != 0 {
        let _ = tx.send(UsnWatchMsg::Event(ChangeEvent::CreatePending {
            file_id: rec.file_ref,
            file_id_128: rec.file_id_128,
            parent_id: rec.parent_ref,
            name: rec.name.clone(),
            attrs: rec.attrs,
            is_dir: rec.is_dir,
        }));
        if !rec.is_dir {
            let _ = tx.send(UsnWatchMsg::StatRefresh {
                file_id: rec.file_ref,
                file_id_128: rec.file_id_128,
            });
        }
        return;
    }
    if (rec.reason & USN_REASON_DATA_EXTEND) != 0
        || (rec.reason & USN_REASON_DATA_TRUNCATION) != 0
        || (rec.reason & USN_REASON_BASIC_INFO_CHANGE) != 0
    {
        if !rec.is_dir {
            let _ = tx.send(UsnWatchMsg::StatRefresh {
                file_id: rec.file_ref,
                file_id_128: rec.file_id_128,
            });
        }
    }
}

/// `ERROR_JOURNAL_ENTRY_DELETED`（1178）判定：游标已被 journal 覆写。
fn is_journal_entry_deleted(e: &windows::core::Error) -> bool {
    use windows::Win32::Foundation::ERROR_JOURNAL_ENTRY_DELETED;
    e.code() == ERROR_JOURNAL_ENTRY_DELETED.to_hresult()
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

// ── Journal 保障 / 卷画像 / MFT 碎片（P0-3、P1-5、P1-6）─────────────────────

/// 目标 Journal 大小（默认 32MB 在忙盘 1–2 天就 wrap；512MB 覆盖数周 churn）。
const ENSURE_JOURNAL_MAXIMUM_SIZE: u64 = 512 * 1024 * 1024;
/// 分配/回收粒度（簇对齐的整数倍即可，64MB 减少碎片式伸缩）。
const ENSURE_JOURNAL_ALLOCATION_DELTA: u64 = 64 * 1024 * 1024;

/// 确保卷上有足够大的 USN Journal（FindX/Everything 做法）。
/// - Journal 缺失 → 创建；
/// - 已有但 `MaximumSize` 小于下限 → 只放大、不缩小（不动用户已调大的配置）；
/// - 无管理员权限 → 返回 `Ok(false)` 并记日志，调用方继续用现有 journal（不致命）。
/// 返回值：是否实际创建/修改过。
pub fn ensure_usn_journal(volume_path: &str) -> Result<bool> {
    use windows::Win32::System::Ioctl::{CREATE_USN_JOURNAL_DATA, FSCTL_CREATE_USN_JOURNAL};

    let dev = volume_device_path(volume_path);
    let h = match open_volume_handle(&dev) {
        Ok(h) => h,
        Err(e) => {
            // 卷都打不开（无权限/非本地卷）：记日志，不阻断服务启动。
            eprintln!("[findx2] ensure_usn_journal: 卷句柄打开失败（{e}），跳过 journal 保障");
            return Ok(false);
        }
    };
    let _close = scope_close(h);

    let jd = match unsafe { query_usn_journal(h) } {
        Ok(jd) => Some(jd),
        Err(_) => None,
    };
    let need_create = jd.is_none();
    let need_grow = jd
        .map(|j| j.MaximumSize < ENSURE_JOURNAL_MAXIMUM_SIZE)
        .unwrap_or(false);
    if !need_create && !need_grow {
        return Ok(false);
    }
    let data = CREATE_USN_JOURNAL_DATA {
        MaximumSize: ENSURE_JOURNAL_MAXIMUM_SIZE,
        AllocationDelta: ENSURE_JOURNAL_ALLOCATION_DELTA,
    };
    let mut returned: u32 = 0;
    let r = unsafe {
        DeviceIoControl(
            h,
            FSCTL_CREATE_USN_JOURNAL,
            Some(&data as *const _ as *const _),
            size_of::<CREATE_USN_JOURNAL_DATA>() as u32,
            None,
            0,
            Some(&mut returned),
            None,
        )
    };
    match r {
        Ok(_) => {
            eprintln!(
                "[findx2] ensure_usn_journal: 卷 {} journal 已{}（512MB/64MB）",
                volume_path,
                if need_create { "创建" } else { "放大" }
            );
            Ok(true)
        }
        Err(e) => {
            // 典型：非管理员 ERROR_ACCESS_DENIED——降级为只读现有 journal。
            eprintln!(
                "[findx2] ensure_usn_journal: 卷 {} 创建/放大 journal 失败（{e}），继续用现有 journal",
                volume_path
            );
            Ok(false)
        }
    }
}

/// 句柄 RAII：函数结束自动 CloseHandle（ensure 等短调用用，避免泄漏）。
struct ScopeClose(windows::Win32::Foundation::HANDLE);
fn scope_close(h: windows::Win32::Foundation::HANDLE) -> ScopeClose {
    ScopeClose(h)
}
impl Drop for ScopeClose {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// 卷是否承担 seek 惩罚（HDD/部分 USB 盘 true；SSD/NVMe false）。
/// 实现：`IOCTL_STORAGE_QUERY_PROPERTY + StorageDeviceSeekPenaltyProperty`
///（内核直接给结论，比 heuristic 准，比 WMI 轻）。
/// 查询失败（无权限/虚拟卷）→ false（保持现有行为，不 regression）。
pub fn volume_incurs_seek_penalty(volume_path: &str) -> bool {
    use windows::Win32::System::Ioctl::{
        PropertyStandardQuery, StorageDeviceSeekPenaltyProperty, DEVICE_SEEK_PENALTY_DESCRIPTOR,
        IOCTL_STORAGE_QUERY_PROPERTY, STORAGE_PROPERTY_QUERY,
    };

    let dev = volume_device_path(volume_path);
    let h = match open_volume_handle(&dev) {
        Ok(h) => h,
        Err(_) => return false,
    };
    let _close = scope_close(h);

    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceSeekPenaltyProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    let mut out = DEVICE_SEEK_PENALTY_DESCRIPTOR::default();
    let mut returned: u32 = 0;
    let r = unsafe {
        DeviceIoControl(
            h,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(&query as *const _ as *const _),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(&mut out as *mut _ as *mut _),
            size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32,
            Some(&mut returned),
            None,
        )
    };
    r.is_ok() && out.IncursSeekPenalty.0 != 0
}

/// `$MFT` 自身的 extents 数（碎片度）。FRN 0 打开 MFT + `FSCTL_GET_RETRIEVAL_POINTERS`
/// 逐段走。失败（无权限/非 NTFS）→ None。调用方阈值（如 >64）warn 提示整理。
pub fn mft_extent_count(volume_path: &str) -> Option<usize> {
    use windows::Win32::Storage::FileSystem::{
        FileIdType, OpenFileById, FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_DESCRIPTOR,
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    use windows::Win32::System::Ioctl::{
        FSCTL_GET_RETRIEVAL_POINTERS, RETRIEVAL_POINTERS_BUFFER, STARTING_VCN_INPUT_BUFFER,
    };

    let dev = volume_device_path(volume_path);
    let vol = open_volume_handle(&dev).ok()?;
    let _close_vol = scope_close(vol);

    // MFT 自身 FRN = 0。
    let mut desc = FILE_ID_DESCRIPTOR::default();
    desc.dwSize = size_of::<FILE_ID_DESCRIPTOR>() as u32;
    desc.Type = FileIdType;
    desc.Anonymous.FileId = 0;
    let hmft = unsafe {
        OpenFileById(
            vol,
            &desc,
            FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            FILE_FLAG_BACKUP_SEMANTICS,
        )
    }
    .ok()?;
    if hmft.is_invalid() {
        return None;
    }
    let _close_mft = scope_close(hmft);

    let mut total_extents: usize = 0;
    let mut start_vcn: i64 = 0;
    loop {
        let input = STARTING_VCN_INPUT_BUFFER {
            StartingVcn: start_vcn,
        };
        let mut out = vec![0u8; 64 * 1024];
        let mut returned: u32 = 0;
        let r = unsafe {
            DeviceIoControl(
                hmft,
                FSCTL_GET_RETRIEVAL_POINTERS,
                Some(&input as *const _ as *const _),
                size_of::<STARTING_VCN_INPUT_BUFFER>() as u32,
                Some(out.as_mut_ptr() as *mut _),
                out.len() as u32,
                Some(&mut returned),
                None,
            )
        };
        if r.is_err() || returned < size_of::<RETRIEVAL_POINTERS_BUFFER>() as u32 {
            break;
        }
        let hdr = unsafe { &*(out.as_ptr() as *const RETRIEVAL_POINTERS_BUFFER) };
        let n = hdr.ExtentCount as usize;
        if n == 0 {
            break;
        }
        total_extents += n;
        // 下一段起点：最后一个 extent 的 NextVcn（Extents 数组紧随头部）。
        let ext_size = 16usize; // NextVcn(i64) + Lcn(i64)
        let base = size_of::<RETRIEVAL_POINTERS_BUFFER>();
        let last_off = base + (n - 1) * ext_size;
        if last_off + 8 > out.len() {
            break;
        }
        let next_vcn = i64::from_le_bytes(out[last_off..last_off + 8].try_into().ok()?);
        if next_vcn <= start_vcn {
            break;
        }
        start_vcn = next_vcn;
        if total_extents > 100_000 {
            break;
        }
    }
    Some(total_extents)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(fr: u64, reason: u32, name: &str) -> ParsedWatchRec {
        ParsedWatchRec {
            file_ref: fr,
            parent_ref: 100,
            reason,
            attrs: 0x20,
            name: name.to_string(),
            is_dir: false,
            file_id_128: None,
        }
    }

    #[test]
    fn coalesce_merges_same_frn_storm() {
        let v = vec![
            rec(1, USN_REASON_FILE_CREATE, "a.txt"),
            rec(1, USN_REASON_DATA_EXTEND, "a.txt"),
            rec(1, USN_REASON_DATA_TRUNCATION, "a.txt"),
            rec(1, USN_REASON_BASIC_INFO_CHANGE, "a.txt"),
            rec(2, USN_REASON_FILE_CREATE, "b.txt"),
        ];
        let out = coalesce_watch_batch(v);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].file_ref, 1);
        assert_eq!(
            out[0].reason,
            USN_REASON_FILE_CREATE | USN_REASON_DATA_EXTEND | USN_REASON_DATA_TRUNCATION | USN_REASON_BASIC_INFO_CHANGE
        );
        assert_eq!(out[1].file_ref, 2);
    }

    #[test]
    fn coalesce_keeps_latest_name_and_order() {
        let v = vec![
            rec(1, USN_REASON_RENAME_OLD_NAME, "old.txt"),
            rec(2, USN_REASON_FILE_CREATE, "x.txt"),
            rec(1, USN_REASON_RENAME_NEW_NAME, "new.txt"),
        ];
        let out = coalesce_watch_batch(v);
        assert_eq!(out.len(), 2);
        // 输出按 FRN 首次出现顺序
        assert_eq!(out[0].file_ref, 1);
        assert_eq!(out[0].name, "new.txt");
        assert!(out[0].reason & USN_REASON_RENAME_NEW_NAME != 0);
        assert!(out[0].reason & USN_REASON_RENAME_OLD_NAME != 0);
        assert_eq!(out[1].file_ref, 2);
    }

    #[test]
    fn coalesce_empty_batch() {
        let out = coalesce_watch_batch(vec![]);
        assert!(out.is_empty());
    }
}
