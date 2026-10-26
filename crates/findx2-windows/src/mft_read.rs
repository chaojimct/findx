//! 通过卷句柄直读 $MFT，一次性提取 in-use 文件/目录的 size/mtime/ctime。
//!
//! 使用 `FSCTL_GET_NTFS_VOLUME_DATA` 获取 MFT 偏移和大小，
//! 然后顺序读取 MFT 原始数据并解析每条记录。
//!
//! 注意：卷句柄上的 `ReadFile` 读取原始磁盘扇区，不走文件系统 extent 映射。
//! 当 MFT 碎片化时，只能读到第一个连续 extent（通常覆盖 5-20% 的记录）。
//! 剩余记录由调用方通过 NtQueryDirectoryFile 等方式补全。

#[cfg(windows)]
mod imp {
    use std::collections::HashMap;
    use std::mem::size_of;

    use windows::Win32::Foundation::{CloseHandle, GENERIC_READ};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_BEGIN, FILE_CURRENT, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadFile, SetFilePointerEx,
    };
    use windows::Win32::System::Ioctl::{
        FSCTL_GET_NTFS_VOLUME_DATA, NTFS_VOLUME_DATA_BUFFER,
    };
    use windows::Win32::System::IO::DeviceIoControl;
    use windows::core::PCWSTR;

    const MFT_MAGIC_FILE: &[u8; 4] = b"FILE";
    const MFT_MAGIC_BAAD: &[u8; 4] = b"BAAD";
    const READ_BUF_SIZE: usize = 64 * 1024 * 1024;
    const DEFAULT_RECORD_SIZE: usize = 1024;

    struct MftMeta {
        size: u64,
        mtime: u64,
        ctime: u64,
    }

    /// 直读 $MFT，返回 `(record_size, HashMap<record_number, (size, mtime, ctime)>)`。
    ///
    /// 通过卷句柄顺序读取 MFT 原始数据。当 MFT 碎片化时只覆盖第一个 extent，
    /// 命中率取决于 MFT 碎片程度。调用方应通过 NtQueryDirectoryFile 补全未命中的文件。
    pub fn read_mft_metadata(
        volume: &str,
    ) -> windows::core::Result<(usize, HashMap<u64, (u64, u64, u64)>)> {
        let vol_path = normalize_volume_path(volume);
        let vol_wide: Vec<u16> = vol_path.encode_utf16().chain(Some(0)).collect();
        let h = unsafe {
            CreateFileW(
                PCWSTR(vol_wide.as_ptr()),
                GENERIC_READ.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                None,
            )
        }?;
        if h.is_invalid() {
            let _ = unsafe { CloseHandle(h) };
            return Err(windows::core::Error::from_win32());
        }

        let result = (|| -> windows::core::Result<(usize, HashMap<u64, (u64, u64, u64)>)> {
            // 获取卷信息
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
                )?;
            }
            let vol_data =
                unsafe { &*(vol_data_buf.as_ptr() as *const NTFS_VOLUME_DATA_BUFFER) };
            let mft_start_lcn = vol_data.MftStartLcn;
            let bytes_per_cluster = vol_data.BytesPerCluster as u64;
            let mft_offset: u64 = (mft_start_lcn as u64) * bytes_per_cluster;
            let mft_valid_len: u64 = vol_data.MftValidDataLength as u64;

            findx2_core::progress!(
                "MFT 直读：MftStartLcn={}, BytesPerCluster={}, MFT偏移={:#x}, 有效长度={}MB",
                mft_start_lcn,
                bytes_per_cluster,
                mft_offset,
                mft_valid_len / (1024 * 1024),
            );

            // Seek 到 MFT 起始位置
            unsafe {
                let mut new_pos: i64 = 0;
                SetFilePointerEx(h, mft_offset as i64, Some(&mut new_pos), FILE_BEGIN)?;
            }

            // 读取第一条记录来确定 record_size
            let record_size = {
                let mut probe = vec![0u8; 4096];
                let mut bytes_read: u32 = 0;
                unsafe {
                    ReadFile(
                        h,
                        Some(probe.as_mut_slice()),
                        Some(&mut bytes_read),
                        None,
                    )?;
                }
                if bytes_read < 42 {
                    let _ = unsafe { CloseHandle(h) };
                    return Err(windows::core::Error::from_win32());
                }
                let rec_alloc =
                    u32::from_le_bytes(probe[0x1C..0x20].try_into().unwrap()) as usize;
                if rec_alloc >= 512 && rec_alloc <= 4096 {
                    rec_alloc
                } else {
                    DEFAULT_RECORD_SIZE
                }
            };

            // 重新 seek 回 MFT 起始
            unsafe {
                let mut new_pos: i64 = 0;
                SetFilePointerEx(h, mft_offset as i64, Some(&mut new_pos), FILE_BEGIN)?;
            }

            findx2_core::progress!(
                "MFT 直读：record_size={}, 开始顺序读取…",
                record_size,
            );

            // 顺序读取并解析 MFT 记录
            let mut mft_buf = vec![0u8; READ_BUF_SIZE];
            let mut meta_map: HashMap<u64, (u64, u64, u64)> = HashMap::new();
            let mut total_read: u64 = 0;
            let mut records_parsed: u64 = 0;
            let mut records_in_use: u64 = 0;
            let mut batch_idx: u64 = 0;
            let mut consecutive_empty: u32 = 0;

            while total_read < mft_valid_len {
                let to_read = std::cmp::min(mft_buf.len() as u64, mft_valid_len - total_read)
                    as u32;
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
                    )?;
                }
                if bytes_read == 0 {
                    break;
                }
                let data = &mft_buf[..bytes_read as usize];
                batch_idx += 1;

                let mut off = 0usize;
                let mut batch_parsed = 0u64;
                while off + record_size <= data.len() {
                    let rec = &data[off..off + record_size];
                    let rec_num = (total_read + off as u64) / record_size as u64;
                    off += record_size;

                    if let Some(meta) = parse_mft_record_for_meta(rec) {
                        records_parsed += 1;
                        batch_parsed += 1;
                        if meta.size > 0 || meta.mtime > 0 || meta.ctime > 0 {
                            records_in_use += 1;
                            meta_map.insert(rec_num, (meta.size, meta.mtime, meta.ctime));
                        }
                    }
                }

                // 连续空批次检测：如果连续 32 批没有解析到任何记录，说明已经
                // 读过了 MFT 的有效数据区域（碎片化导致读到了非 MFT 数据）。
                if batch_parsed == 0 {
                    consecutive_empty += 1;
                    if consecutive_empty >= 32 {
                        findx2_core::progress!(
                            "MFT 直读：连续 {} 批空数据，停止读取",
                            consecutive_empty,
                        );
                        break;
                    }
                } else {
                    consecutive_empty = 0;
                }

                let consumed = off as u64;
                let unconsumed = bytes_read as u64 - consumed;
                if unconsumed > 0 {
                    unsafe {
                        SetFilePointerEx(h, -(unconsumed as i64), None, FILE_CURRENT)?;
                    }
                }
                total_read += consumed;

                if batch_idx % 8 == 0 {
                    let pct = if mft_valid_len > 0 {
                        (total_read * 100) / mft_valid_len
                    } else {
                        0
                    };
                    findx2_core::progress!(
                        "MFT 直读：已读 {}MB ({}%)，已解析 {} 条，in_use {} 条",
                        total_read / (1024 * 1024),
                        pct,
                        records_parsed,
                        records_in_use,
                    );
                }
            }

            findx2_core::progress!(
                "MFT 直读完成：record_size={}, 总记录 {}, in_use {}，成功提取元数据 {} 条",
                record_size,
                records_parsed,
                records_in_use,
                meta_map.len(),
            );

            Ok((record_size, meta_map))
        })();

        let _ = unsafe { CloseHandle(h) };
        result
    }

    fn parse_mft_record_for_meta(record: &[u8]) -> Option<MftMeta> {
        if record.len() < 42 {
            return None;
        }
        let magic = &record[0..4];
        if magic == MFT_MAGIC_BAAD {
            return None;
        }
        if magic != MFT_MAGIC_FILE {
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
            let attr_len =
                u32::from_le_bytes(record[off + 4..off + 8].try_into().ok()?) as usize;
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
                            ctime = u64::from_le_bytes(
                                record[abs_off..abs_off + 8].try_into().ok()?,
                            );
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
                    } else {
                        if attr_data.len() >= 0x38 {
                            size =
                                u64::from_le_bytes(attr_data[0x30..0x38].try_into().ok()?);
                        }
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

    pub fn lookup_meta(
        meta_map: &HashMap<u64, (u64, u64, u64)>,
        frn: u64,
    ) -> Option<(u64, u64, u64)> {
        let rec_num = frn & 0x0000_FFFF_FFFF_FFFF;
        meta_map.get(&rec_num).copied()
    }

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
}

#[cfg(windows)]
pub use imp::{read_mft_metadata, lookup_meta};
