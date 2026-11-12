//! Dev-only measured-tree repository. No index.bin, service, IPC, file contents,
//! recursive mtime freshness claim, or background volume scan. Versioned bounded
//! snapshots store UTF-16 basenames + parent indices, never repeated full paths.
use crate::disk_usage::{self, Limits, Node, Progress, Scan, VolumeSpace, tree_storage_bytes};
use crate::disktree::lock;
use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
const SCHEMA: u32 = 3;
pub(crate) const MAX_NODES: usize = 8_000_000;
const MAX_UNPACKED: u64 = 2 * 1024 * 1024 * 1024;
// Cache budget includes conservative downstream lookup/scene/identity overhead.
const MEMORY_BUDGET: usize = 256 * 1024 * 1024;
const READ_UNPACKED: u64 = 256 * 1024 * 1024;
const DISK_BUDGET: u64 = 512 * 1024 * 1024;
const ROOT_LIMIT: usize = 8;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Measured,
    Memory,
    Snapshot,
}
#[derive(Clone)]
pub struct Cached {
    pub scan: Arc<Scan>,
    pub source: Source,
    pub captured: u64,
}
impl Cached {
    pub fn label(&self) -> String {
        let source = match self.source {
            Source::Measured => "本地批量实测",
            Source::Memory => "内存缓存 · 未验证新鲜度",
            Source::Snapshot => "持久快照 · 未验证新鲜度",
        };
        format!(
            "{source} · 扫描时点 {}（本地）{}",
            crate::search_logic::format_time(self.captured),
            if self.scan.is_legacy_entry_truncation() {
                " · 旧的条目或时间上限截断结果，请手动重新扫描"
            } else if self.scan.memory_limit_reached {
                " · 达到内存预算的部分结果，请缩小范围或在高级选项中提高内存上限后手动重扫"
            } else if self.scan.alloc_failed {
                " · 系统内存不足，已保留已测部分"
            } else if self.scan.partial {
                " · 部分结果"
            } else {
                ""
            }
        )
    }
}
/// Keep future indexed-logical data explicitly separate from actual allocation.
/// This contract accepts actual allocation only. A future indexed-logical sketch
/// needs a separate typed result/API and must never be labeled measured allocation.
/// No formal engine endpoint is queried or invented by this implementation.
pub trait MeasuredUsageProvider: Send + Sync {
    fn measure(
        &self,
        root: &Path,
        cancel: Arc<AtomicBool>,
        progress: Arc<Progress>,
        limits: Limits,
    ) -> Result<Scan, String>;
}
pub struct MeasuredFilesystem;
impl MeasuredUsageProvider for MeasuredFilesystem {
    fn measure(
        &self,
        root: &Path,
        cancel: Arc<AtomicBool>,
        progress: Arc<Progress>,
        limits: Limits,
    ) -> Result<Scan, String> {
        disk_usage::scan(root, cancel, progress, limits)
    }
}
#[derive(Clone, Hash, PartialEq, Eq)]
struct Key {
    volume: u64,
    id: u128,
    created: u128,
}
fn key(root: &Path) -> Option<Key> {
    let (volume, id) = disk_usage::root_identity(root)?;
    let created = std::fs::symlink_metadata(root)
        .ok()?
        .created()
        .ok()
        .and_then(|s| s.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    Some(Key {
        volume,
        id,
        created,
    })
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub(crate) fn snapshot_allowed(node_count: usize, _unpacked_hint: u64) -> Result<(), String> {
    if node_count > MAX_NODES {
        return Err(format!(
            "快照超出磁盘缓存节点上限（{MAX_NODES}），已拒绝写入缓存；扫描结果仍留在内存，不会因此截断"
        ));
    }
    Ok(())
}
fn memory_cost(scan: &Scan) -> usize {
    tree_storage_bytes(&scan.nodes)
        + (scan.nodes.capacity() - scan.nodes.len()) * std::mem::size_of::<Node>()
}
struct Entry {
    value: Cached,
    key: Key,
    bytes: usize,
}
#[derive(Default)]
struct State {
    entries: VecDeque<Entry>,
    flights: HashMap<Key, Arc<Job>>,
}
pub struct Repository {
    directory: PathBuf,
    #[cfg(test)]
    reads: std::sync::atomic::AtomicUsize,
    state: Mutex<State>,
    provider: Arc<dyn MeasuredUsageProvider>,
}
pub struct Job {
    root: PathBuf,
    key: Option<Key>,
    force: bool,
    limits: Limits,
    started: AtomicBool,
    result: Mutex<Option<Result<Cached, String>>>,
    done: Condvar,
    pub cancel: Arc<AtomicBool>,
    pub progress: Arc<Progress>,
}
impl Repository {
    pub fn global() -> Arc<Self> {
        static GLOBAL: OnceLock<Arc<Repository>> = OnceLock::new();
        GLOBAL
            .get_or_init(|| {
                Arc::new(Self::new(
                    if std::env::var_os("FINDX_NATIVE_USAGE_OPEN").is_some()
                        && !std::env::args().any(|arg| arg == "--live")
                    {
                        std::env::temp_dir()
                            .join(format!("findx-fixture-cache-{}", std::process::id()))
                    } else {
                        crate::isolation::isolated_data_dir().join("usage-cache-v1")
                    },
                    Arc::new(MeasuredFilesystem),
                ))
            })
            .clone()
    }
    pub fn new(directory: PathBuf, provider: Arc<dyn MeasuredUsageProvider>) -> Self {
        Self {
            directory,
            #[cfg(test)]
            reads: std::sync::atomic::AtomicUsize::new(0),
            state: Mutex::new(State::default()),
            provider,
        }
    }
    pub fn request(&self, root: &Path, force: bool, limits: Limits) -> Arc<Job> {
        let key = key(root);
        let mut state = lock(&self.state);
        if let Some(job) = key.as_ref().and_then(|k| state.flights.get(k)) {
            return job.clone();
        }
        let job = Arc::new(Job {
            root: root.to_owned(),
            key: key.clone(),
            force,
            limits,
            started: AtomicBool::new(false),
            result: Mutex::new(None),
            done: Condvar::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(Progress::default()),
        });
        if let Some(key) = key {
            state.flights.insert(key, job.clone());
        }
        job
    }
    fn snapshot_path(&self, key: &Key) -> PathBuf {
        self.directory.join(format!(
            "{:016x}-{:032x}-{:032x}.fxdu",
            key.volume, key.id, key.created
        ))
    }
    fn remember(&self, key: Key, value: Cached) {
        let bytes = memory_cost(&value.scan);
        if bytes > MEMORY_BUDGET {
            return;
        }
        let mut state = lock(&self.state);
        state.entries.retain(|e| e.key != key);
        state.entries.push_front(Entry { value, key, bytes });
        while state.entries.len() > ROOT_LIMIT
            || state.entries.iter().map(|e| e.bytes).sum::<usize>() > MEMORY_BUDGET
        {
            state.entries.pop_back();
        }
    }
    /// Ancestor coverage is an old observation, not recursive freshness. A partial
    /// snapshot only covers nodes actually present; never synthesize missing children.
    pub fn lookup(&self, root: &Path) -> Option<Cached> {
        self.lookup_reported(root).ok().flatten()
    }
    /// Interactive cache reads preserve resource refusal reasons; never rewrite a snapshot.
    pub fn lookup_reported(&self, root: &Path) -> Result<Option<Cached>, String> {
        let Some(root_key) = key(root) else {
            return Ok(None);
        };
        {
            let mut state = lock(&self.state);
            if let Some(index) = state
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| {
                    (e.key == root_key
                        || e.value
                            .scan
                            .nodes
                            .first()
                            .is_some_and(|n| root.starts_with(&n.path)))
                        && e.value
                            .scan
                            .nodes
                            .iter()
                            .any(|n| n.directory && n.path == root)
                })
                .max_by_key(|(i, e)| (e.value.captured, std::cmp::Reverse(*i)))
                .map(|(i, _)| i)
            {
                let Some(entry) = state.entries.remove(index) else {
                    return Ok(None);
                };
                let stored_key = entry.key.clone();
                let mut value = entry.value.clone();
                value.source = Source::Memory;
                state.entries.push_front(entry);
                drop(state);
                let _ = stored_key;
                return Ok(Some(value));
            }
        }
        // Exact root first, then identity-named ancestors only. Never enumerate and
        // inflate every stored root to discover coverage (compressed size is not RSS).
        for ancestor in root.ancestors() {
            let Some(candidate) = self::key(ancestor) else {
                continue;
            };
            let path = self.snapshot_path(&candidate);
            if !path.is_file() {
                continue;
            }
            let (stored_key, value) = match self.read_snapshot(&path, Some(root)) {
                Ok(value) => value,
                Err(reason)
                    if reason.contains("预算")
                        || reason.contains("budget")
                        || reason.contains("资源不足") =>
                {
                    return Err(format!("快照未载入：{reason}；原快照未修改，可手动扫描"));
                }
                Err(_) => continue,
            };
            if stored_key != candidate
                || value
                    .scan
                    .nodes
                    .first()
                    .is_none_or(|n| self::key(&n.path).as_ref() != Some(&stored_key))
                || !value
                    .scan
                    .nodes
                    .iter()
                    .any(|n| n.directory && n.path == root)
            {
                continue;
            }
            self.remember(stored_key, value.clone());
            return Ok(Some(value));
        }
        Ok(None)
    }
    pub fn run(&self, job: &Arc<Job>) -> Result<Cached, String> {
        if job.started.swap(true, Ordering::AcqRel) {
            let mut result = lock(&job.result);
            while result.is_none() {
                result = job.done.wait(result).unwrap_or_else(|e| e.into_inner());
            }
            return result.clone().unwrap();
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if !job.force {
                if let Some(value) = self.lookup(&job.root) {
                    return Ok(value);
                }
            }
            let scan = Arc::new(self.provider.measure(
                &job.root,
                job.cancel.clone(),
                job.progress.clone(),
                job.limits,
            )?);
            let value = Cached {
                scan,
                source: Source::Measured,
                captured: now(),
            };
            if let Some(key) = job.key.clone() {
                // A root replaced while scanning must never be persisted under its old identity.
                if self::key(&job.root).as_ref() == Some(&key) {
                    self.remember(key.clone(), value.clone());
                    if let Err(e) = self.write_snapshot(&key, &value, Some(&job.cancel)) {
                        eprintln!("usage_snapshot skipped (scan kept in memory): {e}");
                    }
                }
            }
            Ok(value)
        }))
        .unwrap_or_else(|_| Err("占用任务异常终止；未覆盖已有快照".into()));
        *lock(&job.result) = Some(result.clone());
        job.done.notify_all();
        if let Some(key) = &job.key {
            lock(&self.state).flights.remove(key);
        }
        result
    }
    fn write_snapshot(
        &self,
        key: &Key,
        value: &Cached,
        cancel: Option<&AtomicBool>,
    ) -> Result<(), String> {
        std::fs::create_dir_all(&self.directory).map_err(|e| e.to_string())?;
        snapshot_allowed(value.scan.nodes.len(), 0)?;
        if memory_cost(&value.scan) > MEMORY_BUDGET {
            return Err(
                "仅拒绝快照缓存：展开内存超出 256MiB 缓存预算；完整扫描结果仍保留在内存".into(),
            );
        }
        let header = Header::from_scan(key, value);
        let encoded = serde_json::to_vec(&header).map_err(|e| e.to_string())?;
        let destination = self.snapshot_path(key);
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let temp = self.directory.join(format!(
            "write-{}-{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> Result<(), String> {
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .map_err(|e| e.to_string())?;
            let mut zip = zip::ZipWriter::new(file);
            zip.start_file(
                "tree.v1",
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .compression_level(Some(1)),
            )
            .map_err(|e| e.to_string())?;
            let mut written = 0u64;
            macro_rules! put {
                ($x:expr) => {{
                    let bytes = $x;
                    written += bytes.len() as u64;
                    if written > MAX_UNPACKED {
                        return Err("snapshot uncompressed budget".into());
                    }
                    zip.write_all(bytes).map_err(|e| e.to_string())?;
                }};
            }
            put!(b"FXDU\x01\0");
            put!(&(encoded.len() as u32).to_le_bytes());
            put!(&encoded);
            for (index, n) in value.scan.nodes.iter().enumerate() {
                if index % 1024 == 0 && cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                    return Err("snapshot cancelled; partial result retained in memory".into());
                }
                let name = if n.parent.is_none() {
                    vec![]
                } else {
                    encode(n.path.file_name().ok_or("missing name")?)
                };
                if name.len() > 32768 {
                    return Err("name budget".into());
                }
                put!(&(name.len() as u32).to_le_bytes());
                for c in name {
                    put!(&c.to_le_bytes());
                }
                put!(&(n.parent.map_or(u32::MAX, |i| i as u32)).to_le_bytes());
                put!(&[
                    u8::from(n.directory)
                        | u8::from(n.hardlink_duplicate) * 2
                        | u8::from(n.uncertain) * 4,
                    n.category.min(5) as u8
                ]);
                for number in [
                    n.entries,
                    n.files,
                    n.directories,
                    n.own_allocated,
                    n.logical,
                    n.mtime,
                    n.allocated,
                ] {
                    put!(&number.to_le_bytes());
                }
            }
            let file = zip.finish().map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            if file.metadata().map_err(|e| e.to_string())?.len() > DISK_BUDGET {
                return Err("compressed disk budget".into());
            }
            drop(file);
            atomic_replace(&temp, &destination)?;
            self.prune();
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result
    }
    fn read_snapshot(
        &self,
        path: &Path,
        requested: Option<&Path>,
    ) -> Result<(Key, Cached), String> {
        #[cfg(test)]
        self.reads.fetch_add(1, Ordering::Relaxed);
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        if file.metadata().map_err(|e| e.to_string())?.len() > DISK_BUDGET {
            return Err("compressed budget".into());
        }
        let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        let entry = archive.by_name("tree.v1").map_err(|e| e.to_string())?;
        if entry.size() > READ_UNPACKED {
            return Err("decompression budget".into());
        }
        let mut reader = entry.take(READ_UNPACKED);
        fn bytes<const N: usize>(r: &mut impl Read) -> Result<[u8; N], String> {
            let mut a = [0; N];
            r.read_exact(&mut a).map_err(|e| e.to_string())?;
            Ok(a)
        }
        if &bytes::<6>(&mut reader)? != b"FXDU\x01\0" {
            return Err("magic".into());
        }
        let length = u32::from_le_bytes(bytes(&mut reader)?) as usize;
        if length > 256 * 1024 {
            return Err("header budget".into());
        }
        let mut json = vec![0; length];
        reader.read_exact(&mut json).map_err(|e| e.to_string())?;
        let header: Header = serde_json::from_slice(&json).map_err(|e| e.to_string())?;
        if header.schema == 0
            || header.schema > SCHEMA
            || header.provider != "win32-batch-unnamed-v1"
            || header.nodes == 0
            || header.nodes > MAX_NODES
        {
            return Err("schema/policy/node count".into());
        }
        let key = Key {
            volume: header.volume_id,
            id: u128::from_str_radix(&header.file_id, 16).map_err(|e| e.to_string())?,
            created: u128::from_str_radix(&header.created, 16).map_err(|e| e.to_string())?,
        };
        let root = PathBuf::from(decode(&header.root)?);
        if requested.is_some_and(|r| !r.starts_with(&root)) {
            return Err("outside coverage".into());
        }
        if header
            .nodes
            .saturating_mul(std::mem::size_of::<Node>() + 128)
            > MEMORY_BUDGET
        {
            return Err("快照展开内存预算；保留原快照，需手动重扫或提高资源预算".into());
        }
        let mut nodes = Vec::<Node>::new();
        let mut resident = 0usize;
        for index in 0..header.nodes {
            let length = u32::from_le_bytes(bytes(&mut reader)?) as usize;
            if length > 32768 {
                return Err("name budget".into());
            }
            let mut name = Vec::with_capacity(length);
            for _ in 0..length {
                name.push(u16::from_le_bytes(bytes(&mut reader)?));
            }
            let parent = u32::from_le_bytes(bytes(&mut reader)?);
            let flags = bytes::<2>(&mut reader)?;
            if flags[0] & !7 != 0 || flags[1] > 5 {
                return Err("node flags".into());
            }
            let parent = if parent == u32::MAX {
                None
            } else {
                Some(parent as usize)
            };
            if (index == 0) != (parent.is_none())
                || parent.is_some_and(|p| p >= index || !nodes[p].directory)
            {
                return Err("parent ordering".into());
            }
            if index > 0
                && (name.is_empty()
                    || name.iter().any(|c| matches!(*c, 0 | 47 | 92 | 58))
                    || name == [46]
                    || name == [46, 46])
            {
                return Err("basename only".into());
            }
            let path = if let Some(p) = parent {
                nodes[p].path.join(decode(&name)?)
            } else {
                root.clone()
            };
            let mut numbers = [0u64; 7];
            for n in &mut numbers {
                *n = u64::from_le_bytes(bytes(&mut reader)?);
            }
            disk_usage::try_push_node(
                &mut nodes,
                Node {
                    path,
                    parent,
                    children: vec![],
                    entries: numbers[0],
                    files: numbers[1],
                    directories: numbers[2],
                    own_allocated: numbers[3],
                    logical: numbers[4],
                    mtime: numbers[5],
                    allocated: numbers[6],
                    directory: flags[0] & 1 != 0,
                    hardlink_duplicate: flags[0] & 2 != 0,
                    uncertain: flags[0] & 4 != 0,
                    category: flags[1] as usize,
                },
                &mut resident,
                Limits::from_bytes(MEMORY_BUDGET),
            )
            .map_err(|reason| format!("快照展开内存资源不足：{reason:?}；原文件未改动"))?;
        }
        let mut trailing = [0];
        if reader.read(&mut trailing).map_err(|e| e.to_string())? != 0 {
            return Err("trailing data".into());
        }
        let sizes = nodes.iter().map(|n| n.allocated).collect::<Vec<_>>();
        for n in &mut nodes {
            n.children
                .sort_unstable_by_key(|&j| (std::cmp::Reverse(sizes[j]), j));
        }
        let volume = header
            .volume
            .map(|v| -> Result<VolumeSpace, String> {
                Ok(VolumeSpace {
                    root: PathBuf::from(decode(&v.0)?),
                    total: v.1,
                    free: v.2,
                    available: v.3,
                })
            })
            .transpose()?;
        let scan = Scan {
            logical_generation: None,
            nodes,
            errors: header.errors,
            skipped_links: header.skipped,
            hardlinks: header.hardlinks,
            unknown: header.unknown,
            partial: header.partial,
            cancelled: header.cancelled,
            elapsed: Duration::from_millis(header.elapsed_ms),
            entry_limit_reached: header.entry_limit,
            time_limit_reached: header.time_limit,
            memory_limit_reached: header.memory_limit,
            alloc_failed: header.alloc_failed,
            limits: Limits {
                memory_bytes: if header.limit_memory > 0 {
                    header.limit_memory as usize
                } else {
                    crate::disk_usage::DEFAULT_MEMORY_BUDGET
                },
            },
            volume,
        };
        Ok((
            key,
            Cached {
                scan: Arc::new(scan),
                source: Source::Snapshot,
                captured: header.captured,
            },
        ))
    }
    fn prune(&self) {
        let Ok(entries) = std::fs::read_dir(&self.directory) else {
            return;
        };
        let mut files = entries
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "fxdu"))
            .filter_map(|e| Some((e.path(), e.metadata().ok()?)))
            .collect::<Vec<_>>();
        files.sort_by_key(|(_, m)| std::cmp::Reverse(m.modified().ok()));
        let mut used = 0;
        for (i, (path, m)) in files.into_iter().enumerate() {
            used += m.len();
            if i >= ROOT_LIMIT || used > DISK_BUDGET {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
struct Header {
    schema: u32,
    provider: String,
    root: Vec<u16>,
    volume_id: u64,
    file_id: String,
    created: String,
    nodes: usize,
    captured: u64,
    errors: u64,
    skipped: u64,
    hardlinks: u64,
    unknown: u64,
    partial: bool,
    cancelled: bool,
    elapsed_ms: u64,
    entry_limit: bool,
    time_limit: bool,
    limit_entries: usize,
    limit_secs: u64,
    #[serde(default)]
    memory_limit: bool,
    #[serde(default)]
    alloc_failed: bool,
    #[serde(default)]
    limit_memory: u64,
    volume: Option<(Vec<u16>, u64, u64, u64)>,
}
impl Header {
    fn from_scan(key: &Key, value: &Cached) -> Self {
        let s = &value.scan;
        Self {
            schema: SCHEMA,
            provider: "win32-batch-unnamed-v1".into(),
            root: encode(s.nodes[0].path.as_os_str()),
            volume_id: key.volume,
            file_id: format!("{:032x}", key.id),
            created: format!("{:032x}", key.created),
            nodes: s.nodes.len(),
            captured: value.captured,
            errors: s.errors,
            skipped: s.skipped_links,
            hardlinks: s.hardlinks,
            unknown: s.unknown,
            partial: s.partial,
            cancelled: s.cancelled,
            elapsed_ms: s.elapsed.as_millis().min(u64::MAX as u128) as u64,
            entry_limit: s.entry_limit_reached,
            time_limit: s.time_limit_reached,
            limit_entries: 0,
            limit_secs: 0,
            memory_limit: s.memory_limit_reached,
            alloc_failed: s.alloc_failed,
            limit_memory: s.limits.memory_bytes as u64,
            volume: s
                .volume
                .as_ref()
                .map(|v| (encode(v.root.as_os_str()), v.total, v.free, v.available)),
        }
    }
}
#[cfg(windows)]
fn encode(s: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    s.encode_wide().collect()
}
#[cfg(windows)]
fn decode(s: &[u16]) -> Result<std::ffi::OsString, String> {
    use std::os::windows::ffi::OsStringExt;
    Ok(std::ffi::OsString::from_wide(s))
}
#[cfg(not(windows))]
fn encode(s: &std::ffi::OsStr) -> Vec<u16> {
    s.to_string_lossy().encode_utf16().collect()
}
#[cfg(not(windows))]
fn decode(s: &[u16]) -> Result<std::ffi::OsString, String> {
    String::from_utf16(s)
        .map(Into::into)
        .map_err(|e| e.to_string())
}
#[cfg(windows)]
fn atomic_replace(src: &Path, dst: &Path) -> Result<(), String> {
    use windows::{
        Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        },
        core::PCWSTR,
    };
    let src = encode(src.as_os_str())
        .into_iter()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let dst = encode(dst.as_os_str())
        .into_iter()
        .chain(Some(0))
        .collect::<Vec<_>>();
    unsafe {
        MoveFileExW(
            PCWSTR(src.as_ptr()),
            PCWSTR(dst.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|e| e.to_string())
}
#[cfg(not(windows))]
fn atomic_replace(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::rename(src, dst).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new(label: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "findx-usage-{label}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(p.join("data/child")).unwrap();
            std::fs::write(p.join("data/one"), vec![1u8; 8192]).unwrap();
            std::fs::write(p.join("data/child/two"), vec![2u8; 4096]).unwrap();
            Self(p)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    struct Counted(AtomicU64);
    impl MeasuredUsageProvider for Counted {
        fn measure(
            &self,
            root: &Path,
            c: Arc<AtomicBool>,
            p: Arc<Progress>,
            l: Limits,
        ) -> Result<Scan, String> {
            self.0.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(20));
            disk_usage::scan(root, c, p, l)
        }
    }

    #[test]
    fn inflated_snapshot_header_is_rejected_before_tree_allocation() {
        let f = Fixture::new("read-budget");
        let provider = Arc::new(Counted(AtomicU64::new(0)));
        let repo = Repository::new(f.0.join("cache"), provider);
        let root = f.0.join("data");
        let value = repo
            .run(&repo.request(&root, true, Limits::default()))
            .unwrap();
        let k = key(&root).unwrap();
        let mut header = Header::from_scan(&k, &value);
        header.nodes = 2_000_000;
        let json = serde_json::to_vec(&header).unwrap();
        let fake = f.0.join("inflated.fxdu");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&fake).unwrap());
        zip.start_file("tree.v1", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"FXDU\x01\0").unwrap();
        zip.write_all(&(json.len() as u32).to_le_bytes()).unwrap();
        zip.write_all(&json).unwrap();
        zip.finish().unwrap();
        let reason = repo.read_snapshot(&fake, Some(&root)).err().unwrap();
        assert!(reason.contains("展开内存预算"), "{reason}");
    }
    #[test]
    fn lookup_decodes_only_requested_identity_and_never_touches_unrelated_snapshots() {
        let a = Fixture::new("scope-a");
        let b = Fixture::new("scope-b");
        let provider = Arc::new(Counted(AtomicU64::new(0)));
        let directory = a.0.join("cache");
        let writer = Repository::new(directory.clone(), provider.clone());
        let root_a = a.0.join("data");
        let root_b = b.0.join("data");
        writer
            .run(&writer.request(&root_a, true, Limits::default()))
            .unwrap();
        writer
            .run(&writer.request(&root_b, true, Limits::default()))
            .unwrap();
        let unrelated = writer.snapshot_path(&key(&root_b).unwrap());
        let before = std::fs::read(&unrelated).unwrap();
        let modified = std::fs::metadata(&unrelated).unwrap().modified().unwrap();
        let reader = Repository::new(directory.clone(), provider.clone());
        let value = reader.lookup(&root_a).unwrap();
        assert_eq!(reader.reads.load(Ordering::Relaxed), 1);
        assert_eq!(lock(&reader.state).entries.len(), 1);
        assert_eq!(value.scan.nodes[0].path, root_a);
        assert_eq!(std::fs::read(&unrelated).unwrap(), before);
        assert_eq!(
            std::fs::metadata(&unrelated).unwrap().modified().unwrap(),
            modified
        );
        // A miss must not decompress unrelated roots either.
        let missing = a.0.join("unmeasured");
        std::fs::create_dir(&missing).unwrap();
        assert!(reader.lookup(&missing).is_none());
        assert_eq!(reader.reads.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn same_key_single_worker_memory_ancestor_and_restart_snapshot_are_zero_scans() {
        let f = Fixture::new("cache");
        let provider = Arc::new(Counted(AtomicU64::new(0)));
        let root = f.0.join("data");
        let directory = f.0.join("cache");
        let repo = Arc::new(Repository::new(directory.clone(), provider.clone()));
        let first = repo.request(&root, false, Limits::default());
        let second = repo.request(&root, true, Limits::default());
        assert!(Arc::ptr_eq(&first, &second));
        std::thread::scope(|s| {
            let a = repo.clone();
            let first = first.clone();
            let b = repo.clone();
            let second = second.clone();
            let t1 = s.spawn(move || a.run(&first).unwrap());
            let t2 = s.spawn(move || b.run(&second).unwrap());
            let v1 = t1.join().unwrap();
            let v2 = t2.join().unwrap();
            assert!(Arc::ptr_eq(&v1.scan, &v2.scan));
        });
        assert_eq!(provider.0.load(Ordering::Relaxed), 1);
        let reused = repo
            .run(&repo.request(&root, false, Limits::default()))
            .unwrap();
        assert_eq!(reused.source, Source::Memory);
        assert!(reused.label().contains("扫描时点"));
        assert!(!reused.label().contains("秒前"));
        let child = repo.lookup(&root.join("child")).unwrap();
        assert_eq!(child.source, Source::Memory);
        assert!(Arc::ptr_eq(&child.scan, &reused.scan));
        // A new repository has no in-memory tree; only the compact on-disk snapshot.
        let restarted = Repository::new(directory.clone(), provider.clone());
        let cached = restarted.lookup(&root.join("child")).unwrap();
        assert_eq!(cached.source, Source::Snapshot);
        assert_eq!(cached.scan.nodes[0].logical, 12288);
        assert!(!cached.scan.partial);
        assert!(cached.label().contains("未验证新鲜度"));
        assert_eq!(provider.0.load(Ordering::Relaxed), 1);
        let child_process = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "usage_cache::tests::separate_process_snapshot_reader",
                "--nocapture",
            ])
            .env("FINDX_USAGE_FIXTURE_ROOT", &root)
            .env("FINDX_USAGE_FIXTURE_CACHE", &directory)
            .output()
            .unwrap();
        assert!(
            child_process.status.success(),
            "{}{}",
            String::from_utf8_lossy(&child_process.stdout),
            String::from_utf8_lossy(&child_process.stderr)
        );
        let snapshot = std::fs::read_dir(&directory)
            .unwrap()
            .filter_map(Result::ok)
            .find(|e| e.path().extension().is_some_and(|x| x == "fxdu"))
            .unwrap()
            .path();
        std::fs::write(&snapshot, b"truncated").unwrap();
        assert!(
            Repository::new(directory.clone(), provider.clone())
                .lookup(&root)
                .is_none()
        );
        assert_eq!(provider.0.load(Ordering::Relaxed), 1);
        let fresh = repo
            .run(&repo.request(&root, true, Limits::default()))
            .unwrap();
        assert_eq!(fresh.source, Source::Measured);
        assert_eq!(provider.0.load(Ordering::Relaxed), 2);
        assert!(
            Repository::new(directory.clone(), provider.clone())
                .lookup(&root)
                .is_some()
        );
        assert!(
            !std::fs::read_dir(&directory)
                .unwrap()
                .filter_map(Result::ok)
                .any(|e| e.path().extension().is_some_and(|x| x == "tmp"))
        );
        println!(
            "usage_cache fixture: measure_calls=2; memory/ancestor/new_repository_snapshot reuse caused 0 additional scans; corrupted snapshot rejected; refresh atomically replaced"
        );
    }
    #[test]
    fn separate_process_snapshot_reader() {
        let Some(root) = std::env::var_os("FINDX_USAGE_FIXTURE_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let directory = PathBuf::from(
            std::env::var_os("FINDX_USAGE_FIXTURE_CACHE").expect("owned fixture cache"),
        );
        assert!(
            root.parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("findx-usage-cache-")
        );
        assert_eq!(root.parent(), directory.parent());
        let provider = Arc::new(Counted(AtomicU64::new(0)));
        let repo = Repository::new(directory, provider.clone());
        let value = repo
            .lookup(&root.join("child"))
            .expect("existing snapshot must survive a separate process");
        assert_eq!(value.source, Source::Snapshot);
        assert_eq!(value.scan.nodes[0].logical, 12288);
        assert_eq!(provider.0.load(Ordering::Relaxed), 0);
    }
    #[test]
    fn partial_snapshot_preserves_budget_and_never_fakes_missing_coverage() {
        let f = Fixture::new("partial");
        let provider = Arc::new(Counted(AtomicU64::new(0)));
        let root = f.0.join("data");
        for i in 0..10 {
            std::fs::write(root.join(format!("extra-{i}")), [1u8; 16]).unwrap();
        }
        let repo = Repository::new(f.0.join("cache"), provider.clone());
        let value = repo
            .run(&repo.request(&root, true, Limits::from_bytes(2048)))
            .unwrap();
        assert!(
            value.scan.partial && value.scan.memory_limit_reached,
            "{}",
            value.scan.partial_reasons()
        );
        assert!(!value.scan.entry_limit_reached && !value.scan.time_limit_reached);
        assert!(value.scan.nodes.len() < 12);
        let restart = Repository::new(f.0.join("cache"), provider.clone());
        let loaded = restart.lookup(&root).unwrap();
        assert!(loaded.scan.partial && loaded.scan.memory_limit_reached);
        assert!(
            loaded.label().contains("内存预算") || loaded.label().contains("部分结果"),
            "{}",
            loaded.label()
        );
        assert!(restart.lookup(&root.join("not-observed")).is_none());
        assert_eq!(provider.0.load(Ordering::Relaxed), 1);
    }
    #[cfg(windows)]
    #[test]
    fn snapshot_utf16_basename_roundtrip_is_lossless() {
        use std::os::windows::ffi::OsStringExt;
        let f = Fixture::new("utf16");
        let root = f.0.join("data");
        let name = std::ffi::OsString::from_wide(&[
            0x6d4b, 0x8bd5, 0xd800, 0x002e, 0x0062, 0x0069, 0x006e,
        ]);
        let path = root.join(name);
        std::fs::write(&path, [1u8; 128]).unwrap();
        let provider = Arc::new(Counted(AtomicU64::new(0)));
        let repo = Repository::new(f.0.join("cache"), provider.clone());
        repo.run(&repo.request(&root, true, Limits::default()))
            .unwrap();
        let restart = Repository::new(f.0.join("cache"), provider);
        let loaded = restart.lookup(&root).unwrap();
        assert!(loaded.scan.nodes.iter().any(|n| n.path == path));
    }
    #[test]
    fn root_budget_bounds_memory_and_disk_cache_entries() {
        let f = Fixture::new("lru");
        let provider = Arc::new(Counted(AtomicU64::new(0)));
        let directory = f.0.join("cache");
        let repo = Repository::new(directory.clone(), provider);
        for i in 0..ROOT_LIMIT + 2 {
            let root = f.0.join(format!("root-{i}"));
            std::fs::create_dir(&root).unwrap();
            std::fs::write(root.join("public.bin"), [0; 64]).unwrap();
            repo.run(&repo.request(&root, true, Limits::default()))
                .unwrap();
        }
        let state = lock(&repo.state);
        assert!(state.entries.len() <= ROOT_LIMIT);
        assert!(state.entries.iter().map(|e| e.bytes).sum::<usize>() <= MEMORY_BUDGET);
        drop(state);
        let files = std::fs::read_dir(directory)
            .unwrap()
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        assert!(files.len() <= ROOT_LIMIT);
        assert!(
            files
                .iter()
                .map(|e| e.metadata().unwrap().len())
                .sum::<u64>()
                <= DISK_BUDGET
        );
    }
}
