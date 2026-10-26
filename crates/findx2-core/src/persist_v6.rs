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

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use roaring::RoaringBitmap;

use crate::index::{
    DirEntry, FileEntry, FrnIdxMap,
    IndexStore, Mutation, VolumeState,
};
use crate::persist::{
    dir_entry_from_bytes, dir_entry_to_bytes, file_entry_from_disk_bytes_v5,
    file_entry_to_disk_bytes_v5, load_exclude_sidecar, mount_trigram_sidecars,
    save_trigram_sidecars, FILE_ENTRY_V5_DISK_SIZE,
};
use crate::Result;

pub const V6_VERSION: u32 = 6;
const ENTRY_SIZE: usize = FILE_ENTRY_V5_DISK_SIZE; // 32
const FRN_SIZE: usize = 8;
const DIR_SIZE: usize = 24;
const RANGE_SIZE: usize = 8;

const F_MANIFEST: &str = "manifest.json";
const F_ENTRIES: &str = "entries.dat";
const F_FRNS: &str = "frns.dat";
const F_DIRS: &str = "dirs.dat";
const F_RANGES: &str = "ranges.dat";
const F_NAMES: &str = "names.dat";
const F_DIRPATHS: &str = "dirpaths.dat";

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
        self.dir.join(F_MANIFEST).is_file()
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct V6Volume {
    letter: u8,
    serial: u32,
    journal_id: u64,
    last_usn: u64,
    first_entry_idx: u32,
    root_prefix: String,
    volume_id: String,
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
struct V6Manifest {
    version: u32,
    generation: u64,
    entry_count: usize,
    dir_count: usize,
    names_len: usize,
    dir_paths_len: usize,
    metadata_ready: bool,
    volumes: Vec<V6Volume>,
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
    Delta { applied: usize, bytes: u64, generation: u64 },
    /// 调用方应降级走全量分段重写（`v6_save_full`）。
    NeedFull { reason: String },
}



fn read_manifest(layout: &V6Layout) -> Result<V6Manifest> {
    let p = layout.file(F_MANIFEST);
    let bytes = std::fs::read(&p).map_err(|e| {
        crate::Error::Persist(format!("v6 读 manifest 失败 {}: {e}", p.display()))
    })?;
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

/// 原子写小文件：tmp + sync + rename（Windows 先删目标）。
fn write_file_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = File::create(&tmp)
            .map_err(|e| crate::Error::Persist(format!("v6 写 tmp 失败: {e}")))?;
        f.write_all(bytes)
            .map_err(|e| crate::Error::Persist(format!("v6 写 tmp 失败: {e}")))?;
        f.sync_all().ok();
    }
    #[cfg(windows)]
    let _ = std::fs::remove_file(path);
    std::fs::rename(&tmp, path)
        .map_err(|e| crate::Error::Persist(format!("v6 rename 失败: {e}")))?;
    Ok(())
}

fn write_section_atomic(layout: &V6Layout, name: &str, bytes: &[u8]) -> Result<()> {
    write_file_atomic(&layout.file(name), bytes)
}

fn read_section_exact(layout: &V6Layout, name: &str, expect_len: usize) -> Result<Vec<u8>> {
    let p = layout.file(name);
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
    // 文件可能比期望长（上次崩溃的 torn tail）：只读前缀，尾部下次提交截掉。
    let mut f = f;
    let mut buf = vec![0u8; expect_len];
    if expect_len > 0 {
        f.read_exact(&mut buf)
            .map_err(|e| crate::Error::Persist(format!("v6 读段失败: {e}")))?;
    }
    Ok(buf)
}

/// 全量分段重写（迁移 / `FullResync` / 降级）。调用方持有 store 读锁即可，
/// 文件 IO 期间不碰 store；与增量 commit 共用 `persist_lock` 串行。
pub fn v6_save_full(layout: &V6Layout, store: &IndexStore) -> Result<u64> {
    std::fs::create_dir_all(&layout.dir)
        .map_err(|e| crate::Error::Persist(format!("v6 建目录失败: {e}")))?;

    let prev_gen = std::fs::read(layout.file(F_MANIFEST))
        .ok()
        .and_then(|b| serde_json::from_slice::<V6Manifest>(&b).ok())
        .map(|m| m.generation)
        .unwrap_or(0);

    // entries：拼连续缓冲一次写（与 v5 相同格局，但独立文件、可追加）。
    let mut entry_bytes = Vec::with_capacity(store.entries.len() * ENTRY_SIZE);
    for e in &store.entries {
        entry_bytes.extend_from_slice(&file_entry_to_disk_bytes_v5(e));
    }
    write_section_atomic(layout, F_ENTRIES, &entry_bytes)?;

    let frn_bytes: &[u8] = unsafe {
        std::slice::from_raw_parts(
            store.frns.as_ptr() as *const u8,
            store.frns.len() * FRN_SIZE,
        )
    };
    write_section_atomic(layout, F_FRNS, frn_bytes)?;

    let mut dir_bytes = Vec::with_capacity(store.dirs.len() * DIR_SIZE);
    for d in &store.dirs {
        dir_bytes.extend_from_slice(&dir_entry_to_bytes(d));
    }
    write_section_atomic(layout, F_DIRS, &dir_bytes)?;

    let mut range_bytes = Vec::with_capacity(store.dir_path_ranges.len() * RANGE_SIZE);
    for (a, b) in &store.dir_path_ranges {
        range_bytes.extend_from_slice(&a.to_le_bytes());
        range_bytes.extend_from_slice(&b.to_le_bytes());
    }
    write_section_atomic(layout, F_RANGES, &range_bytes)?;

    write_section_atomic(layout, F_NAMES, &store.names_buf)?;
    write_section_atomic(layout, F_DIRPATHS, &store.dir_paths_buf)?;

    let m = V6Manifest {
        version: V6_VERSION,
        generation: prev_gen + 1,
        entry_count: store.entries.len(),
        dir_count: store.dirs.len(),
        names_len: store.names_buf.len(),
        dir_paths_len: store.dir_paths_buf.len(),
        metadata_ready: store.metadata_ready,
        volumes: store.volumes.iter().map(V6Volume::from).collect(),
    };
    let json = serde_json::to_vec(&m)
        .map_err(|e| crate::Error::Persist(format!("v6 manifest 序列化失败: {e}")))?;
    write_file_atomic(&layout.file(F_MANIFEST), &json)?;
    // trigram pending 边车随主数据一起落盘（v5 同序：主文件之后）。
    save_trigram_sidecars(&layout.index_bin, store);
    crate::progress!(
        "v6 全量：{} 条目 / {} 目录，generation {}",
        m.entry_count,
        m.dir_count,
        m.generation
    );
    Ok(m.generation)
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
    out.as_mut_slice().copy_from_slice(unsafe {
        std::slice::from_raw_parts(buf.as_ptr() as *const u64, n)
    });
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
    let m = read_manifest(layout)?;
    let entry_count = m.entry_count;
    let dir_count = m.dir_count;

    let entries = parse_entries(&read_section_exact(
        layout,
        F_ENTRIES,
        entry_count * ENTRY_SIZE,
    )?)?;
    let frns = parse_frns(&read_section_exact(layout, F_FRNS, entry_count * FRN_SIZE)?)?;
    if frns.len() != entries.len() {
        return Err(crate::Error::Persist("v6 frns 与 entries 不等长".into()));
    }
    let dirs = parse_dirs(&read_section_exact(layout, F_DIRS, dir_count * DIR_SIZE)?)?;
    let ranges = parse_ranges(
        &read_section_exact(layout, F_RANGES, dir_count * RANGE_SIZE)?,
        dir_count,
    )?;
    let names_buf = read_section_exact(layout, F_NAMES, m.names_len)?;
    let dir_paths_buf = read_section_exact(layout, F_DIRPATHS, m.dir_paths_len)?;

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
    let excluded_dirs = load_exclude_sidecar(&layout.index_bin);

    // trigram 边车挂载（.tri mmap + .tri.pending，与 v5 同语义）+ cjk 位图重建。
    let (trigram, tri_pending) = mount_trigram_sidecars(&layout.index_bin, entry_count);

    crate::progress!(
        "v6 加载：{} 条目 / {} 目录（generation {}，trigram {}）",
        entries.len(),
        dirs.len(),
        m.generation,
        if trigram.is_some() { "on" } else { "off" },
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
        trigram,
        tri_pending,
        cjk_names: RoaringBitmap::new(),
        excluded_dirs,
        mutation_log: None,
    };
    store.rebuild_cjk_bitmap();
    Ok(store)
}

/// 优先 v6，回退 v5（供 CLI status / service 启动共用）。
/// v6 manifest 存在但段损坏时也回退 v5（若 v5 存在）——调用方（service 启动）随后会
/// 用内存状态全量重写 v6，自愈；总比拒绝启动强。
pub fn load_store_auto(index_bin: &Path) -> Result<IndexStore> {
    let layout = V6Layout::for_index(index_bin);
    if layout.manifest_exists() {
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

fn open_rw(path: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| crate::Error::Persist(format!("v6 打开 {} 失败: {e}", path.display())))
}

fn pwrite(f: &mut File, off: u64, bytes: &[u8]) -> Result<()> {
    f.seek(SeekFrom::Start(off))
        .map_err(|e| crate::Error::Persist(format!("v6 seek 失败: {e}")))?;
    f.write_all(bytes)
        .map_err(|e| crate::Error::Persist(format!("v6 pwrite 失败: {e}")))?;
    Ok(())
}

/// 增量提交。调用方已把 store 锁释放，只带 `batch` 进来做文件 IO。
/// 任何校验失败 → `NeedFull`（调用方降级全量），绝不写坏文件。
pub fn v6_try_commit(layout: &V6Layout, batch: &CommitBatch) -> Result<CommitOutcome> {
    if batch.mutations.iter().any(|m| matches!(m, Mutation::FullResync)) {
        return Ok(CommitOutcome::NeedFull {
            reason: "batch 含 FullResync".into(),
        });
    }
    if batch.mutations.is_empty() {
        return Ok(CommitOutcome::Delta {
            applied: 0,
            bytes: 0,
            generation: read_manifest(layout).map(|m| m.generation).unwrap_or(0),
        });
    }
    let m = match read_manifest(layout) {
        // manifest 缺失/损坏 → 全量重建即可自愈（调用方随后会重写全部段 + 新 manifest）。
        // 这里绝不能直接 Err：service 已回退 v5 加载成功，全量一次就好。
        Ok(m) => m,
        Err(e) => {
            return Ok(CommitOutcome::NeedFull {
                reason: format!("manifest 不可用（{e:#}），降级全量"),
            });
        }
    };

    if batch.dir_paths_len != m.dir_paths_len {
        return Ok(CommitOutcome::NeedFull {
            reason: "dir_paths 变化，走全量".into(),
        });
    }

    // 打开 + 截到 manifest 长度（丢弃崩溃 tail）。
    let mut fe = open_rw(&layout.file(F_ENTRIES)).map_err(|_| {
        crate::Error::Persist("v6 缺段，降级全量".into())
    })?;
    let mut ff = open_rw(&layout.file(F_FRNS)).map_err(|_| {
        crate::Error::Persist("v6 缺段，降级全量".into())
    })?;
    let mut fd = open_rw(&layout.file(F_DIRS)).map_err(|_| {
        crate::Error::Persist("v6 缺段，降级全量".into())
    })?;
    let mut fr = open_rw(&layout.file(F_RANGES)).map_err(|_| {
        crate::Error::Persist("v6 缺段，降级全量".into())
    })?;
    let mut fn_ = open_rw(&layout.file(F_NAMES)).map_err(|_| {
        crate::Error::Persist("v6 缺段，降级全量".into())
    })?;

    let exp_e = (m.entry_count * ENTRY_SIZE) as u64;
    let exp_f = (m.entry_count * FRN_SIZE) as u64;
    let exp_d = (m.dir_count * DIR_SIZE) as u64;
    let exp_r = (m.dir_count * RANGE_SIZE) as u64;
    for (f, exp, name) in [
        (&fe, exp_e, F_ENTRIES),
        (&ff, exp_f, F_FRNS),
        (&fd, exp_d, F_DIRS),
        (&fr, exp_r, F_RANGES),
        (&fn_, m.names_len as u64, F_NAMES),
    ] {
        let len = f
            .metadata()
            .map_err(|e| crate::Error::Persist(format!("v6 stat 失败: {e}")))?
            .len();
        if len < exp {
            return Ok(CommitOutcome::NeedFull {
                reason: format!("段 {name} 短于 manifest（{len} < {exp}），降级全量"),
            });
        }
    }
    fe.set_len(exp_e).map_err(|e| crate::Error::Persist(format!("v6 截断失败: {e}")))?;
    ff.set_len(exp_f).map_err(|e| crate::Error::Persist(format!("v6 截断失败: {e}")))?;
    fd.set_len(exp_d).map_err(|e| crate::Error::Persist(format!("v6 截断失败: {e}")))?;
    fr.set_len(exp_r).map_err(|e| crate::Error::Persist(format!("v6 截断失败: {e}")))?;
    fn_.set_len(m.names_len as u64)
        .map_err(|e| crate::Error::Persist(format!("v6 截断失败: {e}")))?;

    // 回放：跟踪各文件当前尾，用于校验追加偏移与内存一致；同时记录实际动过的文件，
    // sync 时只刷这些（日常全是 Meta 小 patch 的话，只 sync entries.dat + manifest）。
    let mut cur_e = m.entry_count;
    let mut cur_d = m.dir_count;
    let mut cur_n = m.names_len;
    let mut bytes: u64 = 0;
    let (mut t_e, mut t_f, mut t_d, mut t_r, mut t_n) = (false, false, false, false, false);

    // names 追加 helper：校验内存偏移 == 文件尾（错位说明日志漏记 → 全量）。
    // 磁盘约定与内存 `append_name(..., tail_null=true)` 一致：名字字节 + 1 字节 0
    //（目录名同样带尾 null，便于整块 memchr 扫描）。
    let mut append_name = |f: &mut File, expect_off: u32, data: &[u8], bytes: &mut u64| -> Result<()> {
        if expect_off as usize != cur_n {
            return Err(crate::Error::Persist(format!(
                "v6 names 偏移错位（内存 {expect_off} vs 文件 {cur_n}），降级全量"
            )));
        }
        pwrite(f, cur_n as u64, data)?;
        cur_n += data.len();
        *bytes += data.len() as u64;
        pwrite(f, cur_n as u64, &[0u8])?;
        cur_n += 1;
        *bytes += 1;
        Ok(())
    };

    let need_full = |reason: String| Ok::<CommitOutcome, crate::Error>(CommitOutcome::NeedFull { reason });

    for mut_ in &batch.mutations {
        match mut_ {
            Mutation::FullResync => {
                return need_full("batch 含 FullResync".into());
            }
            Mutation::Meta { idx, size, mtime, ctime } => {
                let idx = *idx as usize;
                if idx >= m.entry_count {
                    return need_full(format!("Meta idx {idx} 越界"));
                }
                let mut b = [0u8; 16];
                b[0..8].copy_from_slice(&size.to_le_bytes());
                b[8..12].copy_from_slice(&mtime.to_le_bytes());
                b[12..16].copy_from_slice(&ctime.to_le_bytes());
                pwrite(&mut fe, (idx * ENTRY_SIZE) as u64, &b)?;
                bytes += 16;
                t_e = true;
            }
            Mutation::PatchEntry { idx, entry } => {
                let idx = *idx as usize;
                if idx >= m.entry_count {
                    return need_full(format!("PatchEntry idx {idx} 越界"));
                }
                let b = file_entry_to_disk_bytes_v5(entry);
                pwrite(&mut fe, (idx * ENTRY_SIZE) as u64, &b)?;
                bytes += ENTRY_SIZE as u64;
                t_e = true;
            }
            Mutation::PatchDir { di, dir } => {
                let di = *di as usize;
                if di >= m.dir_count {
                    return need_full(format!("PatchDir {di} 越界"));
                }
                let b = dir_entry_to_bytes(dir);
                pwrite(&mut fd, (di * DIR_SIZE) as u64, &b)?;
                bytes += DIR_SIZE as u64;
                t_d = true;
            }
            Mutation::PushFile { entry, frn, name } => {
                append_name(&mut fn_, entry.name_offset, name, &mut bytes)?;
                let b = file_entry_to_disk_bytes_v5(entry);
                pwrite(&mut fe, (cur_e * ENTRY_SIZE) as u64, &b)?;
                pwrite(&mut ff, (cur_e * FRN_SIZE) as u64, &frn.to_le_bytes())?;
                bytes += (ENTRY_SIZE + FRN_SIZE) as u64;
                cur_e += 1;
                t_e = true;
                t_f = true;
                t_n = true;
            }
            Mutation::PushDir { entry, frn, dir, name } => {
                append_name(&mut fn_, entry.name_offset, name, &mut bytes)?;
                let eb = file_entry_to_disk_bytes_v5(entry);
                pwrite(&mut fe, (cur_e * ENTRY_SIZE) as u64, &eb)?;
                pwrite(&mut ff, (cur_e * FRN_SIZE) as u64, &frn.to_le_bytes())?;
                let db = dir_entry_to_bytes(dir);
                pwrite(&mut fd, (cur_d * DIR_SIZE) as u64, &db)?;
                pwrite(&mut fr, (cur_d * RANGE_SIZE) as u64, &[0u8; RANGE_SIZE])?;
                bytes += (ENTRY_SIZE + FRN_SIZE + DIR_SIZE + RANGE_SIZE) as u64;
                cur_e += 1;
                cur_d += 1;
                t_e = true;
                t_f = true;
                t_d = true;
                t_r = true;
                t_n = true;
            }
            Mutation::RepathFile { idx, entry, name } => {
                let idx = *idx as usize;
                if idx >= m.entry_count {
                    return need_full(format!("RepathFile idx {idx} 越界"));
                }
                append_name(&mut fn_, entry.name_offset, name, &mut bytes)?;
                let b = file_entry_to_disk_bytes_v5(entry);
                pwrite(&mut fe, (idx * ENTRY_SIZE) as u64, &b)?;
                bytes += ENTRY_SIZE as u64;
                t_e = true;
                t_n = true;
            }
            Mutation::RepathDir { idx, di, entry, dir, name } => {
                let (idx, di) = (*idx as usize, *di as usize);
                if idx >= m.entry_count || di >= m.dir_count {
                    return need_full(format!("RepathDir {idx}/{di} 越界"));
                }
                append_name(&mut fn_, entry.name_offset, name, &mut bytes)?;
                let eb = file_entry_to_disk_bytes_v5(entry);
                pwrite(&mut fe, (idx * ENTRY_SIZE) as u64, &eb)?;
                let db = dir_entry_to_bytes(dir);
                pwrite(&mut fd, (di * DIR_SIZE) as u64, &db)?;
                bytes += (ENTRY_SIZE + DIR_SIZE) as u64;
                t_e = true;
                t_d = true;
                t_n = true;
            }
        }
    }

    // 终态校验：文件尾必须 == 内存快照长度（否则日志与内存不一致，全量兜底）。
    if cur_e != batch.entry_count || cur_d != batch.dir_count || cur_n != batch.names_len {
        return need_full(format!(
            "commit 尾校验失败（entries {cur_e} vs {}，dirs {cur_d} vs {}，names {cur_n} vs {}）",
            batch.entry_count, batch.dir_count, batch.names_len
        ));
    }

    for (f, touched) in [(&fe, t_e), (&ff, t_f), (&fd, t_d), (&fr, t_r), (&fn_, t_n)] {
        if touched {
            f.sync_all().map_err(|e| crate::Error::Persist(format!("v6 sync 失败: {e}")))?;
        }
    }

    let nm = V6Manifest {
        version: V6_VERSION,
        generation: m.generation + 1,
        entry_count: batch.entry_count,
        dir_count: batch.dir_count,
        names_len: batch.names_len,
        dir_paths_len: batch.dir_paths_len,
        metadata_ready: batch.metadata_ready,
        volumes: batch.volumes.iter().map(V6Volume::from).collect(),
    };
    let json = serde_json::to_vec(&nm)
        .map_err(|e| crate::Error::Persist(format!("v6 manifest 序列化失败: {e}")))?;
    write_file_atomic(&layout.file(F_MANIFEST), &json)?;

    // trigram pending 边车随主数据一起落盘（主 manifest 之后，与 v5 同序）。
    // 没有快照（.tri 从未构建）就不写，避免产生"有 pending 无快照"的中间态。
    if batch.has_trigram {
        if let Err(e) = crate::trigram::save_pending_sidecar(
            &layout.index_bin,
            &batch.tri_pending,
            batch.tri_snap,
        ) {
            crate::progress!("trigram：pending 边车写入失败（下一轮重试）: {e}");
        }
    }

    Ok(CommitOutcome::Delta {
        applied: batch.mutations.len(),
        bytes,
        generation: nm.generation,
    })
}
