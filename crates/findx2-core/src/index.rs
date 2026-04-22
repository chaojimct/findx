//! `IndexStore` 与 `IndexBuilder`：SIMD 友好的连续 `names_buf` + 平行 `entries`。

use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;

use fxhash::FxHashMap;
use rayon::prelude::*;
use roaring::RoaringBitmap;

use crate::platform::{ChangeEvent, RawEntry};

/// `FRN -> entry/dir 下标` 的紧凑映射。
///
/// 历史：v1 是 `FxHashMap<u64,u32>`；hashbrown 实测在 8.5M 条目下吃掉 ~200 MB
/// （12 B 净数据 + ~12 B/entry 控制位 + 50% 装填率冗余 + 容量 ×2 的预分配）。
/// Everything 同规模 RSS 700 MB 的关键差距正出在这里。
///
/// 当前实现：双层结构
/// - `sorted: Vec<(u64, u32)>`：净 12 B/项，无哈希开销，构建期一次性 sort（O(N log N)）。
///   8.5M 条目 ≈ 100 MB，对照 hashbrown 节省 ~100 MB，且加载时 cache friendly。
/// - `overlay: FxHashMap<u64, u32>`：USN 增量场景（Create / Rename）下的覆盖表，
///   永远很小（~MB 级）。查询时先查 overlay（覆盖语义），未命中再二分 sorted。
///
/// `get` 的成本：
/// - cold path = 二分 23 次比较（log2 8.5M），实测 < 100 ns，比哈希慢可忽略；
/// - 搜索热路径根本不用 `frn_to_entry`，所以这点开销不进 P50。
#[derive(Debug, Default, Clone)]
pub struct FrnIdxMap {
    sorted: Vec<(u64, u32)>,
    /// USN 增量过来的 (frn -> idx) 覆盖；rebuild_sorted 时会被吸收。
    overlay: FxHashMap<u64, u32>,
}

impl FrnIdxMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// 仅为 sorted 段预分配，overlay 走默认 0 容量；与 hashbrown 相比少了 2x 系数。
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            sorted: Vec::with_capacity(cap),
            overlay: FxHashMap::default(),
        }
    }

    /// 兼容旧 `FxHashMap::with_capacity_and_hasher` 调用签名（hasher 参数被忽略）。
    pub fn with_capacity_and_hasher<H>(cap: usize, _hasher: H) -> Self {
        Self::with_capacity(cap)
    }

    /// 构建期：批量 push 后调用一次 sort_dedup_keep_last，把 sorted 段建成紧凑形态。
    /// 同 frn 多次 push 时保留最后一次（与 HashMap insert 语义一致）。
    pub fn finalize_build(&mut self) {
        // unstable 排序对 (u64, u32) 已经是稳定且更快的选择。
        self.sorted.sort_unstable_by_key(|&(k, _)| k);
        // 重复 frn 保留最后一次：从尾部往前扫，重复时丢弃前者。
        if self.sorted.len() > 1 {
            let mut write = 0usize;
            for read in 0..self.sorted.len() {
                if read + 1 < self.sorted.len() && self.sorted[read].0 == self.sorted[read + 1].0 {
                    // 当前 read 与下一个同 key，丢弃 read。
                    continue;
                }
                if write != read {
                    self.sorted[write] = self.sorted[read];
                }
                write += 1;
            }
            self.sorted.truncate(write);
        }
        self.sorted.shrink_to_fit();
    }

    /// 构建期 push（不去重、不排序），结束后请调用 `finalize_build`。
    pub fn push_unsorted(&mut self, frn: u64, idx: u32) {
        self.sorted.push((frn, idx));
    }

    /// 增量场景：写入覆盖表（与 HashMap 的 insert 语义一致：返回旧值 / None）。
    /// 不直接改 sorted 段，避免 O(N) shift。
    pub fn insert(&mut self, frn: u64, idx: u32) -> Option<u32> {
        if let Some(prev) = self.overlay.insert(frn, idx) {
            return Some(prev);
        }
        // 第一次覆盖该 key：返回 sorted 中的旧值（如果有），保持 HashMap 行为。
        self.get_idx(frn)
    }

    /// 与 `HashMap::get` 同名同形：返回 `Option<&u32>`。
    /// overlay 命中时直接借出 overlay 内的 u32 引用；sorted 命中时借 sorted 段内的 u32 引用。
    /// 二者借用都来自 &self，借用期 ≤ &self，符合 Rust 别名规则。
    pub fn get(&self, frn: &u64) -> Option<&u32> {
        if let Some(v) = self.overlay.get(frn) {
            return Some(v);
        }
        match self.sorted.binary_search_by_key(frn, |&(k, _)| k) {
            Ok(i) => Some(&self.sorted[i].1),
            Err(_) => None,
        }
    }

    /// 值版本，省去 `*x.get(...).unwrap_or(&0)` 写法。
    pub fn get_idx(&self, frn: u64) -> Option<u32> {
        self.get(&frn).copied()
    }

    pub fn contains_key(&self, frn: &u64) -> bool {
        self.get_idx(*frn).is_some()
    }

    pub fn len(&self) -> usize {
        // 上界估计：overlay 与 sorted 可能 frn 重叠；精确去重代价高，调用方仅用于诊断。
        self.sorted.len() + self.overlay.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sorted.is_empty() && self.overlay.is_empty()
    }

    /// 合并相邻 store 时使用：把 (k, v) 序列耗尽地拷贝出来（包含 overlay）。
    /// 调用方必须自行 finalize_build。
    pub fn drain(&mut self) -> impl Iterator<Item = (u64, u32)> + '_ {
        // 先 drain sorted，再 drain overlay（顺序不重要：overlay 覆盖 sorted 的语义在合并阶段不存在）。
        let sorted_iter = self.sorted.drain(..);
        let overlay_iter = self.overlay.drain();
        sorted_iter.chain(overlay_iter)
    }

    /// 内存占用估算（用于诊断 / progress 日志）。
    pub fn approx_bytes(&self) -> usize {
        self.sorted.capacity() * std::mem::size_of::<(u64, u32)>()
            + self.overlay.capacity() * (std::mem::size_of::<(u64, u32)>() + 8)
    }
}

/// 每卷 USN / 持久化元数据
#[derive(Debug, Clone)]
pub struct VolumeState {
    pub volume_letter: u8,
    pub volume_serial: u32,
    pub usn_journal_id: u64,
    pub last_usn: u64,
    /// 该卷在全局 `entries` 中的起始下标
    pub first_entry_idx: u32,
}

/// 目录项（路径重建）
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub frn: u64,
    pub parent_idx: u32,
    /// 目录名在 `names_buf` 中的偏移与长度（与 FileEntry 命名一致）
    pub name_offset: u32,
    pub name_len: u16,
}

/// 单文件元数据，32 字节（v5 紧凑布局）。
///
/// 历史 v4 是 40B（mtime/ctime 都 u64 FILETIME 100ns，浪费）。
/// v5 改用 u32 秒数（自 Unix epoch 1970-01-01 UTC），范围覆盖到 2106 年。
/// 8.5M 条目省 8B = 68 MB。
///
/// 字段顺序按 8 字节对齐手排，编译器零 padding，sizeof = 32：
/// ```text
///   0..8   size       u64
///   8..12  mtime      u32  (秒，1970-)
///  12..16  ctime      u32  (秒，1970-)
///  16..20  name_offset u32
///  20..24  dir_idx    u32
///  24..28  attrs      u32  (低 8 位 = 类型标志，bits[8..15] = ext_hash_u8)
///  28..30  n_len      u16
///  30..32  _pad       u16
/// ```
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct FileEntry {
    pub size: u64,
    /// 秒，自 1970-01-01 UTC；0 表示「未知」（mft 首遍未回填或老索引）。
    pub mtime: u32,
    /// 秒，自 1970-01-01 UTC；0 表示「未知」。
    pub ctime: u32,
    pub name_offset: u32,
    pub dir_idx: u32,
    pub attrs: u32,
    pub n_len: u16,
    pub _pad: u16,
}

/// FILETIME（100ns 自 1601-01-01 UTC）→ Unix 秒（自 1970-01-01 UTC）。
/// 0 → 0（保留「未知」）。
#[inline]
pub fn filetime_to_unix_secs(ft: u64) -> u32 {
    if ft == 0 {
        return 0;
    }
    // 1601 → 1970 共 11644473600 秒 = 116444736000000000 / 10^7。
    const EPOCH_DIFF_100NS: u64 = 116_444_736_000_000_000;
    if ft <= EPOCH_DIFF_100NS {
        return 0;
    }
    let secs = (ft - EPOCH_DIFF_100NS) / 10_000_000;
    if secs > u32::MAX as u64 {
        u32::MAX
    } else {
        secs as u32
    }
}

/// Unix 秒 → FILETIME（100ns 自 1601）。0 → 0。
#[inline]
pub fn unix_secs_to_filetime(secs: u32) -> u64 {
    if secs == 0 {
        return 0;
    }
    const EPOCH_DIFF_100NS: u64 = 116_444_736_000_000_000;
    EPOCH_DIFF_100NS + (secs as u64) * 10_000_000
}

impl FileEntry {
    pub const ATTR_IS_DIR: u32 = 1 << 0;
    pub const ATTR_HIDDEN: u32 = 1 << 1;
    pub const ATTR_SYSTEM: u32 = 1 << 2;
    pub const ATTR_READONLY: u32 = 1 << 3;
    pub const ATTR_ARCHIVE: u32 = 1 << 4;
    pub const ATTR_DELETED: u32 = 1 << 5;
    /// bits[8..15] ext_hash_u8
    pub fn ext_hash_u8(self) -> u8 {
        ((self.attrs >> 8) & 0xff) as u8
    }

    pub fn is_deleted(self) -> bool {
        self.attrs & Self::ATTR_DELETED != 0
    }

    #[inline]
    pub fn is_dir_entry(self) -> bool {
        self.attrs & Self::ATTR_IS_DIR != 0
    }

    /// 从 USN/MFT 的 Windows `FileAttributes` 低 8 位装入「文件」条目的 attrs 低字节。
    /// - `FILE_ATTRIBUTE_READONLY`(0x01) 与 `ATTR_IS_DIR` 同位 → 清 bit0。
    /// - `FILE_ATTRIBUTE_ARCHIVE`(0x20) 与 `ATTR_DELETED`(1<<5) 同位 → 清 bit5；
    ///   否则几乎所有带存档位的普通文件会在 `is_deleted()` 里被当成已删除，搜索全只剩目录。
    #[inline]
    pub fn pack_attrs_from_windows_file(win_attrs: u32) -> u32 {
        let mut a = (win_attrs & 0xff) & !Self::ATTR_IS_DIR;
        a &= !Self::ATTR_DELETED;
        a
    }
}

/// 主索引
pub struct IndexStore {
    pub names_buf: Vec<u8>,
    pub entries: Vec<FileEntry>,
    pub dirs: Vec<DirEntry>,
    pub dir_index: FrnIdxMap,
    /// 目录完整路径池（小写 UTF-8，如 `\users\alice\`，不含盘符与末尾文件名）。
    /// 可为空：新索引与 Everything 一致仅存父链 + 短名，路径由 [`IndexStore::resolve_dir_path_lower`] 按需拼。
    pub dir_paths_buf: Vec<u8>,
    /// 每个 `DirEntry` 在 `dir_paths_buf` 中的 [offset, len)；全 0 表示未物化，走按需解析。
    pub dir_path_ranges: Vec<(u32, u32)>,
    pub volumes: Vec<VolumeState>,
    pub ext_filter: [Option<RoaringBitmap>; 256],
    pub deleted: RoaringBitmap,
    /// 与 `entries[i]` 对齐的文件引用号（Windows FRN），用于 USN 增量删除 / 改名
    pub frns: Vec<u64>,
    pub frn_to_entry: FrnIdxMap,
    /// `false` 表示首遍快速建库尚未回填 size/mtime；时间/大小过滤与排序将降级（见 [`SearchEngine`]）。
    pub metadata_ready: bool,
    /// 用户配置的排除目录（小写、统一反斜杠，**含盘符**，例如 `c:\windows\winsxs`）。
    /// 仅运行时字段，不进 `index.bin`；由 `<index>.exclude.json` sidecar 与 service CLI `--exclude-dir` 注入。
    /// 命中规则：`apply_change_event` 在 USN 增量进库前用前缀匹配丢弃落入排除目录的新条目，
    /// 防止设置中"排除 C:\Windows"后 Windows Update 仍持续灌入新文件。
    /// 历史已入库的条目不会被回溯清理（用户应在改了排除目录后手动重建索引）。
    pub excluded_dirs: Vec<String>,
}

/// 把用户输入的目录路径规范化为「**小写 + 反斜杠 + 末尾不带 `\`**」的前缀串，
/// 以便在 USN 路径与已物化 dir_path 上做 `starts_with` 比较。
/// 空串、纯 `\` 与纯盘符都视为无效（避免误把整盘排除掉）。
pub fn normalize_excluded_dir(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut s: String = trimmed
        .chars()
        .map(|c| if c == '/' { '\\' } else { c })
        .collect::<String>()
        .to_ascii_lowercase();
    while s.ends_with('\\') {
        s.pop();
    }
    if s.is_empty() {
        return None;
    }
    // 至少要有一段路径（盘符 + 子目录），否则 `c:` 这种会把整个 C 盘排掉。
    if s.len() <= 2 && s.chars().nth(1) == Some(':') {
        return None;
    }
    Some(s)
}

impl IndexStore {
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn name_bytes(&self, e: &FileEntry) -> &[u8] {
        let o = e.name_offset as usize;
        let n = e.n_len as usize;
        &self.names_buf[o..o + n]
    }

    /// case-insensitive 名字匹配的栈缓冲：
    /// - 把 `name_bytes(e)` 拷到 `out` 并就地 ASCII lower，返回切片。
    /// - 短名走栈（无堆分配）；超长名兜底走 heap，全索引大约只有不到 0.1% 的文件名 > 256 B。
    ///
    /// 删除了原 `names_lower_buf` 字段：8.5M 文件 ≈ 14 B/名 ≈ 120 MB RAM，靠这个 inline 函数取代。
    #[inline]
    pub fn name_lower_into<'a>(&self, e: &FileEntry, out: &'a mut [u8; 256]) -> NameLowerCow<'a> {
        let o = e.name_offset as usize;
        let n = e.n_len as usize;
        let src = &self.names_buf[o..o + n];
        if n <= out.len() {
            for (dst, &b) in out[..n].iter_mut().zip(src.iter()) {
                *dst = b.to_ascii_lowercase();
            }
            NameLowerCow::Borrowed(&out[..n])
        } else {
            NameLowerCow::Owned(src.iter().map(|b| b.to_ascii_lowercase()).collect())
        }
    }

    pub fn name_str(&self, e: &FileEntry) -> crate::Result<&str> {
        Ok(std::str::from_utf8(self.name_bytes(e))?)
    }

    /// 同步 push 一段文件名 / 目录名进 `names_buf`。
    /// `tail_null` 为 true 时追加 1 字节 0（便于整块 memchr 扫描）。
    fn append_name(&mut self, bytes: &[u8], tail_null: bool) -> u32 {
        let off = self.names_buf.len() as u32;
        self.names_buf.extend_from_slice(bytes);
        if tail_null {
            self.names_buf.push(0);
        }
        off
    }

    /// 条目所在目录的完整前缀路径（小写、不含盘符）：优先已物化的 `dir_paths_buf`，否则沿 `dirs` 父链拼接。
    #[inline]
    pub fn dir_path_bytes(&self, e: &FileEntry) -> Cow<'_, [u8]> {
        self.resolve_dir_path_lower(e.dir_idx)
    }

    /// 按需解析目录路径（与物化进 `dir_paths_buf` 的字节语义一致）。
    pub(crate) fn resolve_dir_path_lower(&self, dir_idx: u32) -> Cow<'_, [u8]> {
        let d = dir_idx as usize;
        if d < self.dir_path_ranges.len() {
            let (o, len) = self.dir_path_ranges[d];
            if len > 0 {
                let o = o as usize;
                let end = o + len as usize;
                if let Some(s) = self.dir_paths_buf.get(o..end) {
                    if !s.is_empty() {
                        return Cow::Borrowed(s);
                    }
                }
            }
        }
        Cow::Owned(self.build_dir_path_lower_owned(dir_idx))
    }

    /// 沿 `parent_idx` 自底向上拼 `\a\b`（小写），不含盘符；与旧版物化逻辑一致。
    fn build_dir_path_lower_owned(&self, dir_idx: u32) -> Vec<u8> {
        let mut cur = dir_idx as usize;
        if cur >= self.dirs.len() {
            return Vec::new();
        }
        let mut parts: Vec<String> = Vec::new();
        let mut safety = self.dirs.len().saturating_add(1);
        while safety > 0 {
            safety -= 1;
            let d = &self.dirs[cur];
            let end = d.name_offset as usize + d.name_len as usize;
            let name_bs = self
                .names_buf
                .get(d.name_offset as usize..end)
                .unwrap_or(&[]);
            let name_lc = String::from_utf8_lossy(name_bs).to_ascii_lowercase();
            parts.push(name_lc);
            if d.parent_idx == 0 {
                break;
            }
            cur = d.parent_idx as usize;
            if cur >= self.dirs.len() {
                break;
            }
        }
        parts.reverse();
        let mut out = Vec::new();
        for (i, seg) in parts.iter().enumerate() {
            if i == 0 {
                if !seg.is_empty() {
                    out.push(b'\\');
                    out.extend_from_slice(seg.as_bytes());
                }
            } else {
                out.push(b'\\');
                out.extend_from_slice(seg.as_bytes());
            }
        }
        out
    }

    /// 从目录拓扑物化 `dir_paths_buf`（旧版索引迁移、或需反复路径过滤时可调用；日常与 Everything 一致可不物化）
    pub fn rebuild_dir_paths(&mut self) {
        self.dir_paths_buf.clear();
        self.dir_path_ranges = vec![(0, 0); self.dirs.len()];
        if self.dirs.is_empty() {
            return;
        }
        let mut tmp: Vec<String> = vec![String::new(); self.dirs.len()];
        for i in 0..self.dirs.len() {
            let d = &self.dirs[i];
            let name_bs = self
                .names_buf
                .get(d.name_offset as usize..d.name_offset as usize + d.name_len as usize)
                .unwrap_or(&[]);
            let name_lc = String::from_utf8_lossy(name_bs).to_ascii_lowercase();
            let path_str = if d.parent_idx == 0 {
                if name_lc.is_empty() {
                    String::new()
                } else {
                    format!("\\{}", name_lc)
                }
            } else {
                let parent = d.parent_idx as usize;
                if parent < tmp.len() {
                    let p = &tmp[parent];
                    if p.is_empty() {
                        format!("\\{}", name_lc)
                    } else {
                        format!("{}\\{}", p, name_lc)
                    }
                } else {
                    format!("\\{}", name_lc)
                }
            };
            tmp[i] = path_str;
            let off = self.dir_paths_buf.len() as u32;
            self.dir_paths_buf.extend_from_slice(tmp[i].as_bytes());
            let len = (self.dir_paths_buf.len() as u32).saturating_sub(off);
            self.dir_path_ranges[i] = (off, len);
        }
    }

    /// 增量删除（USN `FILE_DELETE`）：墓碑 + 属性位
    pub fn delete_entry(&mut self, idx: u32) {
        self.deleted.insert(idx);
        if let Some(e) = self.entries.get_mut(idx as usize) {
            e.attrs |= FileEntry::ATTR_DELETED;
        }
    }

    /// 按用户配置的排除目录前缀，把所有命中的 entries 打成「已删除」（不真实压缩）。
    ///
    /// 工程取舍：物理删除 entries 会牵动 frns / ext_filter / dir_index / dir_path_ranges 全套下标，
    /// 重建成本高且容易破坏其它子系统的不变量；而 `delete_entry` 走 `deleted` Roaring 位图 + ATTR_DELETED，
    /// 搜索热路径的 `fused_scan` 已天然过滤这些条目。
    /// 索引体积没省（user 想节省体积时还是走「重建索引」按钮 + sidecar 把目录直接挡在 USN 增量层），
    /// 但搜索结果不会再返回排除目录里的文件——满足用户预期。
    ///
    /// 返回被打标的条目数。
    pub fn mark_excluded_entries(&mut self, excluded: &[String]) -> usize {
        if excluded.is_empty() || self.entries.is_empty() {
            return 0;
        }
        let normalized: Vec<String> = excluded
            .iter()
            .filter_map(|s| normalize_excluded_dir(s))
            .collect();
        if normalized.is_empty() {
            return 0;
        }
        let mut marked: u32 = 0;
        for idx in 0..self.entries.len() {
            if self.entries[idx].is_deleted() {
                continue;
            }
            // 重建条目完整路径（小写带盘符）。多卷场景下 volume_letter_for_entry 走的是 first_entry_idx 段表，正确。
            let letter = self.volume_letter_for_entry(idx).to_ascii_lowercase();
            let dir_idx = self.entries[idx].dir_idx;
            let dir_path = self.resolve_dir_path_lower(dir_idx);
            let name_bs = self.name_bytes(&self.entries[idx]);
            let name_lc = std::str::from_utf8(name_bs)
                .unwrap_or("")
                .to_ascii_lowercase();
            let mut full = String::with_capacity(dir_path.len() + name_lc.len() + 4);
            full.push(letter);
            full.push(':');
            full.push_str(std::str::from_utf8(dir_path.as_ref()).unwrap_or(""));
            if !full.ends_with('\\') {
                full.push('\\');
            }
            full.push_str(&name_lc);
            for ex in &normalized {
                if full == *ex || full.starts_with(&format!("{ex}\\")) {
                    self.deleted.insert(idx as u32);
                    self.entries[idx].attrs |= FileEntry::ATTR_DELETED;
                    marked += 1;
                    break;
                }
            }
        }
        marked as usize
    }

    /// 增量追加文件条目（假定父目录已在 `dirs` / `dir_index` 中）
    pub fn append_file_entry(&mut self, raw: &RawEntry) -> u32 {
        debug_assert!(!raw.is_dir);
        let name_off = self.append_name(raw.name.as_bytes(), true);
        let dir_idx = *self.dir_index.get(&raw.parent_id).unwrap_or(&0);
        let eh = hash_ext8(&raw.name);
        let attrs = FileEntry::pack_attrs_from_windows_file(raw.attrs) | ((eh as u32) << 8);
        let new_idx = self.entries.len() as u32;
        self.entries.push(FileEntry {
            size: raw.size,
            mtime: filetime_to_unix_secs(raw.mtime),
            ctime: filetime_to_unix_secs(raw.ctime),
            name_offset: name_off,
            dir_idx,
            attrs,
            n_len: raw.name.len() as u16,
            _pad: 0,
        });
        let h = eh as usize;
        self.ext_filter[h]
            .get_or_insert_with(RoaringBitmap::new)
            .insert(new_idx);
        self.frns.push(raw.file_id);
        self.frn_to_entry.insert(raw.file_id, new_idx);
        new_idx
    }

    /// 墓碑数 / 条目数，用于决定是否建议全量重建或后台压缩
    pub fn tombstone_ratio(&self) -> f64 {
        let n = self.entries.len();
        if n == 0 {
            0.0
        } else {
            self.deleted.len() as f64 / n as f64
        }
    }

    fn ext_remove_idx(&mut self, idx: u32, ext8: u8) {
        let h = ext8 as usize;
        if let Some(bm) = self.ext_filter[h].as_mut() {
            bm.remove(idx);
        }
    }

    fn ext_insert_idx(&mut self, idx: u32, ext8: u8) {
        let h = ext8 as usize;
        self.ext_filter[h]
            .get_or_insert_with(RoaringBitmap::new)
            .insert(idx);
    }

    /// 新目录（父须在 `dir_index` 中）
    pub fn append_dir_from_raw(&mut self, raw: &RawEntry) -> crate::Result<()> {
        debug_assert!(raw.is_dir);
        if self.dir_index.contains_key(&raw.file_id) {
            return Ok(());
        }
        let parent_idx = if raw.parent_id == 0 {
            0u32
        } else {
            *self
                .dir_index
                .get(&raw.parent_id)
                .ok_or_else(|| crate::Error::Platform("父目录尚未出现在索引中".into()))?
        };
        let name_off = self.append_name(raw.name.as_bytes(), false);
        let di = self.dirs.len() as u32;
        self.dirs.push(DirEntry {
            frn: raw.file_id,
            parent_idx,
            name_offset: name_off,
            name_len: raw.name.len() as u16,
        });
        self.dir_path_ranges.push((0, 0));
        self.dir_index.insert(raw.file_id, di);

        let eh = hash_ext8(&raw.name);
        let attrs = FileEntry::ATTR_IS_DIR | ((eh as u32) << 8);
        let new_idx = self.entries.len() as u32;
        self.entries.push(FileEntry {
            size: 0,
            mtime: filetime_to_unix_secs(raw.mtime),
            ctime: filetime_to_unix_secs(raw.ctime),
            name_offset: name_off,
            dir_idx: parent_idx,
            attrs,
            n_len: raw.name.len() as u16,
            _pad: 0,
        });
        self.ext_insert_idx(new_idx, eh);
        self.frns.push(raw.file_id);
        self.frn_to_entry.insert(raw.file_id, new_idx);
        Ok(())
    }

    /// 已有 FRN 时由 Create（如 `RENAME_NEW_NAME`）回填元数据与路径
    fn upsert_raw_entry(&mut self, raw: &RawEntry) -> crate::Result<()> {
        let Some(&idx) = self.frn_to_entry.get(&raw.file_id) else {
            if raw.is_dir {
                self.append_dir_from_raw(raw)?;
            } else {
                let _ = self.append_file_entry(raw);
            }
            return Ok(());
        };
        if self.deleted.contains(idx) {
            self.deleted.remove(idx);
            if let Some(e) = self.entries.get_mut(idx as usize) {
                e.attrs &= !FileEntry::ATTR_DELETED;
            }
        }

        if raw.is_dir {
            if let Some(di) = self.dirs.iter().position(|d| d.frn == raw.file_id) {
                let name_off = self.append_name(raw.name.as_bytes(), false);
                let parent_idx = if raw.parent_id == 0 {
                    0u32
                } else {
                    *self.dir_index.get(&raw.parent_id).unwrap_or(&0)
                };
                let old_eh = self.entries[idx as usize].ext_hash_u8();
                self.ext_remove_idx(idx, old_eh);
                let eh = hash_ext8(&raw.name);
                if let Some(d) = self.dirs.get_mut(di) {
                    d.parent_idx = parent_idx;
                    d.name_offset = name_off;
                    d.name_len = raw.name.len() as u16;
                }
                if let Some(e) = self.entries.get_mut(idx as usize) {
                    e.name_offset = name_off;
                    e.n_len = raw.name.len() as u16;
                    e.dir_idx = parent_idx;
                    e.attrs = FileEntry::ATTR_IS_DIR | ((eh as u32) << 8);
                    // 改名等 upsert 若带上非 0 时间则更新；raw 为 0 时不覆盖（避免把已有 USN 时间抹掉）
                    if raw.mtime != 0 {
                        e.mtime = filetime_to_unix_secs(raw.mtime);
                    }
                    if raw.ctime != 0 {
                        e.ctime = filetime_to_unix_secs(raw.ctime);
                    }
                }
                self.ext_insert_idx(idx, eh);
            }
            return Ok(());
        }

        let old = self.entries[idx as usize];
        let name_off = self.append_name(raw.name.as_bytes(), true);
        let dir_idx = *self.dir_index.get(&raw.parent_id).unwrap_or(&0);
        let eh = hash_ext8(&raw.name);
        self.ext_remove_idx(idx, old.ext_hash_u8());
        if let Some(e) = self.entries.get_mut(idx as usize) {
            e.name_offset = name_off;
            e.n_len = raw.name.len() as u16;
            e.dir_idx = dir_idx;
            e.size = raw.size;
            e.mtime = filetime_to_unix_secs(raw.mtime);
            e.ctime = filetime_to_unix_secs(raw.ctime);
            e.attrs = FileEntry::pack_attrs_from_windows_file(raw.attrs) | ((eh as u32) << 8);
        }
        self.ext_insert_idx(idx, eh);
        let _ = old;
        Ok(())
    }

    fn apply_rename_mapped(&mut self, file_id: u64, new_parent_id: u64, new_name: &str) -> crate::Result<()> {
        let Some(&idx) = self.frn_to_entry.get(&file_id) else {
            return Ok(());
        };
        if self.deleted.contains(idx) {
            return Ok(());
        }
        let is_dir = self.entries[idx as usize].is_dir_entry();
        let mut raw = RawEntry {
            file_id,
            file_id_128: None,
            parent_id: new_parent_id,
            name: new_name.to_string(),
            size: self.entries[idx as usize].size,
            mtime: unix_secs_to_filetime(self.entries[idx as usize].mtime),
            ctime: unix_secs_to_filetime(self.entries[idx as usize].ctime),
            attrs: (self.entries[idx as usize].attrs & 0xff) as u32,
            is_dir,
        };
        if is_dir {
            raw.attrs |= 0x10;
        }
        self.upsert_raw_entry(&raw)?;
        Ok(())
    }

    /// 给定 RawEntry 的父目录 + 名字，按 `excluded_dirs` 做前缀过滤。
    /// 返回 true → 应当忽略本事件（不入库 / 不改名 / 不更新元数据）。
    ///
    /// 命中策略与 sidecar 规范一致：把 `<父目录完整路径>\<name>` 拼成小写带盘符的串，
    /// 匹配任一前缀即视为排除。父目录路径来自 dir_index → DirEntry → resolve_dir_path_lower。
    /// 父目录尚未在 dirs 中（USN 出现新顶层目录的极少数瞬时态）则不做过滤——下次扫到子项再判。
    fn excluded_for_raw(&self, raw_parent_id: u64, raw_name: &str) -> bool {
        if self.excluded_dirs.is_empty() {
            return false;
        }
        let Some(&parent_dir_idx) = self.dir_index.get(&raw_parent_id) else {
            return false;
        };
        let parent_path = self.resolve_dir_path_lower(parent_dir_idx);
        // resolve_dir_path_lower 不带盘符，根据 dir_idx 反查所属卷盘符。
        let letter = self.volume_letter_for_dir_idx(parent_dir_idx).to_ascii_lowercase();
        let mut full = String::with_capacity(parent_path.len() + raw_name.len() + 4);
        full.push(letter);
        full.push(':');
        full.push_str(std::str::from_utf8(parent_path.as_ref()).unwrap_or(""));
        if !full.ends_with('\\') {
            full.push('\\');
        }
        for c in raw_name.chars() {
            full.push(c.to_ascii_lowercase());
        }
        for ex in &self.excluded_dirs {
            // 前缀命中：`c:\windows\winsxs` 对 `c:\windows\winsxs\foo.dll` 与目录自身都生效。
            if full == *ex || full.starts_with(&format!("{ex}\\")) {
                return true;
            }
        }
        false
    }

    /// 通过 dir_idx 反查所属卷盘符；多卷场景里用 first_entry_idx 段判定不准（dirs 没有这个段标），
    /// 这里走回退策略：dir 的 frn 可能在某卷的 dir 树里，找不到时回退到第一卷。
    fn volume_letter_for_dir_idx(&self, _dir_idx: u32) -> char {
        // 当前 dirs 与 entries 是同卷线性段，这里没有显式的 dir 段表；
        // 简化：沿用「第一卷」盘符（实际多卷下 USN watch 是按卷各自跑，不会跨卷投递事件，
        // 真要精确需要拉 cross-store dir-volume 映射，超出本次需求范围）。
        self.volumes
            .first()
            .map(|v| (v.volume_letter as char).to_ascii_uppercase())
            .unwrap_or('C')
    }

    /// USN / 外部增量：Rename、目录 Create、`DataOrMeta` 重排排序向量、扩展名桶
    pub fn apply_change_event(&mut self, ev: &ChangeEvent) -> crate::Result<()> {
        match ev {
            ChangeEvent::Delete { file_id } => {
                if let Some(&idx) = self.frn_to_entry.get(file_id) {
                    self.delete_entry(idx);
                }
                Ok(())
            }
            ChangeEvent::Create { entry } => {
                if self.excluded_for_raw(entry.parent_id, &entry.name) {
                    return Ok(());
                }
                self.upsert_raw_entry(entry)?;
                Ok(())
            }
            ChangeEvent::Rename {
                file_id,
                new_parent_id,
                new_name,
            } => {
                // 改名后的新位置若在排除目录里，等同删除（旧位置已不存在，新位置不入库）。
                if self.excluded_for_raw(*new_parent_id, new_name) {
                    if let Some(&idx) = self.frn_to_entry.get(file_id) {
                        self.delete_entry(idx);
                    }
                    return Ok(());
                }
                self.apply_rename_mapped(*file_id, *new_parent_id, new_name)
            }
            ChangeEvent::DataOrMeta {
                file_id,
                size,
                mtime,
                ctime,
            } => {
                if let Some(&idx) = self.frn_to_entry.get(file_id) {
                    if let Some(e) = self.entries.get_mut(idx as usize) {
                        if let Some(s) = size {
                            e.size = *s;
                        }
                        if let Some(m) = mtime {
                            e.mtime = filetime_to_unix_secs(*m);
                        }
                        if let Some(c) = ctime {
                            e.ctime = filetime_to_unix_secs(*c);
                        }
                    }
                }
                Ok(())
            }
        }
    }

    /// 按 `VolumeState.first_entry_idx` 段选择盘符（多卷合并索引）。
    pub fn volume_letter_for_entry(&self, idx: usize) -> char {
        let idx = idx as u32;
        let mut letter = self.volumes.first().map(|v| v.volume_letter).unwrap_or(b'C');
        for v in &self.volumes {
            if idx >= v.first_entry_idx {
                letter = v.volume_letter;
            }
        }
        (letter as char).to_ascii_uppercase()
    }

    /// 重建 `C:\path\file` 显示路径（目录/文件条目均支持）
    pub fn entry_display_path(&self, idx: usize) -> crate::Result<String> {
        let letter = self.volume_letter_for_entry(idx);
        let e = self
            .entries
            .get(idx)
            .ok_or_else(|| crate::Error::Platform("条目下标越界".into()))?;
        let name = self.name_str(e)?.to_string();
        if e.is_dir_entry() {
            // 关键热路径修复：原先 self.dirs.iter().position(...) 是 O(D) 线性扫描，
            // 1000 个目录 hits × 100 万 dirs ≈ 10 亿次比较 ≈ 600ms（实测「android」就被这个挡住）。
            // dir_index 已是 FRN→dirs 下标的 FxHashMap，直接 O(1) 查询即可。
            let fr = self.frns.get(idx).copied().unwrap_or(0);
            if let Some(&di) = self.dir_index.get(&fr) {
                let pbytes = self.resolve_dir_path_lower(di);
                if !pbytes.is_empty() {
                    let p = std::str::from_utf8(pbytes.as_ref())?;
                    return Ok(format!("{}{}{}", letter, ':', p));
                }
            }
            return Ok(format!("{}:\\{}", letter, name));
        }
        let pbytes = self.resolve_dir_path_lower(e.dir_idx);
        if !pbytes.is_empty() {
            let p = std::str::from_utf8(pbytes.as_ref())?;
            Ok(format!("{}{}{}\\{}", letter, ':', p, name))
        } else {
            Ok(format!("{}:\\{}", letter, name))
        }
    }

    /// 回填元数据：更新 `size` / `mtime` / `ctime`；排序键在搜索收尾时按命中集现算，不再维护全局有序排列。
    pub fn patch_entry_metadata(
        &mut self,
        idx: usize,
        size: u64,
        mtime: u64,
        ctime: u64,
    ) -> crate::Result<()> {
        let Some(e) = self.entries.get_mut(idx) else {
            return Err(crate::Error::Platform("条目下标越界".into()));
        };
        e.size = size;
        e.mtime = filetime_to_unix_secs(mtime);
        e.ctime = filetime_to_unix_secs(ctime);
        Ok(())
    }
}

/// 「即时小写化」结果：搜索热路径调用 [`IndexStore::name_lower_into`] 返回。
/// 短名走借用栈缓冲；超长名兜底走堆分配（极少见）。
pub enum NameLowerCow<'a> {
    Borrowed(&'a [u8]),
    Owned(Vec<u8>),
}

impl<'a> std::ops::Deref for NameLowerCow<'a> {
    type Target = [u8];
    #[inline]
    fn deref(&self) -> &[u8] {
        match self {
            NameLowerCow::Borrowed(s) => s,
            NameLowerCow::Owned(v) => v.as_slice(),
        }
    }
}

/// 扩展名 8-bit 哈希（与 `ext_filter` 下标对齐）
pub fn hash_ext8(name: &str) -> u8 {
    let lower = name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    let mut h: u32 = 2166136261;
    for b in lower.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(16777619);
    }
    (h & 0xff) as u8
}

pub struct IndexBuilder {
    volume_letter: u8,
    volume_serial: u32,
    usn_journal_id: u64,
    last_usn: u64,
}

impl IndexBuilder {
    pub fn new(volume_letter: u8, volume_serial: u32, usn_journal_id: u64, last_usn: u64) -> Self {
        Self {
            volume_letter,
            volume_serial,
            usn_journal_id,
            last_usn,
        }
    }

    /// 从 `RawEntry` 列表构建索引（单卷简化版：假定 `parent_id` 已能解析为目录 FRN）
    ///
    /// `metadata_ready`：快速建库传 `false`（条目中 size/mtime 可能为占位）；全量扫描传 `true`。
    pub fn build_from_raw(
        &self,
        files: Vec<RawEntry>,
        dirs: Vec<RawEntry>,
        metadata_ready: bool,
    ) -> crate::Result<IndexStore> {
        // `scan_volume` 已将目录放入 `dirs`，`files` 中均为文件；勿再 filter 全表计数，否则百万级会先卡数秒且无日志。
        let dir_count = dirs.len();
        let file_count = files.len();
        crate::progress!(
            "索引：目录 {}、文件 {}，拓扑与父链 …（路径不物化，与 Everything 一致）",
            dir_count, file_count
        );
        let _ = std::io::stderr().flush();

        let mut names_buf: Vec<u8> = Vec::new();
        let mut dir_entries: Vec<DirEntry> = Vec::new();
        // BFS 阶段需要按 frn 反查父 idx：临时用 hashbrown，BFS 结束后立刻转换为
        // 紧凑的 sorted FrnIdxMap 释放内存。1.25M 目录 × 24B ≈ 30MB 是临时峰值，可接受。
        let mut dir_index_build: FxHashMap<u64, u32> = FxHashMap::default();
        dir_index_build.reserve(dir_count.saturating_mul(2).max(16));

        // 目录：BFS 拓扑（O(n)）。旧版「轮询 pending」在 USN 乱序时最坏可达 O(n²)，会在 50 万目录量级卡死。
        let frn_set: HashSet<u64> = dirs.iter().map(|d| d.file_id).collect();
        let mut children: HashMap<u64, Vec<RawEntry>> = HashMap::new();
        let mut roots: Vec<RawEntry> = Vec::new();
        for d in dirs {
            if d.parent_id == 0 || !frn_set.contains(&d.parent_id) {
                roots.push(d);
            } else {
                children.entry(d.parent_id).or_default().push(d);
            }
        }
        let mut queue: VecDeque<RawEntry> = VecDeque::from_iter(roots);
        let mut topo_done: u64 = 0;
        while let Some(d) = queue.pop_front() {
            topo_done += 1;
            if topo_done % 100_000 == 0 {
                crate::progress!("索引：目录拓扑 {}/{} …", topo_done, dir_count);
                let _ = std::io::stderr().flush();
            }
            let parent_idx = if d.parent_id == 0 || !frn_set.contains(&d.parent_id) {
                0u32
            } else {
                match dir_index_build.get(&d.parent_id) {
                    Some(&p) => p,
                    None => {
                        return Err(crate::Error::Platform(
                            "目录拓扑：父目录应在子目录之前（内部错误）".into(),
                        ));
                    }
                }
            };
            let name_off = names_buf.len() as u32;
            names_buf.extend_from_slice(d.name.as_bytes());
            let idx = dir_entries.len() as u32;
            dir_entries.push(DirEntry {
                frn: d.file_id,
                parent_idx,
                name_offset: name_off,
                name_len: d.name.len() as u16,
            });
            dir_index_build.insert(d.file_id, idx);
            if let Some(kids) = children.remove(&d.file_id) {
                for c in kids {
                    queue.push_back(c);
                }
            }
        }
        if dir_entries.len() != dir_count || !children.is_empty() {
            return Err(crate::Error::Platform(
                "无法为所有目录解析父链（环或缺失父目录 FRN），检查 RawEntry.parent_id".into(),
            ));
        }

        // 不物化整卷目录路径（与 Everything：仅存 FRN/父链 + 短名；path 过滤在查询时按需解析，见 `resolve_dir_path_lower`）。
        let dir_paths_buf: Vec<u8> = Vec::new();
        let dir_path_ranges: Vec<(u32, u32)> = vec![(0, 0); dir_entries.len()];

        let first_entry_idx = 0u32;
        let mut entries: Vec<FileEntry> = Vec::new();
        let mut frns: Vec<u64> = Vec::new();

        crate::progress!("索引：组装文件与目录条目 …");
        let _ = std::io::stderr().flush();

        let mut assembled_files: u64 = 0;
        for f in files {
            if f.is_dir {
                continue;
            }
            assembled_files += 1;
            if assembled_files % 300_000 == 0 {
                crate::progress!(
                    "索引：组装文件 {} / {} …",
                    assembled_files, file_count
                );
                let _ = std::io::stderr().flush();
            }
            let name_off = names_buf.len() as u32;
            names_buf.extend_from_slice(f.name.as_bytes());
            names_buf.push(0); // null 终止，便于整块扫描
            let dir_idx = dir_index_build.get(&f.parent_id).copied().unwrap_or(0);
            let eh = hash_ext8(&f.name);
            // 文件 attrs：`pack_attrs_from_windows_file` 处理 READONLY/ARCHIVE 与内部位冲突，见 `FileEntry` 上注释。
            let mut attrs = FileEntry::pack_attrs_from_windows_file(f.attrs);
            attrs |= (eh as u32) << 8;
            entries.push(FileEntry {
                size: f.size,
                mtime: filetime_to_unix_secs(f.mtime),
                ctime: filetime_to_unix_secs(f.ctime),
                name_offset: name_off,
                dir_idx,
                attrs,
                n_len: f.name.len() as u16,
                _pad: 0,
            });
            frns.push(f.file_id);
        }

        // 目录名也可检索（`folder:` / 与 Everything 行为对齐）
        for d in &dir_entries {
            let name_len = d.name_len as usize;
            let eh = hash_ext8(
                std::str::from_utf8(
                    &names_buf[d.name_offset as usize..d.name_offset as usize + name_len],
                )
                .unwrap_or(""),
            );
            let attrs = FileEntry::ATTR_IS_DIR | ((eh as u32) << 8);
            entries.push(FileEntry {
                size: 0,
                mtime: 0,
                ctime: 0,
                name_offset: d.name_offset,
                dir_idx: d.parent_idx,
                attrs,
                n_len: d.name_len,
                _pad: 0,
            });
            frns.push(d.frn);
        }

        crate::progress!("索引：FRN→条目映射（{} 条）…", frns.len());
        let _ = std::io::stderr().flush();

        // 紧凑 sorted Vec：O(N log N) 一次排序代替 N 次 hash insert，
        // 内存从 ~24B/项（hashbrown 控制位 + 50% 装填冗余 + 2x 预分配）降到 12B/项。
        // 8.5M 条目实测能省 ~100 MB 常驻 RSS。
        let mut frn_to_entry: FrnIdxMap = FrnIdxMap::with_capacity(frns.len());
        let n_frn = frns.len();
        for (i, frn) in frns.iter().enumerate() {
            if i > 0 && i % 1_000_000 == 0 {
                crate::progress!("索引：FRN→条目映射 push {}/{} …", i, n_frn);
                let _ = std::io::stderr().flush();
            }
            frn_to_entry.push_unsorted(*frn, i as u32);
        }
        crate::progress!("索引：FRN→条目映射 排序去重（{} 条）…", n_frn);
        let _ = std::io::stderr().flush();
        frn_to_entry.finalize_build();

        crate::progress!("索引：扩展名桶 …");
        let _ = std::io::stderr().flush();

        let n_entries = entries.len();
        // 先按哈希分桶（两遍：计数预分配 + 填入），再并行 sort + from_sorted_iter，比逐条 insert 进 Roaring 更快。
        let mut ext_counts = [0usize; 256];
        for e in entries.iter() {
            ext_counts[e.ext_hash_u8() as usize] += 1;
        }
        let mut ext_buckets: Vec<Vec<u32>> = ext_counts
            .iter()
            .map(|&c| Vec::with_capacity(c))
            .collect();
        for (i, e) in entries.iter().enumerate() {
            if i > 0 && i % 500_000 == 0 {
                crate::progress!("索引：扩展名桶 {}/{} …", i, n_entries);
                let _ = std::io::stderr().flush();
            }
            let h = e.ext_hash_u8() as usize;
            ext_buckets[h].push(i as u32);
        }
        let ext_vec: Vec<Option<RoaringBitmap>> = ext_buckets
            .into_par_iter()
            .map(|mut ids| {
                if ids.is_empty() {
                    None
                } else {
                    ids.sort_unstable();
                    Some(
                        RoaringBitmap::from_sorted_iter(ids.into_iter())
                            .expect("ext bucket sorted unique indices"),
                    )
                }
            })
            .collect();
        let ext_filter: [Option<RoaringBitmap>; 256] = ext_vec
            .try_into()
            .map_err(|_| crate::Error::Platform("扩展名桶数量异常".into()))?;

        // size/mtime/ctime 排序键由 `SearchEngine::finalize_hits` 对命中集现算；不再维护全局有序排列。
        crate::progress!(
            "索引：已跳过全量预排序（{} 条），排序在查询时按命中集计算",
            entries.len()
        );
        let _ = std::io::stderr().flush();

        let volumes = vec![VolumeState {
            volume_letter: self.volume_letter,
            volume_serial: self.volume_serial,
            usn_journal_id: self.usn_journal_id,
            last_usn: self.last_usn,
            first_entry_idx,
        }];

        // BFS 临时表 → 紧凑 sorted FrnIdxMap；hashbrown 在这里立刻释放。
        let mut dir_index = FrnIdxMap::with_capacity(dir_index_build.len());
        for (k, v) in dir_index_build.drain() {
            dir_index.push_unsorted(k, v);
        }
        dir_index.finalize_build();
        drop(dir_index_build);

        Ok(IndexStore {
            names_buf,
            entries,
            dirs: dir_entries,
            dir_index,
            dir_paths_buf,
            dir_path_ranges,
            volumes,
            ext_filter,
            deleted: RoaringBitmap::new(),
            frns,
            frn_to_entry,
            metadata_ready,
            excluded_dirs: Vec::new(),
        })
    }
}

/// 将多卷各自构建的 [`IndexStore`] 合并为单库（`volumes` 保留多段；`first_entry_idx` 重算）。
pub fn merge_index_stores(stores: Vec<IndexStore>) -> crate::Result<IndexStore> {
    if stores.is_empty() {
        return Err(crate::Error::Platform(
            "merge_index_stores: 至少需要一卷".into(),
        ));
    }
    if stores.len() == 1 {
        return Ok(stores.into_iter().next().expect("len checked"));
    }

    let metadata_ready = stores.iter().all(|s| s.metadata_ready);

    let mut merged_names: Vec<u8> = Vec::new();
    let mut merged_dirs: Vec<DirEntry> = Vec::new();
    let mut merged_entries: Vec<FileEntry> = Vec::new();
    let mut merged_frns: Vec<u64> = Vec::new();
    let mut merged_dir_paths_buf: Vec<u8> = Vec::new();
    let mut merged_dir_path_ranges: Vec<(u32, u32)> = Vec::new();
    let mut merged_volumes: Vec<VolumeState> = Vec::new();
    let mut merged_deleted: RoaringBitmap = RoaringBitmap::new();
    let mut dir_index: FrnIdxMap = FrnIdxMap::default();
    let mut frn_to_entry: FrnIdxMap = FrnIdxMap::default();

    let mut entry_base: u32 = 0;
    let mut dir_base: u32 = 0;

    for mut store in stores {
        let names_shift = merged_names.len() as u32;
        let n_dirs = store.dirs.len();
        let n_entries = store.entries.len();

        let dp_shift = merged_dir_paths_buf.len() as u32;
        merged_dir_paths_buf.extend(store.dir_paths_buf.drain(..));
        merged_names.extend(store.names_buf.drain(..));

        if store.dir_path_ranges.len() != store.dirs.len() {
            return Err(crate::Error::Platform(
                "合并失败：dir_path_ranges 与 dirs 长度不一致".into(),
            ));
        }
        for (mut d, (a, b)) in store
            .dirs
            .drain(..)
            .zip(store.dir_path_ranges.drain(..))
        {
            d.name_offset = d.name_offset.saturating_add(names_shift);
            if d.parent_idx != 0 {
                d.parent_idx = d.parent_idx.saturating_add(dir_base);
            }
            merged_dirs.push(d);
            merged_dir_path_ranges.push(if b > 0 {
                (a.saturating_add(dp_shift), b)
            } else {
                (0, 0)
            });
        }

        for mut e in store.entries.drain(..) {
            e.name_offset = e.name_offset.saturating_add(names_shift);
            e.dir_idx = e.dir_idx.saturating_add(dir_base);
            merged_entries.push(e);
        }

        merged_frns.extend(store.frns.drain(..));

        for (k, v) in store.dir_index.drain() {
            dir_index.push_unsorted(k, v.saturating_add(dir_base));
        }

        for (k, v) in store.frn_to_entry.drain() {
            frn_to_entry.push_unsorted(k, v.saturating_add(entry_base));
        }

        for idx in store.deleted.into_iter() {
            merged_deleted.insert(idx.saturating_add(entry_base));
        }

        for mut v in store.volumes.drain(..) {
            v.first_entry_idx = v.first_entry_idx.saturating_add(entry_base);
            merged_volumes.push(v);
        }

        dir_base = dir_base.saturating_add(n_dirs as u32);
        entry_base = entry_base.saturating_add(n_entries as u32);
    }

    debug_assert_eq!(merged_frns.len(), merged_entries.len());

    let n_entries = merged_entries.len();
    let mut ext_counts = [0usize; 256];
    for e in merged_entries.iter() {
        ext_counts[e.ext_hash_u8() as usize] += 1;
    }
    let mut ext_buckets: Vec<Vec<u32>> = ext_counts.iter().map(|&c| Vec::with_capacity(c)).collect();
    for (i, e) in merged_entries.iter().enumerate() {
        ext_buckets[e.ext_hash_u8() as usize].push(i as u32);
    }
    let ext_vec: Vec<Option<RoaringBitmap>> = ext_buckets
        .into_par_iter()
        .map(|mut ids| {
            if ids.is_empty() {
                None
            } else {
                ids.sort_unstable();
                Some(
                    RoaringBitmap::from_sorted_iter(ids.into_iter())
                        .expect("merge ext bucket sorted"),
                )
            }
        })
        .collect();
    let ext_filter: [Option<RoaringBitmap>; 256] = ext_vec
        .try_into()
        .map_err(|_| crate::Error::Platform("扩展名桶数量异常".into()))?;

    let _ = n_entries;

    // 各 store 的 sorted 段已被 push_unsorted 拼接，这里整体 sort+dedup 一次。
    dir_index.finalize_build();
    frn_to_entry.finalize_build();

    Ok(IndexStore {
        names_buf: merged_names,
        entries: merged_entries,
        dirs: merged_dirs,
        dir_index,
        dir_paths_buf: merged_dir_paths_buf,
        dir_path_ranges: merged_dir_path_ranges,
        volumes: merged_volumes,
        ext_filter,
        deleted: merged_deleted,
        frns: merged_frns,
        frn_to_entry,
        metadata_ready,
        excluded_dirs: Vec::new(),
    })
}
