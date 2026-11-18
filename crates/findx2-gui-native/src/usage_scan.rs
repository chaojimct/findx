//! Production Windows batch walker. The scoped fixed-pool directory dispatch adapts
//! DiskTree scan.rs::dispatch_fixed (9b18b27), Copyright (c) 2026 Tobi Lütke, MIT:
//! disktree/LICENSE.disktree. Windows records use FindX's full 128-bit identities
//! and exact unnamed-stream allocation, not upstream's 64-bit inode model/MFT path.
use super::*;
use crate::disktree::lock;
use std::{
    collections::HashSet,
    ffi::OsString,
    os::windows::{ffi::OsStringExt, fs::OpenOptionsExt, io::AsRawHandle},
    sync::{Mutex, atomic::AtomicUsize},
};
use windows::Win32::{
    Foundation::HANDLE,
    Storage::FileSystem::{
        FILE_ID_EXTD_DIR_INFO, FILE_ID_INFO, FileIdExtdDirectoryInfo,
        FileIdExtdDirectoryRestartInfo, FileIdInfo, GetFileInformationByHandleEx,
    },
};
#[derive(Clone)]
struct Entry {
    path: PathBuf,
    directory: bool,
    attributes: u32,
    logical: u64,
    allocated: Option<u64>,
    mtime: u64,
    id: Option<(u64, u128)>,
}
struct Walk {
    nodes: Mutex<Vec<Node>>,
    seen: Mutex<HashSet<(u64, u128)>>,
    cancel: Arc<AtomicBool>,
    progress: Arc<Progress>,
    limits: Limits,
    errors: AtomicU64,
    skipped: AtomicU64,
    hardlinks: AtomicU64,
    unknown: AtomicU64,
    partial: AtomicBool,
    memory_limit: AtomicBool,
    alloc_failed: AtomicBool,
    used: AtomicUsize,
    pending: AtomicUsize,
}
impl Walk {
    fn stopped(&self) -> bool {
        if self.cancel.load(Ordering::Relaxed) {
            self.partial.store(true, Ordering::Relaxed);
            return true;
        }
        if self.memory_limit.load(Ordering::Relaxed) || self.alloc_failed.load(Ordering::Relaxed) {
            return true;
        }
        false
    }
    fn error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
        self.partial.store(true, Ordering::Relaxed);
    }
}
/// One handle per directory; 64KiB aligned pages. Unsupported filesystems alone
/// use the exact per-path fallback. A mid-stream error never restarts/duplicates entries.
fn enumerate(
    path: &Path,
    expected: Option<(u64, u128)>,
    ctx: &Walk,
    mut emit: impl FnMut(Entry) -> bool,
) -> Result<bool, String> {
    let handle = std::fs::OpenOptions::new()
        .access_mode(0x81)
        .share_mode(7)
        .custom_flags(0x02200000)
        .open(path)
        .map_err(|e| e.to_string())?;
    let mut identity = FILE_ID_INFO::default();
    ctx.progress.metadata_calls.fetch_add(1, Ordering::Relaxed);
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(handle.as_raw_handle()),
            FileIdInfo,
            (&mut identity as *mut FILE_ID_INFO).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    }
    .map_err(|e| e.to_string())?;
    let identity = (
        identity.VolumeSerialNumber,
        u128::from_le_bytes(identity.FileId.Identifier),
    );
    if expected.is_some_and(|x| x != identity) {
        return Err("目录身份在扫描期间变化".into());
    }
    let mut buffer = vec![0u64; 8192];
    let capacity = buffer.len() * 8;
    let header = std::mem::offset_of!(FILE_ID_EXTD_DIR_INFO, FileName);
    let mut first = true;
    loop {
        if ctx.stopped() {
            return Ok(true);
        }
        buffer.fill(0);
        ctx.progress.directory_calls.fetch_add(1, Ordering::Relaxed);
        let result = unsafe {
            GetFileInformationByHandleEx(
                HANDLE(handle.as_raw_handle()),
                if first {
                    FileIdExtdDirectoryRestartInfo
                } else {
                    FileIdExtdDirectoryInfo
                },
                buffer.as_mut_ptr().cast(),
                capacity as u32,
            )
        };
        match result {
            Ok(()) => {}
            Err(e) if e.code().0 as u32 == 0x80070012 => return Ok(true),
            Err(e)
                if first && matches!(e.code().0 as u32, 0x80070001 | 0x80070032 | 0x80070057) =>
            {
                return Ok(false);
            }
            Err(e) => return Err(e.to_string()),
        }
        first = false;
        let mut offset = 0usize;
        loop {
            if offset
                .checked_add(std::mem::size_of::<FILE_ID_EXTD_DIR_INFO>())
                .is_none_or(|n| n > capacity)
            {
                return Err("无效批枚举头".into());
            }
            let ptr = unsafe { buffer.as_ptr().cast::<u8>().add(offset) };
            // Read only the fixed header (FileName is a one-element tail member).
            let entry = unsafe { std::ptr::read_unaligned(ptr.cast::<FILE_ID_EXTD_DIR_INFO>()) };
            let bytes = entry.FileNameLength as usize;
            let end = offset
                .checked_add(header)
                .and_then(|n| n.checked_add(bytes))
                .ok_or("批枚举长度溢出")?;
            let next = entry.NextEntryOffset as usize;
            if bytes == 0
                || bytes % 2 != 0
                || end > capacity
                || (next != 0
                    && (next < header + bytes || next % 8 != 0 || offset + next >= capacity))
            {
                return Err("无效批枚举记录".into());
            }
            let name =
                unsafe { std::slice::from_raw_parts(ptr.add(header).cast::<u16>(), bytes / 2) };
            let name = OsString::from_wide(name);
            if name != "." && name != ".." {
                let record = Entry {
                    path: path.join(name),
                    directory: entry.FileAttributes & 0x10 != 0,
                    attributes: entry.FileAttributes,
                    logical: entry.EndOfFile.max(0) as u64,
                    allocated: u64::try_from(entry.AllocationSize).ok(),
                    mtime: ((entry.LastWriteTime as u64).saturating_sub(116_444_736_000_000_000))
                        / 10_000_000,
                    id: Some((identity.0, u128::from_le_bytes(entry.FileId.Identifier))),
                };
                if !emit(record) {
                    return Ok(true);
                }
            }
            if next == 0 {
                break;
            }
            offset += next;
        }
    }
}
fn dispatch<'s>(
    scope: &rayon::Scope<'s>,
    ctx: &'s Walk,
    parent: usize,
    identity: Option<(u64, u128)>,
) {
    // At most 4096 scheduled Rayon jobs; overflow is coalesced into this worker's
    // iterative ID-only list, bounded by the explicit node budget. No lost folders,
    // unbounded task fanout, recursive worker stack, or duplicated queued paths.
    let mut deferred = vec![(parent, identity)];
    while let Some((parent, identity)) = deferred.pop() {
        if ctx.stopped() {
            break;
        }
        let path = lock(&ctx.nodes)[parent].path.clone();
        dispatch_one(scope, ctx, parent, path, identity, &mut deferred);
    }
    ctx.pending.fetch_sub(1, Ordering::Relaxed);
}
fn dispatch_one<'s>(
    scope: &rayon::Scope<'s>,
    ctx: &'s Walk,
    parent: usize,
    path: PathBuf,
    identity: Option<(u64, u128)>,
    deferred: &mut Vec<(usize, Option<(u64, u128)>)>,
) {
    if ctx.stopped() {
        return;
    }
    #[cfg(debug_assertions)]
    if crate::isolation::development_var_os("FINDX_NATIVE_FIXTURE_LOCK").is_some()
        && crate::isolation::development_var_os("FINDX_NATIVE_SCAN_FIXTURE_DELAY_MS").is_some()
    {
        let ms = crate::isolation::development_var("FINDX_NATIVE_SCAN_FIXTURE_DELAY_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
            .min(1000);
        for _ in 0..ms / 10 {
            if ctx.stopped() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    let active = ctx.progress.active_workers.fetch_add(1, Ordering::Relaxed) + 1;
    ctx.progress
        .peak_workers
        .fetch_max(active, Ordering::Relaxed);
    let preview_path = {
        let nodes = lock(&ctx.nodes);
        let mut chain = Vec::new();
        let mut at = Some(parent);
        while let Some(i) = at {
            if i != 0 {
                chain.push(i);
            }
            at = nodes[i].parent;
        }
        chain.reverse();
        chain
            .into_iter()
            .take(6)
            .map(|i| {
                (
                    i,
                    nodes[i]
                        .path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .chars()
                        .take(192)
                        .collect::<String>(),
                )
            })
            .collect::<Vec<_>>()
    };
    let classes = [
        crate::disktree::classify::Category::Code,
        crate::disktree::classify::Category::AgentScratch,
        crate::disktree::classify::Category::Toolchain,
        crate::disktree::classify::Category::Synced,
        crate::disktree::classify::Category::Git,
        crate::disktree::classify::Category::Media,
        crate::disktree::classify::Category::Documents,
        crate::disktree::classify::Category::Cache,
        crate::disktree::classify::Category::Other,
    ];
    let mut preview_bytes = [0u64; 9];
    let mut preview_files = [0u64; 9];
    let mut preview_items = 0usize;
    let mut emit = |mut e: Entry| {
        if ctx.stopped() {
            return false;
        }
        // Reparse points, cloud placeholders, offline/recall entries are boundaries.
        if e.attributes & (0x400 | 0x1000 | 0x40000 | 0x400000) != 0 {
            ctx.skipped.fetch_add(1, Ordering::Relaxed);
            return true;
        }
        // Sparse/compressed and unidentifiable records need exact refinement. Never logical=>allocated.
        if e.attributes & (0x200 | 0x800) != 0 || e.id.is_none_or(|i| i.1 == 0) {
            ctx.progress.metadata_calls.fetch_add(1, Ordering::Relaxed);
            match info(&e.path) {
                Ok(i) if e.id.is_none_or(|id| id.1 == 0 || id == i.identity) => {
                    e.allocated = Some(i.allocated);
                    e.id = Some(i.identity);
                }
                _ => {
                    e.allocated = None;
                    ctx.error();
                }
            }
        }
        if e.id
            .is_some_and(|id| identity.is_some_and(|root| root.0 != id.0))
        {
            ctx.skipped.fetch_add(1, Ordering::Relaxed);
            return true;
        }
        let duplicate = if e.allocated.is_some() {
            if let Some(id) = e.id.filter(|id| id.1 != 0) {
                let mut seen = lock(&ctx.seen);
                if seen.try_reserve(1).is_err() {
                    ctx.alloc_failed.store(true, Ordering::Relaxed);
                    ctx.partial.store(true, Ordering::Relaxed);
                    return false;
                }
                !seen.insert(id)
            } else {
                false
            }
        } else {
            false
        };
        if duplicate && e.directory {
            ctx.skipped.fetch_add(1, Ordering::Relaxed);
            return true;
        }
        if duplicate {
            ctx.hardlinks.fetch_add(1, Ordering::Relaxed);
        }
        let unknown = e.allocated.is_none() || e.id.is_none_or(|i| i.1 == 0);
        if unknown {
            ctx.unknown.fetch_add(1, Ordering::Relaxed);
            ctx.partial.store(true, Ordering::Relaxed);
        }
        let occupied = if duplicate {
            0
        } else {
            e.allocated.unwrap_or(0)
        };
        let id = {
            let mut nodes = lock(&ctx.nodes);
            let mut used = ctx.used.load(Ordering::Relaxed);
            let node = Node {
                path: e.path.clone(),
                parent: Some(parent),
                children: vec![],
                entries: 1,
                files: u64::from(!e.directory),
                directories: u64::from(e.directory),
                category: category(&e.path, e.directory),
                own_allocated: occupied,
                directory: e.directory,
                logical: if e.directory { 0 } else { e.logical },
                mtime: e.mtime,
                allocated: occupied,
                hardlink_duplicate: duplicate,
                uncertain: unknown,
            };
            match try_push_node(&mut nodes, node, &mut used, ctx.limits) {
                Ok(id) => {
                    ctx.used.store(used, Ordering::Relaxed);
                    id
                }
                Err(Admission::MemoryBudget) => {
                    ctx.memory_limit.store(true, Ordering::Relaxed);
                    ctx.partial.store(true, Ordering::Relaxed);
                    return false;
                }
                Err(Admission::AllocFailed) => {
                    ctx.alloc_failed.store(true, Ordering::Relaxed);
                    ctx.partial.store(true, Ordering::Relaxed);
                    return false;
                }
                Err(Admission::Accept { .. }) => unreachable!(),
            }
        };
        ctx.progress.entries.fetch_add(1, Ordering::Relaxed);
        #[cfg(test)]
        ctx.progress.test_checkpoint();
        ctx.progress
            .allocated
            .fetch_add(occupied, Ordering::Relaxed);
        if e.directory {
            ctx.progress.directories.fetch_add(1, Ordering::Relaxed);
        }
        let class = crate::disktree::scan_preview::file_class(&e.path) as usize;
        preview_bytes[class] = preview_bytes[class].saturating_add(occupied);
        preview_files[class] += u64::from(!e.directory);
        preview_items += 1;
        if preview_items >= 128 {
            {
                let mut p = lock(&ctx.progress.preview);
                for i in 0..9 {
                    if preview_bytes[i] > 0 || preview_files[i] > 0 {
                        p.add_path(
                            &preview_path,
                            classes[i],
                            preview_bytes[i],
                            preview_files[i],
                        );
                    }
                }
            }
            preview_bytes = [0; 9];
            preview_files = [0; 9];
            preview_items = 0;
        }
        if e.directory {
            // A finite queued-directory budget, independent of the fixed IO worker count.
            if ctx.pending.fetch_add(1, Ordering::Relaxed) < 4096 {
                scope.spawn(move |scope| dispatch(scope, ctx, id, e.id));
            } else {
                ctx.pending.fetch_sub(1, Ordering::Relaxed);
                if deferred.try_reserve(1).is_err() {
                    ctx.alloc_failed.store(true, Ordering::Relaxed);
                    ctx.partial.store(true, Ordering::Relaxed);
                    return false;
                }
                deferred.push((id, e.id));
            }
        }
        true
    };
    match enumerate(&path, identity, ctx, &mut emit) {
        Ok(true) => {}
        Ok(false) => {
            // Exact compatibility fallback only for unsupported batch classes; no follow_links.
            match std::fs::read_dir(&path) {
                Ok(entries) => {
                    for entry in entries {
                        if ctx.stopped() {
                            break;
                        }
                        let Ok(entry) = entry else {
                            ctx.error();
                            continue;
                        };
                        let path = entry.path();
                        ctx.progress.metadata_calls.fetch_add(1, Ordering::Relaxed);
                        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                            ctx.error();
                            continue;
                        };
                        if boundary(&metadata) {
                            ctx.skipped.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                        let exact = info(&path);
                        let allocated = exact.as_ref().ok().map(|i| i.allocated);
                        let id = exact.ok().map(|i| i.identity);
                        let mtime = metadata
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map_or(0, |d| d.as_secs());
                        if !emit(Entry {
                            directory: metadata.is_dir(),
                            attributes: 0,
                            path,
                            logical: metadata.len(),
                            allocated,
                            mtime,
                            id,
                        }) {
                            break;
                        }
                    }
                }
                Err(_) => ctx.error(),
            }
        }
        Err(_) => {
            ctx.error();
            lock(&ctx.nodes)[parent].uncertain = true;
        }
    }
    if preview_items > 0 {
        let mut p = lock(&ctx.progress.preview);
        for i in 0..9 {
            if preview_bytes[i] > 0 || preview_files[i] > 0 {
                p.add_path(
                    &preview_path,
                    classes[i],
                    preview_bytes[i],
                    preview_files[i],
                );
            }
        }
    }
    ctx.progress.active_workers.fetch_sub(1, Ordering::Relaxed);
}
pub(super) fn scan(
    root: &Path,
    cancel: Arc<AtomicBool>,
    progress: Arc<Progress>,
    limits: Limits,
) -> Result<Scan, String> {
    let start = Instant::now();
    let metadata = std::fs::symlink_metadata(root).map_err(|e| e.to_string())?;
    if !metadata.is_dir() || boundary(&metadata) {
        return Err("仅扫描普通本地目录，不跟随重解析/云占位边界".into());
    }
    progress.metadata_calls.fetch_add(1, Ordering::Relaxed);
    let exact = info(root)?;
    let occupied = exact.allocated;
    let root_node = Node {
        path: root.to_owned(),
        parent: None,
        children: vec![],
        entries: 1,
        files: 0,
        directories: 1,
        category: 0,
        own_allocated: occupied,
        directory: true,
        logical: 0,
        mtime: metadata
            .modified()
            .ok()
            .and_then(|v| v.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |v| v.as_secs()),
        allocated: occupied,
        hardlink_duplicate: false,
        uncertain: false,
    };
    progress.entries.store(1, Ordering::Relaxed);
    progress.directories.store(1, Ordering::Relaxed);
    progress.allocated.store(occupied, Ordering::Relaxed);
    lock(&progress.preview).directory(root);
    let mut root_nodes = Vec::new();
    let mut root_used = 0;
    try_push_node(&mut root_nodes, root_node, &mut root_used, limits)
        .map_err(|r| format!("根目录内存资源不足：{r:?}"))?;
    let ctx = Walk {
        nodes: Mutex::new(root_nodes),
        seen: Mutex::new([exact.identity].into_iter().collect()),
        cancel: cancel.clone(),
        progress: progress.clone(),
        limits,
        errors: AtomicU64::new(0),
        skipped: AtomicU64::new(0),
        hardlinks: AtomicU64::new(0),
        unknown: AtomicU64::new(0),
        partial: AtomicBool::new(false),
        memory_limit: AtomicBool::new(false),
        alloc_failed: AtomicBool::new(false),
        used: AtomicUsize::new(root_used),
        pending: AtomicUsize::new(1),
    };
    let workers = std::thread::available_parallelism()
        .map_or(2, |n| n.get())
        .clamp(1, 4);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .thread_name(|i| format!("findx-usage-{i}"))
        .build()
        .map_err(|e| e.to_string())?;
    pool.scope(|scope| dispatch(scope, &ctx, 0, Some(exact.identity)));
    let enumerated = start.elapsed();
    let mut result = Scan {
        logical_generation: None,
        nodes: ctx.nodes.into_inner().unwrap_or_else(|e| e.into_inner()),
        errors: ctx.errors.load(Ordering::Relaxed),
        skipped_links: ctx.skipped.load(Ordering::Relaxed),
        hardlinks: ctx.hardlinks.load(Ordering::Relaxed),
        unknown: ctx.unknown.load(Ordering::Relaxed),
        partial: ctx.partial.load(Ordering::Relaxed),
        cancelled: cancel.load(Ordering::Relaxed),
        elapsed: Duration::ZERO,
        entry_limit_reached: false,
        time_limit_reached: false,
        memory_limit_reached: ctx.memory_limit.load(Ordering::Relaxed),
        alloc_failed: ctx.alloc_failed.load(Ordering::Relaxed),
        limits,
        volume: volume_space(root).ok(),
    };
    // Children always have larger indices. Complete aggregation of already-read data
    // even after cancel; do not return a mathematically half-aggregated tree.
    let mut colors = vec![[0u64; 6]; result.nodes.len()];
    for i in (0..result.nodes.len()).rev() {
        let n = &result.nodes[i];
        colors[i][n.category.min(5)] = colors[i][n.category.min(5)].saturating_add(n.own_allocated);
        if let Some(parent) = n.parent {
            let allocated = n.allocated;
            let logical = n.logical;
            let entries = n.entries;
            let files = n.files;
            let dirs = n.directories;
            let uncertain = n.uncertain;
            for c in 0..6 {
                colors[parent][c] = colors[parent][c].saturating_add(colors[i][c]);
            }
            let p = &mut result.nodes[parent];
            p.allocated = p.allocated.saturating_add(allocated);
            p.logical = p.logical.saturating_add(logical);
            p.entries = p.entries.saturating_add(entries);
            p.files = p.files.saturating_add(files);
            p.directories = p.directories.saturating_add(dirs);
            p.uncertain |= uncertain;
        }
        if i % 1024 == 0 && cancel.load(Ordering::Relaxed) {
            std::thread::yield_now();
            result.partial = true;
            result.cancelled = true;
        }
    }
    let sizes = result.nodes.iter().map(|n| n.allocated).collect::<Vec<_>>();
    for (i, n) in result.nodes.iter_mut().enumerate() {
        n.children
            .sort_unstable_by_key(|&j| (std::cmp::Reverse(sizes[j]), j));
        if n.directory {
            n.category = (0..6).max_by_key(|&c| colors[i][c]).unwrap_or(0);
        }
    }
    result.elapsed = start.elapsed();
    eprintln!(
        "usage_scan entries={} enum_ms={} aggregate_ms={} directory_api={} metadata_queries={} workers={} peak={} partial={}",
        result.nodes.len(),
        enumerated.as_millis(),
        result.elapsed.saturating_sub(enumerated).as_millis(),
        progress.directory_calls.load(Ordering::Relaxed),
        progress.metadata_calls.load(Ordering::Relaxed),
        workers,
        progress.peak_workers.load(Ordering::Relaxed),
        result.partial
    );
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn production_batch_path_avoids_per_file_metadata_queries_and_bounds_workers() {
        let root = std::env::temp_dir().join(format!(
            "findx-production-batch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        std::fs::create_dir(&root).unwrap();
        for d in 0..24 {
            let dir = root.join(format!("dir-{d}"));
            std::fs::create_dir(&dir).unwrap();
            for i in 0..48 {
                std::fs::write(dir.join(format!("file-{i}.bin")), [d as u8; 8192]).unwrap();
            }
        }
        let progress = Arc::new(Progress::default());
        let result = scan(
            &root,
            Arc::new(AtomicBool::new(false)),
            progress.clone(),
            Limits::default(),
        )
        .unwrap();
        assert!(!result.partial, "{}", result.partial_reasons());
        assert_eq!(result.nodes[0].files, 1152);
        assert_eq!(result.nodes[0].logical, 1152 * 8192);
        assert!(progress.peak_workers.load(Ordering::Relaxed) <= 4);
        assert!(progress.directory_calls.load(Ordering::Relaxed) > 0);
        assert!(
            progress.metadata_calls.load(Ordering::Relaxed) < 64,
            "ordinary file path fell back to individual metadata queries"
        );
        let layout = crate::usage_view::build_scene(
            &result,
            crate::usage_view::SceneKey {
                scan: 1,
                root: 0,
                depth: 3,
                width: 1000,
                height: 600,
                detail: 1,
            },
        );
        assert!(!layout.tiles.is_empty());
        println!(
            "public fixture: files={} enum+aggregate_ms={} batch_directory_api={} metadata_queries={} peak_workers={} layout_us={} tiles={}",
            result.nodes[0].files,
            result.elapsed.as_millis(),
            progress.directory_calls.load(Ordering::Relaxed),
            progress.metadata_calls.load(Ordering::Relaxed),
            progress.peak_workers.load(Ordering::Relaxed),
            layout.layout_elapsed.as_micros(),
            layout.tiles.len()
        );
    }
}
