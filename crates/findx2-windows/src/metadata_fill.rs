//! 卷句柄 + `OpenFileById` 批量回填文件元数据（size/mtime/ctime）。
//!
//! 是 fast 首遍 + 异步回填、与 full-stat MFT 枚举共用的唯一回填入口。
//! 历史上还有一条 "顺序读 \\?\\X:\\$MFT 一次性建立 FRN→meta 表" 的快路径
//! （FindX C++ `LoadNtfsMftMetaMap`），实测在 Win10/11 用户态 100% 被
//! `ERROR_ACCESS_DENIED(5)` 拒访，已彻底删除，不再纠结。
//!
//! 设计：
//! - 每个 rayon 任务独立打开一次卷句柄（`CreateFileW("\\\\.\\X:")`），块内多次复用，
//!   避免每条 entry 一次 `CreateFile`；
//! - 任务粒度 = `threads × CHUNKS_PER_THREAD`，再夹下限避免太细；
//! - 进度通过回调透出，便于 service 把 `(done, total)` 推给 IPC；
//! - 取消通过 `Arc<AtomicBool>`，任意时刻 set true 后剩余 chunk 立即跳过。

#[cfg(windows)]
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
#[cfg(windows)]
use std::sync::Arc;

#[cfg(windows)]
use rayon::prelude::*;
#[cfg(windows)]
use windows::core::PCWSTR;
#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ};
#[cfg(windows)]
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING,
};

#[cfg(windows)]
use crate::open_by_id::fetch_file_metadata_by_id;

/// 回填 metadata 时每个 rayon 任务承担的 entry 数量下限。
/// 块过小会让 `CreateFileW(\\\\.\\X:)` 发生太频繁、调度开销盖住 IO；
/// 实测 8.5M 文件时块 = 8K 是个比较稳的甜点（每卷句柄被复用 8K 次）。
#[cfg(windows)]
const MIN_CHUNK: usize = 4096;
/// 每线程目标块数。再大没有意义（rayon 自己会 work-stealing）。
#[cfg(windows)]
const CHUNKS_PER_THREAD: usize = 8;
/// 默认每隔多少条回报一次进度（外部不传 `progress` 时只打 tracing 日志）。
#[cfg(windows)]
const PROGRESS_EVERY_DEFAULT: usize = 200_000;

/// 一条 entry 的元数据更新：`(idx_in_input_array, size, mtime_filetime, ctime_filetime)`。
#[cfg(windows)]
pub type MetaUpdate = (usize, u64, u64, u64);

/// `OpenFileById` 批量回填 API。
///
/// - `volume_device_path`：例如 `\\\\.\\C:`（与 `volume_path()` 一致）。
/// - `file_ids` / `file_id_128s`：和 `indices` 同 owner，**前者按下标索引**。
/// - `indices`：要回填的下标集合（在 `file_ids` 上），允许稀疏。
/// - `progress`：每完成若干条调用一次（`(done, total)`），主要给 service IPC 用；
///    传 `None` 则只打 `progress!` 日志。
/// - `cancel`：传 `Some` 后，外部 set true 即刻跳过剩余块。
///
/// 返回所有成功取到 metadata 的更新，乱序；失败的 entry 直接丢弃（极少数 reparse / 破坏文件）。
#[cfg(windows)]
pub fn fill_metadata_by_id_pooled(
    volume_device_path: &str,
    file_ids: &[u64],
    file_id_128s: &[Option<[u8; 16]>],
    indices: &[usize],
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    cancel: Option<Arc<AtomicBool>>,
) -> Vec<MetaUpdate> {
    if indices.is_empty() {
        return Vec::new();
    }

    let total = indices.len();
    let threads = std::thread::available_parallelism()
        .map(|x| x.get())
        .unwrap_or(8)
        .max(1);
    let desired_chunks = threads.saturating_mul(CHUNKS_PER_THREAD);
    let mut chunk_sz = (total + desired_chunks - 1) / desired_chunks;
    if chunk_sz < MIN_CHUNK {
        chunk_sz = MIN_CHUNK;
    }

    findx2_core::progress!(
        "OpenFileById 池化回填：total={}, threads={}, chunk_sz={} (~{} 块)",
        total,
        threads,
        chunk_sz,
        (total + chunk_sz - 1) / chunk_sz,
    );

    let done = Arc::new(AtomicUsize::new(0));
    let path_owned = volume_device_path.to_string();

    let nested: Vec<Vec<MetaUpdate>> = indices
        .par_chunks(chunk_sz)
        .map(|chunk| {
            if let Some(c) = cancel.as_ref() {
                if c.load(Ordering::Relaxed) {
                    return Vec::new();
                }
            }

            // 每个 rayon 任务独立打开一次卷句柄；块内 OpenFileById 全部复用它。
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

            let mut out = Vec::with_capacity(chunk.len());
            for (i, &idx) in chunk.iter().enumerate() {
                // 每隔较多条目检查一次 cancel，避免 atomic load 频率太高。
                if i & 0xFF == 0 {
                    if let Some(c) = cancel.as_ref() {
                        if c.load(Ordering::Relaxed) {
                            break;
                        }
                    }
                }
                let frn = file_ids[idx];
                let id128 = file_id_128s.get(idx).copied().flatten();
                if let Some((sz, mt, ct)) =
                    unsafe { fetch_file_metadata_by_id(vol_h, frn, id128) }
                {
                    out.push((idx, sz, mt, ct));
                }
            }
            let _ = unsafe { CloseHandle(vol_h) };

            // 进度上报：用块完成数累加，不再每条 atomic.add，省一点点。
            let prev = done.fetch_add(chunk.len(), Ordering::Relaxed);
            let cur = (prev + chunk.len()).min(total);
            let prev_m = prev / PROGRESS_EVERY_DEFAULT;
            let cur_m = cur / PROGRESS_EVERY_DEFAULT;
            if cur_m > prev_m || cur >= total {
                findx2_core::progress!("OpenFileById 进度: {}/{} ({}%)", cur, total, cur * 100 / total.max(1));
            }
            if let Some(cb) = progress {
                cb(cur, total);
            }
            out
        })
        .collect();

    let mut all = Vec::with_capacity(total);
    for v in nested {
        all.extend(v);
    }
    findx2_core::progress!(
        "OpenFileById 池化回填结束：成功 {} / 请求 {}",
        all.len(),
        total
    );
    all
}

#[cfg(not(windows))]
pub type MetaUpdate = (usize, u64, u64, u64);

#[cfg(not(windows))]
pub fn fill_metadata_by_id_pooled(
    _volume_device_path: &str,
    _file_ids: &[u64],
    _file_id_128s: &[Option<[u8; 16]>],
    _indices: &[usize],
    _progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    _cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Vec<MetaUpdate> {
    Vec::new()
}
