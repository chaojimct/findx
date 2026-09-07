//! 三字节组合（trigram）倒排索引 —— 子串查询的剪枝层。
//!
//! 设计参考 plocate：把「全表 SIMD 扫描」变成「倒排表求交 + 少量候选验证」。
//!
//! ## 数据布局（边车文件 `<index>.tri`，与 index.bin 同生命周期）
//!
//! ```text
//! [0..4]   magic "FTRI"
//! [4..8]   version u32
//! [8..12]  snapshot_entry_count u32   —— 建表时的 entries.len()
//! [12..16] tri_count u32              —— 不同 trigram 键数量
//! [16..24] blobs_len u64
//! [24..]   table: tri_count × { key u32, off u32, len u32 }（按 key 升序）
//! [..]     blobs：串接的序列化 RoaringBitmap（posting = 命中该 trigram 的 entry idx）
//! ```
//!
//! ## 正确性模型（关键）
//!
//! trigram 只是**必要条件过滤器**：候选集 ⊇ 真实命中集，之后仍走原有的
//! memmem 逐条验证链。因此任何陈旧数据最多造成多余验证，绝不漏报：
//! - 删除的条目 —— 验证链的 deleted 检查兜底；
//! - 改名后旧 posting 残留 —— 验证链的 memmem 兜底；
//! - 新增 / 改名的新名字 —— `tri_pending` 位图（USN 写路径维护）并入候选；
//! - `idx >= snapshot_entry_count` 的条目一律并入候选（构建期之后的增量）。
//!
//! ## 内存策略
//!
//! posting 数据留在 mmap 里按需反序列化（页缓存共享、可逐出、私有 RSS 近零），
//! 反序列化结果进小 LRU（打字递增式查询会反复命中同一批 trigram）。
//!
//! ## 剪枝放弃条件（回退全表扫描）
//!
//! - needle（小写后）不足 3 字节；
//! - case_sensitive 查询（索引只覆盖 ASCII 小写字节）；
//! - 拼音路径（音译命中不含 ASCII trigram，字节倒排无法覆盖）；
//! - 交集候选超过全表 1/3（位图收集成本超过直接扫描）。

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use memmap2::Mmap;
use parking_lot::Mutex;
use rayon::prelude::*;
use roaring::RoaringBitmap;

use crate::index::IndexStore;
use crate::Result;

const TRI_MAGIC: u32 = u32::from_le_bytes(*b"FTRI");
const TRI_VERSION: u32 = 1;

/// 给一段 mmap 区域发预取提示（Windows 8+ `PrefetchVirtualMemory`，Unix `posix_madvise`）。
///
/// 动机：mmap 挂载后首次访问要吃逐页 page fault——49.8 MiB 的边车 ≈ 12k 次 fault，
/// 表现为「服务启动后第一查比后续慢一个量级」。该 API 让内核一次性把整段顺序读入页缓存。
/// 仅是提示：失败无害（返回值忽略），不改变任何正确性行为。
///
/// 动态解析（GetProcAddress）而非直接链接：符号仅 Win8+ 存在，静态链接会让进程在
/// Win7 上直接起不来。缓存函数指针，加载期调用几次的开销可忽略。
pub(crate) fn prefetch_mmap(bytes: &[u8]) {
    #[cfg(windows)]
    {
        use std::ffi::c_void;
        use std::sync::OnceLock;

        #[repr(C)]
        struct MemoryRangeEntry {
            virtual_address: *mut c_void,
            number_of_bytes: usize,
        }
        type PrefetchVirtualMemoryFn =
            unsafe extern "system" fn(isize, usize, *const MemoryRangeEntry, u32) -> i32;

        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetModuleHandleA(name: *const u8) -> isize;
            fn GetProcAddress(module: isize, name: *const u8) -> *const c_void;
        }

        static FN: OnceLock<Option<PrefetchVirtualMemoryFn>> = OnceLock::new();
        let f = *FN.get_or_init(|| unsafe {
            let module_name = c"kernel32";
            let api_name = c"PrefetchVirtualMemory";
            let module = GetModuleHandleA(module_name.as_ptr() as *const u8);
            if module == 0 {
                return None;
            }
            let sym = GetProcAddress(module, api_name.as_ptr() as *const u8);
            if sym.is_null() {
                return None;
            }
            // SAFETY: GetProcAddress 返回的就是函数指针本体（不是指向指针的指针），
            // 必须整体 transmute 成 fn 类型；若当 `*const fn` 再解引用会把函数体
            // 开头的指令字节当地址调用（实测直接 AV）。ABI 由 Win32 定义保证。
            Some(std::mem::transmute::<*const c_void, PrefetchVirtualMemoryFn>(sym))
        });

        if let (Some(f), false) = (f, bytes.is_empty()) {
            let entry = MemoryRangeEntry {
                virtual_address: bytes.as_ptr() as *mut c_void,
                number_of_bytes: bytes.len(),
            };
            // -1 = GetCurrentProcess() 伪句柄。
            unsafe { f(-1, 1, &entry, 0) };
        }
    }
    #[cfg(unix)]
    {
        if bytes.is_empty() {
            return;
        }
        // POSIX_MADV_WILLNEED：提示内核尽快把这段顺序读进页缓存，失败无害。
        unsafe {
            let _ = libc::posix_madvise(
                bytes.as_ptr() as *mut libc::c_void,
                bytes.len(),
                libc::POSIX_MADV_WILLNEED,
            );
        }
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = bytes;
    }
}

/// 头部 24 字节；table 紧随其后（4 字节对齐，`TriRec` 与磁盘逐字节一致）。
const TRI_HEADER_SIZE: usize = 24;
const TRI_REC_SIZE: usize = 12;

/// 反序列化位图的 LRU 容量（字节预算；到位后随机淘汰一半）。
/// 64MB ≈ 3000 万 idx，足够容纳 GUI 打字过程中反复出现的全部热 trigram。
const CACHE_BYTES_BUDGET: usize = 64 * 1024 * 1024;

#[repr(C)]
#[derive(Clone, Copy)]
struct TriRec {
    /// 3 字节小写 trigram 打包成 u32（b0<<16 | b1<<8 | b2），高 8 位恒 0。
    key: u32,
    off: u32,
    len: u32,
}

impl TriRec {
    fn to_bytes(self) -> [u8; TRI_REC_SIZE] {
        let mut b = [0u8; TRI_REC_SIZE];
        b[0..4].copy_from_slice(&self.key.to_le_bytes());
        b[4..8].copy_from_slice(&self.off.to_le_bytes());
        b[8..12].copy_from_slice(&self.len.to_le_bytes());
        b
    }
}

/// 从小写字节流提取 trigram 键（滑动窗口去重）。
fn trigram_keys(lower: &[u8]) -> Vec<u32> {
    let mut keys = Vec::with_capacity(lower.len().saturating_sub(2));
    if lower.len() < 3 {
        return keys;
    }
    for w in lower.windows(3) {
        keys.push((w[0] as u32) << 16 | (w[1] as u32) << 8 | w[2] as u32);
    }
    keys
}

/// mmap 支撑的 trigram 倒排索引（只读；增量由 `tri_pending` 承接，重建生成新文件）。
pub struct TrigramIndex {
    /// Box 保证 mmap 指向地址稳定；table / blobs 的 'static 视图由它保活。
    /// 字段本身不被读取（保活用途），读取的是它派生出的 table / blobs。
    #[allow(dead_code)]
    map: Box<Mmap>,
    snapshot_entry_count: u32,
    tri_count: u32,
    /// 安全性：切片实际借自 `map`（Box'd，地址稳定），只通过 `&self` 再借用对外，
    /// 不会逃逸出 TrigramIndex 的生命周期。
    table: &'static [TriRec],
    blobs: &'static [u8],
    cache: Mutex<BmCache>,
}

struct BmCache {
    map: HashMap<u32, Arc<RoaringBitmap>>,
    bytes: usize,
}

impl BmCache {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            bytes: 0,
        }
    }
}

impl TrigramIndex {
    pub fn snapshot_entry_count(&self) -> u32 {
        self.snapshot_entry_count
    }

    pub fn tri_count(&self) -> u32 {
        self.tri_count
    }

    /// mmap 加载边车；文件缺失 / 损坏一律返回 Ok(None)（搜索回退全表扫描，不阻断启动）。
    pub fn load(path: &Path) -> Result<Option<Self>> {
        let Ok(file) = File::open(path) else {
            return Ok(None);
        };
        let Ok(map) = (unsafe { Mmap::map(&file) }) else {
            return Ok(None);
        };
        // 先装箱（堆地址稳定），再从箱内切 'static 视图，避免「切片借栈上 map、
        // map 又被 move 进 struct」的自引用借用错误。
        let boxed: Box<Mmap> = Box::new(map);
        let mm: &[u8] = &boxed[..];
        if mm.len() < TRI_HEADER_SIZE {
            return Ok(None);
        }
        let magic = u32::from_le_bytes(mm[0..4].try_into().unwrap());
        if magic != TRI_MAGIC {
            return Ok(None);
        }
        let version = u32::from_le_bytes(mm[4..8].try_into().unwrap());
        if version != TRI_VERSION {
            return Ok(None);
        }
        let snapshot_entry_count = u32::from_le_bytes(mm[8..12].try_into().unwrap());
        let tri_count = u32::from_le_bytes(mm[12..16].try_into().unwrap());
        let blobs_len = u64::from_le_bytes(mm[16..24].try_into().unwrap()) as usize;

        let table_bytes = tri_count as usize * TRI_REC_SIZE;
        let Some(table_region) = mm.get(TRI_HEADER_SIZE..TRI_HEADER_SIZE + table_bytes) else {
            return Ok(None);
        };
        let Some(blobs) = mm.get(TRI_HEADER_SIZE + table_bytes..) else {
            return Ok(None);
        };
        if blobs.len() != blobs_len {
            return Ok(None);
        }
        if !(table_region.as_ptr() as usize).is_multiple_of(std::mem::align_of::<TriRec>()) {
            // mmap 基址按页对齐，头部 24B + 12B 记录恒 4 对齐；此分支实际不可达，防御性保留。
            return Ok(None);
        }
        let table: &'static [TriRec] = unsafe {
            std::slice::from_raw_parts(table_region.as_ptr() as *const TriRec, tri_count as usize)
        };
        // SAFETY: blobs 借自堆上 Box<Mmap>（地址稳定），TrigramIndex 持有该 Box 保活；
        // from_raw_parts 把生命周期抬到 'static，切片仅经 &self 对外、不逃逸出本结构体。
        let blobs: &'static [u8] = unsafe {
            std::slice::from_raw_parts(blobs.as_ptr(), blobs.len())
        };
        // 格式校验通过才预取（垃圾文件不值得占页缓存）；整个 map 都会用到
        // （table + blobs），整段预取最划算。
        prefetch_mmap(mm);

        Ok(Some(Self {
            map: boxed,
            snapshot_entry_count,
            tri_count,
            table,
            blobs,
            cache: Mutex::new(BmCache::new()),
        }))
    }

    fn rec(&self, key: u32) -> Option<TriRec> {
        self.table
            .binary_search_by(|r| r.key.cmp(&key))
            .ok()
            .map(|i| self.table[i])
    }

    fn bm(&self, key: u32) -> Option<Arc<RoaringBitmap>> {
        let rec = self.rec(key)?;
        let blob = self
            .blobs
            .get(rec.off as usize..rec.off as usize + rec.len as usize)?;
        {
            let cache = self.cache.lock();
            if let Some(hit) = cache.map.get(&key) {
                return Some(Arc::clone(hit));
            }
        }
        let bm = RoaringBitmap::deserialize_from(blob).ok()?;
        let arc = Arc::new(bm);
        let mut cache = self.cache.lock();
        if !cache.map.contains_key(&key) {
            // 粗略字节数 ≈ 元素数 × 2B（数组容器主导）+ 容器头开销。
            let approx = (arc.len() as usize) * 2 + (arc.len() as usize / 65536 + 1) * 8;
            cache.bytes += approx;
            cache.map.insert(key, Arc::clone(&arc));
            if cache.bytes > CACHE_BYTES_BUDGET {
                // 超预算随机淘汰一半（HashMap 无序即近似随机；打字场景热度集中，简单够用）。
                let evict: Vec<u32> = cache.map.keys().step_by(2).copied().collect();
                for k in evict {
                    if let Some(v) = cache.map.remove(&k) {
                        let approx = (v.len() as usize) * 2 + (v.len() as usize / 65536 + 1) * 8;
                        cache.bytes = cache.bytes.saturating_sub(approx);
                    }
                    if cache.bytes <= CACHE_BYTES_BUDGET / 2 {
                        break;
                    }
                }
            }
        }
        Some(arc)
    }

    /// 小写 needle → 候选位图（**不含** pending / 尾部增量，由调用方并入）。
    /// `None` = 不适合剪枝（<3 字节）；`Some(空集)` = 库内没有任何名字含这些 trigram。
    pub fn lookup_candidates(&self, needle_lower: &[u8]) -> Option<RoaringBitmap> {
        if needle_lower.len() < 3 {
            return None;
        }
        let keys = trigram_keys(needle_lower);
        debug_assert!(!keys.is_empty());
        // 按 posting 长度升序求交：先小集合把基数压下来，后面常见 trigram 只做位与。
        let mut recs: Vec<(u32, TriRec)> = Vec::with_capacity(keys.len());
        for &k in &keys {
            if let Some(r) = self.rec(k) {
                if !recs.iter().any(|(kk, _)| *kk == k) {
                    recs.push((k, r));
                }
            }
        }
        // needle 的某个 trigram 在库内完全不存在 → 名字里必然不包含 needle → 空候选。
        // 但仅当该 trigram 属于 needle 的「必然出现的窗口」——所有窗口都不存在时才为空；
        // 部分缺失时交集自然为空，统一走交集路径即可。
        if recs.is_empty() {
            return Some(RoaringBitmap::new());
        }
        recs.sort_by_key(|(_, r)| r.len);

        let mut acc = (*self.bm(recs[0].0)?).clone();
        for (k, _) in &recs[1..] {
            if acc.is_empty() {
                break;
            }
            let bm = self.bm(*k)?;
            acc &= bm.as_ref();
        }
        Some(acc)
    }
}

/// 并行构建 + 原子落盘（`.tmp.<pid>` + rename，与 save_index_bin 同一套防半截策略）。
/// 构建完写一个空 pending 边车（与快照同步）。
pub fn build_and_save(store: &IndexStore, index_path: &Path) -> Result<()> {
    let tri_path = tri_sidecar_path(index_path);
    let started = std::time::Instant::now();

    let n = store.entries.len();
    // 分片并行：每线程对连续 idx 段建局部 HashMap<trigram, RoaringBitmap>（idx 递增 → 容器追加友好），
    // 再 reduce 合并（roaring 原地 union）。8.5M 条目实测 ~3-5s，构建期一次性成本。
    // 注：rayon 没有 StepBy 的 ParallelIterator 实现，先收集起点再进并行迭代器。
    let chunk = (n / rayon::current_num_threads().max(1)).max(1);
    let starts: Vec<usize> = (0..n).step_by(chunk).collect();
    let merged: HashMap<u32, RoaringBitmap> = starts
        .into_par_iter()
        .map(|start| {
            let end = (start + chunk).min(n);
            let mut local: HashMap<u32, RoaringBitmap> = HashMap::new();
            let mut buf = [0u8; 256];
            for idx in start..end {
                let e = &store.entries[idx];
                if e.is_deleted() {
                    continue;
                }
                let lower = store.name_lower_into(e, &mut buf);
                for k in trigram_keys(lower.as_ref()) {
                    local.entry(k).or_default().insert(idx as u32);
                }
            }
            local
        })
        .reduce(HashMap::<u32, RoaringBitmap>::new, |mut a, mut b| {
            if a.len() < b.len() {
                std::mem::swap(&mut a, &mut b);
            }
            for (k, bm) in b {
                *a.entry(k).or_default() |= bm;
            }
            a
        });

    let mut keys: Vec<u32> = merged.keys().copied().collect();
    keys.sort_unstable();

    let mut table_bytes: Vec<u8> = Vec::with_capacity(keys.len() * TRI_REC_SIZE);
    let mut blobs: Vec<u8> = Vec::new();
    for &k in &keys {
        let bm = merged.get(&k).expect("keys 来自 merged");
        let off = blobs.len() as u32;
        bm.serialize_into(&mut blobs)
            .map_err(|e| crate::Error::Persist(e.to_string()))?;
        let len = blobs.len() as u32 - off;
        table_bytes.extend_from_slice(&TriRec { key: k, off, len }.to_bytes());
    }

    let tmp_path = {
        let mut p = tri_path.as_os_str().to_owned();
        p.push(format!(".tmp.{}", std::process::id()));
        PathBuf::from(p)
    };
    let _ = std::fs::remove_file(&tmp_path);
    {
        let mut f = File::create(&tmp_path)?;
        let mut hdr = [0u8; TRI_HEADER_SIZE];
        hdr[0..4].copy_from_slice(&TRI_MAGIC.to_le_bytes());
        hdr[4..8].copy_from_slice(&TRI_VERSION.to_le_bytes());
        hdr[8..12].copy_from_slice(&(n as u32).to_le_bytes());
        hdr[12..16].copy_from_slice(&(keys.len() as u32).to_le_bytes());
        hdr[16..24].copy_from_slice(&(blobs.len() as u64).to_le_bytes());
        f.write_all(&hdr)?;
        f.write_all(&table_bytes)?;
        f.write_all(&blobs)?;
        f.sync_data().ok();
    }
    #[cfg(windows)]
    let _ = std::fs::remove_file(&tri_path);
    std::fs::rename(&tmp_path, &tri_path)?;

    save_pending_sidecar(index_path, &RoaringBitmap::new(), n as u32)?;

    crate::progress!(
        "trigram：构建完成 {}（{} 键 / {} 条目 / {:.1} MiB，耗时 {:.2}s）",
        tri_path.display(),
        keys.len(),
        n,
        (table_bytes.len() + blobs.len()) as f64 / (1024.0 * 1024.0),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

/// `<index>.tri`
pub fn tri_sidecar_path(index_path: &Path) -> PathBuf {
    let mut p = index_path.as_os_str().to_owned();
    p.push(".tri");
    PathBuf::from(p)
}

/// `<index>.tri.pending`：snapshot_entry_count + 序列化 pending 位图。
/// 顺序保证：`.tri` 先写、pending 后写 —— 崩溃窗口内旧 pending 对新快照是超集，
/// 只会多算候选（验证链兜底），不会漏。
pub fn pending_sidecar_path(index_path: &Path) -> PathBuf {
    let mut p = index_path.as_os_str().to_owned();
    p.push(".tri.pending");
    PathBuf::from(p)
}

pub fn save_pending_sidecar(
    index_path: &Path,
    pending: &RoaringBitmap,
    snapshot_entry_count: u32,
) -> Result<()> {
    let path = pending_sidecar_path(index_path);
    let mut body = Vec::with_capacity(64);
    body.extend_from_slice(&snapshot_entry_count.to_le_bytes());
    pending
        .serialize_into(&mut body)
        .map_err(|e| crate::Error::Persist(e.to_string()))?;
    let tmp_path = {
        let mut p = path.as_os_str().to_owned();
        p.push(format!(".tmp.{}", std::process::id()));
        PathBuf::from(p)
    };
    let _ = std::fs::remove_file(&tmp_path);
    std::fs::write(&tmp_path, &body)?;
    #[cfg(windows)]
    let _ = std::fs::remove_file(&path);
    std::fs::rename(&tmp_path, &path)?;
    Ok(())
}

/// 读 pending 边车；文件缺失 / 损坏返回 None（调用方应按保守策略处理）。
pub fn load_pending_sidecar(index_path: &Path) -> Option<(RoaringBitmap, u32)> {
    let path = pending_sidecar_path(index_path);
    let body = std::fs::read(path).ok()?;
    if body.len() < 4 {
        return None;
    }
    let snap = u32::from_le_bytes(body[0..4].try_into().ok()?);
    let bm = RoaringBitmap::deserialize_from(&body[4..]).ok()?;
    Some((bm, snap))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::IndexBuilder;
    use crate::platform::RawEntry;

    /// 每个测试独立子目录（cargo test 并行跑；共用同一目录会因开头的
    /// `remove_dir_all` 互相删掉对方的边车文件）。
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "findx2-tri-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn raw(id: u64, parent: u64, name: &str, is_dir: bool) -> RawEntry {
        RawEntry {
            file_id: id,
            file_id_128: None,
            parent_id: parent,
            name: name.to_string(),
            size: if is_dir { 0 } else { 1024 },
            mtime: 0,
            ctime: 0,
            attrs: if is_dir { 0x10 } else { 0 },
            is_dir,
        }
    }

    #[test]
    fn build_lookup_roundtrip() {
        let names = [
            "android-studio",
            "config.json",
            "conftest.py",
            "马春天.txt",
            "readme.md",
        ];
        let files: Vec<RawEntry> = names
            .iter()
            .enumerate()
            .map(|(i, n)| raw(100 + i as u64, 1, n, false))
            .collect();
        let dirs = vec![raw(1, 0, "C:", true)];
        let store = IndexBuilder::new(b'C', 0, 0, 0)
            .build_from_raw(files, dirs, true)
            .unwrap();

        let dir = temp_dir("roundtrip");
        let idx = dir.join("index.bin");
        build_and_save(&store, &idx).unwrap();

        let tri = TrigramIndex::load(&tri_sidecar_path(&idx))
            .unwrap()
            .expect("tri 应加载成功");
        assert_eq!(tri.snapshot_entry_count() as usize, store.entries.len());

        let find_hits = |needle: &str| -> usize {
            let cand = tri
                .lookup_candidates(needle.as_bytes())
                .expect(">=3 字节必有候选");
            let lower: Vec<u8> = needle.as_bytes().to_vec();
            cand.iter()
                .filter(|&i| {
                    let e = &store.entries[i as usize];
                    let nb = store.name_bytes(e);
                    let lo: Vec<u8> = nb.iter().map(|b| b.to_ascii_lowercase()).collect();
                    memchr::memmem::find(&lo, &lower).is_some()
                })
                .count()
        };
        // 必要条件：候选 ⊇ 真实命中。
        assert!(find_hits("ndro") >= 1);
        assert!(find_hits("onf") >= 2, "config.json + conftest.py");
        assert!(find_hits("eadm") >= 1);
        assert_eq!(
            tri.lookup_candidates("zzz".as_bytes()).unwrap().len(),
            0,
            "库内不存在的 trigram → 空候选"
        );
        assert!(
            tri.lookup_candidates("ab".as_bytes()).is_none(),
            "<3 字节不剪枝"
        );
        // 中文按 UTF-8 字节窗口同样可查。
        let spring = "春天".as_bytes();
        let cand = tri.lookup_candidates(spring).expect("utf8 窗口 >=3");
        assert!(cand.len() >= 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pending_sidecar_roundtrip() {
        let dir = temp_dir("pending");
        let idx = dir.join("index.bin");
        let mut bm = RoaringBitmap::new();
        bm.insert(3u32);
        bm.insert(1_000_000u32);
        save_pending_sidecar(&idx, &bm, 42).unwrap();
        let (loaded, snap) = load_pending_sidecar(&idx).expect("应能读回");
        assert_eq!(snap, 42);
        assert_eq!(loaded.len(), 2);
        assert!(loaded.contains(3));
        assert!(loaded.contains(1_000_000));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_returns_none() {
        let dir = temp_dir("missing");
        let idx = dir.join("index.bin");
        assert!(TrigramIndex::load(&tri_sidecar_path(&idx)).unwrap().is_none());
        assert!(load_pending_sidecar(&idx).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
