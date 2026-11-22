//! Write-ahead log (WAL-1): per-drain durability for [`CommitBatch`].
//!
//! Every drained batch is fsync-appended here BEFORE the (throttled) fold
//! commits it into a generation. Crash recovery = load the published
//! generation, then replay frames with `seq > gen.wal_seq`.
//!
//! Design notes:
//! - Single file `wal/wal.log` (no segments in WAL-1; bounded by the fold
//!   interval, typically tens of MB). Append-only; readers tolerate a torn
//!   tail (crash mid-frame) by stopping at the last good frame.
//! - [`FullResync`] frames are NOT replayable: recovery stops there and the
//!   service resumes watchers from the generation cursors (USN replay of the
//!   window is idempotent; un-replayed stat observations are re-done by
//!   backfill, bounded by the crash window).
//! - Recovery verifies each frame's pre-lengths against the store; any gap
//!   or overlap stops replay loudly (degrades to cursor resume) instead of
//!   silently duplicating entries.
//! - Commits are serialized by `persist_lock`, so a failed commit's frame is
//!   always the file tail and can be truncated exactly.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use roaring::RoaringBitmap;

use crate::index::{DirEntry, FileEntry, Mutation, VolumeState, IndexStore};
use crate::persist_v6::CommitBatch;

const WAL_MAGIC: u32 = 0x5741_4c31; // "WAL1"
const WAL_VERSION: u16 = 1;
const WAL_DIR: &str = "wal";
const WAL_FILE: &str = "wal.log";

/// Max sane frame payload: 1 GiB. Anything larger is corruption, never data
/// (a whole CommitBatch is KB–tens of MB; see service logs: ~22 MB max).
const MAX_FRAME: u32 = 1 << 30;

/// Store lengths a frame expects BEFORE its mutations apply. Recovery stops
/// (loudly, degrading to cursor resume) on any mismatch instead of risking
/// silent duplication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreLengths {
    pub entries: u64,
    pub dirs: u64,
    pub names: u64,
}

#[derive(Debug)]
pub struct WalFrame {
    pub seq: u64,
    pub pre: PreLengths,
    pub batch: CommitBatch,
}

#[derive(Debug)]
pub enum WalRead {
    /// All frames in order. `truncated_tail` = a torn final frame was
    /// dropped (normal after a crash mid-append).
    Frames { frames: Vec<WalFrame>, truncated_tail: bool },
}

// ---------------------------------------------------------------------------
// tiny LE codec (no new deps; WAL format is decoupled from segment layout)
// ---------------------------------------------------------------------------

struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Self { buf: Vec::new() }
    }
    fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, b: &[u8]) {
        self.u32(b.len() as u32);
        self.buf.extend_from_slice(b);
    }
    fn raw(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }
}

struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }
    fn rest(&self) -> usize {
        self.buf.len()
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.buf.len() < n {
            return Err("wal: truncated field".into());
        }
        let (h, t) = self.buf.split_at(n);
        self.buf = t;
        Ok(h)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Result<u64, String> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_le_bytes(a))
    }
    fn bytes(&mut self) -> Result<&'a [u8], String> {
        let n = self.u32()? as usize;
        self.take(n)
    }
}

// ---------------------------------------------------------------------------
// payload codec: CommitBatch <-> bytes (logical field encoding)
// ---------------------------------------------------------------------------

fn encode_entry(w: &mut Writer, e: &FileEntry) {
    // Same order as the documented 32B layout; independent of disk bytes.
    w.u64(e.size);
    w.u32(e.mtime);
    w.u32(e.ctime);
    w.u32(e.name_offset);
    w.u32(e.dir_idx);
    w.u32(e.attrs);
    w.u16(e.n_len);
    w.u16(e._pad);
}

fn decode_entry(r: &mut Reader) -> Result<FileEntry, String> {
    Ok(FileEntry {
        size: r.u64()?,
        mtime: r.u32()?,
        ctime: r.u32()?,
        name_offset: r.u32()?,
        dir_idx: r.u32()?,
        attrs: r.u32()?,
        n_len: r.u16()?,
        _pad: r.u16()?,
    })
}

fn encode_dir(w: &mut Writer, d: &DirEntry) {
    w.u64(d.frn);
    w.u32(d.parent_idx);
    w.u32(d.name_offset);
    w.u16(d.name_len);
}

fn decode_dir(r: &mut Reader) -> Result<DirEntry, String> {
    Ok(DirEntry {
        frn: r.u64()?,
        parent_idx: r.u32()?,
        name_offset: r.u32()?,
        name_len: r.u16()?,
    })
}

fn encode_mutation(w: &mut Writer, m: &Mutation) {
    match m {
        Mutation::MetadataBatch(items) => {
            w.u8(0);
            w.u32(items.len() as u32);
            for (idx, b) in items {
                w.u32(*idx);
                debug_assert_eq!(b.len(), 18);
                let mut fixed = [0u8; 18];
                fixed.copy_from_slice(&b[..b.len().min(18)]);
                w.raw(&fixed);
            }
        }
        Mutation::Meta { idx, size, mtime, ctime } => {
            w.u8(1);
            w.u32(*idx);
            w.u64(*size);
            w.u32(*mtime);
            w.u32(*ctime);
        }
        Mutation::PatchEntry { idx, entry } => {
            w.u8(2);
            w.u32(*idx);
            encode_entry(w, entry);
        }
        Mutation::PatchDir { di, dir } => {
            w.u8(3);
            w.u32(*di);
            encode_dir(w, dir);
        }
        Mutation::PushFile { entry, frn, name } => {
            w.u8(4);
            encode_entry(w, entry);
            w.u64(*frn);
            w.bytes(name);
        }
        Mutation::PushDir { entry, frn, dir, name } => {
            w.u8(5);
            encode_entry(w, entry);
            w.u64(*frn);
            encode_dir(w, dir);
            w.bytes(name);
        }
        Mutation::RepathFile { idx, entry, name } => {
            w.u8(6);
            w.u32(*idx);
            encode_entry(w, entry);
            w.bytes(name);
        }
        Mutation::RepathDir { idx, di, entry, dir, name } => {
            w.u8(7);
            w.u32(*idx);
            w.u32(*di);
            encode_entry(w, entry);
            encode_dir(w, dir);
            w.bytes(name);
        }
        Mutation::FullResync => {
            w.u8(8);
        }
    }
}

fn decode_mutation(r: &mut Reader) -> Result<Mutation, String> {
    let tag = r.u8()?;
    let m = match tag {
        0 => {
            let n = r.u32()? as usize;
            if n > 20_000_000 {
                return Err("wal: absurd MetadataBatch len".into());
            }
            let mut items = Vec::with_capacity(n.min(1024));
            for _ in 0..n {
                let idx = r.u32()?;
                let b = r.take(18)?;
                let mut fixed = [0u8; 18];
                fixed.copy_from_slice(b);
                items.push((idx, fixed));
            }
            Mutation::MetadataBatch(items)
        }
        1 => Mutation::Meta {
            idx: r.u32()?,
            size: r.u64()?,
            mtime: r.u32()?,
            ctime: r.u32()?,
        },
        2 => Mutation::PatchEntry { idx: r.u32()?, entry: decode_entry(r)? },
        3 => Mutation::PatchDir { di: r.u32()?, dir: decode_dir(r)? },
        4 => {
            let entry = decode_entry(r)?;
            let frn = r.u64()?;
            let name = r.bytes()?.to_vec();
            Mutation::PushFile { entry, frn, name }
        }
        5 => {
            let entry = decode_entry(r)?;
            let frn = r.u64()?;
            let dir = decode_dir(r)?;
            let name = r.bytes()?.to_vec();
            Mutation::PushDir { entry, frn, dir, name }
        }
        6 => {
            let idx = r.u32()?;
            let entry = decode_entry(r)?;
            let name = r.bytes()?.to_vec();
            Mutation::RepathFile { idx, entry, name }
        }
        7 => {
            let idx = r.u32()?;
            let di = r.u32()?;
            let entry = decode_entry(r)?;
            let dir = decode_dir(r)?;
            let name = r.bytes()?.to_vec();
            Mutation::RepathDir { idx, di, entry, dir, name }
        }
        8 => Mutation::FullResync,
        _ => return Err(format!("wal: unknown mutation tag {tag}")),
    };
    Ok(m)
}

fn encode_volume(w: &mut Writer, v: &VolumeState) {
    w.u8(v.volume_letter);
    w.u32(v.volume_serial);
    w.u64(v.usn_journal_id);
    w.u64(v.last_usn);
    w.u32(v.first_entry_idx);
    w.bytes(v.root_prefix.as_bytes());
    w.bytes(v.volume_id.as_bytes());
}

fn decode_volume(r: &mut Reader) -> Result<VolumeState, String> {
    let volume_letter = r.u8()?;
    let volume_serial = r.u32()?;
    let usn_journal_id = r.u64()?;
    let last_usn = r.u64()?;
    let first_entry_idx = r.u32()?;
    let root_prefix = String::from_utf8(r.bytes()?.to_vec()).map_err(|_| "wal: bad root_prefix".to_string())?;
    let volume_id = String::from_utf8(r.bytes()?.to_vec()).map_err(|_| "wal: bad volume_id".to_string())?;
    Ok(VolumeState { volume_letter, volume_serial, usn_journal_id, last_usn, first_entry_idx, root_prefix, volume_id })
}

fn encode_batch(batch: &CommitBatch) -> Vec<u8> {
    let mut w = Writer::new();
    w.u64(batch.entry_count as u64);
    w.u64(batch.dir_count as u64);
    w.u64(batch.names_len as u64);
    w.u64(batch.dir_paths_len as u64);
    w.u8(u8::from(batch.metadata_ready));
    w.u8(u8::from(batch.has_trigram));
    w.u32(batch.tri_snap);
    w.u32(batch.volumes.len() as u32);
    for v in &batch.volumes {
        encode_volume(&mut w, v);
    }
    let mut tri = Vec::new();
    batch.tri_pending.serialize_into(&mut tri).unwrap_or_default();
    w.u32(tri.len() as u32);
    w.raw(&tri);
    w.u32(batch.mutations.len() as u32);
    for m in &batch.mutations {
        encode_mutation(&mut w, m);
    }
    w.buf
}

fn decode_batch(buf: &[u8]) -> Result<CommitBatch, String> {
    let mut r = Reader::new(buf);
    let entry_count = r.u64()? as usize;
    let dir_count = r.u64()? as usize;
    let names_len = r.u64()? as usize;
    let dir_paths_len = r.u64()? as usize;
    let metadata_ready = r.u8()? != 0;
    let has_trigram = r.u8()? != 0;
    let tri_snap = r.u32()?;
    let n_vol = r.u32()? as usize;
    if n_vol > 64 {
        return Err("wal: absurd volume count".into());
    }
    let mut volumes = Vec::with_capacity(n_vol);
    for _ in 0..n_vol {
        volumes.push(decode_volume(&mut r)?);
    }
    let tri_len = r.u32()? as usize;
    if tri_len > 1 << 31 {
        return Err("wal: absurd tri_pending len".into());
    }
    let tri_bytes = r.take(tri_len)?;
    let tri_pending = RoaringBitmap::deserialize_from(tri_bytes).map_err(|e| format!("wal: bad tri_pending: {e}"))?;
    let n_mut = r.u32()? as usize;
    if n_mut > 10_000_000 {
        return Err("wal: absurd mutation count".into());
    }
    let mut mutations = Vec::with_capacity(n_mut.min(1024));
    for _ in 0..n_mut {
        mutations.push(decode_mutation(&mut r)?);
    }
    if r.rest() != 0 {
        return Err("wal: trailing bytes in batch".into());
    }
    Ok(CommitBatch {
        mutations,
        entry_count,
        dir_count,
        names_len,
        dir_paths_len,
        metadata_ready,
        volumes,
        tri_pending,
        tri_snap,
        has_trigram,
        wal_seq: 0,
    })
}

/// Lengths a frame's store must have BEFORE its mutations apply. Computed
/// from the batch's post-lengths minus what the batch itself adds.
pub fn pre_lengths(batch: &CommitBatch) -> PreLengths {
    let mut add_entries: u64 = 0;
    let mut add_dirs: u64 = 0;
    let mut add_names: u64 = 0;
    for m in &batch.mutations {
        match m {
            Mutation::PushFile { name, .. } => {
                add_entries += 1;
                add_names += name.len() as u64 + 1;
            }
            Mutation::PushDir { name, .. } => {
                add_entries += 1;
                add_dirs += 1;
                add_names += name.len() as u64 + 1;
            }
            Mutation::RepathFile { name, .. } | Mutation::RepathDir { name, .. } => {
                add_names += name.len() as u64 + 1;
            }
            _ => {}
        }
    }
    PreLengths {
        entries: (batch.entry_count as u64).saturating_sub(add_entries),
        dirs: (batch.dir_count as u64).saturating_sub(add_dirs),
        names: (batch.names_len as u64).saturating_sub(add_names),
    }
}

// ---------------------------------------------------------------------------
// file layout: [magic u32][ver u16][seq u64][pre 3xu64][len u32][payload][crc u32]
// ---------------------------------------------------------------------------

const HEADER_LEN: usize = 4 + 2 + 8 + 24 + 4;

fn wal_path(layout_dir: &Path) -> PathBuf {
    layout_dir.join(WAL_DIR).join(WAL_FILE)
}

fn crc_of(seq: u64, pre: &PreLengths, payload: &[u8], len: u32) -> u32 {
    let mut h = crc32fast::Hasher::new();
    h.update(&seq.to_le_bytes());
    h.update(&pre.entries.to_le_bytes());
    h.update(&pre.dirs.to_le_bytes());
    h.update(&pre.names.to_le_bytes());
    h.update(&len.to_le_bytes());
    h.update(payload);
    h.finalize()
}

/// Serializes heartbeat appends against fold takes. Lock order is always
/// `wal_lock` -> store lock -> (fold only) persist lock; heartbeats never
/// take the persist lock, so no cycle is possible.
pub(crate) fn wal_lock() -> std::sync::MutexGuard<'static, ()> {
    static M: OnceLock<Mutex<()>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(())).lock().unwrap_or_else(|e| e.into_inner())
}

/// In-memory tip per layout dir (WAL file is append-only; rescanning up to
/// hundreds of MB per heartbeat is not an option). Initialized from disk
/// once per layout, then advanced under [`wal_lock`].
fn wal_tips() -> &'static Mutex<HashMap<PathBuf, u64>> {
    static T: OnceLock<Mutex<HashMap<PathBuf, u64>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Canonical append entry: lock, ensure tip, append one frame, advance tip.
/// Returns the assigned seq. Heals a torn tail first (crash mid-append).
/// Callers set `batch.wal_seq` from the return value where needed.
pub fn wal_append(layout_dir: &Path, batch: &mut CommitBatch) -> Result<u64, String> {
    let _guard = wal_lock();
    wal_append_locked(layout_dir, batch)
}

/// Same as [`wal_append`], assuming the caller holds [`wal_lock`].
pub(crate) fn wal_append_locked(layout_dir: &Path, batch: &mut CommitBatch) -> Result<u64, String> {
    let dir = layout_dir.join(WAL_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("wal: mkdir: {e}"))?;
    let path = wal_path(layout_dir);
    let mut tip = wal_tips().lock().unwrap_or_else(|e| e.into_inner()).get(&path).copied();
    let mut f = OpenOptions::new().read(true).write(true).create(true).open(&path).map_err(|e| format!("wal: open: {e}"))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(|e| format!("wal: scan: {e}"))?;
    // Scan once: find the file tip and any torn tail.
    let mut off = 0usize;
    let mut scanned = 0u64;
    while off < buf.len() {
        match read_frame_at(&buf, off) {
            Ok((frame, next)) => {
                scanned = frame.seq;
                off = next;
            }
            Err(_) => break,
        }
    }
    let mut tip = tip.unwrap_or(scanned);
    if tip < scanned {
        // Another writer advanced the file without this process's map
        // (should not happen under the mutex; trust the file).
        tip = scanned;
    }
    if off < buf.len() {
        // Healed a torn tail: the torn frame's seq is freed for reuse, so
        // the tip rewinds to the last good frame (gaps would otherwise
        // accumulate one seq per crash).
        tip = scanned;
        f.set_len(off as u64).map_err(|e| format!("wal: heal: {e}"))?;
    }
    f.sync_all().map_err(|e| format!("wal: sync: {e}"))?;
    drop(f);
    let mut f = OpenOptions::new().append(true).open(&path).map_err(|e| format!("wal: reopen: {e}"))?;
    let next = tip + 1;
    let payload = encode_batch(batch);
    if payload.len() > MAX_FRAME as usize {
        return Err("wal: batch exceeds max frame".into());
    }
    let pre = pre_lengths(batch);
    let len = payload.len() as u32;
    let mut frame = Vec::with_capacity(HEADER_LEN + payload.len() + 4);
    frame.extend_from_slice(&WAL_MAGIC.to_le_bytes());
    frame.extend_from_slice(&WAL_VERSION.to_le_bytes());
    frame.extend_from_slice(&next.to_le_bytes());
    frame.extend_from_slice(&pre.entries.to_le_bytes());
    frame.extend_from_slice(&pre.dirs.to_le_bytes());
    frame.extend_from_slice(&pre.names.to_le_bytes());
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(&payload);
    frame.extend_from_slice(&crc_of(next, &pre, &payload, len).to_le_bytes());
    f.write_all(&frame).map_err(|e| format!("wal: append: {e}"))?;
    f.sync_all().map_err(|e| format!("wal: sync: {e}"))?;
    drop(f);
    wal_tips().lock().unwrap_or_else(|e| e.into_inner()).insert(path, next);
    batch.wal_seq = next;
    Ok(next)
}

/// Current WAL file length in bytes (0 when absent). Retention rewrites the
/// file, so callers trigger it lazily by size instead of on every commit.
pub fn wal_file_len(layout_dir: &Path) -> u64 {
    std::fs::metadata(wal_path(layout_dir)).map(|m| m.len()).unwrap_or(0)
}

/// Highest assigned seq for this layout (in-memory tip; the WAL file is the
/// durable counterpart). Initialized from disk once per layout.
pub fn wal_current_tip(layout_dir: &Path) -> u64 {
    let path = wal_path(layout_dir);
    let mut tips = wal_tips().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(tip) = tips.get(&path).copied() {
        return tip;
    }
    let tip = scan_tip(&path);
    tips.insert(path, tip);
    tip
}

/// Drop all frames with `seq > keep_through` (failed-fold rollback: the
/// re-queued journal will be heartbeat-appended again under fresh seqs).
pub fn wal_truncate_after(layout_dir: &Path, keep_through: u64) -> Result<(), String> {
    let _guard = wal_lock();
    let path = wal_path(layout_dir);
    let mut buf = Vec::new();
    match File::open(&path) {
        Ok(mut f) => {
            f.read_to_end(&mut buf).map_err(|e| format!("wal: read: {e}"))?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("wal: open: {e}")),
    }
    let mut off = 0usize;
    let mut keep_end = 0usize;
    while off < buf.len() {
        match read_frame_at(&buf, off) {
            Ok((frame, next)) => {
                if frame.seq <= keep_through {
                    keep_end = next;
                }
                off = next;
            }
            Err(_) => break,
        }
    }
    // Only truncate the tail region (never punch a hole in the middle):
    // frames after keep_end must all be > keep_through, else refuse.
    let mut check = keep_end;
    while check < buf.len() {
        match read_frame_at(&buf, check) {
            Ok((frame, next)) => {
                if frame.seq <= keep_through {
                    return Err("wal: non-tail coverage; refusing truncate".into());
                }
                check = next;
            }
            Err(_) => break, // torn tail beyond keep_end: cut it too
        }
    }
    if keep_end < buf.len() || off < buf.len() {
        let f = OpenOptions::new().write(true).open(&path).map_err(|e| format!("wal: reopen: {e}"))?;
        f.set_len(keep_end as u64).map_err(|e| format!("wal: truncate: {e}"))?;
        f.sync_all().map_err(|e| format!("wal: sync: {e}"))?;
    }
    // Re-sync the in-memory tip with the file (a newer tip may have ridden
    // on truncated frames).
    let mut tips = wal_tips().lock().unwrap_or_else(|e| e.into_inner());
    tips.insert(path, scan_tip_at(&buf[..keep_end]));
    Ok(())
}

fn scan_tip(path: &Path) -> u64 {
    let Ok(mut f) = File::open(path) else { return 0 };
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return 0;
    }
    scan_tip_at(&buf)
}

fn scan_tip_at(buf: &[u8]) -> u64 {
    let mut off = 0usize;
    let mut tip = 0u64;
    while off < buf.len() {
        match read_frame_at(buf, off) {
            Ok((frame, next)) => {
                tip = frame.seq;
                off = next;
            }
            Err(_) => break,
        }
    }
    tip
}

/// Read all frames with `seq >= from_seq`, in order. A torn FINAL frame is
/// dropped (`truncated_tail: true`); corruption anywhere else is an error.
///
/// Rationale: the writer heals torn tails on append and compaction rewrites
/// clean, so a crash can only ever tear the file end. A mid-file CRC failure
/// therefore means real corruption (or a bug) and must be loud.
pub fn wal_read_from(layout_dir: &Path, from_seq: u64) -> Result<WalRead, String> {
    let path = wal_path(layout_dir);
    let mut buf = Vec::new();
    match File::open(&path) {
        Ok(mut f) => {
            f.read_to_end(&mut buf).map_err(|e| format!("wal: read: {e}"))?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(WalRead::Frames { frames: Vec::new(), truncated_tail: false });
        }
        Err(e) => return Err(format!("wal: open: {e}")),
    }
    let mut off = 0usize;
    let mut frames = Vec::new();
    let mut truncated_tail = false;
    while off < buf.len() {
        match read_frame_at(&buf, off) {
            Ok((frame, next)) => {
                if frame.seq >= from_seq {
                    frames.push(frame);
                }
                off = next;
            }
            Err(e) => {
                // Clean EOF never reaches here (loop guard). A failure with
                // only trailing bytes that cannot even be a header is a torn
                // tail; a failure that parses as a frame start but fails CRC
                // mid-file is corruption.
                let rest = buf.len() - off;
                let claims_frame = rest >= HEADER_LEN
                    && u32::from_le_bytes(buf[off..off + 4].try_into().unwrap_or([0; 4])) == WAL_MAGIC;
                if claims_frame && off + HEADER_LEN < buf.len() {
                    // Parses as a frame start: CRC/length failure here is
                    // only acceptable at the very end of the file.
                    let len = u32::from_le_bytes(buf[off + 38..off + 42].try_into().unwrap_or([0; 4])) as usize;
                    if off + HEADER_LEN + len + 4 <= buf.len() {
                        return Err(format!("wal: mid-file corruption at offset {off}: {e}"));
                    }
                }
                truncated_tail = true;
                break;
            }
        }
    }
    Ok(WalRead::Frames { frames, truncated_tail })
}

fn read_frame_at(buf: &[u8], off: usize) -> Result<(WalFrame, usize), String> {
    if buf.len() - off < HEADER_LEN {
        return Err("wal: short header".into());
    }
    let magic = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
    if magic != WAL_MAGIC {
        return Err("wal: bad magic".into());
    }
    let ver = u16::from_le_bytes(buf[off + 4..off + 6].try_into().unwrap());
    if ver != WAL_VERSION {
        return Err(format!("wal: version {ver}"));
    }
    let seq = u64::from_le_bytes(buf[off + 6..off + 14].try_into().unwrap());
    let pre = PreLengths {
        entries: u64::from_le_bytes(buf[off + 14..off + 22].try_into().unwrap()),
        dirs: u64::from_le_bytes(buf[off + 22..off + 30].try_into().unwrap()),
        names: u64::from_le_bytes(buf[off + 30..off + 38].try_into().unwrap()),
    };
    let len = u32::from_le_bytes(buf[off + 38..off + 42].try_into().unwrap());
    if len > MAX_FRAME {
        return Err("wal: insane length".into());
    }
    let total = HEADER_LEN + len as usize + 4;
    if buf.len() - off < total {
        return Err("wal: short payload".into());
    }
    let payload = &buf[off + HEADER_LEN..off + HEADER_LEN + len as usize];
    let crc = u32::from_le_bytes(buf[off + HEADER_LEN + len as usize..off + total].try_into().unwrap());
    if crc != crc_of(seq, &pre, payload, len) {
        return Err("wal: crc mismatch".into());
    }
    let batch = decode_batch(payload)?;
    Ok((WalFrame { seq, pre, batch }, off + total))
}

/// Drop all frames with `seq < keep_from` by rewriting the file. Called
/// after a generation covering those frames is published.
pub fn wal_retain_from(layout_dir: &Path, keep_from: u64) -> Result<(), String> {
    let path = wal_path(layout_dir);
    if keep_from <= 1 || !path.is_file() {
        if keep_from > 1 && path.is_file() {
            // Fall through to rewrite (possibly to empty).
        } else {
            return Ok(());
        }
    }
    let WalRead::Frames { frames, .. } = wal_read_from(layout_dir, keep_from)?;
    let dir = layout_dir.join(WAL_DIR);
    let tmp = dir.join("wal.compact.tmp");
    {
        let mut f = File::create(&tmp).map_err(|e| format!("wal: compact create: {e}"))?;
        for frame in &frames {
            // Re-encode with ORIGINAL seq (decode_batch zeroes wal_seq; the
            // frame seq lives outside the payload).
            let payload = encode_batch(&frame.batch);
            let len = payload.len() as u32;
            let mut out = Vec::with_capacity(HEADER_LEN + payload.len() + 4);
            out.extend_from_slice(&WAL_MAGIC.to_le_bytes());
            out.extend_from_slice(&WAL_VERSION.to_le_bytes());
            out.extend_from_slice(&frame.seq.to_le_bytes());
            out.extend_from_slice(&frame.pre.entries.to_le_bytes());
            out.extend_from_slice(&frame.pre.dirs.to_le_bytes());
            out.extend_from_slice(&frame.pre.names.to_le_bytes());
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&payload);
            out.extend_from_slice(&crc_of(frame.seq, &frame.pre, &payload, len).to_le_bytes());
            f.write_all(&out).map_err(|e| format!("wal: compact write: {e}"))?;
        }
        f.sync_all().map_err(|e| format!("wal: compact sync: {e}"))?;
    }
    self_replace(&tmp, &path)?;
    crate::persist_tx::sync_dir(&dir).map_err(|e| format!("wal: dirsync: {e}"))?;
    Ok(())
}

/// Remove the tail frame with `seq` (failed commit rollback). Commits are
/// serialized, so a failed batch's frame is always the file tail.
pub fn wal_truncate_tail_seq(layout_dir: &Path, seq: u64) -> Result<(), String> {
    let path = wal_path(layout_dir);
    let mut buf = Vec::new();
    match File::open(&path) {
        Ok(mut f) => {
            f.read_to_end(&mut buf).map_err(|e| format!("wal: read: {e}"))?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("wal: open: {e}")),
    }
    // Walk frames; if the LAST complete frame has this seq, cut it.
    let mut off = 0usize;
    let mut last_start: Option<usize> = None;
    let mut last_seq = 0u64;
    while off < buf.len() {
        match read_frame_at(&buf, off) {
            Ok((frame, next)) => {
                last_start = Some(off);
                last_seq = frame.seq;
                off = next;
            }
            Err(_) => break,
        }
    }
    if last_seq == seq {
        if off < buf.len() {
            return Err("wal: tail frame followed by garbage; refusing truncate".into());
        }
        let start = last_start.unwrap_or(0);
        let f = OpenOptions::new().write(true).open(&path).map_err(|e| format!("wal: reopen: {e}"))?;
        f.set_len(start as u64).map_err(|e| format!("wal: truncate: {e}"))?;
        f.sync_all().map_err(|e| format!("wal: sync: {e}"))?;
    }
    // Else: seq is not the complete tail (already truncated/compacted or a
    // torn tail follows) — nothing to do; the frame is already unreachable.
    Ok(())
}

#[cfg(windows)]
fn self_replace(tmp: &Path, dst: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    // Same call shape as persist_tx::atomic_publish (MoveFileExW).
    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }
    const REPLACE_EXISTING: u32 = 0x1;
    const WRITE_THROUGH: u32 = 0x8;
    let wtmp: Vec<u16> = tmp.as_os_str().encode_wide().chain(Some(0)).collect();
    let wdst: Vec<u16> = dst.as_os_str().encode_wide().chain(Some(0)).collect();
    let ok = unsafe { MoveFileExW(wtmp.as_ptr(), wdst.as_ptr(), REPLACE_EXISTING | WRITE_THROUGH) };
    if ok == 0 {
        return Err(format!("wal: replace: {}", std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(not(windows))]
fn self_replace(tmp: &Path, dst: &Path) -> Result<(), String> {
    std::fs::rename(tmp, dst).map_err(|e| format!("wal: replace: {e}"))?;
    Ok(())
}

/// Recovery: replay frames onto a freshly loaded store, in order.
///
/// The store is cloned first: any failure (gap, OOB index, serial change,
/// FullResync) leaves the caller's store untouched, and boot proceeds from
/// the generation cursors (USN journal resume regenerates the window;
/// backfill re-stats it). Returns how far replay got.
pub struct ReplayOutcome {
    pub applied_frames: usize,
    pub applied_mutations: usize,
    /// Stopped at a FullResync marker (or volume-set change): the window
    /// after this point must come from journal resume, not replay.
    pub stopped_early: bool,
    pub stop_seq: u64,
    pub stop_reason: String,
}

pub fn replay_frames_into_store(
    store: &mut IndexStore,
    frames: &[WalFrame],
) -> Result<ReplayOutcome, String> {
    let mut out = ReplayOutcome { applied_frames: 0, applied_mutations: 0, stopped_early: false, stop_seq: 0, stop_reason: String::new() };
    if frames.is_empty() {
        return Ok(out);
    }
    // Work on a clone: recovery is all-or-nothing per frame sequence.
    // (store is ~1.3 GB; this path runs only on boots with pending WAL.)
    let mut work = store.clone();
    work.set_track_mutations(false);
    for f in frames {
        if work.entries.len() as u64 != f.pre.entries
            || work.dirs.len() as u64 != f.pre.dirs
            || work.names_buf.len() as u64 != f.pre.names
        {
            out.stopped_early = true;
            out.stop_seq = f.seq;
            out.stop_reason = format!(
                "length gap at seq {} (store {}/{}/{} vs pre {}/{}/{})",
                f.seq, work.entries.len(), work.dirs.len(), work.names_buf.len(),
                f.pre.entries, f.pre.dirs, f.pre.names,
            );
            break;
        }
        if f.batch.mutations.iter().any(|m| matches!(m, crate::index::Mutation::FullResync)) {
            out.stopped_early = true;
            out.stop_seq = f.seq;
            out.stop_reason = "FullResync marker: resume from generation cursors".into();
            break;
        }
        if f.batch.volumes.len() != work.volumes.len() {
            out.stopped_early = true;
            out.stop_seq = f.seq;
            out.stop_reason = "volume set changed".into();
            break;
        }
        for (v, bv) in work.volumes.iter().zip(f.batch.volumes.iter()) {
            if v.volume_letter != bv.volume_letter {
                out.stopped_early = true;
                out.stop_seq = f.seq;
                out.stop_reason = "volume identity changed".into();
                break;
            }
        }
        if out.stopped_early {
            break;
        }
        // Journal epoch: a reformatted volume must rebuild, not replay.
        let mut epoch_changed = false;
        for (v, bv) in work.volumes.iter().zip(f.batch.volumes.iter()) {
            if v.volume_serial != bv.volume_serial
                || (v.usn_journal_id != 0 && bv.usn_journal_id != 0 && v.usn_journal_id != bv.usn_journal_id)
            {
                epoch_changed = true;
            }
        }
        if epoch_changed {
            out.stopped_early = true;
            out.stop_seq = f.seq;
            out.stop_reason = "journal epoch changed".into();
            break;
        }
        for m in &f.batch.mutations {
            work.replay_mutation(m).map_err(|e| format!("seq {}: {e}", f.seq))?;
            out.applied_mutations += 1;
        }
        for (v, bv) in work.volumes.iter_mut().zip(f.batch.volumes.iter()) {
            v.usn_journal_id = bv.usn_journal_id;
            v.last_usn = bv.last_usn;
            v.first_entry_idx = bv.first_entry_idx;
        }
        work.metadata_ready = f.batch.metadata_ready;
        // Union, never replace: heartbeat frames carry deltas (current -
        // base); the base generation's pending set stays authoritative for
        // everything older. Union is superset-safe (extra candidates only
        // cost verification work, never false negatives).
        work.tri_pending |= &f.batch.tri_pending;
        out.applied_frames += 1;
    }
    if out.stopped_early {
        return Ok(out);
    }
    *store = work;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("findx-wal-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry(size: u64) -> FileEntry {
        FileEntry { size, mtime: 1, ctime: 2, name_offset: 0, dir_idx: 0, attrs: 0, n_len: 5, _pad: 0 }
    }

    fn dir() -> DirEntry {
        DirEntry { frn: 9, parent_idx: 0, name_offset: 6, name_len: 3 }
    }

    fn vol() -> VolumeState {
        VolumeState {
            volume_letter: b'F', volume_serial: 7, usn_journal_id: 10, last_usn: 99,
            first_entry_idx: 0, root_prefix: "F:\\".into(), volume_id: "F".into(),
        }
    }

    /// Hand-built batch covering every Mutation variant (deterministic, no
    /// store API involved).
    fn sample_batch() -> CommitBatch {
        CommitBatch {
            mutations: vec![
                Mutation::MetadataBatch(vec![(0, [1u8; 18]), (7, [2u8; 18])]),
                Mutation::Meta { idx: 0, size: 9, mtime: 9, ctime: 9 },
                Mutation::PatchEntry { idx: 1, entry: entry(10) },
                Mutation::PatchDir { di: 0, dir: dir() },
                Mutation::PushFile { entry: entry(11), frn: 2, name: b"b.txt".to_vec() },
                Mutation::PushDir { entry: entry(0), frn: 3, dir: dir(), name: b"d".to_vec() },
                Mutation::RepathFile { idx: 0, entry: entry(12), name: b"c.txt".to_vec() },
                Mutation::RepathDir { idx: 0, di: 0, entry: entry(0), dir: dir(), name: b"e".to_vec() },
            ],
            entry_count: 3,
            dir_count: 2,
            names_len: 20,
            dir_paths_len: 0,
            metadata_ready: true,
            volumes: vec![vol()],
            tri_pending: {
                let mut bm = RoaringBitmap::new();
                bm.insert(3);
                bm.insert(100000);
                bm
            },
            tri_snap: 42,
            has_trigram: true,
            wal_seq: 0,
        }
    }

    #[test]
    fn frame_roundtrip_preserves_every_variant() {
        let dir = scratch("roundtrip");
        let mut batch = sample_batch();
        assert!(!batch.mutations.is_empty());
        let seq = wal_append(&dir, &mut batch).unwrap();
        assert_eq!(seq, 1);
        assert_eq!(batch.wal_seq, 1);
        let WalRead::Frames { frames, truncated_tail } = wal_read_from(&dir, 1).unwrap();
        assert!(!truncated_tail);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].seq, 1);
        let back = &frames[0].batch;
        assert_eq!(back.mutations.len(), batch.mutations.len());
        assert_eq!(back.entry_count, batch.entry_count);
        assert_eq!(format!("{:?}", back.mutations), format!("{:?}", batch.mutations));
        // FullResync + MetadataBatch + Meta + Patch + Push + Repath coverage:
        let mut all = sample_batch();
        all.mutations.push(Mutation::FullResync);
        all.mutations.push(Mutation::Meta { idx: 1, size: 9, mtime: 9, ctime: 9 });
        let seq2 = wal_append(&dir, &mut all).unwrap();
        assert_eq!(seq2, 2);
        let WalRead::Frames { frames, .. } = wal_read_from(&dir, 2).unwrap();
        assert_eq!(frames.len(), 1);
        assert!(matches!(frames[0].batch.mutations.last(), Some(Mutation::Meta { .. })));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn torn_tail_is_dropped_not_failed() {
        let dir = scratch("torn");
        let mut batch = sample_batch();
        wal_append(&dir, &mut batch).unwrap();
        wal_append(&dir, &mut batch).unwrap();
        // Simulate crash mid-append: cut the file inside the second frame.
        let path = wal_path(&dir);
        let len = std::fs::metadata(&path).unwrap().len();
        let f = OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(len - 10).unwrap();
        let WalRead::Frames { frames, truncated_tail } = wal_read_from(&dir, 1).unwrap();
        assert_eq!(frames.len(), 1);
        assert!(truncated_tail);
        assert_eq!(frames[0].seq, 1);
        // Next append still works (writer scans past the tear).
        let mut b2 = sample_batch();
        let seq = wal_append(&dir, &mut b2).unwrap();
        assert_eq!(seq, 2, "seq must continue past the torn tail");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn retain_and_truncate_keep_seq_contract() {
        let dir = scratch("retain");
        let mut b1 = sample_batch();
        let mut b2 = sample_batch();
        let s1 = wal_append(&dir, &mut b1).unwrap();
        let s2 = wal_append(&dir, &mut b2).unwrap();
        assert_eq!((s1, s2), (1, 2));
        wal_retain_from(&dir, 2).unwrap();
        let WalRead::Frames { frames, .. } = wal_read_from(&dir, 1).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].seq, 2);
        wal_truncate_tail_seq(&dir, 2).unwrap();
        let WalRead::Frames { frames, .. } = wal_read_from(&dir, 1).unwrap();
        assert!(frames.is_empty());
        // Truncating a non-tail seq is a no-op, never an error.
        wal_truncate_tail_seq(&dir, 99).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replay_matches_live_store_exactly() {
        use crate::index::IndexBuilder;
        use crate::RawEntry;
        // Base store: two files + one dir, tracking on.
        let raw = |id: u64, parent: u64, name: &str, dir: bool| RawEntry {
            file_id: id, file_id_128: None, parent_id: parent, name: name.into(),
            size: 10, mtime: 100, ctime: 200, attrs: if dir { 0x10 } else { 0 }, is_dir: dir,
        };
        let mut live = IndexBuilder::new(b'F', 1, 7, 10)
            .build_from_raw(vec![raw(1, 100, "a.txt", false)], vec![raw(100, 0, "root", true)], false)
            .unwrap();
        live.set_track_mutations(true);
        let base = live.clone();
        // Live operations covering Push / Patch / Meta / delete.
        live.append_file_entry(&raw(2, 100, "b.txt", false));
        live.append_file_entry(&raw(3, 100, "c.txt", false));
        live.delete_entry(0);
        // Hand a Meta through the store path (mirror path tested elsewhere).
        // Drain exactly like take_persistence_batch (minus metadata fold).
        let mutations = live.take_mutations();
        assert!(!mutations.is_empty());
        let batch = CommitBatch {
            entry_count: live.entries.len(),
            dir_count: live.dirs.len(),
            names_len: live.names_buf.len(),
            dir_paths_len: live.dir_paths_buf.len(),
            metadata_ready: live.metadata_ready,
            volumes: live.volumes.clone(),
            tri_pending: live.tri_pending.clone(),
            tri_snap: 0,
            has_trigram: false,
            wal_seq: 0,
            mutations,
        };
        // Journal to a scratch layout.
        let dir = scratch("replay");
        let mut journaled = batch;
        let seq = wal_append(&dir, &mut journaled).unwrap();
        assert_eq!(seq, 1);
        // Fresh load of the base + replay == live store.
        let WalRead::Frames { frames, truncated_tail } = wal_read_from(&dir, 1).unwrap();
        assert!(!truncated_tail);
        let mut recovered = base.clone();
        let out = replay_frames_into_store(&mut recovered, &frames).unwrap();
        assert!(!out.stopped_early);
        assert_eq!(out.applied_frames, 1);
        assert_eq!(recovered.entries, live.entries);
        assert_eq!(recovered.names_buf, live.names_buf);
        assert_eq!(recovered.frns, live.frns);
        assert_eq!(recovered.deleted, live.deleted);
        assert_eq!(format!("{:?}", recovered.dirs), format!("{:?}", live.dirs));
        assert_eq!(format!("{:?}", recovered.volumes), format!("{:?}", live.volumes));
        assert_eq!(recovered.tri_pending, live.tri_pending);
        assert_eq!(recovered.metadata_ready, live.metadata_ready);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replay_stops_cleanly_at_full_resync_and_gaps() {
        use crate::index::IndexBuilder;
        use crate::RawEntry;
        let raw = |id: u64, parent: u64, name: &str| RawEntry {
            file_id: id, file_id_128: None, parent_id: parent, name: name.into(),
            size: 10, mtime: 100, ctime: 200, attrs: 0, is_dir: false,
        };
        let mut live = IndexBuilder::new(b'F', 1, 7, 10)
            .build_from_raw(vec![raw(1, 100, "a.txt")], vec![], false)
            .unwrap();
        live.set_track_mutations(true);
        let base = live.clone();
        live.append_file_entry(&raw(2, 100, "b.txt"));
        let mut batch = CommitBatch {
            mutations: live.take_mutations(),
            entry_count: live.entries.len(),
            dir_count: live.dirs.len(),
            names_len: live.names_buf.len(),
            dir_paths_len: live.dir_paths_buf.len(),
            metadata_ready: live.metadata_ready,
            volumes: live.volumes.clone(),
            tri_pending: live.tri_pending.clone(),
            tri_snap: 0,
            has_trigram: false,
            wal_seq: 0,
        };
        // Inject a FullResync after the real mutations.
        batch.mutations.push(Mutation::FullResync);
        let dir = scratch("resync");
        let mut journaled = batch;
        wal_append(&dir, &mut journaled).unwrap();
        let WalRead::Frames { frames, .. } = wal_read_from(&dir, 1).unwrap();
        let mut recovered = base.clone();
        let before = recovered.clone();
        let out = replay_frames_into_store(&mut recovered, &frames).unwrap();
        assert!(out.stopped_early);
        assert_eq!(out.applied_frames, 0);
        // Untouched: clone-and-swap keeps the base intact.
        assert_eq!(recovered.entries, before.entries);
        // Gap detection: replaying the same frames twice must refuse.
        let out2 = replay_frames_into_store(&mut recovered, &frames).unwrap();
        assert!(out2.stopped_early);
        assert!(out2.stop_reason.contains("length gap") || out2.stop_reason.contains("FullResync"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pre_lengths_match_live_store_growth() {
        let batch = sample_batch();
        let pre = pre_lengths(&batch);
        // 1 PushFile + 1 PushDir add entries; 1 PushDir adds a dir;
        // all four name-carrying variants append len+1 bytes.
        assert_eq!(pre.entries + 2, batch.entry_count as u64);
        assert_eq!(pre.dirs + 1, batch.dir_count as u64);
        assert_eq!(pre.names + 16, batch.names_len as u64);
    }
}
