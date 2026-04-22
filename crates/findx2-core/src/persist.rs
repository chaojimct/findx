//! `index.bin` 持久化。
//!
//! v4 关键变更（同时修一个存量 UB / 数据丢失 bug）：
//!
//! - **`FileEntry` 字段级 LE 编码**：`std::mem::size_of::<FileEntry>() == 48`（含 align/tail padding），
//!   旧 v3 通过 `*(e as *const FileEntry as *const [u8; 40])` 取头 40 字节写盘，
//!   导致 `attrs` 字段从来没被持久化（所有 v3 索引加载后 `is_dir_entry()` 恒为 false）。
//!   v4 改为按字段独立 `to_le_bytes`，attrs 真正进盘。
//! - **去掉 `size_order/mtime_order/ctime_order` 三段占位向量**：每段 = `entry_count × u32`，
//!   排序键由 `SearchEngine::finalize_hits` 现算，无需持久化。
//! - 写入用 4MB `BufWriter` + `sync_data`，读取用 4MB `BufReader`，
//!   避免百万条目下逐字段 syscall 风暴。
//!
//! v2/v3 仍可加载（兼容路径）：
//! - v3：entries 段按 40 字节字段级 LE 解析，attrs 用 0 占位，
//!   加载收尾扫 `dirs` 表回填 `ATTR_IS_DIR | ext_hash<<8`，业务逻辑不丢。
//! - v3：跳过尾部三段 `*_order`。
//! - 下次 `save_index_bin` 自动升级到 v4。

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use roaring::RoaringBitmap;

use crate::index::{hash_ext8, DirEntry, FileEntry, IndexStore, VolumeState};
use crate::Result;

const MAGIC: u32 = 0x4644_5832; // "FDX2" 小端

pub struct IndexHeader {
    pub version: u32,
    pub entry_count: u64,
    pub dir_count: u64,
    pub names_buf_len: u64,
    pub flags: u32,
}

impl IndexHeader {
    fn serialize(&self, volumes_len: usize) -> [u8; 64] {
        let mut b = [0u8; 64];
        b[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        b[4..8].copy_from_slice(&self.version.to_le_bytes());
        b[8..16].copy_from_slice(&self.entry_count.to_le_bytes());
        b[16..24].copy_from_slice(&self.dir_count.to_le_bytes());
        b[24..32].copy_from_slice(&self.names_buf_len.to_le_bytes());
        b[32..36].copy_from_slice(&self.flags.to_le_bytes());
        b[36..40].copy_from_slice(&(volumes_len as u32).to_le_bytes());
        b
    }
    fn read(h: &[u8; 64]) -> Result<(Self, u32)> {
        let magic = u32::from_le_bytes(h[0..4].try_into().unwrap());
        if magic != MAGIC {
            return Err(crate::Error::Persist("魔数不匹配，非 findx2 索引".into()));
        }
        Ok((
            Self {
                version: u32::from_le_bytes(h[4..8].try_into().unwrap()),
                entry_count: u64::from_le_bytes(h[8..16].try_into().unwrap()),
                dir_count: u64::from_le_bytes(h[16..24].try_into().unwrap()),
                names_buf_len: u64::from_le_bytes(h[24..32].try_into().unwrap()),
                flags: u32::from_le_bytes(h[32..36].try_into().unwrap()),
            },
            u32::from_le_bytes(h[36..40].try_into().unwrap()),
        ))
    }
}

fn serialize_volume(v: &VolumeState) -> [u8; 32] {
    let mut o = [0u8; 32];
    o[0] = v.volume_letter;
    o[4..8].copy_from_slice(&v.volume_serial.to_le_bytes());
    o[8..16].copy_from_slice(&v.usn_journal_id.to_le_bytes());
    o[16..24].copy_from_slice(&v.last_usn.to_le_bytes());
    o[24..28].copy_from_slice(&v.first_entry_idx.to_le_bytes());
    o
}

fn deserialize_volume(b: &[u8; 32]) -> VolumeState {
    VolumeState {
        volume_letter: b[0],
        volume_serial: u32::from_le_bytes(b[4..8].try_into().unwrap()),
        usn_journal_id: u64::from_le_bytes(b[8..16].try_into().unwrap()),
        last_usn: u64::from_le_bytes(b[16..24].try_into().unwrap()),
        first_entry_idx: u32::from_le_bytes(b[24..28].try_into().unwrap()),
    }
}

/// v2：含 `dir_paths_buf` / `dir_path_ranges` / `frns`，尾部三段 `*_order` 占位向量。
const FORMAT_VERSION_V2: u32 = 2;
/// v3：与 v2 字节兼容，`flags` 新增 `FLAG_METADATA_PENDING` 语义。
const FORMAT_VERSION_V3: u32 = 3;
/// v4：FileEntry 字段级 LE 编码（attrs 进盘，修复 v3 UB），尾部去掉三段 `*_order`。
const FORMAT_VERSION_V4: u32 = 4;
/// v5：FileEntry 紧凑布局（mtime/ctime u64 FILETIME → u32 unix-secs，40B → 32B）。
/// 8.5M 文件可省 ~68 MB 持久化体积，内存同样减半。
const FORMAT_VERSION_V5: u32 = 5;
/// 当前写入的版本。
const FORMAT_VERSION_CURRENT: u32 = FORMAT_VERSION_V5;
/// 快速首遍建库未完成元数据回填（`IndexStore.metadata_ready == false`）
const FLAG_METADATA_PENDING: u32 = 1;

/// FileEntry 在 v3/v4 持久化里固定 40 字节字段级 LE 布局：
/// `[0..4 name_offset][4..6 n_len][6..8 _pad][8..12 dir_idx][12..16 attrs]
///  [16..24 size][24..32 mtime u64 FILETIME][32..40 ctime u64 FILETIME]`
const FILE_ENTRY_V4_DISK_SIZE: usize = 40;

/// v5 紧凑布局，32 字节（与内存 FileEntry sizeof 一致）：
/// `[0..8 size][8..12 mtime u32 unix-secs][12..16 ctime u32 unix-secs]
///  [16..20 name_offset][20..24 dir_idx][24..28 attrs][28..30 n_len][30..32 _pad]`
const FILE_ENTRY_V5_DISK_SIZE: usize = 32;

fn file_entry_to_disk_bytes_v5(e: &FileEntry) -> [u8; FILE_ENTRY_V5_DISK_SIZE] {
    let mut b = [0u8; FILE_ENTRY_V5_DISK_SIZE];
    b[0..8].copy_from_slice(&e.size.to_le_bytes());
    b[8..12].copy_from_slice(&e.mtime.to_le_bytes());
    b[12..16].copy_from_slice(&e.ctime.to_le_bytes());
    b[16..20].copy_from_slice(&e.name_offset.to_le_bytes());
    b[20..24].copy_from_slice(&e.dir_idx.to_le_bytes());
    b[24..28].copy_from_slice(&e.attrs.to_le_bytes());
    b[28..30].copy_from_slice(&e.n_len.to_le_bytes());
    b[30..32].copy_from_slice(&e._pad.to_le_bytes());
    b
}

fn file_entry_from_disk_bytes_v5(b: &[u8; FILE_ENTRY_V5_DISK_SIZE]) -> FileEntry {
    FileEntry {
        size: u64::from_le_bytes(b[0..8].try_into().unwrap()),
        mtime: u32::from_le_bytes(b[8..12].try_into().unwrap()),
        ctime: u32::from_le_bytes(b[12..16].try_into().unwrap()),
        name_offset: u32::from_le_bytes(b[16..20].try_into().unwrap()),
        dir_idx: u32::from_le_bytes(b[20..24].try_into().unwrap()),
        attrs: u32::from_le_bytes(b[24..28].try_into().unwrap()),
        n_len: u16::from_le_bytes(b[28..30].try_into().unwrap()),
        _pad: u16::from_le_bytes(b[30..32].try_into().unwrap()),
    }
}

/// v3/v4 老格式加载：mtime/ctime 还是 u64 FILETIME，落地时转 u32 secs。
fn file_entry_from_disk_bytes_v4(b: &[u8; FILE_ENTRY_V4_DISK_SIZE], with_attrs: bool) -> FileEntry {
    let mt = u64::from_le_bytes(b[24..32].try_into().unwrap());
    let ct = u64::from_le_bytes(b[32..40].try_into().unwrap());
    FileEntry {
        size: u64::from_le_bytes(b[16..24].try_into().unwrap()),
        mtime: crate::index::filetime_to_unix_secs(mt),
        ctime: crate::index::filetime_to_unix_secs(ct),
        name_offset: u32::from_le_bytes(b[0..4].try_into().unwrap()),
        dir_idx: u32::from_le_bytes(b[8..12].try_into().unwrap()),
        // v3 该 4 字节是 align padding（写时未写入 attrs），加载时务必置 0、由 dirs 表回填。
        attrs: if with_attrs {
            u32::from_le_bytes(b[12..16].try_into().unwrap())
        } else {
            0
        },
        n_len: u16::from_le_bytes(b[4..6].try_into().unwrap()),
        _pad: u16::from_le_bytes(b[6..8].try_into().unwrap()),
    }
}

/// 与 `entries` 对齐的 FRN 表（写入时与头 `entry_count` 必须一致，避免服务加载失败）
fn frns_for_persist(store: &IndexStore) -> Vec<u64> {
    let n = store.entries.len();
    let mut frns = store.frns.clone();
    if frns.len() != n {
        frns.resize(n, 0);
    }
    frns
}

/// 写入 `index.bin`（任意 `Write`）
pub fn write_index_bin<W: Write>(w: &mut W, store: &IndexStore) -> Result<()> {
    let f = w;
    let entry_count = store.entries.len() as u64;
    let frns_aligned = frns_for_persist(store);
    debug_assert_eq!(frns_aligned.len(), entry_count as usize);
    let dir_count = store.dirs.len() as u64;
    let names_len = store.names_buf.len() as u64;
    let mut flags = 0u32;
    if !store.metadata_ready {
        flags |= FLAG_METADATA_PENDING;
    }
    let hdr = IndexHeader {
        version: FORMAT_VERSION_CURRENT,
        entry_count,
        dir_count,
        names_buf_len: names_len,
        flags,
    };
    let vol_len = store.volumes.len();
    f.write_all(&hdr.serialize(vol_len))?;

    for v in &store.volumes {
        f.write_all(&serialize_volume(v))?;
    }

    f.write_all(&store.names_buf)?;
    let dpl = store.dir_paths_buf.len() as u64;
    f.write_all(&dpl.to_le_bytes())?;
    f.write_all(&store.dir_paths_buf)?;
    let n_ranges = store.dir_path_ranges.len() as u64;
    f.write_all(&n_ranges.to_le_bytes())?;
    for (a, b) in &store.dir_path_ranges {
        f.write_all(&a.to_le_bytes())?;
        f.write_all(&b.to_le_bytes())?;
    }

    // 单次拷贝到一个 32 × N 的连续缓冲后整段 write_all，保留一次大块 I/O；不再走每条 syscall。
    let mut entry_bytes: Vec<u8> = Vec::with_capacity(store.entries.len() * FILE_ENTRY_V5_DISK_SIZE);
    for e in &store.entries {
        entry_bytes.extend_from_slice(&file_entry_to_disk_bytes_v5(e));
    }
    f.write_all(&entry_bytes)?;

    // DirEntry 24 字节带末尾 6 字节 padding；逐条写以保持与历史布局完全一致。
    for d in &store.dirs {
        f.write_all(&dir_entry_to_bytes(d))?;
    }

    let n_frn = entry_count;
    f.write_all(&n_frn.to_le_bytes())?;
    // u64 LE Vec：直接借字节视图。
    let frn_bytes: &[u8] = unsafe {
        std::slice::from_raw_parts(
            frns_aligned.as_ptr() as *const u8,
            frns_aligned.len() * std::mem::size_of::<u64>(),
        )
    };
    f.write_all(frn_bytes)?;

    for slot in &store.ext_filter {
        if let Some(bm) = slot {
            let mut v = Vec::new();
            bm.serialize_into(&mut v)
                .map_err(|e| crate::Error::Persist(e.to_string()))?;
            let len = v.len() as u32;
            f.write_all(&len.to_le_bytes())?;
            f.write_all(&v)?;
        } else {
            f.write_all(&0u32.to_le_bytes())?;
        }
    }

    let mut del = Vec::new();
    store
        .deleted
        .serialize_into(&mut del)
        .map_err(|e| crate::Error::Persist(e.to_string()))?;
    let len = del.len() as u32;
    f.write_all(&len.to_le_bytes())?;
    f.write_all(&del)?;
    Ok(())
}

/// 写入 `index.bin`（4MB BufWriter + sync_data + 原子 rename，避免崩溃 / 并发覆写产生半截文件）。
///
/// 关键修复（v0.x 之前 service 把 C/D/E 三个卷各放一个 watch 线程，每线程都对同一份 `index.bin`
/// 周期性调本函数；旧实现 `File::create(path)` 直接覆写目标，三卷同毫秒并发写会互相截断
/// 把 `index.bin` 写坏，下次启动 service 就 `failed to fill whole buffer` 而拒绝加载）：
/// 1. 先写到 `<path>.tmp.<pid>` 临时文件，flush + sync_data；
/// 2. 用 `fs::rename` 原子替换目标文件（Windows / Unix 同步语义都支持覆盖目标）。
///
/// 即便仍有多线程同时进入本函数，最终落盘的 `index.bin` 永远是某一次完整写入，不会出现半截。
/// service 端额外用 Mutex 序列化以省 I/O，但本层已经保证文件级一致性。
pub fn save_index_bin(path: &Path, store: &IndexStore) -> Result<()> {
    let started = std::time::Instant::now();
    crate::progress!(
        "持久化：开始写入 {}（条目 {}，目录 {}）…",
        path.display(),
        store.entry_count(),
        store.dirs.len()
    );

    let tmp_path = {
        let mut p = path.as_os_str().to_owned();
        p.push(format!(".tmp.{}", std::process::id()));
        std::path::PathBuf::from(p)
    };

    // 临时文件如果之前残留就先删掉，否则 create 会复用旧 inode 但内容会被截到 0 后写入。
    let _ = std::fs::remove_file(&tmp_path);

    {
        let f = File::create(&tmp_path)?;
        let mut w = BufWriter::with_capacity(4 * 1024 * 1024, f);
        write_index_bin(&mut w, store)?;
        w.flush()?;
        let f = w
            .into_inner()
            .map_err(|e| crate::Error::Persist(e.to_string()))?;
        let _ = f.sync_data();
    }

    // Windows fs::rename 在目标存在时会失败，需要先删；Unix 上 rename 自带原子覆盖。
    // 这里统一先 remove + rename，结合 service 侧 Mutex 串行化已无并发问题。
    #[cfg(windows)]
    {
        let _ = std::fs::remove_file(path);
    }
    if let Err(e) = std::fs::rename(&tmp_path, path) {
        // rename 失败时尽量保留 tmp 让运维能手工补救，并把错误透传出去。
        return Err(crate::Error::Persist(format!(
            "rename {} -> {} 失败: {e}",
            tmp_path.display(),
            path.display()
        )));
    }

    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    crate::progress!(
        "持久化：写入完成 {}，{:.1} MiB，耗时 {:.2}s",
        path.display(),
        bytes as f64 / (1024.0 * 1024.0),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn dir_entry_to_bytes(d: &DirEntry) -> [u8; 24] {
    let mut b = [0u8; 24];
    b[0..8].copy_from_slice(&d.frn.to_le_bytes());
    b[8..12].copy_from_slice(&d.parent_idx.to_le_bytes());
    b[12..16].copy_from_slice(&d.name_offset.to_le_bytes());
    b[16..18].copy_from_slice(&d.name_len.to_le_bytes());
    b
}

/// 从 `index.bin` 加载（4MB BufReader；entries / dirs / frns 大块读入）。
///
/// 兼容 v2/v3：跳过尾部三段 `*_order`；v3 entries.attrs 全 0，按 dirs 表回填 `ATTR_IS_DIR`。
pub fn load_index_bin(path: &Path) -> Result<IndexStore> {
    let started = std::time::Instant::now();
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    crate::progress!(
        "持久化：开始加载 {}（{:.1} MiB）…",
        path.display(),
        bytes as f64 / (1024.0 * 1024.0)
    );
    let file = File::open(path)?;
    let mut r = BufReader::with_capacity(4 * 1024 * 1024, file);

    let mut h = [0u8; 64];
    r.read_exact(&mut h)?;
    let (hdr, nvol) = IndexHeader::read(&h)?;
    if hdr.version > FORMAT_VERSION_CURRENT {
        return Err(crate::Error::Persist(format!(
            "索引版本 {} 高于当前可识别 {}，请升级 findx2",
            hdr.version, FORMAT_VERSION_CURRENT
        )));
    }

    let mut vbuf = vec![0u8; (nvol as usize) * 32];
    r.read_exact(&mut vbuf)?;
    let mut volumes = Vec::with_capacity(nvol as usize);
    for chunk in vbuf.chunks_exact(32) {
        let mut a = [0u8; 32];
        a.copy_from_slice(chunk);
        volumes.push(deserialize_volume(&a));
    }

    let mut names_buf = vec![0u8; hdr.names_buf_len as usize];
    r.read_exact(&mut names_buf)?;

    let dir_paths_buf: Vec<u8>;
    let dir_path_ranges: Vec<(u32, u32)>;

    if hdr.version >= FORMAT_VERSION_V2 {
        let mut dpb_len = [0u8; 8];
        r.read_exact(&mut dpb_len)?;
        let dl = u64::from_le_bytes(dpb_len) as usize;
        dir_paths_buf = read_exact_vec(&mut r, dl)?;
        let mut nr_b = [0u8; 8];
        r.read_exact(&mut nr_b)?;
        let nr = u64::from_le_bytes(nr_b) as usize;
        let mut ranges = Vec::with_capacity(nr);
        let mut pair = [0u8; 8];
        for _ in 0..nr {
            r.read_exact(&mut pair)?;
            let a = u32::from_le_bytes(pair[0..4].try_into().unwrap());
            let b = u32::from_le_bytes(pair[4..8].try_into().unwrap());
            ranges.push((a, b));
        }
        dir_path_ranges = ranges;
    } else {
        dir_paths_buf = Vec::new();
        dir_path_ranges = Vec::new();
    }

    let n_entries = hdr.entry_count as usize;
    let mut entries: Vec<FileEntry> = Vec::with_capacity(n_entries);
    if n_entries > 0 {
        if hdr.version >= FORMAT_VERSION_V5 {
            // v5 紧凑布局，32B/entry。
            let bytes_total = n_entries * FILE_ENTRY_V5_DISK_SIZE;
            let mut buf = vec![0u8; bytes_total];
            r.read_exact(&mut buf)?;
            for chunk in buf.chunks_exact(FILE_ENTRY_V5_DISK_SIZE) {
                let mut arr = [0u8; FILE_ENTRY_V5_DISK_SIZE];
                arr.copy_from_slice(chunk);
                entries.push(file_entry_from_disk_bytes_v5(&arr));
            }
        } else {
            // v3/v4 老格式，40B/entry；时间字段在 from 内做 FILETIME → unix-secs 的迁移。
            let with_attrs = hdr.version >= FORMAT_VERSION_V4;
            let bytes_total = n_entries * FILE_ENTRY_V4_DISK_SIZE;
            let mut buf = vec![0u8; bytes_total];
            r.read_exact(&mut buf)?;
            for chunk in buf.chunks_exact(FILE_ENTRY_V4_DISK_SIZE) {
                let mut arr = [0u8; FILE_ENTRY_V4_DISK_SIZE];
                arr.copy_from_slice(chunk);
                entries.push(file_entry_from_disk_bytes_v4(&arr, with_attrs));
            }
        }
    }

    let mut dirs = Vec::with_capacity(hdr.dir_count as usize);
    let mut dbuf = [0u8; 24];
    for _ in 0..hdr.dir_count {
        r.read_exact(&mut dbuf)?;
        dirs.push(DirEntry {
            frn: u64::from_le_bytes(dbuf[0..8].try_into().unwrap()),
            parent_idx: u32::from_le_bytes(dbuf[8..12].try_into().unwrap()),
            name_offset: u32::from_le_bytes(dbuf[12..16].try_into().unwrap()),
            name_len: u16::from_le_bytes(dbuf[16..18].try_into().unwrap()),
        });
    }

    let frns: Vec<u64> = if hdr.version >= FORMAT_VERSION_V2 {
        let mut nb = [0u8; 8];
        r.read_exact(&mut nb)?;
        let n_stored = u64::from_le_bytes(nb) as usize;
        const MAX_FRN: usize = 64 * 1024 * 1024;
        if n_stored > MAX_FRN {
            return Err(crate::Error::Persist("frns 计数异常（过大）".into()));
        }
        let mut v: Vec<u64> = vec![0u64; n_stored];
        if n_stored > 0 {
            // u64 LE 大块读入。
            let bytes: &mut [u8] = unsafe {
                std::slice::from_raw_parts_mut(v.as_mut_ptr() as *mut u8, n_stored * 8)
            };
            r.read_exact(bytes)?;
        }
        if v.len() != n_entries {
            if v.len() < n_entries {
                v.resize(n_entries, 0);
            } else {
                v.truncate(n_entries);
            }
        }
        v
    } else {
        vec![0u64; n_entries]
    };

    // v2/v3：跳过 size_order / mtime_order / ctime_order
    if hdr.version < FORMAT_VERSION_V4 {
        for _ in 0..3 {
            skip_u32_vec(&mut r)?;
        }
    }

    let mut ext_filter: [Option<RoaringBitmap>; 256] = std::array::from_fn(|_| None);
    let mut len_buf = [0u8; 4];
    for slot in &mut ext_filter {
        r.read_exact(&mut len_buf)?;
        let len = u32::from_le_bytes(len_buf) as usize;
        if len > 0 {
            let bm_buf = read_exact_vec(&mut r, len)?;
            *slot = Some(
                RoaringBitmap::deserialize_from(bm_buf.as_slice())
                    .map_err(|e| crate::Error::Persist(e.to_string()))?,
            );
        }
    }

    r.read_exact(&mut len_buf)?;
    let dlen = u32::from_le_bytes(len_buf) as usize;
    let del_buf = read_exact_vec(&mut r, dlen)?;
    let deleted = RoaringBitmap::deserialize_from(del_buf.as_slice())
        .map_err(|e| crate::Error::Persist(e.to_string()))?;

    // 加载阶段同样走 sorted Vec（push_unsorted + finalize_build），
    // 比 hashbrown 在 8.5M 条目下省 ~100MB RSS、且加载更快（无 hash 计算）。
    let mut dir_index: crate::index::FrnIdxMap =
        crate::index::FrnIdxMap::with_capacity(dirs.len());
    for (i, d) in dirs.iter().enumerate() {
        dir_index.push_unsorted(d.frn, i as u32);
    }
    dir_index.finalize_build();

    let mut frn_to_entry: crate::index::FrnIdxMap =
        crate::index::FrnIdxMap::with_capacity(frns.len());
    for (i, fr) in frns.iter().enumerate() {
        if *fr != 0 {
            frn_to_entry.push_unsorted(*fr, i as u32);
        }
    }
    frn_to_entry.finalize_build();

    // v3：attrs 历史从未写盘，靠 dirs 表 + ext_hash 回填，否则 `is_dir_entry()` 全 false、文件夹筛选恒为空。
    if hdr.version < FORMAT_VERSION_V4 {
        repair_attrs_v3(&mut entries, &dirs, &names_buf, &frn_to_entry, &deleted);
    } else {
        // v4/v5：历史上曾把 Windows 低字节 attrs 直接写入，`FILE_ATTRIBUTE_ARCHIVE`(0x20) 与
        // `ATTR_DELETED`(1<<5) 同位，持久化后普通文件会被 `is_deleted()` 全部滤掉。
        // 真删除以 `deleted` Roaring 位图为准（`delete_entry` 必写位图）；不在位图中的条目清除误设的 ATTR_DELETED。
        repair_attrs_deleted_bitmap_alignment(&mut entries, &deleted);
    }

    let metadata_ready = if hdr.version >= FORMAT_VERSION_V3 {
        (hdr.flags & FLAG_METADATA_PENDING) == 0
    } else {
        true
    };

    let mut store = IndexStore {
        names_buf,
        entries,
        dirs,
        dir_index,
        dir_paths_buf,
        dir_path_ranges,
        volumes,
        ext_filter,
        deleted,
        frns,
        frn_to_entry,
        metadata_ready,
        excluded_dirs: Vec::new(),
    };

    if hdr.version < FORMAT_VERSION_V2 {
        store.rebuild_dir_paths();
    }

    // 边车「排除目录」零格式破坏地随 index.bin 一起加载；GUI 改设置后直接覆写边车，下次 service 启动即生效。
    store.excluded_dirs = load_exclude_sidecar(path);
    if !store.excluded_dirs.is_empty() {
        crate::progress!(
            "持久化：已加载排除目录 {} 条（来自 {}）",
            store.excluded_dirs.len(),
            exclude_sidecar_path(path).display()
        );
    }

    crate::progress!(
        "持久化：加载完成 {}（v{}，条目 {}，目录 {}），耗时 {:.2}s",
        path.display(),
        hdr.version,
        store.entry_count(),
        store.dirs.len(),
        started.elapsed().as_secs_f64()
    );

    Ok(store)
}

/// v3 兼容修复：根据 dirs 表与 deleted bitmap 回填 entries 的 `ATTR_IS_DIR` / `ATTR_DELETED`，
/// 顺带把 `ext_hash_u8()` 高 8 位补回去（搜索可选过滤路径会用到）。
fn repair_attrs_v3(
    entries: &mut [FileEntry],
    dirs: &[DirEntry],
    names_buf: &[u8],
    frn_to_entry: &crate::index::FrnIdxMap,
    deleted: &RoaringBitmap,
) {
    for d in dirs {
        if let Some(&idx) = frn_to_entry.get(&d.frn) {
            if let Some(e) = entries.get_mut(idx as usize) {
                let nl = d.name_len as usize;
                let off = d.name_offset as usize;
                let name = std::str::from_utf8(
                    names_buf.get(off..off + nl).unwrap_or(&[]),
                )
                .unwrap_or("");
                let eh = hash_ext8(name);
                e.attrs = FileEntry::ATTR_IS_DIR | ((eh as u32) << 8);
            }
        }
    }
    for (i, e) in entries.iter_mut().enumerate() {
        if (e.attrs & FileEntry::ATTR_IS_DIR) == 0 {
            // 非目录条目：补 ext_hash（基于文件名首段或后缀）；attrs 低位标志位无来源，置 0 即可。
            let off = e.name_offset as usize;
            let nl = e.n_len as usize;
            let name = std::str::from_utf8(
                names_buf.get(off..off + nl).unwrap_or(&[]),
            )
            .unwrap_or("");
            let eh = hash_ext8(name);
            e.attrs = (eh as u32) << 8;
        }
        if deleted.contains(i as u32) {
            e.attrs |= FileEntry::ATTR_DELETED;
        }
    }
}

/// v4+ 加载后：以 `deleted` 位图为「已删除」真源，去掉仅因与 Windows 存档位冲突而误置的 `ATTR_DELETED`。
fn repair_attrs_deleted_bitmap_alignment(entries: &mut [FileEntry], deleted: &RoaringBitmap) {
    for (i, e) in entries.iter_mut().enumerate() {
        if !deleted.contains(i as u32) {
            e.attrs &= !FileEntry::ATTR_DELETED;
        }
    }
}

fn read_exact_vec<R: Read>(r: &mut R, n: usize) -> Result<Vec<u8>> {
    let mut v = vec![0u8; n];
    if n > 0 {
        r.read_exact(&mut v)?;
    }
    Ok(v)
}

fn skip_u32_vec<R: Read + Seek>(r: &mut R) -> Result<()> {
    let mut nb = [0u8; 8];
    r.read_exact(&mut nb)?;
    let n = u64::from_le_bytes(nb);
    let bytes = n
        .checked_mul(4)
        .ok_or_else(|| crate::Error::Persist("v2/v3 *_order 长度溢出".into()))?;
    r.seek(SeekFrom::Current(bytes as i64))?;
    Ok(())
}

/// 与 index.bin 同目录的「排除目录」边车文件名（`<index>.exclude.json`）。
/// 走 sidecar 而不是塞进 index.bin 是为了零格式破坏：
/// - 老的 v5 index.bin 不会因为「加了排除目录字段」而变 v6；
/// - service / CLI 启动时各自读 sidecar，写权限只在 GUI 设置面板里。
pub fn exclude_sidecar_path(index_path: &Path) -> std::path::PathBuf {
    let mut p = index_path.as_os_str().to_owned();
    p.push(".exclude.json");
    std::path::PathBuf::from(p)
}

/// 读 sidecar；不存在或解析失败一律返回空 vec（不要因为 sidecar 损坏让 service 起不来）。
pub fn load_exclude_sidecar(index_path: &Path) -> Vec<String> {
    let p = exclude_sidecar_path(index_path);
    let Ok(bytes) = std::fs::read(&p) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Vec::new();
    };
    let arr = v
        .get("excluded_dirs")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    arr.into_iter()
        .filter_map(|x| x.as_str().map(|s| s.to_string()))
        .filter_map(|s| crate::index::normalize_excluded_dir(&s))
        .collect()
}

/// 写 sidecar；目录不存在时自动 mkdir。
pub fn save_exclude_sidecar(index_path: &Path, excluded: &[String]) -> Result<()> {
    let p = exclude_sidecar_path(index_path);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let normalized: Vec<String> = excluded
        .iter()
        .filter_map(|s| crate::index::normalize_excluded_dir(s))
        .collect();
    let v = serde_json::json!({ "excluded_dirs": normalized });
    let s = serde_json::to_string_pretty(&v)
        .map_err(|e| crate::Error::Persist(e.to_string()))?;
    std::fs::write(&p, s.as_bytes())?;
    Ok(())
}

/// 生成 `index.bin.zst` 备份
pub fn save_index_zst(path: &Path, store: &IndexStore) -> Result<()> {
    let mut bin = Vec::new();
    write_index_bin(&mut bin, store)?;
    let compressed = zstd::encode_all(bin.as_slice(), 3)
        .map_err(|e| crate::Error::Persist(e.to_string()))?;
    let mut out = File::create(path)?;
    out.write_all(&compressed)?;
    Ok(())
}
