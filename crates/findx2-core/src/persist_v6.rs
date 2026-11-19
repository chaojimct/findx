//! v6 增量持久化：分段文件 + 原子 manifest。
//!
//! ## 为什么需要 v6
//! v5 `index.bin` 是单个 500MB+ 快照，每次落盘全量重写（`save_index_bin`），
//! 且序列化期间持 `index_store()` 读锁 5-15 秒——期间所有 USN 写锁被饿死，
//! 任务管理器里就是周期性的 `0% CPU + 100MB/s 磁盘`。
//!
//! ## v6 布局（`<index>.v6/` 目录）
//! - `manifest.json`：版本/代数/各段长度/游标，原子 tmp+rename 提交（commit 点）。
//! - `entries.dat`：`N×32B` 定长记录，原地 pwrite + 尾部追加。
//! - `frns.dat`：`N×8B`，同上。
//! - `dirs.dat`：`D×24B`，同上（实践中只追加）。
//! - `ranges.dat`：`D×8B`，同上（恒 `(0,0)` 追加）。
//! - `names.dat`：变长名池，只追加（改名追加新串，旧串泄漏，下次全量回收）。
//! - `dirpaths.dat`：通常为空；增量期间不可变（变了就走全量）。
//! - `ext_filter` / `deleted` 位图**不在盘上**：纯派生数据，load 时从
//!   `entries.attrs` + 名字重建（与内存状态恒一致，因为每次变更双写）。
//!
//! ## 崩溃一致性（commit 协议）
//! 1. 读 manifest（M），各文件截到 M 长度（丢弃上次崩溃的 torn tail）。
//! 2. 回放本批 Mutation：原地 pwrite / 尾部 append，逐个校验偏移（对不上 → NeedFull）。
//! 3. `sync_all` 全部数据文件。
//! 4. 原子 rename 新 manifest（代数+1，长度=内存快照长度，游标=内存当前游标）。
//!
//! - 崩溃在 4 之前：manifest 还是旧的，文件多余 tail 下次被截掉；游标也是旧的，
//!   重启从旧游标回放 USN，幂等重做（`upsert`/`delete` 均幂等，names 泄漏无害）。
//! - 崩溃在 4 之后：全部 durable。
//! - 原地 patch（16/32B）小于扇区，NTFS 上原子。

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use roaring::RoaringBitmap;

use crate::index::{DirEntry, FileEntry, FrnIdxMap, IndexStore, Mutation, VolumeState};
use crate::persist::{
    dir_entry_from_bytes, file_entry_from_disk_bytes_v5,
    load_exclude_sidecar, mount_trigram_sidecars,
    FILE_ENTRY_V5_DISK_SIZE,
};
use crate::Result;

pub const V6_VERSION: u32 = 6;
pub(crate) const ENTRY_SIZE: usize = FILE_ENTRY_V5_DISK_SIZE; // 32
pub(crate) const FRN_SIZE: usize = 8;
pub(crate) const DIR_SIZE: usize = 24;
pub(crate) const RANGE_SIZE: usize = 8;

pub(crate) const F_MANIFEST: &str = "manifest.json";
pub(crate) const F_ENTRIES: &str = "entries.dat";
pub(crate) const F_FRNS: &str = "frns.dat";
pub(crate) const F_DIRS: &str = "dirs.dat";
pub(crate) const F_RANGES: &str = "ranges.dat";
pub(crate) const F_NAMES: &str = "names.dat";
pub(crate) const F_DIRPATHS: &str = "dirpaths.dat";

/// v6 存储目录定位。`index.bin` → `index.bin.v6/`。
#[derive(Debug, Clone)]
pub struct V6Layout {
    pub dir: PathBuf,
    pub index_bin: PathBuf,
}

impl V6Layout {
    pub fn for_index(index_bin: &Path) -> Self {
        let mut dir = index_bin.as_os_str().to_owned();
        dir.push(".v6");
        Self {
            dir: PathBuf::from(dir),
            index_bin: index_bin.to_owned(),
        }
    }

    pub fn manifest_exists(&self) -> bool {
        self.dir.join(crate::persist_tx::F_PUBLISHED).is_file() || self.dir.join(F_MANIFEST).is_file()
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct V6Volume {
    pub letter: u8,
    pub serial: u32,
    pub journal_id: u64,
    pub last_usn: u64,
    pub first_entry_idx: u32,
    pub root_prefix: String,
    pub volume_id: String,
}

impl From<&VolumeState> for V6Volume {
    fn from(v: &VolumeState) -> Self {
        Self {
            letter: v.volume_letter,
            serial: v.volume_serial,
            journal_id: v.usn_journal_id,
            last_usn: v.last_usn,
            first_entry_idx: v.first_entry_idx,
            root_prefix: v.root_prefix.clone(),
            volume_id: v.volume_id.clone(),
        }
    }
}

impl From<&V6Volume> for VolumeState {
    fn from(v: &V6Volume) -> Self {
        Self {
            volume_letter: v.letter,
            volume_serial: v.serial,
            usn_journal_id: v.journal_id,
            last_usn: v.last_usn,
            first_entry_idx: v.first_entry_idx,
            root_prefix: v.root_prefix.clone(),
            volume_id: v.volume_id.clone(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct V6Manifest {
    pub version: u32,
    pub generation: u64,
    pub entry_count: usize,
    pub dir_count: usize,
    pub names_len: usize,
    pub dir_paths_len: usize,
    pub metadata_ready: bool,
    pub volumes: Vec<V6Volume>,
}

/// commit 时调用方（持有 store 锁）一次性快照的提交批。
/// 内存快照 + drain 出来的日志，文件 IO 时不再碰 store。
/// `tri_pending` 随主数据一起快照、随 manifest 一起落盘（`.tri.pending` 边车），
/// 否则重启后改名条目会从剪枝候选里漏掉（v5 同理，见 `save_trigram_sidecars`）。
#[derive(Debug)]
pub struct CommitBatch {
    pub mutations: Vec<Mutation>,
    pub entry_count: usize,
    pub dir_count: usize,
    pub names_len: usize,
    pub dir_paths_len: usize,
    pub metadata_ready: bool,
    pub volumes: Vec<VolumeState>,
    pub tri_pending: RoaringBitmap,
    /// `trigram.snapshot_entry_count()`，无边车时为 0（此时不写 pending 边车）。
    pub tri_snap: u32,
    pub has_trigram: bool,
}

impl CommitBatch {
    pub fn is_empty(&self) -> bool {
        self.mutations.is_empty()
    }
}

#[derive(Debug)]
pub enum CommitOutcome {
    Delta {
        applied: usize,
        bytes: u64,
        generation: u64,
    },
    /// 调用方应降级走全量分段重写（`v6_save_full`）。
    NeedFull { reason: String },
}

pub(crate) fn read_legacy_manifest(layout: &V6Layout) -> Result<V6Manifest> {
    let p = layout.file(F_MANIFEST);
    let bytes = std::fs::read(&p)
        .map_err(|e| crate::Error::Persist(format!("v6 读 manifest 失败 {}: {e}", p.display())))?;
    let m: V6Manifest = serde_json::from_slice(&bytes)
        .map_err(|e| crate::Error::Persist(format!("v6 manifest 解析失败: {e}")))?;
    if m.version != V6_VERSION {
        return Err(crate::Error::Persist(format!(
            "v6 manifest 版本 {} 不受支持（期望 {V6_VERSION}）",
            m.version
        )));
    }
    Ok(m)
}

pub fn v6_save_full(layout: &V6Layout, store: &IndexStore) -> Result<u64> {
    crate::persist_tx::save_full_generation(layout, store)
}

fn parse_entries(buf: &[u8]) -> Result<Vec<FileEntry>> {
    if buf.len() % ENTRY_SIZE != 0 {
        return Err(crate::Error::Persist("v6 entries 长度非 32 整数倍".into()));
    }
    let mut out = Vec::with_capacity(buf.len() / ENTRY_SIZE);
    for chunk in buf.chunks_exact(ENTRY_SIZE) {
        let mut a = [0u8; ENTRY_SIZE];
        a.copy_from_slice(chunk);
        out.push(file_entry_from_disk_bytes_v5(&a));
    }
    Ok(out)
}

fn parse_frns(buf: &[u8]) -> Result<Vec<u64>> {
    if buf.len() % FRN_SIZE != 0 {
        return Err(crate::Error::Persist("v6 frns 长度非 8 整数倍".into()));
    }
    let n = buf.len() / FRN_SIZE;
    let mut out = vec![0u64; n];
    // LE 假设：x86_64 Windows 小端，磁盘即 LE，直接转写即可。
    out.as_mut_slice()
        .copy_from_slice(unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u64, n) });
    Ok(out)
}

fn parse_dirs(buf: &[u8]) -> Result<Vec<DirEntry>> {
    if buf.len() % DIR_SIZE != 0 {
        return Err(crate::Error::Persist("v6 dirs 长度非 24 整数倍".into()));
    }
    let mut out = Vec::with_capacity(buf.len() / DIR_SIZE);
    for chunk in buf.chunks_exact(DIR_SIZE) {
        let mut a = [0u8; DIR_SIZE];
        a.copy_from_slice(chunk);
        out.push(dir_entry_from_bytes(&a));
    }
    Ok(out)
}

fn parse_ranges(buf: &[u8], n: usize) -> Result<Vec<(u32, u32)>> {
    if buf.len() != n * RANGE_SIZE {
        return Err(crate::Error::Persist("v6 ranges 长度与目录数不一致".into()));
    }
    let mut out = Vec::with_capacity(n);
    for chunk in buf.chunks_exact(RANGE_SIZE) {
        let a = u32::from_le_bytes(chunk[0..4].try_into().unwrap());
        let b = u32::from_le_bytes(chunk[4..8].try_into().unwrap());
        out.push((a, b));
    }
    Ok(out)
}

/// 从 entries 重建 `ext_filter`（两遍计数 + `from_sorted_iter`，与建库同速）。
fn rebuild_ext(entries: &[FileEntry]) -> [Option<RoaringBitmap>; 256] {
    use rayon::prelude::*;
    let mut counts = [0usize; 256];
    for e in entries.iter() {
        counts[e.ext_hash_u8() as usize] += 1;
    }
    let mut buckets: Vec<Vec<u32>> = counts.iter().map(|&c| Vec::with_capacity(c)).collect();
    for (i, e) in entries.iter().enumerate() {
        buckets[e.ext_hash_u8() as usize].push(i as u32);
    }
    let vec: Vec<Option<RoaringBitmap>> = buckets
        .into_par_iter()
        .map(|ids| {
            if ids.is_empty() {
                None
            } else {
                Some(RoaringBitmap::from_sorted_iter(ids.into_iter()).expect("v6 ext sorted"))
            }
        })
        .collect();
    vec.try_into().expect("256 ext buckets")
}

fn rebuild_deleted(entries: &[FileEntry]) -> RoaringBitmap {
    let ids: Vec<u32> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.is_deleted())
        .map(|(i, _)| i as u32)
        .collect();
    if ids.is_empty() {
        RoaringBitmap::new()
    } else {
        RoaringBitmap::from_sorted_iter(ids.into_iter()).expect("v6 deleted sorted")
    }
}

pub fn v6_load(layout: &V6Layout) -> Result<IndexStore> {
    crate::persist_tx::load_current(layout)
}

pub(crate) fn load_legacy_flat(layout: &V6Layout) -> Result<IndexStore> {
    let m = read_legacy_manifest(layout)?;
    load_from_dir(&layout.dir, &layout.index_bin, &m, None, None, true)
}

pub(crate) fn load_from_dir(
    dir: &std::path::Path,
    index_bin: &Path,
    m: &V6Manifest,
    tri: Option<std::path::PathBuf>,
    pending: Option<std::path::PathBuf>,
    use_classic_trigram: bool,
) -> Result<IndexStore> {
    let layout_index = index_bin;
    let entry_count = m.entry_count;
    let dir_count = m.dir_count;

    let entries = parse_entries(&read_section_in(dir, F_ENTRIES, entry_count * ENTRY_SIZE)?)?;
    let frns = parse_frns(&read_section_in(dir, F_FRNS, entry_count * FRN_SIZE)?)?;
    if frns.len() != entries.len() {
        return Err(crate::Error::Persist("v6 frns 与 entries 不等长".into()));
    }
    let dirs = parse_dirs(&read_section_in(dir, F_DIRS, dir_count * DIR_SIZE)?)?;
    let ranges = parse_ranges(&read_section_in(dir, F_RANGES, dir_count * RANGE_SIZE)?, dir_count)?;
    let names_buf = read_section_in(dir, F_NAMES, m.names_len)?;
    let dir_paths_buf = read_section_in(dir, F_DIRPATHS, m.dir_paths_len)?;

    let mut store = assemble_v6_store(entries, frns, dirs, ranges, names_buf, dir_paths_buf, m, layout_index)?;
    if tri.is_some() || pending.is_some() {
        let (trigram, tri_pending) = mount_generation_trigram(tri, pending, store.entries.len());
        store.trigram = trigram;
        store.tri_pending = tri_pending;
    } else if use_classic_trigram {
        let (trigram, tri_pending) = mount_trigram_sidecars(index_bin, store.entries.len());
        store.trigram = trigram;
        store.tri_pending = tri_pending;
    }
    Ok(store)
}

fn mount_generation_trigram(
    tri: Option<std::path::PathBuf>,
    pending: Option<std::path::PathBuf>,
    entry_count: usize,
) -> (Option<std::sync::Arc<crate::trigram::TrigramIndex>>, roaring::RoaringBitmap) {
    let trigram = match tri {
        Some(p) => crate::trigram::TrigramIndex::load(&p).ok().flatten().map(std::sync::Arc::new),
        None => None,
    };
    let mut tri_pending = roaring::RoaringBitmap::new();
    if let (Some(t), Some(p)) = (trigram.as_ref(), pending) {
        if let Some((bm, snap)) = crate::trigram::load_pending_from_path(&p) {
            if snap == t.snapshot_entry_count() {
                tri_pending = bm;
                for i in t.snapshot_entry_count()..entry_count as u32 { tri_pending.insert(i); }
            } else {
                crate::progress!("trigram：generation pending snap mismatch; pruning disabled this load");
                return (None, roaring::RoaringBitmap::new());
            }
        }
    }
    (trigram, tri_pending)
}

fn assemble_v6_store(
    entries: Vec<crate::index::FileEntry>,
    frns: Vec<u64>,
    dirs: Vec<crate::index::DirEntry>,
    ranges: Vec<(u32, u32)>,
    names_buf: Vec<u8>,
    dir_paths_buf: Vec<u8>,
    m: &V6Manifest,
    index_bin: &Path,
) -> Result<IndexStore> {
    let mut dir_index = FrnIdxMap::with_capacity(dirs.len());
    for (i, d) in dirs.iter().enumerate() {
        dir_index.push_unsorted(d.frn, i as u32);
    }
    dir_index.finalize_build();

    let mut frn_to_entry = FrnIdxMap::with_capacity(frns.len());
    for (i, fr) in frns.iter().enumerate() {
        if *fr != 0 {
            frn_to_entry.push_unsorted(*fr, i as u32);
        }
    }
    frn_to_entry.finalize_build();

    let ext_filter = rebuild_ext(&entries);
    let deleted = rebuild_deleted(&entries);
    let volumes: Vec<VolumeState> = m.volumes.iter().map(VolumeState::from).collect();
    let excluded_dirs = load_exclude_sidecar(index_bin);
    crate::progress!(
        "v6 加载：{} 条目 / {} 目录（generation {}）",
        entries.len(),
        dirs.len(),
        m.generation,
    );
    let mut store = IndexStore {
        names_buf,
        entries,
        dirs,
        dir_index,
        dir_paths_buf,
        dir_path_ranges: ranges,
        volumes,
        ext_filter,
        deleted,
        frns,
        frn_to_entry,
        metadata_ready: m.metadata_ready,
        trigram: None,
        tri_pending: RoaringBitmap::new(),
        cjk_names: RoaringBitmap::new(),
        excluded_dirs,
        mutation_log: None,
    };
    store.rebuild_scoped_identity_maps()?;
    store.rebuild_cjk_bitmap();
    Ok(store)
}

fn read_section_in(dir: &std::path::Path, name: &str, expect_len: usize) -> Result<Vec<u8>> {
    let p = dir.join(name);
    let f = File::open(&p)
        .map_err(|e| crate::Error::Persist(format!("v6 缺段 {}: {e}", p.display())))?;
    let meta = f
        .metadata()
        .map_err(|e| crate::Error::Persist(format!("v6 stat 失败: {e}")))?;
    if (meta.len() as usize) < expect_len {
        return Err(crate::Error::Persist(format!(
            "v6 段 {} 长度 {} 小于 manifest 期望 {}（文件损坏，走全量重建）",
            p.display(),
            meta.len(),
            expect_len
        )));
    }
    let mut f = f;
    let mut buf = vec![0u8; expect_len];
    if expect_len > 0 {
        f.read_exact(&mut buf)
            .map_err(|e| crate::Error::Persist(format!("v6 读段失败: {e}")))?;
    }
    Ok(buf)
}

/// 优先 published 代际 / 旧扁平 v6，回退 v5。
/// 已发布代际完整性失败时不与扁平遗留段混装；若仍有 v5 单文件则回退该只读副本。
pub fn load_store_auto(index_bin: &Path) -> Result<IndexStore> {
    let layout = V6Layout::for_index(index_bin);
    if crate::persist_tx::has_published(&layout) || layout.dir.join(F_MANIFEST).is_file() {
        match v6_load(&layout) {
            Ok(s) => return Ok(s),
            Err(e) => {
                crate::progress!("v6 加载失败（{e:#}），回退 v5");
                if index_bin.is_file() {
                    return crate::persist::load_index_bin(index_bin);
                }
                return Err(e);
            }
        }
    }
    crate::persist::load_index_bin(index_bin)
}

pub fn v6_try_commit(layout: &V6Layout, batch: &CommitBatch) -> Result<CommitOutcome> {
    crate::persist_tx::try_commit_generation(layout, batch)
}
