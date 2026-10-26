//! MFT 读取方案基准测试：对比不同方式的速度、覆盖率、元数据完整性。
//!
//! 用法：`cargo run --bin bench_mft -- --volume C`

use std::mem::size_of;
use std::time::Instant;

use rayon::prelude::*;

use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_BEGIN, FILE_CURRENT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_SEQUENTIAL_SCAN,
    FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    ReadFile, SetFilePointerEx,
};
use windows::Win32::System::Ioctl::{
    FSCTL_ENUM_USN_DATA, FSCTL_GET_NTFS_VOLUME_DATA, FSCTL_GET_RETRIEVAL_POINTERS,
    FSCTL_QUERY_USN_JOURNAL, MFT_ENUM_DATA_V0, NTFS_VOLUME_DATA_BUFFER,
    RETRIEVAL_POINTERS_BUFFER, STARTING_VCN_INPUT_BUFFER, USN_JOURNAL_DATA_V0, USN_RECORD_V2,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::core::PCWSTR;

const MFT_MAGIC_FILE: &[u8; 4] = b"FILE";
const MFT_MAGIC_BAAD: &[u8; 4] = b"BAAD";
const READ_BUF_SIZE: usize = 64 * 1024 * 1024; // 64MB
const DEFAULT_RECORD_SIZE: usize = 1024;
/// FSCTL_GET_NTFS_FILE_RECORD = CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 66, METHOD_BUFFERED, FILE_ANY_ACCESS)
const FSCTL_GET_NTFS_FILE_RECORD: u32 = 0x00090064;

fn normalize_volume_path(volume: &str) -> String {
    let v = volume.trim().trim_end_matches('\\');
    if v.len() == 1
        && v.chars()
            .next()
            .map(|c| c.is_ascii_alphabetic())
            .unwrap_or(false)
    {
        format!(r"\\.\{}:", v.to_ascii_uppercase())
    } else if v.ends_with(':') && v.len() == 2 {
        format!(r"\\.\{}", v.to_ascii_uppercase())
    } else if v.starts_with(r"\\.\") {
        v.to_string()
    } else {
        format!(r"\\.\{}", v)
    }
}

fn open_volume(volume: &str) -> HANDLE {
    let vol_path = normalize_volume_path(volume);
    let vol_wide: Vec<u16> = vol_path.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let h = CreateFileW(
            PCWSTR(vol_wide.as_ptr()),
            GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        );
        match h {
            Ok(h) if !h.is_invalid() => h,
            Ok(h) => {
                CloseHandle(h);
                panic!("CreateFileW 返回无效句柄: {vol_path}");
            }
            Err(e) => panic!("CreateFileW 失败: {vol_path}: {e}"),
        }
    }
}

// ============================================================
// 方案 1：原始磁盘读（当前 mft_read.rs 的方法）
// ============================================================
struct RawMftResult {
    records_parsed: u64,
    records_in_use: u64,
    meta_entries: usize,
    has_size: u64,
    has_mtime: u64,
}

fn bench_raw_mft_read(volume: &str) -> (RawMftResult, std::time::Duration) {
    let h = open_volume(volume);
    let start = Instant::now();

    let mut vol_data_buf = vec![0u8; size_of::<NTFS_VOLUME_DATA_BUFFER>() + 256];
    let mut returned: u32 = 0;
    unsafe {
        DeviceIoControl(
            h,
            FSCTL_GET_NTFS_VOLUME_DATA,
            None,
            0,
            Some(vol_data_buf.as_mut_ptr() as *mut _),
            vol_data_buf.len() as u32,
            Some(&mut returned),
            None,
        )
        .expect("FSCTL_GET_NTFS_VOLUME_DATA 失败");
    }
    let vol_data = unsafe { &*(vol_data_buf.as_ptr() as *const NTFS_VOLUME_DATA_BUFFER) };
    let mft_start_lcn = vol_data.MftStartLcn;
    let bytes_per_cluster = vol_data.BytesPerCluster as u64;
    let mft_offset: u64 = (mft_start_lcn as u64) * bytes_per_cluster;
    let mft_valid_len: u64 = vol_data.MftValidDataLength as u64;

    println!(
        "  MFT 偏移={:#x}, 有效长度={}MB",
        mft_offset,
        mft_valid_len / (1024 * 1024)
    );

    unsafe {
        let mut new_pos: i64 = 0;
        SetFilePointerEx(h, mft_offset as i64, Some(&mut new_pos), FILE_BEGIN).unwrap();
    }

    let record_size = {
        let mut probe = vec![0u8; 4096];
        let mut bytes_read: u32 = 0;
        unsafe {
            ReadFile(h, Some(probe.as_mut_slice()), Some(&mut bytes_read), None).unwrap();
        }
        let rec_alloc = u32::from_le_bytes(probe[0x1C..0x20].try_into().unwrap()) as usize;
        if rec_alloc >= 512 && rec_alloc <= 4096 {
            rec_alloc
        } else {
            DEFAULT_RECORD_SIZE
        }
    };
    println!("  record_size={record_size}");

    unsafe {
        let mut new_pos: i64 = 0;
        SetFilePointerEx(h, mft_offset as i64, Some(&mut new_pos), FILE_BEGIN).unwrap();
    }

    let mut mft_buf = vec![0u8; READ_BUF_SIZE];
    let mut records_parsed: u64 = 0;
    let mut records_in_use: u64 = 0;
    let mut meta_entries: usize = 0;
    let mut has_size: u64 = 0;
    let mut has_mtime: u64 = 0;
    let mut total_read: u64 = 0;
    let mut consecutive_empty: u32 = 0;

    while total_read < mft_valid_len {
        let to_read = std::cmp::min(mft_buf.len() as u64, mft_valid_len - total_read) as u32;
        if to_read == 0 {
            break;
        }
        let mut bytes_read: u32 = 0;
        unsafe {
            ReadFile(
                h,
                Some(&mut mft_buf[..to_read as usize]),
                Some(&mut bytes_read),
                None,
            )
            .unwrap();
        }
        if bytes_read == 0 {
            break;
        }
        let data = &mft_buf[..bytes_read as usize];

        let mut off = 0usize;
        let mut batch_parsed = 0u64;
        while off + record_size <= data.len() {
            let rec = &data[off..off + record_size];
            off += record_size;

            if let Some(meta) = parse_mft_record_for_meta(rec) {
                records_parsed += 1;
                batch_parsed += 1;
                if meta.size > 0 || meta.mtime > 0 || meta.ctime > 0 {
                    records_in_use += 1;
                    meta_entries += 1;
                    if meta.size > 0 {
                        has_size += 1;
                    }
                    if meta.mtime > 0 {
                        has_mtime += 1;
                    }
                }
            }
        }

        if batch_parsed == 0 {
            consecutive_empty += 1;
            if consecutive_empty >= 32 {
                break;
            }
        } else {
            consecutive_empty = 0;
        }

        let consumed = off as u64;
        let unconsumed = bytes_read as u64 - consumed;
        if unconsumed > 0 {
            unsafe {
                SetFilePointerEx(h, -(unconsumed as i64), None, FILE_CURRENT).unwrap();
            }
        }
        total_read += consumed;
    }

    unsafe { CloseHandle(h) };

    let elapsed = start.elapsed();
    (
        RawMftResult {
            records_parsed,
            records_in_use,
            meta_entries,
            has_size,
            has_mtime,
        },
        elapsed,
    )
}

struct MftMeta {
    size: u64,
    mtime: u64,
    ctime: u64,
}

fn parse_mft_record_for_meta(record: &[u8]) -> Option<MftMeta> {
    if record.len() < 42 {
        return None;
    }
    let magic = &record[0..4];
    if magic == MFT_MAGIC_BAAD || magic != MFT_MAGIC_FILE {
        return None;
    }
    let flags = u16::from_le_bytes(record[0x16..0x18].try_into().ok()?);
    if (flags & 1) == 0 {
        return None;
    }
    let first_attr_off = u16::from_le_bytes(record[0x14..0x16].try_into().ok()?) as usize;

    let mut size: u64 = 0;
    let mut mtime: u64 = 0;
    let mut ctime: u64 = 0;
    let mut found_si = false;

    let mut off = first_attr_off;
    while off + 8 <= record.len() {
        let attr_type = u32::from_le_bytes(record[off..off + 4].try_into().ok()?);
        if attr_type == 0x0000_0000 || attr_type == 0xFFFF_FFFF {
            break;
        }
        let attr_len = u32::from_le_bytes(record[off + 4..off + 8].try_into().ok()?) as usize;
        if attr_len < 24 || off + attr_len > record.len() {
            break;
        }
        let non_resident = record[off + 8];
        let attr_data = &record[off..off + attr_len];

        match attr_type {
            0x10 => {
                if non_resident == 0 && attr_data.len() >= 24 {
                    let content_size =
                        u32::from_le_bytes(attr_data[16..20].try_into().ok()?) as usize;
                    let content_off =
                        u16::from_le_bytes(attr_data[20..22].try_into().ok()?) as usize;
                    let abs_off = off + content_off;
                    if abs_off + 48 <= record.len() && content_size >= 48 {
                        ctime =
                            u64::from_le_bytes(record[abs_off..abs_off + 8].try_into().ok()?);
                        mtime = u64::from_le_bytes(
                            record[abs_off + 8..abs_off + 16].try_into().ok()?,
                        );
                        found_si = true;
                    }
                }
            }
            0x80 => {
                if non_resident == 0 {
                    let content_size =
                        u32::from_le_bytes(attr_data[16..20].try_into().ok()?) as u64;
                    size = content_size;
                } else if attr_data.len() >= 0x38 {
                    size = u64::from_le_bytes(attr_data[0x30..0x38].try_into().ok()?);
                }
            }
            _ => {}
        }
        off += attr_len;
    }
    if !found_si {
        return None;
    }
    Some(MftMeta { size, mtime, ctime })
}

// ============================================================
// 方案 2：FSCTL_ENUM_USN_DATA（内核态枚举，Everything 方案）
// ============================================================
struct EnumUsnResult {
    total_records: u64,
    file_count: u64,
    dir_count: u64,
}

fn bench_enum_usn_data(volume: &str) -> (EnumUsnResult, std::time::Duration) {
    let h = open_volume(volume);

    // 先查询 USN Journal 获取 NextUsn（HighUsn 必须与此对齐，否则枚举无数据）
    let high_usn = unsafe {
        let mut jd = USN_JOURNAL_DATA_V0::default();
        let mut ret: u32 = 0;
        let r = DeviceIoControl(
            h,
            FSCTL_QUERY_USN_JOURNAL,
            None,
            0,
            Some(&mut jd as *mut _ as *mut _),
            size_of::<USN_JOURNAL_DATA_V0>() as u32,
            Some(&mut ret),
            None,
        );
        match r {
            Ok(_) => {
                println!("  USN Journal: NextUsn={}, JournalID={}", jd.NextUsn, jd.UsnJournalID);
                jd.NextUsn
            }
            Err(e) => {
                println!("  警告: FSCTL_QUERY_USN_JOURNAL 失败 ({e})，使用 i64::MAX");
                i64::MAX
            }
        }
    };

    let start = Instant::now();

    let mut total_records: u64 = 0;
    let mut file_count: u64 = 0;
    let mut dir_count: u64 = 0;

    let mut next_start_frn: u64 = 0;
    let mut buf: Vec<u8> = vec![0u8; 64 * 1024];
    let mut bytes_returned: u32 = 0;

    loop {
        let enum_data = MFT_ENUM_DATA_V0 {
            StartFileReferenceNumber: next_start_frn,
            LowUsn: 0,
            HighUsn: high_usn,
        };

        let r = unsafe {
            DeviceIoControl(
                h,
                FSCTL_ENUM_USN_DATA,
                Some(&enum_data as *const _ as _),
                size_of::<MFT_ENUM_DATA_V0>() as u32,
                Some(buf.as_mut_ptr() as _),
                buf.len() as u32,
                Some(&mut bytes_returned),
                None,
            )
        };

        match r {
            Ok(_) => {}
            Err(e) => {
                // ERROR_HANDLE_EOF: HRESULT 0x80070026 或 Win32 error 38
                let code = e.code().0 as u32;
                if code == 38 || code == 0x80070026 {
                    break;
                }
                panic!("FSCTL_ENUM_USN_DATA 失败: {e}");
            }
        }

        if bytes_returned <= 8 {
            break;
        }

        next_start_frn = u64::from_le_bytes(buf[0..8].try_into().unwrap());

        let mut offset = 8usize;
        while offset + 60 < bytes_returned as usize {
            let record = unsafe { &*(buf.as_ptr().add(offset) as *const USN_RECORD_V2) };
            let rec_len = record.RecordLength as usize;
            if rec_len < 60 || offset + rec_len > bytes_returned as usize {
                break;
            }

            total_records += 1;
            let attrs = record.FileAttributes;
            if attrs & 0x10 != 0 {
                dir_count += 1;
            } else {
                file_count += 1;
            }

            offset += rec_len;
        }
    }

    unsafe { CloseHandle(h) };

    let elapsed = start.elapsed();
    (
        EnumUsnResult {
            total_records,
            file_count,
            dir_count,
        },
        elapsed,
    )
}

// ============================================================
// 方案 3：FSCTL_GET_NTFS_FILE_RECORD（按编号逐条读）
// ============================================================
struct GetFileRecordResult {
    success: u64,
    failed: u64,
}

fn bench_get_ntfs_file_record(
    volume: &str,
    max_records: u64,
) -> (GetFileRecordResult, std::time::Duration) {
    let h = open_volume(volume);
    let start = Instant::now();

    let mut success: u64 = 0;
    let mut failed: u64 = 0;

    let mut out_buf: Vec<u8> = vec![0u8; 4096];
    let mut bytes_returned: u32 = 0;

    // 先测试前 10 条并打印详细信息
    let test_count = max_records.min(10);
    for rec_num in 0..test_count {
        bytes_returned = 0;
        let r = unsafe {
            DeviceIoControl(
                h,
                FSCTL_GET_NTFS_FILE_RECORD,
                Some(&rec_num as *const u64 as _),
                size_of::<u64>() as u32,
                Some(out_buf.as_mut_ptr() as _),
                out_buf.len() as u32,
                Some(&mut bytes_returned),
                None,
            )
        };
        eprintln!("  FRN={rec_num}: result={:?}, bytes_returned={bytes_returned}", r.is_ok());
        if r.is_ok() && bytes_returned > 0 {
            // NTFS_FILE_RECORD_OUTPUT_BUFFER:
            //   [0..8]  FileReferenceNumber: i64
            //   [8..12] FileRecordLength: u32
            //   [12..]  FileRecordBuffer: MFT record data
            let file_ref = u64::from_le_bytes(out_buf[0..8].try_into().unwrap());
            let rec_len = u32::from_le_bytes(out_buf[8..12].try_into().unwrap());
            eprintln!("    file_ref={file_ref}, rec_len={rec_len}");
            if bytes_returned as usize > 12 + 4 {
                let magic = &out_buf[12..16];
                eprintln!("    magic={:?}", std::str::from_utf8(magic).unwrap_or("???"));
                if magic == MFT_MAGIC_FILE {
                    success += 1;
                }
            }
        }
    }

    // 继续测试剩余记录
    for rec_num in test_count..max_records {
        bytes_returned = 0;
        let r = unsafe {
            DeviceIoControl(
                h,
                FSCTL_GET_NTFS_FILE_RECORD,
                Some(&rec_num as *const u64 as _),
                size_of::<u64>() as u32,
                Some(out_buf.as_mut_ptr() as _),
                out_buf.len() as u32,
                Some(&mut bytes_returned),
                None,
            )
        };

        match r {
            Ok(_) => {
                if bytes_returned as usize > 12 + 4 {
                    let magic = &out_buf[12..16];
                    if magic == MFT_MAGIC_FILE {
                        success += 1;
                    }
                }
            }
            Err(_) => {
                failed += 1;
            }
        }
    }

    unsafe { let _ = CloseHandle(h); };

    let elapsed = start.elapsed();
    (
        GetFileRecordResult { success, failed },
        elapsed,
    )
}

// ============================================================
// 方案 4：FSCTL_ENUM_USN_DATA + NtQueryDirectoryFile 组合
// ============================================================
struct ComboResult {
    total_files: u64,
    total_dirs: u64,
    dir_query_files: u64,     // NtQueryDirectoryFile 命中的文件数
    dir_query_dirs_ok: u64,   // 成功打开并枚举的目录数
    dir_query_dirs_fail: u64, // 打开失败的目录数
    has_size: u64,            // 最终有 size 的文件数
}

fn bench_combo(volume: &str, threads: usize) -> (ComboResult, std::time::Duration) {
    use std::collections::HashMap;

    let h = open_volume(volume);
    let start_total = Instant::now();

    // ---- 阶段 1: FSCTL_ENUM_USN_DATA 拿所有 FRN + parent_frn ----
    let start_enum = Instant::now();

    // 查询 USN Journal
    let high_usn = unsafe {
        let mut jd = USN_JOURNAL_DATA_V0::default();
        let mut ret: u32 = 0;
        let r = DeviceIoControl(
            h,
            FSCTL_QUERY_USN_JOURNAL,
            None,
            0,
            Some(&mut jd as *mut _ as *mut _),
            size_of::<USN_JOURNAL_DATA_V0>() as u32,
            Some(&mut ret),
            None,
        );
        if r.is_err() {
            println!("  警告: FSCTL_QUERY_USN_JOURNAL 失败");
            i64::MAX
        } else {
            jd.NextUsn
        }
    };

    // 枚举所有 FRN
    let mut files: Vec<(u64, u64)> = Vec::new(); // (frn, parent_frn)
    let mut dirs: Vec<u64> = Vec::new();         // dir frn
    let mut next_start_frn: u64 = 0;
    let mut buf: Vec<u8> = vec![0u8; 256 * 1024]; // 256KB buffer
    let mut bytes_returned: u32 = 0;

    loop {
        let enum_data = MFT_ENUM_DATA_V0 {
            StartFileReferenceNumber: next_start_frn,
            LowUsn: 0,
            HighUsn: high_usn,
        };

        let r = unsafe {
            DeviceIoControl(
                h,
                FSCTL_ENUM_USN_DATA,
                Some(&enum_data as *const _ as _),
                size_of::<MFT_ENUM_DATA_V0>() as u32,
                Some(buf.as_mut_ptr() as _),
                buf.len() as u32,
                Some(&mut bytes_returned),
                None,
            )
        };

        match r {
            Ok(_) => {}
            Err(e) => {
                let code = e.code().0 as u32;
                if code == 38 || code == 0x80070026 {
                    break;
                }
                panic!("FSCTL_ENUM_USN_DATA 失败: {e}");
            }
        }

        if bytes_returned <= 8 {
            break;
        }

        next_start_frn = u64::from_le_bytes(buf[0..8].try_into().unwrap());

        let mut offset = 8usize;
        while offset + 60 < bytes_returned as usize {
            let record = unsafe { &*(buf.as_ptr().add(offset) as *const USN_RECORD_V2) };
            let rec_len = record.RecordLength as usize;
            if rec_len < 60 || offset + rec_len > bytes_returned as usize {
                break;
            }

            let frn = record.FileReferenceNumber;
            let parent_frn = record.ParentFileReferenceNumber;
            let attrs = record.FileAttributes;

            if attrs & 0x10 != 0 {
                // 目录
                dirs.push(frn);
            } else {
                // 文件
                files.push((frn, parent_frn));
            }

            offset += rec_len;
        }
    }

    let enum_elapsed = start_enum.elapsed();
    println!("  阶段1 (FSCTL_ENUM_USN_DATA): {:?}", enum_elapsed);
    println!("    文件: {}, 目录: {}", files.len(), dirs.len());

    // ---- 阶段 2: 按 parent_frn 分组，用 NtQueryDirectoryFile 批量拿 size ----
    // 这里我们用 findx2_windows::fetch_dir_meta_batched 来做。
    // 但由于 bench 是独立 binary，我们直接用底层 API。
    // 简化方案：对每个目录用 CreateFileById 打开，然后 GetFileInformationByHandleEx 枚举子项。

    let start_dir_query = Instant::now();

    // 建 parent_frn → dir_frn 映射（用于打开目录）
    let dir_set: std::collections::HashSet<u64> = dirs.iter().copied().collect();

    // 建 file_frn → index 映射
    let mut frn_to_idx: HashMap<u64, usize> = HashMap::with_capacity(files.len());
    for (i, &(frn, _)) in files.iter().enumerate() {
        frn_to_idx.insert(frn, i);
    }

    // 按 parent_frn 分组
    let mut by_parent: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, &(_, parent_frn)) in files.iter().enumerate() {
        by_parent.entry(parent_frn).or_default().push(i);
    }

    println!("  阶段2: {} 个父目录含待查文件", by_parent.len());

    // 对每个父目录，用 OpenFileById 打开目录，再用 GetFileInformationByHandleEx 枚举
    // 这就是 fetch_dir_meta_batched 的底层逻辑。
    // 我们用简化版本：对有子文件的目录逐个打开并枚举。

    let mut file_sizes: Vec<Option<u64>> = vec![None; files.len()];

    // 获取 volume handle 用于 OpenFileById
    let vol_path = normalize_volume_path(volume);
    let vol_wide: Vec<u16> = vol_path.encode_utf16().chain(Some(0)).collect();

    use windows::Win32::Storage::FileSystem::{
        FileIdBothDirectoryInfo, FileIdType, GetFileInformationByHandleEx,
        OpenFileById, FILE_ID_BOTH_DIR_INFO, FILE_ID_DESCRIPTOR, FILE_LIST_DIRECTORY,
    };

    // 按 parent_frn 排序 —— HDD 上磁头顺序移动，寻道从 ~10ms 降到 ~1-2ms
    let mut by_parent_vec: Vec<(u64, Vec<usize>)> = by_parent.into_iter().collect();
    by_parent_vec.sort_unstable_by_key(|(frn, _)| *frn);
    println!("  阶段2: 已按 FRN 排序（HDD 顺序 I/O 优化）");

    let dir_set = std::sync::Arc::new(dir_set);
    let files = std::sync::Arc::new(files);

    // 单个目录枚举函数，供串行/并行共用
    let enumerate_dir = |parent_frn: &u64, child_indices: &Vec<usize>| -> (u64, u64, u64, u64) {
        // 每次独立打开卷句柄（HANDLE 不是 Send）
        let vol_h = unsafe {
            CreateFileW(
                PCWSTR(vol_wide.as_ptr()),
                GENERIC_READ.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_SEQUENTIAL_SCAN,
                None,
            )
        };
        let vol_h = match vol_h {
            Ok(h) if !h.is_invalid() => h,
            _ => return (0u64, 0u64, 0u64, 0u64),
        };

        if !dir_set.contains(parent_frn) {
            unsafe { let _ = CloseHandle(vol_h); }
            return (0u64, 1u64, 0u64, 0u64);
        }

        let mut desc = FILE_ID_DESCRIPTOR::default();
        desc.dwSize = size_of::<FILE_ID_DESCRIPTOR>() as u32;
        desc.Type = FileIdType;
        desc.Anonymous.FileId = *parent_frn as i64;

        let dir_h = unsafe {
            OpenFileById(
                vol_h,
                &desc,
                FILE_LIST_DIRECTORY.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                windows::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS
                    | windows::Win32::Storage::FileSystem::FILE_FLAG_SEQUENTIAL_SCAN,
            )
        };
        unsafe { let _ = CloseHandle(vol_h); }

        let dir_handle = match dir_h {
            Ok(h) if !h.is_invalid() => h,
            _ => return (0u64, 1u64, 0u64, 0u64),
        };

        let child_frn_set: std::collections::HashSet<u64> =
            child_indices.iter().map(|&i| files[i].0).collect();
        let child_map: HashMap<u64, usize> =
            child_indices.iter().map(|&i| (files[i].0, i)).collect();

        let mut dir_buf: Vec<u64> = vec![0u64; 256 * 1024 / 8];
        let buf_bytes = dir_buf.as_mut_ptr() as *mut u8;
        let buf_len = (dir_buf.len() * 8) as u32;

        let mut files_hit: u64 = 0;
        let mut has_size: u64 = 0;

        loop {
            let r = unsafe {
                GetFileInformationByHandleEx(
                    dir_handle,
                    FileIdBothDirectoryInfo,
                    buf_bytes as *mut _,
                    buf_len,
                )
            };
            if r.is_err() {
                break;
            }

            let mut p = buf_bytes;
            loop {
                let rec = unsafe { &*(p as *const FILE_ID_BOTH_DIR_INFO) };
                let next = rec.NextEntryOffset;
                let is_dir = (rec.FileAttributes & 0x10) != 0;
                let frn = rec.FileId as u64;
                let size = rec.EndOfFile as u64;

                if !is_dir && child_frn_set.contains(&frn) {
                    if let Some(&_file_idx) = child_map.get(&frn) {
                        files_hit += 1;
                        if size > 0 {
                            has_size += 1;
                        }
                    }
                }

                if next == 0 {
                    break;
                }
                p = unsafe { p.add(next as usize) };
            }
        }

        unsafe { let _ = CloseHandle(dir_handle); }
        (1u64, 0u64, files_hit, has_size)
    };

    // 根据 threads 参数决定串行还是并行
    let mut dir_query_dirs_ok: u64 = 0;
    let mut dir_query_dirs_fail: u64 = 0;
    let mut dir_query_files: u64 = 0;
    let mut has_size: u64 = 0;

    if threads <= 1 {
        // 串行模式：HDD 上避免磁头争抢
        println!("  阶段2: 串行模式（HDD 优化）");
        for (parent_frn, child_indices) in &by_parent_vec {
            let (ok, fail, hit, sz) = enumerate_dir(parent_frn, child_indices);
            dir_query_dirs_ok += ok;
            dir_query_dirs_fail += fail;
            dir_query_files += hit;
            has_size += sz;
        }
    } else {
        // 并行模式：用 rayon 限定线程数
        println!("  阶段2: 并行模式（{} 线程）", threads);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        let results: Vec<(u64, u64, u64, u64)> = pool.install(|| {
            by_parent_vec
                .par_iter()
                .map(|(parent_frn, child_indices)| enumerate_dir(parent_frn, child_indices))
                .collect()
        });
        for (ok, fail, hit, sz) in results {
            dir_query_dirs_ok += ok;
            dir_query_dirs_fail += fail;
            dir_query_files += hit;
            has_size += sz;
        }
    }

    let dir_query_elapsed = start_dir_query.elapsed();
    let total_elapsed = start_total.elapsed();

    println!("  阶段2 (NtQueryDirectoryFile): {:?}", dir_query_elapsed);
    println!("    成功打开目录: {}, 失败: {}", dir_query_dirs_ok, dir_query_dirs_fail);
    println!("    命中文件: {}", dir_query_files);
    println!("  总耗时: {:?}", total_elapsed);
    println!("  有 size: {}/{}", has_size, files.len());

    (
        ComboResult {
            total_files: files.len() as u64,
            total_dirs: dirs.len() as u64,
            dir_query_files,
            dir_query_dirs_ok,
            dir_query_dirs_fail,
            has_size,
        },
        total_elapsed,
    )
}

// ============================================================
// 方案 5：FSCTL_GET_RETRIEVAL_POINTERS + 顺序读 MFT 全部 extent
// ============================================================
struct ExtentsResult {
    extent_count: usize,
    records_parsed: u64,
    records_in_use: u64,
    meta_entries: usize,
    has_size: u64,
    has_mtime: u64,
    total_mft_bytes: u64,
}

fn bench_mft_extents(volume: &str) -> (ExtentsResult, std::time::Duration) {
    let vol_path = normalize_volume_path(volume);
    let vol_wide: Vec<u16> = vol_path.encode_utf16().chain(Some(0)).collect();

    // 1) 打开卷句柄（用于后续 raw ReadFile）
    let vol_h = unsafe {
        CreateFileW(
            PCWSTR(vol_wide.as_ptr()),
            GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    }
    .expect("打开卷失败");

    // 获取卷信息
    let (mft_start_lcn, bytes_per_cluster, mft_valid_len) = unsafe {
        let mut vol_data_buf = vec![0u8; size_of::<NTFS_VOLUME_DATA_BUFFER>() + 256];
        let mut returned: u32 = 0;
        DeviceIoControl(
            vol_h,
            FSCTL_GET_NTFS_VOLUME_DATA,
            None,
            0,
            Some(vol_data_buf.as_mut_ptr() as *mut _),
            vol_data_buf.len() as u32,
            Some(&mut returned),
            None,
        )
        .expect("FSCTL_GET_NTFS_VOLUME_DATA 失败");
        let vol_data = &*(vol_data_buf.as_ptr() as *const NTFS_VOLUME_DATA_BUFFER);
        (
            vol_data.MftStartLcn as u64,
            vol_data.BytesPerCluster as u64,
            vol_data.MftValidDataLength as u64,
        )
    };
    println!(
        "  MFT: start_lcn={}, bytes_per_cluster={}, valid_len={}MB",
        mft_start_lcn,
        bytes_per_cluster,
        mft_valid_len / (1024 * 1024)
    );

    // 2) 打开 $MFT 文件句柄，用 FSCTL_GET_RETRIEVAL_POINTERS 拿 extent 映射
    //    用 OpenFileById(vol_h, FRN=0) 打开 MFT 本身
    use windows::Win32::Storage::FileSystem::{
        FileIdType, OpenFileById, FILE_ID_DESCRIPTOR, FILE_LIST_DIRECTORY,
    };

    let mut desc = FILE_ID_DESCRIPTOR::default();
    desc.dwSize = size_of::<FILE_ID_DESCRIPTOR>() as u32;
    desc.Type = FileIdType;
    desc.Anonymous.FileId = 0; // MFT 的 FRN = 0

    let mft_h = unsafe {
        OpenFileById(
            vol_h,
            &desc,
            0, // FILE_READ_ATTRIBUTES
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            FILE_ATTRIBUTE_NORMAL,
        )
    };
    let mft_h = match mft_h {
        Ok(h) if !h.is_invalid() => {
            println!("  $MFT 打开成功 (OpenFileById, FRN=0)");
            h
        }
        _ => {
            // 尝试 BACKUP_SEMANTICS
            let h2 = unsafe {
                OpenFileById(
                    vol_h,
                    &desc,
                    0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    None,
                    FILE_FLAG_BACKUP_SEMANTICS,
                )
            };
            match h2 {
                Ok(h) if !h.is_invalid() => {
                    println!("  $MFT 打开成功 (OpenFileById, FRN=0, BACKUP_SEMANTICS)");
                    h
                }
                _ => panic!("打开 $MFT 失败（OpenFileById FRN=0）"),
            }
        }
    };

    // 收集所有 extent: (start_lcn, byte_count)
    let mut extents: Vec<(u64, u64)> = Vec::new();
    let mut start_vcn: i64 = 0;
    let mut ext_buf: Vec<u8> = vec![0u8; 1024 * 1024]; // 1MB，够存 ~65K extent

    // RETRIEVAL_POINTERS_BUFFER 布局:
    //   [0..4]  ExtentCount: u32
    //   [4..8]  padding
    //   [8..16] StartingVcn: i64
    //   [16..]  Extents[]: 每个 16 bytes (NextVcn: i64, Lcn: i64)
    const HDR_SIZE: usize = 16; // ExtentCount(4) + pad(4) + StartingVcn(8)
    const EXT_SIZE: usize = 16; // NextVcn(8) + Lcn(8)

    loop {
        let input = STARTING_VCN_INPUT_BUFFER { StartingVcn: start_vcn };
        let mut bytes_returned: u32 = 0;

        let r = unsafe {
            DeviceIoControl(
                mft_h,
                FSCTL_GET_RETRIEVAL_POINTERS,
                Some(&input as *const _ as _),
                size_of::<STARTING_VCN_INPUT_BUFFER>() as u32,
                Some(ext_buf.as_mut_ptr() as _),
                ext_buf.len() as u32,
                Some(&mut bytes_returned),
                None,
            )
        };

        match r {
            Ok(_) => {}
            Err(ref e) => {
                let code = e.code().0 as u32;
                if code == 234 || code == 0x800700EA {
                    // ERROR_MORE_DATA: 还有更多 extent，继续
                } else if code == 38 {
                    break;
                } else {
                    panic!("FSCTL_GET_RETRIEVAL_POINTERS 失败: {e} (code={code})");
                }
            }
        }

        let extent_count = unsafe {
            u32::from_le_bytes(ext_buf[0..4].try_into().unwrap()) as usize
        };
        if extent_count == 0 {
            break;
        }

        let base = ext_buf.as_ptr();
        for i in 0..extent_count {
            let p = unsafe { base.add(HDR_SIZE + i * EXT_SIZE) };
            let next_vcn = unsafe { (p as *const i64).read_unaligned() };
            let lcn = unsafe { (p.add(8) as *const i64).read_unaligned() };

            let prev_vcn = if i == 0 {
                start_vcn
            } else {
                let pp = unsafe { base.add(HDR_SIZE + (i - 1) * EXT_SIZE) };
                unsafe { (pp as *const i64).read_unaligned() }
            };

            let lcn_u = lcn as u64;
            let byte_count = (next_vcn - prev_vcn) as u64 * bytes_per_cluster;

            if lcn_u != u64::MAX && byte_count > 0 {
                extents.push((lcn_u, byte_count));
            }

            if i == extent_count - 1 {
                start_vcn = next_vcn;
            }
        }

        if r.is_ok() {
            break;
        }
    }

    unsafe { CloseHandle(mft_h) };

    let total_mft_bytes: u64 = extents.iter().map(|(_, b)| b).sum();
    println!(
        "  Extent 映射: {} 个 extent, MFT 总大小={}MB",
        extents.len(),
        total_mft_bytes / (1024 * 1024)
    );

    let start = Instant::now();

    // 3) 按 extent 顺序从卷句柄读 MFT 原始数据并解析
    let mut mft_buf = vec![0u8; READ_BUF_SIZE];
    let mut records_parsed: u64 = 0;
    let mut records_in_use: u64 = 0;
    let mut meta_entries: usize = 0;
    let mut has_size: u64 = 0;
    let mut has_mtime: u64 = 0;
    let mut total_read: u64 = 0;
    let mut consecutive_empty: u32 = 0;

    // 先探测 record_size
    let record_size = unsafe {
        let offset = extents[0].0 * bytes_per_cluster;
        let mut new_pos: i64 = 0;
        SetFilePointerEx(vol_h, offset as i64, Some(&mut new_pos), FILE_BEGIN).unwrap();
        let mut probe = vec![0u8; 4096];
        let mut bytes_read: u32 = 0;
        ReadFile(vol_h, Some(probe.as_mut_slice()), Some(&mut bytes_read), None).unwrap();
        let rec_alloc = u32::from_le_bytes(probe[0x1C..0x20].try_into().unwrap()) as usize;
        // 重置回起始位置
        SetFilePointerEx(vol_h, 0, Some(&mut new_pos), FILE_BEGIN).unwrap();
        if rec_alloc >= 512 && rec_alloc <= 4096 {
            rec_alloc
        } else {
            DEFAULT_RECORD_SIZE
        }
    };
    println!("  record_size={record_size}");

    for &(ext_lcn, ext_bytes) in &extents {
        let ext_offset = ext_lcn * bytes_per_cluster;
        let mut ext_remaining = ext_bytes;

        unsafe {
            let mut new_pos: i64 = 0;
            SetFilePointerEx(vol_h, ext_offset as i64, Some(&mut new_pos), FILE_BEGIN).unwrap();
        }

        while ext_remaining > 0 {
            let to_read = std::cmp::min(mft_buf.len() as u64, ext_remaining) as u32;
            if to_read == 0 {
                break;
            }
            let mut bytes_read: u32 = 0;
            unsafe {
                ReadFile(
                    vol_h,
                    Some(&mut mft_buf[..to_read as usize]),
                    Some(&mut bytes_read),
                    None,
                )
                .unwrap();
            }
            if bytes_read == 0 {
                break;
            }
            let data = &mft_buf[..bytes_read as usize];

            let mut off = 0usize;
            let mut batch_parsed = 0u64;
            while off + record_size <= data.len() {
                let rec = &data[off..off + record_size];
                off += record_size;

                if let Some(meta) = parse_mft_record_for_meta(rec) {
                    records_parsed += 1;
                    batch_parsed += 1;
                    if meta.size > 0 || meta.mtime > 0 || meta.ctime > 0 {
                        records_in_use += 1;
                        meta_entries += 1;
                        if meta.size > 0 {
                            has_size += 1;
                        }
                        if meta.mtime > 0 {
                            has_mtime += 1;
                        }
                    }
                }
            }

            if batch_parsed == 0 {
                consecutive_empty += 1;
                if consecutive_empty >= 32 {
                    break;
                }
            } else {
                consecutive_empty = 0;
            }

            let consumed = off as u64;
            let unconsumed = bytes_read as u64 - consumed;
            if unconsumed > 0 {
                unsafe {
                    SetFilePointerEx(vol_h, -(unconsumed as i64), None, FILE_CURRENT).unwrap();
                }
            }
            total_read += consumed;
            ext_remaining -= consumed;
        }

        if consecutive_empty >= 32 {
            break;
        }
    }

    unsafe { CloseHandle(vol_h) };

    let elapsed = start.elapsed();
    (
        ExtentsResult {
            extent_count: extents.len(),
            records_parsed,
            records_in_use,
            meta_entries,
            has_size,
            has_mtime,
            total_mft_bytes,
        },
        elapsed,
    )
}

// ============================================================
// Main
// ============================================================
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let volume = args
        .iter()
        .position(|a| a == "--volume")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
        .unwrap_or("C:");
    let threads: usize = args
        .iter()
        .position(|a| a == "--threads")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    println!("=== MFT 读取方案基准测试 ===");
    println!("目标卷: {volume}, 线程数: {threads}\n");

    // 方案 1：原始磁盘读
    println!("--- 方案 1: 原始磁盘读 (mft_read.rs) ---");
    let (raw_result, raw_elapsed) = bench_raw_mft_read(volume);
    println!("  耗时: {:?}", raw_elapsed);
    println!("  解析记录数: {}", raw_result.records_parsed);
    println!("  in_use 记录数: {}", raw_result.records_in_use);
    println!("  成功提取元数据: {}", raw_result.meta_entries);
    println!("  有 size (>0): {}", raw_result.has_size);
    println!("  有 mtime (>0): {}", raw_result.has_mtime);
    let raw_speed = raw_result.records_parsed as f64 / raw_elapsed.as_secs_f64();
    println!("  速度: {:.0} records/sec", raw_speed);
    println!(
        "  覆盖率: {:.1}% (meta/records)",
        raw_result.meta_entries as f64 / raw_result.records_parsed.max(1) as f64 * 100.0
    );
    println!();

    // 方案 2：FSCTL_ENUM_USN_DATA
    println!("--- 方案 2: FSCTL_ENUM_USN_DATA (Everything 方案) ---");
    let (enum_result, enum_elapsed) = bench_enum_usn_data(volume);
    println!("  耗时: {:?}", enum_elapsed);
    println!("  总记录数: {}", enum_result.total_records);
    println!("  文件数: {}", enum_result.file_count);
    println!("  目录数: {}", enum_result.dir_count);
    let enum_speed = enum_result.total_records as f64 / enum_elapsed.as_secs_f64();
    println!("  速度: {:.0} records/sec", enum_speed);
    println!("  注意: 不返回文件大小 (size)，需要额外查询");
    println!();

    // 方案 3：FSCTL_GET_NTFS_FILE_RECORD（只测试前 10000 条）
    let test_max = 10_000u64;
    println!("--- 方案 3: FSCTL_GET_NTFS_FILE_RECORD (逐条读, 测试前 {test_max} 条) ---");
    let (get_result, get_elapsed) = bench_get_ntfs_file_record(volume, test_max);
    println!("  耗时: {:?}", get_elapsed);
    println!("  成功: {}", get_result.success);
    println!("  失败: {}", get_result.failed);
    let get_speed = get_result.success as f64 / get_elapsed.as_secs_f64();
    println!("  速度: {:.0} records/sec", get_speed);
    println!("  注意: 每条需要知道 MFT 编号；无法枚举所有记录");
    println!();

    // 方案 4：FSCTL_ENUM_USN_DATA + NtQueryDirectoryFile 组合
    println!("--- 方案 4: FSCTL_ENUM_USN_DATA + NtQueryDirectoryFile 组合 ---");
    let (combo_result, combo_elapsed) = bench_combo(volume, threads);
    println!("  总耗时: {:?}", combo_elapsed);
    println!("  文件总数: {}", combo_result.total_files);
    println!("  目录总数: {}", combo_result.total_dirs);
    println!("  NtQueryDirectoryFile 命中文件: {}", combo_result.dir_query_files);
    println!("  成功打开目录: {}", combo_result.dir_query_dirs_ok);
    println!("  打开失败目录: {}", combo_result.dir_query_dirs_fail);
    println!("  最终有 size: {}/{}", combo_result.has_size, combo_result.total_files);
    let combo_speed = combo_result.total_files as f64 / combo_elapsed.as_secs_f64();
    println!("  速度: {:.0} files/sec (含 size 获取)", combo_speed);
    println!();

    // 方案 5：FSCTL_GET_RETRIEVAL_POINTERS + 顺序读全部 extent
    println!("--- 方案 5: FSCTL_GET_RETRIEVAL_POINTERS + 顺序读 MFT ---");
    let (ext_result, ext_elapsed) = bench_mft_extents(volume);
    println!("  耗时: {:?}", ext_elapsed);
    println!("  Extent 数: {}", ext_result.extent_count);
    println!("  MFT 总大小: {}MB", ext_result.total_mft_bytes / (1024 * 1024));
    println!("  解析记录数: {}", ext_result.records_parsed);
    println!("  in_use 记录数: {}", ext_result.records_in_use);
    println!("  成功提取元数据: {}", ext_result.meta_entries);
    println!("  有 size (>0): {}", ext_result.has_size);
    println!("  有 mtime (>0): {}", ext_result.has_mtime);
    let ext_speed = ext_result.records_parsed as f64 / ext_elapsed.as_secs_f64();
    println!("  速度: {:.0} records/sec", ext_speed);
    println!(
        "  覆盖率: {:.1}% (meta/records)",
        ext_result.meta_entries as f64 / ext_result.records_parsed.max(1) as f64 * 100.0
    );
    println!();

    // 总结
    println!("=== 总结 ===");
    println!(
        "方案 1 (原始磁盘读): {:.0} records/sec, 元数据完整 (size+mtime+ctime), 但受碎片化限制",
        raw_speed
    );
    println!(
        "方案 2 (FSCTL_ENUM_USN_DATA): {:.0} records/sec, 不受碎片化影响, 但不返回 size",
        enum_speed
    );
    println!(
        "方案 3 (FSCTL_GET_NTFS_FILE_RECORD): {:.0} records/sec, 需要已知编号, 不适合枚举",
        get_speed
    );
    println!();
    println!("推荐组合方案:");
    println!("  1) FSCTL_ENUM_USN_DATA 获取所有 FRN + parent_frn + 时间戳 (不受碎片化)");
    println!("  2) NtQueryDirectoryFile 按目录批量获取 size (已很快)");
    println!("  -> 一次扫描拿到完整元数据，无需 backfill");
}
