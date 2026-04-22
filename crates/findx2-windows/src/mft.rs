//! `FSCTL_ENUM_USN_DATA` 枚举 MFT 中的文件名（需管理员或备份特权）。

#[cfg(windows)]
mod imp {
    use std::ffi::OsString;
    use std::mem::size_of;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    use findx2_core::{RawEntry, VolumeScanner};
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use walkdir::WalkDir;
    use windows::core::{HRESULT, PCWSTR};
    use windows::Win32::Foundation::{CloseHandle, ERROR_HANDLE_EOF, GENERIC_READ, GENERIC_WRITE, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, ExtendedFileIdType, FileIdType,
        GetFileInformationByHandle, GetFinalPathNameByHandleW, OpenFileById,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_128,
        FILE_ID_DESCRIPTOR, FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::Ioctl::{
        FSCTL_ENUM_USN_DATA, MFT_ENUM_DATA_V0, MFT_ENUM_DATA_V1,
    };
    use windows::Win32::System::IO::DeviceIoControl;

    /// USN 记录解析输出。字段：
    /// `(file_id_u64, parent_id_u64, file_attr, name_len, name_off, file_id_128, timestamp_ft)`
    /// - `timestamp_ft`：该记录的 USN TimeStamp（FILETIME u64，0 表示字段不可用）。
    ///   它并非 `$STANDARD_INFORMATION.LastWriteTime`，而是"最近一次 USN 事件的时间"。
    ///   在首次 MFT 枚举这个语境下，它几乎等于文件最后修改时间（偏差秒级，取决于当时的 journal 写入）；
    ///   比让 mtime 一直是 0 强太多，且**零额外 IO**。size 仍然拿不到（USN 记录里没 size 字段）。
    type UsnParsed = (u64, u64, u32, usize, usize, Option<[u8; 16]>, u64);

    /// 与 FindX `FindXNative.cpp` 一致：`MFT_ENUM_DATA_V0/1` 的 `HighUsn` 须为 journal 的 `NextUsn`，不能固定为 -1。
    /// 解析 `FSCTL_ENUM_USN_DATA` 缓冲区中的单条 USN 记录（v2 与 v3，见 Win32 `USN_RECORD_V2` / `USN_RECORD_V3`）。
    fn parse_enum_usn_record(rec: &[u8]) -> Option<UsnParsed> {
        if rec.len() < 8 {
            return None;
        }
        let record_len = u32::from_le_bytes(rec[0..4].try_into().ok()?) as usize;
        if record_len != rec.len() {
            return None;
        }
        let major = u16::from_le_bytes(rec[4..6].try_into().ok()?);
        match major {
            2 => {
                if rec.len() < 60 {
                    return None;
                }
                let file_id = u64::from_le_bytes(rec[8..16].try_into().ok()?);
                let parent_id = u64::from_le_bytes(rec[16..24].try_into().ok()?);
                // USN_RECORD_V2 布局：TimeStamp 在 off=32, 8B (LARGE_INTEGER = FILETIME)。
                let timestamp = u64::from_le_bytes(rec[32..40].try_into().ok()?);
                let file_attr = u32::from_le_bytes(rec[52..56].try_into().ok()?);
                let name_len = u16::from_le_bytes(rec[56..58].try_into().ok()?) as usize;
                let name_off = u16::from_le_bytes(rec[58..60].try_into().ok()?) as usize;
                Some((file_id, parent_id, file_attr, name_len, name_off, None, timestamp))
            }
            3 => {
                if rec.len() < 76 {
                    return None;
                }
                // FILE_ID_128：FindX 取 Identifier 低 8 字节作为 64 位键；完整 16 字节供 OpenFileById 扩展 ID。
                let id128: [u8; 16] = rec[8..24].try_into().ok()?;
                let file_id = u64::from_le_bytes(id128[0..8].try_into().ok()?);
                let parent_id = u64::from_le_bytes(rec[24..32].try_into().ok()?);
                // USN_RECORD_V3 布局：TimeStamp 在 off=48, 8B。
                let timestamp = u64::from_le_bytes(rec[48..56].try_into().ok()?);
                let file_attr = u32::from_le_bytes(rec[68..72].try_into().ok()?);
                let name_len = u16::from_le_bytes(rec[72..74].try_into().ok()?) as usize;
                let name_off = u16::from_le_bytes(rec[74..76].try_into().ok()?) as usize;
                Some((file_id, parent_id, file_attr, name_len, name_off, Some(id128), timestamp))
            }
            _ => None,
        }
    }

    const BUF_SIZE: usize = 256 * 1024;
    /// MFT ioctl 每若干批打印一次进度
    const MFT_PROGRESS_EVERY_BATCHES: u64 = 32;
    /// 目录遍历回退时每若干条打印一次
    const WALK_PROGRESS_EVERY: usize = 50_000;

    /// 全局"已枚举条目数"实时计数器（所有正在跑的扫描线程共同累加）。
    /// 用于让 GUI 在卷扫描过程中也能看到进度数字实时变动，
    /// 而不是等整个卷跑完才在 `index.indexing.json` 写一次。
    /// 设计为全局 atomic 而不是回调参数，是因为 `pub fn scan_volume_*` 已是稳定 API；
    /// 同时 `build_full_disk_index` 又是单例（一次建库不会并发跑两次），全局状态足够用。
    pub static SCAN_LIVE_ENTRIES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    pub struct MftScanner;

    impl VolumeScanner for MftScanner {
        fn scan(&self, volume: &str) -> findx2_core::Result<Vec<RawEntry>> {
            let (files, dirs) = scan_volume(volume)?;
            let mut out = files;
            out.extend(dirs);
            Ok(out)
        }
    }

    /// 全量元数据（$MFT 映射 + 必要时 `OpenFileById`），与历史行为一致。
    pub fn scan_volume(root: &str) -> findx2_core::Result<(Vec<RawEntry>, Vec<RawEntry>)> {
        scan_volume_inner(root, false)
    }

    /// 快速建库：仅 MFT 枚举文件名/FRN，不读 $MFT 属性流、不做 `OpenFileById` 回填；`size`/`mtime` 为占位 0。
    pub fn scan_volume_fast(root: &str) -> findx2_core::Result<(Vec<RawEntry>, Vec<RawEntry>)> {
        scan_volume_inner(root, true)
    }

    /// 返回 `(files, dirs)`；`fast` 为真时跳过元数据以加速首遍建库。
    fn scan_volume_inner(
        root: &str,
        fast: bool,
    ) -> findx2_core::Result<(Vec<RawEntry>, Vec<RawEntry>)> {
        let path = volume_path(root);
        findx2_core::progress!("索引：打开卷 {path} …");
        let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        // 多数样本对卷使用 READ|WRITE，部分环境下仅 READ 会导致 FSCTL_ENUM_USN_DATA 首轮 EOF
        let access = GENERIC_READ.0 | GENERIC_WRITE.0;
        let h = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                None,
            )
        };
        let h = match h {
            Ok(handle) if !handle.is_invalid() => handle,
            Ok(handle) => {
                let _ = unsafe { CloseHandle(handle) };
                return Err(findx2_core::Error::Platform(format!(
                    "CreateFileW 返回无效句柄: {path}"
                )));
            }
            Err(e) => {
                return Err(findx2_core::Error::Platform(format!(
                    "无法打开卷 {path}: {e}"
                )));
            }
        };

        // 历史曾尝试顺序读 `\\?\\X:\\$MFT` 一次性建立 FRN→size/mtime 表（与 FindX C++ 的 LoadNtfsMftMetaMap 相同思路），
        // 实测在 Win10/Win11 上 CreateFileW 返回 ERROR_ACCESS_DENIED(5) — 即便是管理员 token 也被内核拒访，
        // 这条路径在用户态稳定不可用，已彻底删除。元数据统一走 OpenFileById（fast 首遍跳过；service 后台回填）。

        let high_usn = match unsafe { crate::usn::query_usn_journal(h) } {
            Ok(jd) => {
                findx2_core::progress!(
                    "USN Journal：NextUsn={}，MFT_ENUM HighUsn 与此对齐（与 FindX FindXNative 一致）",
                    jd.NextUsn
                );
                jd.NextUsn
            }
            Err(e) => {
                findx2_core::progress!(
                    "警告：FSCTL_QUERY_USN_JOURNAL 失败（{e}），MFT HighUsn 回退为 -1，枚举可能无数据"
                );
                -1i64
            }
        };

        // FindX v1：`MFT_ENUM_DATA_V0` + `HighUsn = jd.NextUsn`
        findx2_core::progress!("索引方式：MFT（FSCTL_ENUM_USN_DATA，V0）…");
        let v0 = MFT_ENUM_DATA_V0 {
            StartFileReferenceNumber: 0,
            LowUsn: 0,
            HighUsn: high_usn,
        };
        let r = enum_usn_data_loop(
            h,
            v0,
            std::mem::size_of::<MFT_ENUM_DATA_V0>() as u32,
            "V0",
        );
        let (mut files, mut dirs, ok) = r?;

        if !ok || (files.is_empty() && dirs.is_empty()) {
            if !ok {
                findx2_core::progress!("提示：V0 未得到有效 ioctl 批次，尝试 V1 …");
            } else {
                findx2_core::progress!("提示：V0 无条目，尝试 V1 …");
            }
            let v1 = MFT_ENUM_DATA_V1 {
                StartFileReferenceNumber: 0,
                LowUsn: 0,
                HighUsn: high_usn,
                MinMajorVersion: 2,
                MaxMajorVersion: 4,
            };
            let r2 = enum_usn_data_loop(
                h,
                v1,
                std::mem::size_of::<MFT_ENUM_DATA_V1>() as u32,
                "V1",
            );
            if let Ok((f, d, ok2)) = r2 {
                if ok2 && (!f.is_empty() || !d.is_empty()) {
                    files = f;
                    dirs = d;
                }
            }
        }

        let _ = unsafe { CloseHandle(h) };

        if files.is_empty() && dirs.is_empty() {
            return scan_volume_walk_fallback(root, fast);
        }

        findx2_core::progress!(
            "MFT 枚举完成：文件 {}，目录 {}。",
            files.len(),
            dirs.len()
        );

        if fast {
            // fast 路径：保留 USN TimeStamp 作为 mtime/ctime 近似值（枚举时零 IO 就能拿到）；
            // size 字段 USN 记录里没有，必须留 0，等 service 后台回填。
            // backfill 判断条件是 `size==0 && !is_dir`，不以 mtime 为准。
            for e in files.iter_mut() {
                e.size = 0;
            }
        } else {
            // 快路径：按目录批量回填。
            // OpenFileById 逐文件做是 IRP_MJ_CREATE+QUERY_INFORMATION 两次内核往返 × N；
            // 改用 NtQueryDirectoryFile（GetFileInformationByHandleEx(FileIdBothDirectoryInfo)）一次 syscall
            // 拿一个目录里一批 (frn, size, mtime, ctime)，摊销到单文件 ~几百纳秒。
            //
            // 两个小优化：
            // 1) 跳过没有任何子文件的目录（C: 上空目录占比不小：缓存目录、空 git 仓的 hooks 等）。
            //    open 一次目录的代价 ~3ms（IRP_MJ_CREATE），nuke 掉是纯赚。
            // 2) 按 FRN 升序排：FRN 近似等于 MFT 物理记录号，升序访问让 Windows 的
            //    Mcb cache / page cache 命中率显著上升（vs 我们枚举时的混乱顺序）。
            let mut parent_has_file: std::collections::HashSet<u64> =
                std::collections::HashSet::with_capacity(files.len());
            for f in files.iter() {
                parent_has_file.insert(f.parent_id);
            }
            let mut dir_list: Vec<(u64, Option<[u8; 16]>)> = dirs
                .iter()
                .filter(|d| parent_has_file.contains(&d.file_id))
                .map(|d| (d.file_id, d.file_id_128))
                .collect();
            dir_list.sort_unstable_by_key(|(frn, _)| *frn);
            findx2_core::progress!(
                "全量元数据：NtQueryDirectoryFile 批量扫描 {} 个目录（{} 中筛掉空目录 {} 个，已按 FRN 排序）…",
                dir_list.len(),
                dirs.len(),
                dirs.len() - dir_list.len(),
            );
            let dir_meta = crate::nt_dir_query::fetch_dir_meta_batched(
                &path,
                &dir_list,
                None,
                None,
            );
            // 建 FRN→(size,mtime,ctime) 哈希；NTFS 单卷不会有重复 FRN。
            let mut by_frn: std::collections::HashMap<u64, (u64, u64, u64)> =
                std::collections::HashMap::with_capacity(dir_meta.len());
            for (frn, sz, mt, ct) in dir_meta {
                by_frn.insert(frn, (sz, mt, ct));
            }
            let mut hit = 0usize;
            let mut miss_indices: Vec<usize> = Vec::new();
            for (i, e) in files.iter_mut().enumerate() {
                if let Some(&(sz, mt, ct)) = by_frn.get(&e.file_id) {
                    e.size = sz;
                    e.mtime = mt;
                    e.ctime = ct;
                    hit += 1;
                } else {
                    miss_indices.push(i);
                }
            }
            findx2_core::progress!(
                "NtQueryDirectoryFile 命中率：{}/{} ({}%)，未命中 {} 条走 OpenFileById 兜底",
                hit,
                files.len(),
                hit * 100 / files.len().max(1),
                miss_indices.len(),
            );
            if !miss_indices.is_empty() {
                fill_files_metadata_open_by_indices(&path, &mut files, &miss_indices);
            }
        }

        // 历史：上一轮怀疑 FSCTL_ENUM_USN_DATA 漏 OneDrive Cloud Files (CFAPI, tag 0x9000601a)
        // 子项，给每个 reparse 目录都用 WalkDir 二扫了一遍。实测 3 个卷共 ~3.4 万个 reparse 目录、
        // 全部「新增 0」，但额外吃了 230+ 秒建库时间——它们其实早被 USN ENUM 当作普通 NTFS 条目
        // 扫到了。默认关闭；只有用户显式 `FINDX2_REPARSE_WALK=1` 才执行（用于异常诊断）。
        if std::env::var("FINDX2_REPARSE_WALK").as_deref() == Ok("1") {
            supplement_reparse_subtrees(root, &mut files, &mut dirs, fast);
        }

        Ok((files, dirs))
    }

    fn volume_drive_letter(root: &str) -> Option<u8> {
        let s = root.trim();
        let c = s.chars().next()?;
        if c.is_ascii_alphabetic() {
            Some(c.to_ascii_uppercase() as u8)
        } else {
            None
        }
    }

    /// 对给定下标的**文件**条目执行 `OpenFileById` 批量回填 size/mtime/ctime。
    /// 仅做"raw -> raw 字段写回"的薄包装，真正的池化逻辑在 `metadata_fill::fill_metadata_by_id_pooled`。
    fn fill_files_metadata_open_by_indices(
        volume_device_path: &str,
        files: &mut Vec<RawEntry>,
        indices: &[usize],
    ) {
        if indices.is_empty() {
            return;
        }
        let file_ids: Vec<u64> = files.iter().map(|e| e.file_id).collect();
        let file_id_128s: Vec<Option<[u8; 16]>> =
            files.iter().map(|e| e.file_id_128).collect();
        let updates = crate::metadata_fill::fill_metadata_by_id_pooled(
            volume_device_path,
            &file_ids,
            &file_id_128s,
            indices,
            None,
            None,
        );
        for (idx, sz, mt, ct) in updates {
            if let Some(e) = files.get_mut(idx) {
                e.size = sz;
                e.mtime = mt;
                e.ctime = ct;
            }
        }
    }

    /// `input_payload`: MFT_ENUM_DATA_V0 / V1 实例，只序列化 `input_len` 字节（二者布局兼容续传时的前 8 字节 FRN）
    fn enum_usn_data_loop<T: Copy>(
        h: windows::Win32::Foundation::HANDLE,
        mut input: T,
        input_len: u32,
        stage: &'static str,
    ) -> findx2_core::Result<(Vec<RawEntry>, Vec<RawEntry>, bool)> {
        let mut files = Vec::new();
        let mut dirs = Vec::new();
        let mut had_enum_batch = false;
        let mut buf = vec![0u8; BUF_SIZE];
        let mut batch_idx: u64 = 0;

        loop {
            let mut returned: u32 = 0;
            let ok = unsafe {
                DeviceIoControl(
                    h,
                    FSCTL_ENUM_USN_DATA,
                    Some((&input as *const T) as *const _),
                    input_len,
                    Some(buf.as_mut_ptr() as *mut _),
                    buf.len() as u32,
                    Some(&mut returned),
                    None,
                )
            };
            if let Err(e) = ok {
                if e.code() == HRESULT::from_win32(ERROR_HANDLE_EOF.0) {
                    break;
                }
                return Err(findx2_core::Error::Platform(format!(
                    "DeviceIoControl(FSCTL_ENUM_USN_DATA): {e}"
                )));
            }
            if returned < 8 {
                break;
            }
            had_enum_batch = true;
            batch_idx += 1;
            let slice = &buf[..returned as usize];
            let next_start = u64::from_le_bytes(slice[0..8].try_into().unwrap());
            let mut off = 8usize;
            let mut recs_this_batch = 0u64;
            while off + 8 <= slice.len() {
                let record_len =
                    u32::from_le_bytes(slice[off..off + 4].try_into().unwrap()) as usize;
                if record_len < 8 || off + record_len > slice.len() {
                    break;
                }
                let rec = &slice[off..off + record_len];
                off += record_len;

                let Some((file_id, parent_id, file_attr, name_len, name_off, file_id_128, ts)) =
                    parse_enum_usn_record(rec)
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
                if name == "." || name == ".." {
                    continue;
                }
                let is_dir = (file_attr & 0x10) != 0;
                // 保留完整 32 位 file_attr：高位含 FILE_ATTRIBUTE_REPARSE_POINT (0x400) 等，
                // 后续补抓 OneDrive cloud reparse 子树时需要识别。
                // mtime/ctime 先填 USN TimeStamp（近似值、零 IO）；后续 full-stat / service 后台
                // 回填时会被真 STANDARD_INFORMATION 覆盖。fast 模式下这就是最终值，用户搜索已可按时间排序/过滤。
                let entry = RawEntry {
                    file_id,
                    file_id_128,
                    parent_id,
                    name,
                    size: 0,
                    mtime: ts,
                    ctime: ts,
                    attrs: file_attr,
                    is_dir,
                };
                if is_dir {
                    dirs.push(entry);
                } else {
                    files.push(entry);
                }
                recs_this_batch += 1;
            }

            // 实时进度：让 GUI 的 indexing.json ticker 能读到当前累计条数。
            // batch 级累加（每批 ~几万条）开销忽略不计，比每条记录递增便宜得多。
            if recs_this_batch > 0 {
                SCAN_LIVE_ENTRIES
                    .fetch_add(recs_this_batch, std::sync::atomic::Ordering::Relaxed);
            }

            if batch_idx % MFT_PROGRESS_EVERY_BATCHES == 0 {
                findx2_core::progress!(
                    "MFT 枚举 [{stage}]：ioctl 批次 {}，本批记录 {}，累计 文件 {} 目录 {} …",
                    batch_idx,
                    recs_this_batch,
                    files.len(),
                    dirs.len()
                );
            }

            if next_start == 0 {
                break;
            }
            // 两种输入结构前 8 字节均为 StartFileReferenceNumber
            let p = (&mut input as *mut T) as *mut u64;
            unsafe {
                *p = next_start;
            }
        }

        Ok((files, dirs, had_enum_batch))
    }

    /// FSCTL 不可用时的回退：目录遍历 + `GetFileInformationByHandle` 取 NTFS 文件 ID（较慢）。
    fn scan_volume_walk_fallback(
        root: &str,
        fast: bool,
    ) -> findx2_core::Result<(Vec<RawEntry>, Vec<RawEntry>)> {
        let root_path = volume_to_folder_root(root);
        findx2_core::progress!(
            "索引方式：目录遍历（MFT 无数据时的回退，较慢）根目录: {}",
            root_path
        );
        let mut files = Vec::new();
        let mut dirs = Vec::new();
        let mut walked: usize = 0;
        for ent in WalkDir::new(&root_path).follow_links(false).into_iter().filter_map(|e| e.ok()) {
            let path = ent.path();
            let is_dir = ent.file_type().is_dir();
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                continue;
            }
            let Some(fid) = file_id_from_path(path) else {
                continue;
            };
            let parent_path = path.parent().unwrap_or(Path::new(&root_path));
            let parent_id = file_id_from_path(parent_path).unwrap_or(0);
            let meta = std::fs::symlink_metadata(path)
                .map_err(|e| findx2_core::Error::Platform(format!("元数据: {e}")))?;
            let attr = if is_dir { 0x10 } else { 0 };
            let entry = RawEntry {
                file_id: fid,
                file_id_128: None,
                parent_id,
                name,
                size: meta.len(),
                mtime: 0,
                ctime: 0,
                attrs: attr,
                is_dir,
            };
            if is_dir {
                dirs.push(entry);
            } else {
                files.push(entry);
            }
            walked += 1;
            if walked % WALK_PROGRESS_EVERY == 0 {
                findx2_core::progress!(
                    "目录遍历：已处理 {} 项，当前 文件 {} 目录 {} …",
                    walked,
                    files.len(),
                    dirs.len()
                );
            }
        }
        findx2_core::progress!(
            "目录遍历完成：共 {} 项，文件 {} 目录 {}。",
            walked,
            files.len(),
            dirs.len()
        );
        if files.is_empty() && dirs.is_empty() {
            return Err(findx2_core::Error::Platform(
                "MFT 枚举与目录遍历均未得到条目：请以管理员运行、确认卷为本地 NTFS。"
                    .into(),
            ));
        }
        if fast {
            // walk_fallback 是 MFT 不可用时（极少见）的兜底，这里本来就没拿到 USN TimeStamp，
            // 只能清零交给 backfill。
            for e in files.iter_mut() {
                e.size = 0;
                e.mtime = 0;
                e.ctime = 0;
            }
            for e in dirs.iter_mut() {
                e.mtime = 0;
                e.ctime = 0;
            }
            return Ok((files, dirs));
        }

        // 目录遍历回退场景下的 full-stat：所有文件直接走 OpenFileById 池化回填。
        // （历史上还会先尝试顺序读 $MFT，已确认用户态 ACCESS_DENIED，删之。）
        let vol_path = volume_path(root);
        let all_indices: Vec<usize> = (0..files.len()).collect();
        if !all_indices.is_empty() {
            findx2_core::progress!(
                "目录遍历回退：OpenFileById 全量回填 {} 个文件 …",
                all_indices.len()
            );
            fill_files_metadata_open_by_indices(&vol_path, &mut files, &all_indices);
        }
        Ok((files, dirs))
    }

    fn volume_to_folder_root(vol: &str) -> String {
        let v = vol.trim().trim_end_matches('\\');
        if v.len() == 1 && v.chars().next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false) {
            format!("{}:\\", v.to_ascii_uppercase())
        } else if v.len() >= 2 && v.as_bytes()[1] == b':' {
            format!("{}\\", v.trim_end_matches('\\'))
        } else {
            format!("{}\\", v)
        }
    }

    fn file_id_from_path(path: &Path) -> Option<u64> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
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
        }
        .ok()?;
        if h.is_invalid() {
            let _ = unsafe { CloseHandle(h) };
            return None;
        }
        let mut bh = BY_HANDLE_FILE_INFORMATION::default();
        let r = unsafe { GetFileInformationByHandle(h, &mut bh) };
        let _ = unsafe { CloseHandle(h) };
        if r.is_err() {
            return None;
        }
        Some(((bh.nFileIndexHigh as u64) << 32) | bh.nFileIndexLow as u64)
    }

    /// 对所有 reparse point 目录用 WalkDir 补抓子树，弥补 FSCTL_ENUM_USN_DATA 漏掉的
    /// OneDrive Cloud Files 占位项。仅采集与本卷同盘符的子项，避免 junction 跨卷重复。
    fn supplement_reparse_subtrees(
        volume_root: &str,
        files: &mut Vec<RawEntry>,
        dirs: &mut Vec<RawEntry>,
        fast: bool,
    ) {
        let reparse_dirs: Vec<(u64, Option<[u8; 16]>)> = dirs
            .iter()
            .filter(|d| (d.attrs & 0x400) != 0)
            .map(|d| (d.file_id, d.file_id_128))
            .collect();
        if reparse_dirs.is_empty() {
            return;
        }
        findx2_core::progress!(
            "reparse 子树补抓：发现 {} 个 reparse 目录（含 OneDrive 云占位等），开始 WalkDir 扫描 …",
            reparse_dirs.len()
        );

        let mut known: HashSet<u64> = files
            .iter()
            .map(|f| f.file_id)
            .chain(dirs.iter().map(|d| d.file_id))
            .collect();

        let vol_path_dev = volume_path(volume_root);
        let wide_vol: Vec<u16> = vol_path_dev.encode_utf16().chain(Some(0)).collect();
        let vol_h_res = unsafe {
            CreateFileW(
                PCWSTR(wide_vol.as_ptr()),
                0, // 仅 metadata：不要 GENERIC_READ，避免 WalkDir 期间 share 冲突
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                None,
            )
        };
        let vol_h = match vol_h_res {
            Ok(h) if !h.is_invalid() => h,
            _ => {
                findx2_core::progress!("reparse 子树补抓：打开卷句柄失败，跳过。");
                return;
            }
        };

        let drive_letter = volume_drive_letter(volume_root)
            .map(|b| b as char)
            .unwrap_or('?')
            .to_ascii_uppercase();

        let mut scanned_dirs = 0u64;
        let mut added_files = 0u64;
        let mut added_dirs = 0u64;

        for (frn, id128) in reparse_dirs {
            let Some(reparse_path) = (unsafe { resolve_path_by_id(vol_h, frn, id128) }) else {
                continue;
            };
            // 路径必须以本卷盘符开头，且不是卷根本身（卷根 reparse 没意义）
            let s = reparse_path.to_string_lossy();
            let bytes = s.as_bytes();
            if bytes.len() < 3
                || (bytes[0].to_ascii_uppercase() != drive_letter as u8)
                || bytes[1] != b':'
            {
                continue;
            }
            scanned_dirs += 1;

            for ent in WalkDir::new(&reparse_path)
                .follow_links(false)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = ent.path();
                if path == reparse_path.as_path() {
                    continue;
                }
                let is_dir = ent.file_type().is_dir();
                let name = match path.file_name().and_then(|s| s.to_str()) {
                    Some(n) if !n.is_empty() => n.to_string(),
                    _ => continue,
                };
                let Some((file_id, parent_id, attr_u32, size, mtime, ctime)) =
                    open_file_info_by_path(path)
                else {
                    continue;
                };
                if !known.insert(file_id) {
                    continue;
                }
                let entry = RawEntry {
                    file_id,
                    file_id_128: None,
                    parent_id,
                    name,
                    size: if fast { 0 } else { size },
                    mtime: if fast { 0 } else { mtime },
                    ctime: if fast { 0 } else { ctime },
                    attrs: attr_u32,
                    is_dir,
                };
                if is_dir {
                    dirs.push(entry);
                    added_dirs += 1;
                } else {
                    files.push(entry);
                    added_files += 1;
                }
            }
        }
        let _ = unsafe { CloseHandle(vol_h) };
        findx2_core::progress!(
            "reparse 子树补抓完成：扫描 {} 个目录，新增 文件 {} 目录 {}。",
            scanned_dirs,
            added_files,
            added_dirs
        );
    }

    /// 用 OpenFileById 打开 reparse 节点本体（不跟随），再 GetFinalPathNameByHandleW 拿绝对路径。
    unsafe fn resolve_path_by_id(
        vol: HANDLE,
        frn: u64,
        id128: Option<[u8; 16]>,
    ) -> Option<PathBuf> {
        let mut desc = FILE_ID_DESCRIPTOR::default();
        desc.dwSize = size_of::<FILE_ID_DESCRIPTOR>() as u32;
        desc.Type = FileIdType;
        desc.Anonymous.FileId = frn as i64;
        let h = match OpenFileById(
            vol,
            &desc,
            FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
        ) {
            Ok(h) if !h.is_invalid() => h,
            _ => {
                let id128 = id128?;
                let mut d2 = FILE_ID_DESCRIPTOR::default();
                d2.dwSize = size_of::<FILE_ID_DESCRIPTOR>() as u32;
                d2.Type = ExtendedFileIdType;
                d2.Anonymous.ExtendedFileId = FILE_ID_128 { Identifier: id128 };
                let h2 = OpenFileById(
                    vol,
                    &d2,
                    FILE_READ_ATTRIBUTES.0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    None,
                    FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                )
                .ok()?;
                if h2.is_invalid() {
                    return None;
                }
                h2
            }
        };
        let mut buf = [0u16; 1024];
        let n = GetFinalPathNameByHandleW(h, &mut buf, FILE_NAME_NORMALIZED);
        let _ = CloseHandle(h);
        if n == 0 || n as usize >= buf.len() {
            return None;
        }
        let mut s: String = OsString::from_wide(&buf[..n as usize])
            .to_string_lossy()
            .into_owned();
        // 去掉前缀 \\?\ ；保留 UNC 路径不动
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            if !stripped.starts_with("UNC\\") {
                s = stripped.to_string();
            }
        }
        Some(PathBuf::from(s))
    }

    /// 跟随 reparse 打开路径上的实际文件 / 占位文件，拿 (FRN, parent FRN, attrs, size, mtime, ctime)。
    fn open_file_info_by_path(
        path: &Path,
    ) -> Option<(u64, u64, u32, u64, u64, u64)> {
        use windows::Win32::Foundation::FILETIME;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // 不带 OPEN_REPARSE_POINT：让 OneDrive 占位以"目标"形式打开（仅读 attrs，不会触发 hydration）
        let h = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                FILE_READ_ATTRIBUTES.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                None,
            )
        }
        .ok()?;
        if h.is_invalid() {
            let _ = unsafe { CloseHandle(h) };
            return None;
        }
        let mut bh = BY_HANDLE_FILE_INFORMATION::default();
        let r = unsafe { GetFileInformationByHandle(h, &mut bh) };
        let _ = unsafe { CloseHandle(h) };
        if r.is_err() {
            return None;
        }
        let frn = ((bh.nFileIndexHigh as u64) << 32) | bh.nFileIndexLow as u64;
        let parent = path
            .parent()
            .and_then(file_id_from_path)
            .unwrap_or(0);
        let size = ((bh.nFileSizeHigh as u64) << 32) | bh.nFileSizeLow as u64;
        let to_u64 = |ft: FILETIME| ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64;
        Some((
            frn,
            parent,
            bh.dwFileAttributes,
            size,
            to_u64(bh.ftLastWriteTime),
            to_u64(bh.ftCreationTime),
        ))
    }

    fn volume_path(volume: &str) -> String {
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
}

#[cfg(windows)]
pub use imp::*;

#[cfg(not(windows))]
use findx2_core::{Error, RawEntry, Result, VolumeScanner};

#[cfg(not(windows))]
pub static SCAN_LIVE_ENTRIES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(not(windows))]
pub struct MftScanner;

#[cfg(not(windows))]
impl VolumeScanner for MftScanner {
    fn scan(&self, _volume: &str) -> Result<Vec<RawEntry>> {
        Err(Error::Platform("findx2-windows 仅在 Windows 上可用".into()))
    }
}

#[cfg(not(windows))]
pub fn scan_volume(_root: &str) -> Result<(Vec<RawEntry>, Vec<RawEntry>)> {
    Err(Error::Platform("findx2-windows 仅在 Windows 上可用".into()))
}

#[cfg(not(windows))]
pub fn scan_volume_fast(_root: &str) -> Result<(Vec<RawEntry>, Vec<RawEntry>)> {
    Err(Error::Platform("findx2-windows 仅在 Windows 上可用".into()))
}
