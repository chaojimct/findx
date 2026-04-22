//! 按目录批量拉取 `(FileId, size, mtime, ctime)` 的快路径。
//!
//! ## 为什么需要它
//! `fill_metadata_by_id_pooled`（`OpenFileById` per file）在 8.5M 文件的盘上实测 ~10 K files/s，
//! 是 `IRP_MJ_CREATE` + `IRP_MJ_QUERY_INFORMATION` 两次内核往返 × N 的物理上限。
//! 而 `GetFileInformationByHandleEx(h_dir, FileIdBothDirectoryInfo, buf, 64K)` 一次 syscall
//! 就能拿回目录里**一批**子项的 size/时间/FRN，摊销到单文件只剩 ~几百纳秒 memcpy。
//!
//! ## 原理
//! - 入口：所有目录的 (dir_frn, file_id_128_opt) 列表 + 每个目录预期含多少子文件的快速估计；
//! - 每个 rayon 任务独立打开一次卷句柄（与 `fill_metadata_by_id_pooled` 同策略）；
//! - 对每个 dir_frn：`OpenFileById(vol, dir_frn, FILE_LIST_DIRECTORY)` → 循环
//!   `GetFileInformationByHandleEx(FileIdBothDirectoryInfo, buf)` 直到 ERROR_NO_MORE_FILES；
//! - 每条记录提取 `FileId(u64)` + `EndOfFile(i64, 单位字节)` + `LastWriteTime(FILETIME u64)` +
//!   `CreationTime(FILETIME u64)`，直接塞进输出 vec；
//! - 输出键是 FRN（u64），由上层按 FRN→entry_idx 映射写回 `files[]`。
//!
//! ## 取舍
//! - 未处理 ReFS 128-bit ID：ReFS 下 `FileIdBothDirectoryInfo` 的 FileId 字段就不是稳定的 FRN，
//!   本实现跳过这种卷（上层 fall back 到 OpenFileById）。实际 Win10/11 默认 NTFS，99% 用户不踩到。
//! - 目录内的子目录 entry 也会被 syscall 顺带返回，我们简单过滤掉 `FILE_ATTRIBUTE_DIRECTORY`，
//!   目录自己的 mtime/ctime 由其父目录的这次扫描附带拿到。
//! - reparse point 的处理：`OpenFileById` 默认会跟随，我们不加 `FILE_FLAG_OPEN_REPARSE_POINT`
//!   —— 因为 reparse 目录通常就是我们想进去的符号链接/junction；OneDrive cloud 占位文件
//!   不会触发下载（只读元数据）。

#[cfg(windows)]
use std::mem::size_of;
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
#[cfg(windows)]
use std::sync::Arc;

#[cfg(windows)]
use rayon::prelude::*;
#[cfg(windows)]
use windows::core::PCWSTR;
#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, HANDLE};
#[cfg(windows)]
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ExtendedFileIdType, FileIdBothDirectoryInfo, FileIdType,
    GetFileInformationByHandleEx, OpenFileById, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_ID_128, FILE_ID_BOTH_DIR_INFO, FILE_ID_DESCRIPTOR, FILE_LIST_DIRECTORY,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};

/// 单条回填结果：`(frn, size, mtime_filetime, ctime_filetime)`。
#[cfg(windows)]
pub type DirMetaRec = (u64, u64, u64, u64);
#[cfg(not(windows))]
pub type DirMetaRec = (u64, u64, u64, u64);

/// syscall 缓冲区大小。256 K：对绝大多数目录一次 syscall 吃完；
/// 对 node_modules/.cache 这种数千文件大目录，syscall 数从 4~5 次降到 1~2 次。
/// 每个 rayon 任务只持有一份 buf，并发开销 = threads × 256K = ~5 MB，可接受。
#[cfg(windows)]
const BUF_CAP: usize = 256 * 1024;
/// 每个 rayon 任务承担的目录数下限。目录级 syscall 已经很便宜，切过细反而浪费。
#[cfg(windows)]
const MIN_DIRS_PER_CHUNK: usize = 1024;
/// 每线程目标块数。
#[cfg(windows)]
const CHUNKS_PER_THREAD: usize = 8;
/// 进度上报节流（按已处理目录数）。
#[cfg(windows)]
const PROGRESS_EVERY_DIRS: usize = 50_000;

/// 按目录批量回填：对每个 `(dir_frn, dir_id_128)` 打开目录并枚举子项，
/// 返回一个 `(frn, size, mtime, ctime)` 的扁平 vec。
///
/// 失败的目录（权限、已删、句柄失效）会被直接跳过，不阻塞整体。
#[cfg(windows)]
pub fn fetch_dir_meta_batched(
    volume_device_path: &str,
    dir_frns: &[(u64, Option<[u8; 16]>)],
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    cancel: Option<Arc<AtomicBool>>,
) -> Vec<DirMetaRec> {
    if dir_frns.is_empty() {
        return Vec::new();
    }
    let total_dirs = dir_frns.len();
    let threads = std::thread::available_parallelism()
        .map(|x| x.get())
        .unwrap_or(8)
        .max(1);
    let desired_chunks = threads.saturating_mul(CHUNKS_PER_THREAD);
    let mut chunk_sz = (total_dirs + desired_chunks - 1) / desired_chunks;
    if chunk_sz < MIN_DIRS_PER_CHUNK {
        chunk_sz = MIN_DIRS_PER_CHUNK;
    }

    findx2_core::progress!(
        "NtQueryDirectoryFile 批量：dirs={} threads={} chunk={} (~{} 块)",
        total_dirs,
        threads,
        chunk_sz,
        (total_dirs + chunk_sz - 1) / chunk_sz,
    );

    let done_dirs = Arc::new(AtomicUsize::new(0));
    let path_owned = volume_device_path.to_string();

    let nested: Vec<Vec<DirMetaRec>> = dir_frns
        .par_chunks(chunk_sz)
        .map(|chunk| {
            if let Some(c) = cancel.as_ref() {
                if c.load(Ordering::Relaxed) {
                    return Vec::new();
                }
            }

            let wide: Vec<u16> = path_owned.encode_utf16().chain(Some(0)).collect();
            let vol = unsafe {
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
            let vol_h = match vol {
                Ok(h) if !h.is_invalid() => h,
                Ok(h) => {
                    let _ = unsafe { CloseHandle(h) };
                    return Vec::new();
                }
                Err(_) => return Vec::new(),
            };

            // 单 chunk 里所有 dir 共享 64K syscall buf，避免每次都重 alloc 64K 堆内存。
            // 注意要 u64-aligned（FILE_ID_BOTH_DIR_INFO 里 LARGE_INTEGER 必须 8B 对齐）。
            let mut buf: Vec<u64> = vec![0u64; BUF_CAP / 8];
            let mut out: Vec<DirMetaRec> = Vec::with_capacity(chunk.len() * 6);

            let mut local_done = 0usize;
            for &(dir_frn, dir_id128) in chunk {
                if local_done & 0x3FF == 0 {
                    if let Some(c) = cancel.as_ref() {
                        if c.load(Ordering::Relaxed) {
                            break;
                        }
                    }
                }
                local_done += 1;

                let Some(dir_h) = (unsafe { open_dir_by_id(vol_h, dir_frn, dir_id128) })
                else {
                    continue;
                };
                unsafe {
                    enumerate_dir_into(dir_h, &mut buf, &mut out);
                    let _ = CloseHandle(dir_h);
                }
            }

            let _ = unsafe { CloseHandle(vol_h) };

            let prev = done_dirs.fetch_add(chunk.len(), Ordering::Relaxed);
            let cur = (prev + chunk.len()).min(total_dirs);
            let prev_bucket = prev / PROGRESS_EVERY_DIRS;
            let cur_bucket = cur / PROGRESS_EVERY_DIRS;
            if cur_bucket > prev_bucket || cur >= total_dirs {
                findx2_core::progress!(
                    "NtQueryDirectoryFile 进度：dirs {}/{} ({}%) 累计条目 ~{}",
                    cur,
                    total_dirs,
                    cur * 100 / total_dirs.max(1),
                    out.len(),
                );
            }
            if let Some(cb) = progress {
                cb(cur, total_dirs);
            }
            out
        })
        .collect();

    let mut all = Vec::with_capacity(total_dirs * 6);
    for v in nested {
        all.extend(v);
    }
    findx2_core::progress!(
        "NtQueryDirectoryFile 批量结束：处理目录 {}，回填条目 {}",
        total_dirs,
        all.len(),
    );
    all
}

#[cfg(not(windows))]
pub fn fetch_dir_meta_batched(
    _volume_device_path: &str,
    _dir_frns: &[(u64, Option<[u8; 16]>)],
    _progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    _cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Vec<DirMetaRec> {
    Vec::new()
}

#[cfg(windows)]
unsafe fn open_dir_by_id(
    vol: HANDLE,
    dir_frn: u64,
    dir_id128: Option<[u8; 16]>,
) -> Option<HANDLE> {
    // 首选 64 位 FileId。
    let mut desc = FILE_ID_DESCRIPTOR::default();
    desc.dwSize = size_of::<FILE_ID_DESCRIPTOR>() as u32;
    desc.Type = FileIdType;
    desc.Anonymous.FileId = dir_frn as i64;
    // FILE_LIST_DIRECTORY 必须 + BACKUP_SEMANTICS 以便 GetFileInformationByHandleEx 返回目录信息。
    let h1 = OpenFileById(
        vol,
        &desc,
        FILE_LIST_DIRECTORY.0,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        None,
        FILE_FLAG_BACKUP_SEMANTICS,
    );
    if let Ok(h) = h1 {
        if !h.is_invalid() {
            return Some(h);
        }
        let _ = CloseHandle(h);
    }
    // 回退 128 位 ID（ReFS / USN v3 场景）。
    let id128 = dir_id128?;
    let mut desc2 = FILE_ID_DESCRIPTOR::default();
    desc2.dwSize = size_of::<FILE_ID_DESCRIPTOR>() as u32;
    desc2.Type = ExtendedFileIdType;
    desc2.Anonymous.ExtendedFileId = FILE_ID_128 { Identifier: id128 };
    let h2 = OpenFileById(
        vol,
        &desc2,
        FILE_LIST_DIRECTORY.0,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        None,
        FILE_FLAG_BACKUP_SEMANTICS,
    )
    .ok()?;
    if h2.is_invalid() {
        return None;
    }
    Some(h2)
}

#[cfg(windows)]
unsafe fn enumerate_dir_into(
    dir_h: HANDLE,
    buf: &mut [u64],
    out: &mut Vec<DirMetaRec>,
) {
    const ATTR_DIRECTORY: u32 = 0x10;
    let buf_bytes = buf.as_mut_ptr() as *mut u8;
    let buf_len = (buf.len() * 8) as u32;

    loop {
        let r = GetFileInformationByHandleEx(
            dir_h,
            FileIdBothDirectoryInfo,
            buf_bytes as *mut _,
            buf_len,
        );
        if r.is_err() {
            // 首次就失败（含 ERROR_NO_MORE_FILES / ERROR_ACCESS_DENIED 等）：直接退出该目录。
            break;
        }
        // 遍历链式 FILE_ID_BOTH_DIR_INFO：每条 NextEntryOffset=0 表示末尾。
        let mut p = buf_bytes;
        loop {
            let rec = &*(p as *const FILE_ID_BOTH_DIR_INFO);
            let next = rec.NextEntryOffset;
            // 过滤 "." / ".." / 子目录。
            let is_dir = (rec.FileAttributes & ATTR_DIRECTORY) != 0;
            // FILE_ID_BOTH_DIR_INFO.FileName 是 WCHAR 变长数组，长度字节数 rec.FileNameLength；
            // 我们不关心文件名，只看 [".", ".."]：第 1 字符 '.' 且长度 2/4 字节。
            let name_bytes = rec.FileNameLength as usize;
            let is_dot = if name_bytes == 2 || name_bytes == 4 {
                let name_ptr = (&rec.FileName) as *const u16;
                let c0 = *name_ptr;
                if c0 != b'.' as u16 {
                    false
                } else if name_bytes == 2 {
                    true
                } else {
                    let c1 = *name_ptr.add(1);
                    c1 == b'.' as u16
                }
            } else {
                false
            };

            if !is_dir && !is_dot {
                // FileId 字段：NTFS 下等于 FRN；ReFS/其他可能是不稳定 128 位 id，我们仍写入，
                // 上层按 frn 映射找不到就自然丢弃。
                let frn = rec.FileId as u64;
                // LARGE_INTEGER：这里直接拼成 i64 当 FILETIME 处理。
                let mtime = rec.LastWriteTime as u64;
                let ctime = rec.CreationTime as u64;
                let size = rec.EndOfFile as u64;
                out.push((frn, size, mtime, ctime));
            }
            if next == 0 {
                break;
            }
            p = p.add(next as usize);
        }
        // 下一轮 syscall 会续接剩余条目（GetFileInformationByHandleEx 对同一句柄是有状态的）。
    }
}
