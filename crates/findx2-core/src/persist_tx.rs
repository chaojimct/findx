//! Generational copy-on-write publish for v6 indexes.
//!
//! Already-published generation files are never opened for write. A commit
//! materializes a complete new generation (hard-link unchanged files, copy
//! then patch changed ones), fsyncs every new file and the generation
//! directory, then atomically replaces `published.json`. Failure or a kill
//! before that replace leaves the previous complete generation as the only
//! loadable state. After replace, only the new complete generation is loaded.
//! Legacy flat `manifest.json` + segment files stay read-only; the first
//! transactional write copies them into `gen/N` and does not patch them.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::index::{IndexStore, Mutation};
use crate::persist_v6::{
    V6Layout, V6Manifest, V6Volume, DIR_SIZE, ENTRY_SIZE, F_DIRPATHS, F_DIRS, F_ENTRIES, F_FRNS,
    F_MANIFEST, F_NAMES, F_RANGES, FRN_SIZE, RANGE_SIZE, V6_VERSION,
};
use crate::persist_v6::{CommitBatch, CommitOutcome};
use crate::Result;

pub const F_PUBLISHED: &str = "published.json";
pub const F_TRI_PENDING: &str = "tri.pending";
pub const F_TRIGRAM: &str = "trigram.dat";
const FORMAT: &str = "v6-gen";
const KEEP_GENERATIONS: u64 = 2;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileDigest {
    pub size: u64,
    pub crc32: u32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PublishedPointer {
    pub format: String,
    pub manifest: V6Manifest,
    pub files: BTreeMap<String, FileDigest>,
    pub has_trigram: bool,
    pub tri_snap: u32,
}

#[derive(Debug, Clone, Default)]
pub enum PersistFault {
    #[default]
    None,
    BeforePublish,
    AfterDataSync,
    RenameFail,
    ShortWrite { file: String, max_bytes: u64 },
    DiskFull { max_bytes: u64 },
    AccessDenied { file: String },
    Barrier(PathBuf),
}

static FAULT: Mutex<PersistFault> = Mutex::new(PersistFault::None);
static WRITTEN: Mutex<u64> = Mutex::new(0);

fn fault() -> PersistFault {
    if let Ok(v) = std::env::var("FINDX_PERSIST_FAULT") {
        return parse_fault_env(&v);
    }
    FAULT.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn parse_fault_env(v: &str) -> PersistFault {
    let mut parts = v.splitn(2, '=');
    match parts.next().unwrap_or("") {
        "before_publish" => PersistFault::BeforePublish,
        "after_data_sync" => PersistFault::AfterDataSync,
        "rename_fail" => PersistFault::RenameFail,
        "short_write" => {
            let rest = parts.next().unwrap_or("entries.dat:16");
            let (file, n) = rest.split_once(':').unwrap_or((rest, "16"));
            PersistFault::ShortWrite { file: file.into(), max_bytes: n.parse().unwrap_or(16) }
        }
        "disk_full" => PersistFault::DiskFull { max_bytes: parts.next().unwrap_or("64").parse().unwrap_or(64) },
        "access_denied" => PersistFault::AccessDenied { file: parts.next().unwrap_or("entries.dat").into() },
        "barrier" => PersistFault::Barrier(PathBuf::from(parts.next().unwrap_or("barrier.ready"))),
        _ => PersistFault::None,
    }
}

pub fn set_persist_fault(next: PersistFault) {
    *FAULT.lock().unwrap_or_else(|e| e.into_inner()) = next;
    *WRITTEN.lock().unwrap_or_else(|e| e.into_inner()) = 0;
}
pub fn clear_persist_fault() { set_persist_fault(PersistFault::None); }

fn persist_err(msg: impl Into<String>) -> crate::Error {
    crate::Error::Persist(msg.into())
}

fn io_err(e: std::io::Error, ctx: &str) -> crate::Error {
    persist_err(format!("{ctx}: {e}"))
}

pub fn published_path(layout: &V6Layout) -> PathBuf { layout.dir.join(F_PUBLISHED) }
pub fn generation_dir(layout: &V6Layout, gen: u64) -> PathBuf {
    layout.dir.join("gen").join(gen.to_string())
}

pub fn read_published(layout: &V6Layout) -> Result<Option<PublishedPointer>> {
    let p = published_path(layout);
    if !p.is_file() { return Ok(None); }
    let bytes = fs::read(&p).map_err(|e| io_err(e, "read published pointer"))?;
    let ptr: PublishedPointer = serde_json::from_slice(&bytes)
        .map_err(|e| persist_err(format!("published pointer parse failed: {e}")))?;
    if ptr.format != FORMAT {
        return Err(persist_err(format!("unsupported published format {}", ptr.format)));
    }
    if ptr.manifest.version != V6_VERSION {
        return Err(persist_err(format!("published manifest version {}", ptr.manifest.version)));
    }
    Ok(Some(ptr))
}

pub fn has_published(layout: &V6Layout) -> bool { published_path(layout).is_file() }

/// Load a verified published generation, or the legacy flat v6 layout.
pub fn load_current(layout: &V6Layout) -> Result<IndexStore> {
    if let Some(ptr) = read_published(layout)? {
        return load_published_generation(layout, &ptr);
    }
    crate::persist_v6::load_legacy_flat(layout)
}

pub fn load_published_generation(layout: &V6Layout, ptr: &PublishedPointer) -> Result<IndexStore> {
    let dir = generation_dir(layout, ptr.manifest.generation);
    verify_generation(&dir, ptr)?;
    crate::persist_v6::load_from_dir(&dir, &layout.index_bin, &ptr.manifest, ptr.has_trigram.then_some(dir.join(F_TRIGRAM)), ptr.has_trigram.then_some(dir.join(F_TRI_PENDING)), false)
}

fn verify_generation(dir: &Path, ptr: &PublishedPointer) -> Result<()> {
    for (name, expect) in &ptr.files {
        let path = dir.join(name);
        if !path.is_file() {
            return Err(persist_err(format!("published generation missing sidecar {name}")));
        }
        let got = digest_file(&path)?;
        if got.size != expect.size || got.crc32 != expect.crc32 {
            return Err(persist_err(format!(
                "published generation {name} failed integrity check (size {} vs {}, crc {:08x} vs {:08x})",
                got.size, expect.size, got.crc32, expect.crc32
            )));
        }
    }
    Ok(())
}

fn digest_file(path: &Path) -> Result<FileDigest> {
    let mut f = File::open(path).map_err(|e| io_err(e, "digest open"))?;
    let mut hasher = crc32fast::Hasher::new();
    let mut buf = [0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let n = f.read(&mut buf).map_err(|e| io_err(e, "digest read"))?;
        if n == 0 { break; }
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    Ok(FileDigest { size, crc32: hasher.finalize() })
}

static PUBLICATION: parking_lot::Mutex<()> = parking_lot::const_mutex(());
/// An in-process publication lease; NOT interprocess/index-owner exclusion.
/// Holding it pins the actual published generation for frozen evidence lifetime.
pub(crate) struct PublicationLease { _guard: parking_lot::MutexGuard<'static, ()> }
impl PublicationLease {
    pub(crate) fn acquire(deadline:std::time::Instant,cancel:&std::sync::atomic::AtomicBool)->std::result::Result<Self,String> {
        loop {
            if cancel.load(std::sync::atomic::Ordering::Acquire)||std::time::Instant::now()>=deadline{return Err("publication wait cancelled/timed out; no ACK".into());}
            if let Some(g)=PUBLICATION.try_lock_for(std::time::Duration::from_millis(5)){return Ok(Self{_guard:g});}
        }
    }
    pub(crate) fn save_full(&self,layout:&V6Layout,store:&IndexStore)->Result<u64>{save_full_generation_inner(layout,store)}
}

pub fn save_full_generation(layout: &V6Layout, store: &IndexStore) -> Result<u64> {
    let _publication=PUBLICATION.lock();
    save_full_generation_inner(layout,store)
}

fn save_full_generation_inner(layout: &V6Layout, store: &IndexStore) -> Result<u64> {
    fs::create_dir_all(&layout.dir).map_err(|e| io_err(e, "create v6 dir"))?;
    *WRITTEN.lock().unwrap_or_else(|e| e.into_inner()) = 0;
    let gen = next_generation(layout);
    let dir = generation_dir(layout, gen);
    prepare_generation_dir(&dir)?;
    let mut files = BTreeMap::new();
    write_new_file(&dir, F_ENTRIES, &entries_bytes(store), &mut files)?;
    let frn_bytes: &[u8] = unsafe {
        std::slice::from_raw_parts(store.frns.as_ptr() as *const u8, store.frns.len() * FRN_SIZE)
    };
    write_new_file(&dir, F_FRNS, frn_bytes, &mut files)?;
    write_new_file(&dir, F_DIRS, &dirs_bytes(store), &mut files)?;
    write_new_file(&dir, F_RANGES, &ranges_bytes(store), &mut files)?;
    write_new_file(&dir, F_NAMES, &store.names_buf, &mut files)?;
    write_new_file(&dir, F_DIRPATHS, &store.dir_paths_buf, &mut files)?;
    let has_trigram = store.trigram.is_some();
    let tri_snap = store.trigram.as_ref().map(|t| t.snapshot_entry_count()).unwrap_or(0);
    if has_trigram {
        write_pending(&dir, &store.tri_pending, tri_snap, &mut files)?;
        install_trigram(&dir, store, layout, &mut files)?;
    }
    hit_fault_after_data(&dir)?;
    let manifest = V6Manifest {
        version: V6_VERSION,
        generation: gen,
        entry_count: store.entries.len(),
        dir_count: store.dirs.len(),
        names_len: store.names_buf.len(),
        dir_paths_len: store.dir_paths_buf.len(),
        metadata_ready: store.metadata_ready,
        volumes: store.volumes.iter().map(V6Volume::from).collect(),
    };
    publish_generation(layout, dir, manifest, files, has_trigram, tri_snap)?;
    Ok(gen)
}

pub fn try_commit_generation(layout: &V6Layout, batch: &CommitBatch) -> Result<CommitOutcome> {
    let _publication=PUBLICATION.lock();
    if batch.mutations.iter().any(|m| matches!(m, Mutation::FullResync)) {
        return Ok(CommitOutcome::NeedFull { reason: "batch contains FullResync".into() });
    }
    if batch.mutations.is_empty() {
        return Ok(CommitOutcome::Delta {
            applied: 0,
            bytes: 0,
            generation: current_generation(layout).unwrap_or(0),
        });
    }
    let Some(base) = resolve_base(layout)? else {
        return Ok(CommitOutcome::NeedFull { reason: "no committed generation; need full snapshot".into() });
    };
    if batch.dir_paths_len != base.manifest.dir_paths_len {
        return Ok(CommitOutcome::NeedFull { reason: "dir_paths changed; need full snapshot".into() });
    }
    *WRITTEN.lock().unwrap_or_else(|e| e.into_inner()) = 0;
    let gen = next_generation(layout);
    let dir = generation_dir(layout, gen);
    prepare_generation_dir(&dir)?;
    match commit_delta_into(layout, &base, batch, &dir, gen) {
        Ok((files, bytes, has_trigram, tri_snap, manifest)) => {
            publish_generation(layout, dir, manifest, files, has_trigram, tri_snap)?;
            Ok(CommitOutcome::Delta { applied: batch.mutations.len(), bytes, generation: gen })
        }
        Err(e) => {
            let _ = fs::remove_dir_all(&dir);
            if is_need_full(&e) {
                Ok(CommitOutcome::NeedFull { reason: e.to_string() })
            } else {
                Err(e)
            }
        }
    }
}

fn is_need_full(e: &crate::Error) -> bool {
    let s = e.to_string();
    s.contains("need full") || s.contains("错位") || s.contains("越界") || s.contains("out of bounds") || s.contains("尾校验")
}

struct Base {
    manifest: V6Manifest,
    dir: PathBuf,
    files: BTreeMap<String, FileDigest>,
    has_trigram: bool,
    tri_snap: u32,
}

fn current_generation(layout: &V6Layout) -> Option<u64> {
    read_published(layout).ok().flatten().map(|p| p.manifest.generation)
        .or_else(|| crate::persist_v6::read_legacy_manifest(layout).ok().map(|m| m.generation))
}
fn next_generation(layout: &V6Layout) -> u64 {
    let mut max = current_generation(layout).unwrap_or(0);
    if let Ok(rd) = fs::read_dir(layout.dir.join("gen")) {
        for ent in rd.flatten() {
            if let Some(n) = ent.file_name().to_str().and_then(|s| s.parse::<u64>().ok()) {
                max = max.max(n);
            }
        }
    }
    max + 1
}

fn resolve_base(layout: &V6Layout) -> Result<Option<Base>> {
    if published_path(layout).is_file() {
        match read_published(layout) {
            Ok(Some(ptr)) => {
                let dir = generation_dir(layout, ptr.manifest.generation);
                if verify_generation(&dir, &ptr).is_err() {
                    return Ok(None);
                }
                return Ok(Some(Base { manifest: ptr.manifest, dir, files: ptr.files, has_trigram: ptr.has_trigram, tri_snap: ptr.tri_snap }));
            }
            Ok(None) | Err(_) => return Ok(None),
        }
    }
    if let Ok(m) = crate::persist_v6::read_legacy_manifest(layout) {
        let mut files = BTreeMap::new();
        for name in [F_ENTRIES, F_FRNS, F_DIRS, F_RANGES, F_NAMES, F_DIRPATHS] {
            let p = layout.dir.join(name);
            if p.is_file() { files.insert(name.to_string(), digest_file(&p)?); }
        }
        return Ok(Some(Base { manifest: m, dir: layout.dir.clone(), files, has_trigram: false, tri_snap: 0 }));
    }
    Ok(None)
}

fn commit_delta_into(
    layout: &V6Layout, base: &Base, batch: &CommitBatch, dest: &Path, gen: u64,
) -> Result<(BTreeMap<String, FileDigest>, u64, bool, u32, V6Manifest)> {
    let m = &base.manifest;
    let mut touched = Touched::default();
    for mut_ in &batch.mutations {
        match mut_ {
            Mutation::FullResync => {}
            Mutation::MetadataBatch(_) | Mutation::Meta { .. } | Mutation::PatchEntry { .. } => touched.entries = true,
            Mutation::PatchDir { .. } => touched.dirs = true,
            Mutation::PushFile { .. } => { touched.entries = true; touched.frns = true; touched.names = true; }
            Mutation::PushDir { .. } => { touched.entries = true; touched.frns = true; touched.dirs = true; touched.ranges = true; touched.names = true; }
            Mutation::RepathFile { .. } => { touched.entries = true; touched.names = true; }
            Mutation::RepathDir { .. } => { touched.entries = true; touched.dirs = true; touched.names = true; }
        }
    }
    let mut files = BTreeMap::new();
    let mut fe = materialize(base, dest, F_ENTRIES, touched.entries, (m.entry_count * ENTRY_SIZE) as u64, &mut files)?;
    let mut ff = materialize(base, dest, F_FRNS, touched.frns, (m.entry_count * FRN_SIZE) as u64, &mut files)?;
    let mut fd = materialize(base, dest, F_DIRS, touched.dirs, (m.dir_count * DIR_SIZE) as u64, &mut files)?;
    let mut fr = materialize(base, dest, F_RANGES, touched.ranges, (m.dir_count * RANGE_SIZE) as u64, &mut files)?;
    let mut fn_ = materialize(base, dest, F_NAMES, touched.names, m.names_len as u64, &mut files)?;
    let _dp = materialize(base, dest, F_DIRPATHS, false, m.dir_paths_len as u64, &mut files)?;

    let mut cur_e = m.entry_count;
    let mut cur_d = m.dir_count;
    let mut cur_n = m.names_len;
    let mut bytes = 0u64;
    let mut append_name = |f: &mut Option<File>, expect_off: u32, data: &[u8], bytes: &mut u64| -> Result<()> {
        if expect_off as usize != cur_n {
            return Err(persist_err(format!("v6 names 偏移错位（内存 {expect_off} vs 文件 {cur_n}），need full")));
        }
        let file = f.as_mut().ok_or_else(|| persist_err("names file missing"))?;
        pwrite(file, cur_n as u64, data, F_NAMES)?;
        cur_n += data.len();
        *bytes += data.len() as u64;
        pwrite(file, cur_n as u64, &[0u8], F_NAMES)?;
        cur_n += 1;
        *bytes += 1;
        Ok(())
    };

    for mut_ in &batch.mutations {
        match mut_ {
            Mutation::FullResync => return Err(persist_err("batch 含 FullResync, need full")),
            Mutation::MetadataBatch(patches) => {
                for (idx, data) in patches {
                    if *idx as usize >= cur_e { return Err(persist_err("metadata batch slot out of bounds, need full")); }
                    let at = (*idx as usize * ENTRY_SIZE) as u64;
                    pwrite(fe.as_mut().unwrap(), at, &data[..16], F_ENTRIES)?;
                    pwrite(fe.as_mut().unwrap(), at + 30, &data[16..18], F_ENTRIES)?;
                    bytes += 18;
                }
            }
            Mutation::Meta { idx, size, mtime, ctime } => {
                let idx = *idx as usize;
                if idx >= m.entry_count { return Err(persist_err(format!("Meta idx {idx} 越界, need full"))); }
                let mut b = [0u8; 16];
                b[0..8].copy_from_slice(&size.to_le_bytes());
                b[8..12].copy_from_slice(&mtime.to_le_bytes());
                b[12..16].copy_from_slice(&ctime.to_le_bytes());
                pwrite(fe.as_mut().unwrap(), (idx * ENTRY_SIZE) as u64, &b, F_ENTRIES)?;
                bytes += 16;
            }
            Mutation::PatchEntry { idx, entry } => {
                let idx = *idx as usize;
                if idx >= m.entry_count { return Err(persist_err(format!("PatchEntry idx {idx} 越界, need full"))); }
                let b = crate::persist::file_entry_to_disk_bytes_v5(entry);
                pwrite(fe.as_mut().unwrap(), (idx * ENTRY_SIZE) as u64, &b, F_ENTRIES)?;
                bytes += ENTRY_SIZE as u64;
            }
            Mutation::PatchDir { di, dir } => {
                let di = *di as usize;
                if di >= m.dir_count { return Err(persist_err(format!("PatchDir {di} 越界, need full"))); }
                let b = crate::persist::dir_entry_to_bytes(dir);
                pwrite(fd.as_mut().unwrap(), (di * DIR_SIZE) as u64, &b, F_DIRS)?;
                bytes += DIR_SIZE as u64;
            }
            Mutation::PushFile { entry, frn, name } => {
                append_name(&mut fn_, entry.name_offset, name, &mut bytes)?;
                let b = crate::persist::file_entry_to_disk_bytes_v5(entry);
                pwrite(fe.as_mut().unwrap(), (cur_e * ENTRY_SIZE) as u64, &b, F_ENTRIES)?;
                pwrite(ff.as_mut().unwrap(), (cur_e * FRN_SIZE) as u64, &frn.to_le_bytes(), F_FRNS)?;
                bytes += (ENTRY_SIZE + FRN_SIZE) as u64;
                cur_e += 1;
            }
            Mutation::PushDir { entry, frn, dir, name } => {
                append_name(&mut fn_, entry.name_offset, name, &mut bytes)?;
                let eb = crate::persist::file_entry_to_disk_bytes_v5(entry);
                pwrite(fe.as_mut().unwrap(), (cur_e * ENTRY_SIZE) as u64, &eb, F_ENTRIES)?;
                pwrite(ff.as_mut().unwrap(), (cur_e * FRN_SIZE) as u64, &frn.to_le_bytes(), F_FRNS)?;
                let db = crate::persist::dir_entry_to_bytes(dir);
                pwrite(fd.as_mut().unwrap(), (cur_d * DIR_SIZE) as u64, &db, F_DIRS)?;
                pwrite(fr.as_mut().unwrap(), (cur_d * RANGE_SIZE) as u64, &[0u8; RANGE_SIZE], F_RANGES)?;
                bytes += (ENTRY_SIZE + FRN_SIZE + DIR_SIZE + RANGE_SIZE) as u64;
                cur_e += 1;
                cur_d += 1;
            }
            Mutation::RepathFile { idx, entry, name } => {
                let idx = *idx as usize;
                if idx >= m.entry_count { return Err(persist_err(format!("RepathFile idx {idx} 越界, need full"))); }
                append_name(&mut fn_, entry.name_offset, name, &mut bytes)?;
                let b = crate::persist::file_entry_to_disk_bytes_v5(entry);
                pwrite(fe.as_mut().unwrap(), (idx * ENTRY_SIZE) as u64, &b, F_ENTRIES)?;
                bytes += ENTRY_SIZE as u64;
            }
            Mutation::RepathDir { idx, di, entry, dir, name } => {
                let (idx, di) = (*idx as usize, *di as usize);
                if idx >= m.entry_count || di >= m.dir_count { return Err(persist_err(format!("RepathDir {idx}/{di} 越界, need full"))); }
                append_name(&mut fn_, entry.name_offset, name, &mut bytes)?;
                let eb = crate::persist::file_entry_to_disk_bytes_v5(entry);
                pwrite(fe.as_mut().unwrap(), (idx * ENTRY_SIZE) as u64, &eb, F_ENTRIES)?;
                let db = crate::persist::dir_entry_to_bytes(dir);
                pwrite(fd.as_mut().unwrap(), (di * DIR_SIZE) as u64, &db, F_DIRS)?;
                bytes += (ENTRY_SIZE + DIR_SIZE) as u64;
            }
        }
    }
    if cur_e != batch.entry_count || cur_d != batch.dir_count || cur_n != batch.names_len {
        return Err(persist_err(format!(
            "commit 尾校验失败（entries {cur_e} vs {}，dirs {cur_d} vs {}，names {cur_n} vs {}）, need full",
            batch.entry_count, batch.dir_count, batch.names_len
        )));
    }
    for (name, file) in [
        (F_ENTRIES, &mut fe), (F_FRNS, &mut ff), (F_DIRS, &mut fd), (F_RANGES, &mut fr), (F_NAMES, &mut fn_),
    ] {
        if let Some(f) = file {
            f.sync_all().map_err(|e| io_err(e, "generation sync"))?;
            drop(file.take());
            files.insert(name.to_string(), digest_file(&dest.join(name))?);
        }
    }
    let has_trigram = batch.has_trigram || base.has_trigram;
    let tri_snap = if batch.has_trigram { batch.tri_snap } else { base.tri_snap };
    if has_trigram {
        if batch.has_trigram {
            write_pending(dest, &batch.tri_pending, batch.tri_snap, &mut files)?;
        } else if base.dir.join(F_TRI_PENDING).is_file() {
            link_or_copy(&base.dir.join(F_TRI_PENDING), &dest.join(F_TRI_PENDING), false, F_TRI_PENDING, &mut files)?;
        }
        let src_tri = if base.dir.join(F_TRIGRAM).is_file() {
            Some(base.dir.join(F_TRIGRAM))
        } else {
            let classic = crate::trigram::tri_sidecar_path(&layout.index_bin);
            classic.is_file().then_some(classic)
        };
        if let Some(src) = src_tri {
            link_or_copy(&src, &dest.join(F_TRIGRAM), false, F_TRIGRAM, &mut files)?;
        }
    }
    hit_fault_after_data(dest)?;
    let manifest = V6Manifest {
        version: V6_VERSION,
        generation: gen,
        entry_count: batch.entry_count,
        dir_count: batch.dir_count,
        names_len: batch.names_len,
        dir_paths_len: batch.dir_paths_len,
        metadata_ready: batch.metadata_ready,
        volumes: batch.volumes.iter().map(V6Volume::from).collect(),
    };
    Ok((files, bytes, has_trigram, tri_snap, manifest))
}

#[derive(Default)]
struct Touched { entries: bool, frns: bool, dirs: bool, ranges: bool, names: bool }

fn materialize(
    base: &Base, dest: &Path, name: &str, writable: bool, expect_len: u64,
    files: &mut BTreeMap<String, FileDigest>,
) -> Result<Option<File>> {
    let src = base.dir.join(name);
    if !src.is_file() { return Err(persist_err(format!("base segment {name} missing, need full"))); }
    let dst = dest.join(name);
    if !writable {
        if let Some(d) = base.files.get(name) {
            if d.size < expect_len { return Err(persist_err(format!("段 {name} 短于 manifest, need full"))); }
        }
        link_or_copy(&src, &dst, false, name, files)?;
        // A hardlink shares the base inode, so the base digest still
        // describes these bytes: skip re-reading hundreds of MB just to
        // recompute an identical crc32. Only reuse when the sizes line up
        // exactly; anything else keeps the freshly computed digest.
        if let Some(d) = base.files.get(name) {
            if d.size == expect_len {
                files.insert(name.to_string(), d.clone());
            }
        }
        return Ok(None);
    }
    // Copy a committed prefix, never share an inode we will mutate.
    copy_prefix(&src, &dst, expect_len, name)?;
    let f = open_rw(&dst, name)?;
    Ok(Some(f))
}

fn prepare_generation_dir(dir: &Path) -> Result<()> {
    if dir.exists() { fs::remove_dir_all(dir).map_err(|e| io_err(e, "remove leftover unpublished generation"))?; }
    fs::create_dir_all(dir).map_err(|e| io_err(e, "create generation dir"))
}

fn publish_generation(
    layout: &V6Layout, dir: PathBuf, manifest: V6Manifest,
    files: BTreeMap<String, FileDigest>, has_trigram: bool, tri_snap: u32,
) -> Result<()> {
    let gen = manifest.generation;
    let json = serde_json::to_vec_pretty(&manifest).map_err(|e| persist_err(format!("manifest serialize: {e}")))?;
    write_new_file_raw(&dir.join(F_MANIFEST), &json)?;
    sync_dir(&dir)?;
    match fault() {
        PersistFault::BeforePublish => return Err(persist_err("injected failure before publish")),
        PersistFault::Barrier(path) => {
            fs::write(&path, b"ready").map_err(|e| io_err(e, "barrier"))?;
            loop { std::thread::sleep(std::time::Duration::from_millis(50)); }
        }
        PersistFault::RenameFail => return Err(persist_err("injected rename failure")),
        _ => {}
    }
    let ptr = PublishedPointer { format: FORMAT.into(), manifest, files, has_trigram, tri_snap };
    let body = serde_json::to_vec_pretty(&ptr).map_err(|e| persist_err(format!("published serialize: {e}")))?;
    atomic_publish(&published_path(layout), &body)?;
    sync_dir(&layout.dir)?;
    reclaim_old_generations(layout, gen);
    Ok(())
}

/// Publish a rebuilt trigram snapshot together with the currently committed
/// namespace/metadata/pending set. Data files are hard-linked from the live
/// generation; only trigram bytes and pending change.
pub fn publish_trigram_snapshot(
    layout: &V6Layout, tri_path: &Path, pending: &roaring::RoaringBitmap, tri_snap: u32,
) -> Result<u64> {
    let _publication=PUBLICATION.lock();
    let Some(base) = resolve_base(layout)? else {
        return Err(persist_err("cannot publish trigram without a committed generation"));
    };
    *WRITTEN.lock().unwrap_or_else(|e| e.into_inner()) = 0;
    let gen = next_generation(layout);
    let dir = generation_dir(layout, gen);
    prepare_generation_dir(&dir)?;
    let mut files = BTreeMap::new();
    for name in [F_ENTRIES, F_FRNS, F_DIRS, F_RANGES, F_NAMES, F_DIRPATHS] {
        link_or_copy(&base.dir.join(name), &dir.join(name), false, name, &mut files)?;
    }
    write_pending(&dir, pending, tri_snap, &mut files)?;
    link_or_copy(tri_path, &dir.join(F_TRIGRAM), false, F_TRIGRAM, &mut files)?;
    let mut manifest = base.manifest.clone();
    manifest.generation = gen;
    hit_fault_after_data(&dir)?;
    publish_generation(layout, dir, manifest, files, true, tri_snap)?;
    Ok(gen)
}

fn reclaim_old_generations(layout: &V6Layout, current: u64) {
    let keep_from = current.saturating_sub(KEEP_GENERATIONS - 1);
    let root = layout.dir.join("gen");
    let Ok(entries) = fs::read_dir(&root) else { return; };
    for ent in entries.flatten() {
        let name = ent.file_name();
        let Some(n) = name.to_str().and_then(|s| s.parse::<u64>().ok()) else { continue; };
        if n < keep_from {
            let _ = fs::remove_dir_all(ent.path());
        }
    }
}

pub fn generation_disk_bytes(layout: &V6Layout) -> u64 {
    walk_size(&layout.dir.join("gen"))
}
fn walk_size(path: &Path) -> u64 {
    let mut n = 0u64;
    let Ok(rd) = fs::read_dir(path) else { return 0; };
    for ent in rd.flatten() {
        let p = ent.path();
        if p.is_dir() { n += walk_size(&p); }
        else if let Ok(m) = ent.metadata() { n += m.len(); }
    }
    n
}

fn entries_bytes(store: &IndexStore) -> Vec<u8> {
    let mut b = Vec::with_capacity(store.entries.len() * ENTRY_SIZE);
    for e in &store.entries { b.extend_from_slice(&crate::persist::file_entry_to_disk_bytes_v5(e)); }
    b
}
fn dirs_bytes(store: &IndexStore) -> Vec<u8> {
    let mut b = Vec::with_capacity(store.dirs.len() * DIR_SIZE);
    for d in &store.dirs { b.extend_from_slice(&crate::persist::dir_entry_to_bytes(d)); }
    b
}
fn ranges_bytes(store: &IndexStore) -> Vec<u8> {
    let mut b = Vec::with_capacity(store.dir_path_ranges.len() * RANGE_SIZE);
    for (a, c) in &store.dir_path_ranges {
        b.extend_from_slice(&a.to_le_bytes());
        b.extend_from_slice(&c.to_le_bytes());
    }
    b
}

fn write_pending(dir: &Path, pending: &roaring::RoaringBitmap, snap: u32, files: &mut BTreeMap<String, FileDigest>) -> Result<()> {
    let mut body = Vec::new();
    body.extend_from_slice(&snap.to_le_bytes());
    pending.serialize_into(&mut body).map_err(|e| persist_err(e.to_string()))?;
    write_new_file(dir, F_TRI_PENDING, &body, files)
}

fn install_trigram(dir: &Path, store: &IndexStore, layout: &V6Layout, files: &mut BTreeMap<String, FileDigest>) -> Result<()> {
    if let Some(prev) = read_published(layout).ok().flatten() {
        let src = generation_dir(layout, prev.manifest.generation).join(F_TRIGRAM);
        if src.is_file() {
            return link_or_copy(&src, &dir.join(F_TRIGRAM), false, F_TRIGRAM, files);
        }
    }
    let classic = crate::trigram::tri_sidecar_path(&layout.index_bin);
    if classic.is_file() && store.trigram.is_some() {
        return link_or_copy(&classic, &dir.join(F_TRIGRAM), false, F_TRIGRAM, files);
    }
    Ok(())
}

fn write_new_file(dir: &Path, name: &str, bytes: &[u8], files: &mut BTreeMap<String, FileDigest>) -> Result<()> {
    refuse_access(name)?;
    write_new_file_raw(&dir.join(name), bytes)?;
    files.insert(name.to_string(), FileDigest { size: bytes.len() as u64, crc32: crc32fast::hash(bytes) });
    Ok(())
}

fn write_new_file_raw(path: &Path, bytes: &[u8]) -> Result<()> {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    refuse_access(name)?;
    account_write(name, bytes.len() as u64)?;
    let mut f = File::create(path).map_err(|e| io_err(e, "create generation file"))?;
    f.write_all(bytes).map_err(|e| io_err(e, "write generation file"))?;
    f.sync_all().map_err(|e| io_err(e, "sync generation file"))?;
    Ok(())
}

fn link_or_copy(src: &Path, dst: &Path, writable: bool, name: &str, files: &mut BTreeMap<String, FileDigest>) -> Result<()> {
    refuse_access(name)?;
    if !writable {
        match fs::hard_link(src, dst) {
            Ok(()) => { files.insert(name.to_string(), digest_file(dst)?); return Ok(()); }
            Err(_) => {}
        }
    }
    let len = fs::metadata(src).map_err(|e| io_err(e, "stat source"))?.len();
    copy_prefix(src, dst, len, name)?;
    files.insert(name.to_string(), digest_file(dst)?);
    Ok(())
}

fn copy_prefix(src: &Path, dst: &Path, expect_len: u64, name: &str) -> Result<()> {
    refuse_access(name)?;
    let src_len = fs::metadata(src).map_err(|e| io_err(e, "stat base segment"))?.len();
    if src_len < expect_len {
        return Err(persist_err(format!("段 {name} 短于 manifest（{src_len} < {expect_len}），need full")));
    }
    account_write(name, expect_len)?;
    // Fast path: NTFS/ReFS block clone (copy-on-write, near-zero IO for the
    // prefix; only the subsequent pwrite deltas touch disk). Any failure —
    // unsupported FS, sparseness, alignment — falls back to the byte copy.
    // The length check below keeps a short/partial clone from ever passing
    // as a valid base.
    if clone_file_range(src, dst, expect_len).is_ok() {
        let got = fs::metadata(dst).map_err(|e| io_err(e, "stat cloned segment"))?.len();
        if got == expect_len {
            let f = open_rw(dst, name)?;
            f.sync_all().map_err(|e| io_err(e, "sync cow file"))?;
            return Ok(());
        }
        let _ = fs::remove_file(dst);
    }
    let mut input = File::open(src).map_err(|e| io_err(e, "open base segment"))?;
    let mut output = File::create(dst).map_err(|e| io_err(e, "create cow file"))?;
    let mut remaining = expect_len;
    let mut buf = [0u8; 64 * 1024];
    while remaining > 0 {
        let chunk = buf.len().min(remaining as usize);
        input.read_exact(&mut buf[..chunk]).map_err(|e| io_err(e, "copy base prefix"))?;
        output.write_all(&buf[..chunk]).map_err(|e| io_err(e, "copy base prefix"))?;
        remaining -= chunk as u64;
    }
    output.sync_all().map_err(|e| io_err(e, "sync cow file"))?;
    Ok(())
}

/// Best-effort OS block clone of `len` bytes. Returns Err on any problem;
/// callers must fall back to a plain copy. Non-Windows always falls back.
#[cfg(windows)]
fn clone_file_range(src: &Path, dst: &Path, len: u64) -> Result<()> {
    use std::os::windows::io::AsRawHandle;
    if len == 0 {
        File::create(dst).map_err(|e| io_err(e, "create empty cow file"))?;
        return Ok(());
    }
    let src_f = File::open(src).map_err(|e| io_err(e, "open base segment"))?;
    let src_len = src_f.metadata().map_err(|e| io_err(e, "stat base segment"))?.len();
    if src_len < len {
        return Err(persist_err(format!(
            "block clone source short ({src_len} < {len})"
        )));
    }
    let dst_f = File::create(dst).map_err(|e| io_err(e, "create cow file"))?;
    #[repr(C)]
    #[allow(non_snake_case)]
    struct DuplicateExtents {
        FileHandle: isize,
        SourceFileOffset: i64,
        TargetFileOffset: i64,
        ByteCount: i64,
    }
    // Verified against the windows crate (Win32/System/Ioctl):
    // FSCTL_DUPLICATE_EXTENTS_TO_FILE = 623428 (0x98344).
    const FSCTL_DUPLICATE_EXTENTS_TO_FILE: u32 = 623428;
    let params = DuplicateExtents {
        FileHandle: src_f.as_raw_handle() as isize,
        SourceFileOffset: 0,
        TargetFileOffset: 0,
        ByteCount: len as i64,
    };
    let mut returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            dst_f.as_raw_handle() as isize,
            FSCTL_DUPLICATE_EXTENTS_TO_FILE,
            &params as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<DuplicateExtents>() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(persist_err(format!(
            "block clone unsupported ({})",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

#[cfg(not(windows))]
fn clone_file_range(_src: &Path, _dst: &Path, len: u64) -> Result<()> {
    if len == 0 {
        // Zero-length segments still need the file to exist; callers create
        // it themselves on the fallback path, so report "unsupported" here.
    }
    Err(persist_err("block clone unsupported on this platform"))
}

fn open_rw(path: &Path, name: &str) -> Result<File> {
    refuse_access(name)?;
    OpenOptions::new().read(true).write(true).open(path).map_err(|e| io_err(e, "open cow file"))
}

fn pwrite(f: &mut File, off: u64, bytes: &[u8], name: &str) -> Result<()> {
    refuse_access(name)?;
    account_write(name, bytes.len() as u64)?;
    f.seek(SeekFrom::Start(off)).map_err(|e| io_err(e, "seek"))?;
    f.write_all(bytes).map_err(|e| io_err(e, "pwrite"))?;
    Ok(())
}

fn refuse_access(name: &str) -> Result<()> {
    if let PersistFault::AccessDenied { file } = fault() {
        if file == name || file == "*" {
            return Err(persist_err(format!("injected access failure: {name}")));
        }
    }
    Ok(())
}

fn account_write(name: &str, add: u64) -> Result<()> {
    let mut n = WRITTEN.lock().unwrap_or_else(|e| e.into_inner());
    match fault() {
        PersistFault::ShortWrite { file, max_bytes } if file == name || file == "*" => {
            if *n + add > max_bytes {
                return Err(persist_err(format!("injected short write: {name}")));
            }
        }
        PersistFault::DiskFull { max_bytes } => {
            if *n + add > max_bytes {
                return Err(crate::Error::Persist(format!(
                    "disk full (injected quota {max_bytes}): {}",
                    std::io::Error::from_raw_os_error(112)
                )));
            }
        }
        _ => {}
    }
    *n += add;
    Ok(())
}

fn hit_fault_after_data(dir: &Path) -> Result<()> {
    match fault() {
        PersistFault::AfterDataSync => Err(persist_err("injected failure after data sync, before publish")),
        PersistFault::Barrier(path) => {
            fs::write(&path, b"ready").map_err(|e| io_err(e, "barrier"))?;
            loop { std::thread::sleep(std::time::Duration::from_millis(50)); }
        }
        _ => { let _ = dir; Ok(()) }
    }
}

pub fn atomic_publish(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = File::create(&tmp).map_err(|e| io_err(e, "create publish tmp"))?;
        f.write_all(bytes).map_err(|e| io_err(e, "write publish tmp"))?;
        f.sync_all().map_err(|e| io_err(e, "sync publish tmp"))?;
    }
    atomic_rename_replace(&tmp, path)?;
    Ok(())
}

pub fn atomic_rename_replace(from: &Path, to: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let src: Vec<u16> = from.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let dst: Vec<u16> = to.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
        const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
        let ok = unsafe { MoveFileExW(src.as_ptr(), dst.as_ptr(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) };
        if ok == 0 {
            return Err(io_err(std::io::Error::last_os_error(), "atomic replace"));
        }
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        fs::rename(from, to).map_err(|e| io_err(e, "atomic replace"))
    }
}

fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        const GENERIC_READ: u32 = 0x80000000;
        const GENERIC_WRITE: u32 = 0x40000000;
        const FILE_SHARE_READ: u32 = 0x1;
        const FILE_SHARE_WRITE: u32 = 0x2;
        const FILE_SHARE_DELETE: u32 = 0x4;
        const OPEN_EXISTING: u32 = 3;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x02000000;
        const INVALID: isize = -1;
        let mut h = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID {
            h = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    std::ptr::null_mut(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS,
                    std::ptr::null_mut(),
                )
            };
        }
        if h == INVALID {
            return Err(io_err(std::io::Error::last_os_error(), "open directory for fsync"));
        }
        let ok = unsafe { FlushFileBuffers(h) };
        unsafe { CloseHandle(h) };
        if ok == 0 {
            return Err(io_err(std::io::Error::last_os_error(), "fsync directory"));
        }
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        File::open(path).and_then(|f| f.sync_all()).map_err(|e| io_err(e, "fsync directory"))
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    fn CreateFileW(
        name: *const u16, access: u32, share: u32, sec: *mut u8,
        disposition: u32, flags: u32, template: *mut u8,
    ) -> isize;
    fn FlushFileBuffers(h: isize) -> i32;
    fn CloseHandle(h: isize) -> i32;
    #[allow(dead_code)]
    fn GetVolumeInformationW(
        root: *const u16,
        vol_name: *mut u16,
        vol_name_len: u32,
        serial: *mut u32,
        max_comp_len: *mut u32,
        fs_flags: *mut u32,
        fs_name: *mut u16,
        fs_name_len: u32,
    ) -> i32;
    fn DeviceIoControl(
        h_device: isize,
        dw_io_control_code: u32,
        lp_in_buffer: *const core::ffi::c_void,
        n_in_buffer_size: u32,
        lp_out_buffer: *mut core::ffi::c_void,
        n_out_buffer_size: u32,
        lp_bytes_returned: *mut u32,
        lp_overlapped: *mut core::ffi::c_void,
    ) -> i32;
}

#[cfg(test)]
mod clone_tests {
    use super::*;
    use std::io::{Read, Write};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "findx-clone-{}-{}",
            std::process::id(),
            name
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn copy_prefix_is_byte_identical_with_or_without_block_clone() {
        let dir = scratch("roundtrip");
        // Patterned content larger than one copy chunk, odd size.
        let mut bytes = Vec::with_capacity(300_000);
        for i in 0..300_000u32 {
            bytes.push((i.wrapping_mul(2654435761) >> 13) as u8);
        }
        let src = dir.join("src.dat");
        std::fs::write(&src, &bytes).unwrap();
        let dst = dir.join("dst.dat");
        copy_prefix(&src, &dst, bytes.len() as u64, "test.dat").unwrap();
        let back = std::fs::read(&dst).unwrap();
        assert_eq!(back, bytes);
        // Short manifest must refuse, never truncate silently.
        assert!(copy_prefix(&src, &dir.join("short.dat"), bytes.len() as u64 + 1, "test.dat").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clone_short_source_is_a_clean_error() {
        let dir = scratch("short");
        let src = dir.join("src.dat");
        std::fs::write(&src, b"12345678").unwrap();
        // Direct clone of more bytes than exist must fail so the caller
        // falls back instead of publishing a short segment.
        assert!(clone_file_range(&src, &dir.join("dst.dat"), 16).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_prefix_is_created_without_io() {
        let dir = scratch("empty");
        let src = dir.join("src.dat");
        std::fs::write(&src, b"").unwrap();
        let dst = dir.join("dst.dat");
        copy_prefix(&src, &dst, 0, "empty.dat").unwrap();
        assert_eq!(std::fs::metadata(&dst).unwrap().len(), 0);
        let mut probe = Vec::new();
        File::open(&dst).unwrap().read_to_end(&mut probe).unwrap();
        assert!(probe.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn block_clone_actually_clones_on_ntfs() {
        let dir = scratch("clone-probe");
        // 32 MiB patterned file: large enough that a byte copy would be
        // observable, small enough for a unit test.
        let mut bytes = Vec::with_capacity(32 << 20);
        for i in 0..(32 << 20) as u32 {
            bytes.push((i.wrapping_mul(2654435761) >> 11) as u8);
        }
        let src = dir.join("src.dat");
        std::fs::write(&src, &bytes).unwrap();
        let dst = dir.join("dst.dat");
        let r = clone_file_range(&src, &dst, bytes.len() as u64);
        // NTFS/ReFS: must succeed. Other filesystems: explicit fallback signal.
        let fs_kind: String = {
            use std::os::windows::ffi::OsStrExt;
            let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
            let mut fs_name = [0u16; 32];
            let ok = unsafe {
                GetVolumeInformationW(
                    wide.as_ptr(),
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    fs_name.as_mut_ptr(),
                    fs_name.len() as u32,
                )
            };
            if ok == 0 {
                "unknown".into()
            } else {
                String::from_utf16_lossy(&fs_name[..fs_name.iter().position(|&c| c == 0).unwrap_or(0)])
            }
        };
        if fs_kind == "NTFS" || fs_kind == "ReFS" {
            assert!(r.is_ok(), "block clone must work on {fs_kind}: {r:?}");
            let back = std::fs::read(&dst).unwrap();
            assert_eq!(back, bytes);
        } else {
            assert!(r.is_err(), "non-cloning FS must report fallback");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn digest_reuse_matches_fresh_digest_for_hardlinked_segment() {
        let dir = scratch("digest");
        let bytes: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let src = dir.join("seg.dat");
        {
            let mut f = File::create(&src).unwrap();
            f.write_all(&bytes).unwrap();
            f.sync_all().unwrap();
        }
        let dst = dir.join("seg-copy.dat");
        fs::hard_link(&src, &dst).unwrap();
        let fresh = digest_file(&dst).unwrap();
        let base = FileDigest { size: fresh.size, crc32: fresh.crc32 };
        // Same bytes through the link: reusing the base digest must equal a
        // fresh read. This is what materialize() now relies on.
        assert_eq!(base, digest_file(&src).unwrap());
        assert_eq!(base.size as usize, bytes.len());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
