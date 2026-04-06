use hashbrown::HashMap;
use pinyin::ToPinyin;
use rayon::prelude::*;
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::collections::HashSet;
use std::ops::Bound;

#[inline]
fn make_key(vol: u16, file_ref: u64) -> u64 {
    ((vol as u64) << 48) | (file_ref & 0x0000_FFFF_FFFF_FFFF)
}

#[derive(Clone)]
#[repr(C)]
pub struct Record {
    pub file_ref: u64,
    pub parent_ref: u64,
    pub name_start: u32,
    pub name_len: u32,
    pub py_start: u32,
    pub py_len: u32,
    /// 无 CJK 时与 initials 相同逻辑的无分隔小拼串；有 CJK 时为连续全拼小写 ASCII（如「你好」→ nihao）。
    pub full_py_start: u32,
    pub full_py_len: u32,
    pub attr: u32,
    pub size: i64,
    pub mtime: i64,
    pub vol: u16,
    pub deleted: u8,
    _pad: u8,
}

/// 与 .NET `StringComparison.OrdinalIgnoreCase` 接近：按 Unicode 标量做简单小写展开后比较。
fn cmp_name_str_ignore_case(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().flat_map(|c| c.to_lowercase());
    let mut bi = b.chars().flat_map(|c| c.to_lowercase());
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) => match ca.cmp(&cb) {
                Ordering::Equal => continue,
                o => return o,
            },
        }
    }
}

fn starts_with_ignore_case(hay: &str, needle: &str) -> bool {
    let mut h = hay.chars().flat_map(|c| c.to_lowercase());
    let mut n = needle.chars().flat_map(|c| c.to_lowercase());
    loop {
        match n.next() {
            None => return true,
            Some(nc) => match h.next() {
                None => return false,
                Some(hc) if hc != nc => return false,
                Some(_) => {}
            },
        }
    }
}

#[inline]
pub(crate) fn pool_utf8<'a>(pool: &'a [u8], start: u32, len: u32) -> &'a str {
    let s = start as usize;
    let e = s + len as usize;
    std::str::from_utf8(&pool[s..e]).unwrap_or("")
}

#[inline]
fn name_contains_cjk_for_pinyin(name: &str) -> bool {
    name.chars()
        .any(|c| matches!(c, '\u{4E00}'..='\u{9FFF}'))
}

fn compute_initials_fast_ascii_only(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        }
    }
    out
}

/// 与 C# PinyinMatcher 全拼轴一致用途：连续小写拼音 + ASCII 字母数字，供「nihao」类前缀秒搜。
fn compute_full_pinyin_compact(name: &str) -> String {
    let mut out = String::with_capacity(name.len().saturating_mul(3));
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            continue;
        }
        if let Some(py) = ch.to_pinyin() {
            out.push_str(&py.plain().to_lowercase());
        }
    }
    out
}

fn compute_initials(name: &str) -> String {
    if !name_contains_cjk_for_pinyin(name) {
        return compute_initials_fast_ascii_only(name);
    }
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            continue;
        }
        let mut pushed = false;
        if let Some(py) = ch.to_pinyin() {
            if let Some(c) = py.plain().chars().next() {
                out.push(c);
                pushed = true;
            }
        }
        if !pushed && !ch.is_ascii() {}
    }
    out
}

/// 与 `cmp_name_str_ignore_case` 一致的折叠键，用于 BTree 顺序。
fn fold_for_ord(s: &str) -> String {
    s.chars().flat_map(|c| c.to_lowercase()).collect()
}

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
struct NameOrdKey {
    folded: String,
    idx: u32,
}

#[derive(Clone, Eq, PartialEq)]
struct PyOrdKey {
    py_folded: String,
    name_folded: String,
    idx: u32,
}

impl Ord for PyOrdKey {
    fn cmp(&self, other: &Self) -> Ordering {
        let a_empty = self.py_folded.is_empty();
        let b_empty = other.py_folded.is_empty();
        if a_empty && b_empty {
            return cmp_name_str_ignore_case(&self.name_folded, &other.name_folded)
                .then_with(|| self.idx.cmp(&other.idx));
        }
        if a_empty {
            return Ordering::Greater;
        }
        if b_empty {
            return Ordering::Less;
        }
        cmp_name_str_ignore_case(&self.py_folded, &other.py_folded)
            .then_with(|| cmp_name_str_ignore_case(&self.name_folded, &other.name_folded))
            .then_with(|| self.idx.cmp(&other.idx))
    }
}

impl PartialOrd for PyOrdKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Default)]
pub struct Engine {
    pub records: Vec<Record>,
    pub name_pool: Vec<u8>,
    pub py_pool: Vec<u8>,
    pub full_py_pool: Vec<u8>,
    pub ref_to_idx: HashMap<u64, u32>,
    pub live_count: u32,
    /// 批量入库时 >0：只追加 records，结束时 rebuild_indexes 一次性建 BTree。
    pub bulk_mode: u32,
    name_btree: BTreeSet<NameOrdKey>,
    py_btree: BTreeSet<PyOrdKey>,
    full_py_btree: BTreeSet<NameOrdKey>,
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    fn name_key_at(&self, idx: u32) -> NameOrdKey {
        let r = &self.records[idx as usize];
        let name = pool_utf8(&self.name_pool, r.name_start, r.name_len);
        NameOrdKey {
            folded: fold_for_ord(name),
            idx,
        }
    }

    fn py_key_at(&self, idx: u32) -> PyOrdKey {
        let r = &self.records[idx as usize];
        let name = pool_utf8(&self.name_pool, r.name_start, r.name_len);
        let py = pool_utf8(&self.py_pool, r.py_start, r.py_len);
        PyOrdKey {
            py_folded: fold_for_ord(py),
            name_folded: fold_for_ord(name),
            idx,
        }
    }

    fn full_py_key_at(&self, idx: u32) -> NameOrdKey {
        let r = &self.records[idx as usize];
        let fp = pool_utf8(&self.full_py_pool, r.full_py_start, r.full_py_len);
        NameOrdKey {
            folded: fold_for_ord(fp),
            idx,
        }
    }

    pub fn begin_bulk(&mut self) {
        self.bulk_mode = self.bulk_mode.saturating_add(1);
    }

    pub fn end_bulk(&mut self) {
        self.bulk_mode = self.bulk_mode.saturating_sub(1);
        if self.bulk_mode == 0 {
            self.rebuild_indexes();
        }
    }

    pub fn clear(&mut self) {
        self.records.clear();
        self.name_pool.clear();
        self.py_pool.clear();
        self.full_py_pool.clear();
        self.ref_to_idx.clear();
        self.name_btree.clear();
        self.py_btree.clear();
        self.full_py_btree.clear();
        self.live_count = 0;
        self.bulk_mode = 0;
    }

    fn push_utf8(pool: &mut Vec<u8>, s: &str) -> (u32, u32) {
        let start = pool.len() as u32;
        pool.extend_from_slice(s.as_bytes());
        let len = s.len() as u32;
        (start, len)
    }

    pub fn add_entry_utf16(
        &mut self,
        vol: u16,
        file_ref: u64,
        parent_ref: u64,
        name_utf16: &[u16],
        attr: u32,
        size: i64,
        mtime: i64,
    ) {
        let name = String::from_utf16_lossy(name_utf16);
        let (ns, nl) = Self::push_utf8(&mut self.name_pool, &name);
        let (ps, pl, fs, fl) = if self.bulk_mode > 0 {
            (0u32, 0u32, 0u32, 0u32)
        } else {
            let initials = compute_initials(&name);
            let full_py = compute_full_pinyin_compact(&name);
            let (ps, pl) = Self::push_utf8(&mut self.py_pool, &initials);
            let (fs, fl) = Self::push_utf8(&mut self.full_py_pool, &full_py);
            (ps, pl, fs, fl)
        };
        let idx = self.records.len() as u32;
        self.records.push(Record {
            file_ref,
            parent_ref,
            name_start: ns,
            name_len: nl,
            py_start: ps,
            py_len: pl,
            full_py_start: fs,
            full_py_len: fl,
            attr,
            size,
            mtime,
            vol,
            deleted: 0,
            _pad: 0,
        });
        self.ref_to_idx.insert(make_key(vol, file_ref), idx);
        self.live_count += 1;
        if self.bulk_mode == 0 {
            self.name_btree.insert(NameOrdKey {
                folded: fold_for_ord(&name),
                idx,
            });
            self.py_btree.insert(self.py_key_at(idx));
            self.full_py_btree.insert(self.full_py_key_at(idx));
        }
    }

    pub fn upsert_entry_utf16(
        &mut self,
        vol: u16,
        file_ref: u64,
        parent_ref: u64,
        name_utf16: &[u16],
        attr: u32,
        size: i64,
        mtime: i64,
    ) {
        let key = make_key(vol, file_ref);
        let name = String::from_utf16_lossy(name_utf16);
        if let Some(&idx) = self.ref_to_idx.get(&key) {
            let was_live = self.records[idx as usize].deleted == 0;
            if was_live && self.bulk_mode == 0 {
                let nk = self.name_key_at(idx);
                let pk = self.py_key_at(idx);
                let fk = self.full_py_key_at(idx);
                self.name_btree.remove(&nk);
                self.py_btree.remove(&pk);
                self.full_py_btree.remove(&fk);
            }
            let r = &mut self.records[idx as usize];
            if r.deleted != 0 {
                r.deleted = 0;
                self.live_count += 1;
            }
            let (ps, pl, fs, fl) = if self.bulk_mode == 0 {
                let initials = compute_initials(&name);
                let full_py = compute_full_pinyin_compact(&name);
                let (ps, pl) = Self::push_utf8(&mut self.py_pool, &initials);
                let (fs, fl) = Self::push_utf8(&mut self.full_py_pool, &full_py);
                (ps, pl, fs, fl)
            } else {
                (0u32, 0u32, 0u32, 0u32)
            };
            let (ns, nl) = Self::push_utf8(&mut self.name_pool, &name);
            r.parent_ref = parent_ref;
            r.name_start = ns;
            r.name_len = nl;
            r.py_start = ps;
            r.py_len = pl;
            r.full_py_start = fs;
            r.full_py_len = fl;
            r.attr = attr;
            r.size = size;
            r.mtime = mtime;
            r.vol = vol;
            if self.bulk_mode == 0 {
                self.name_btree.insert(self.name_key_at(idx));
                self.py_btree.insert(self.py_key_at(idx));
                self.full_py_btree.insert(self.full_py_key_at(idx));
            }
            return;
        }
        self.add_entry_utf16(vol, file_ref, parent_ref, name_utf16, attr, size, mtime);
    }

    pub fn remove_by_ref(&mut self, vol: u16, file_ref: u64) {
        let key = make_key(vol, file_ref);
        if let Some(idx) = self.ref_to_idx.remove(&key) {
            let was_live = self.records[idx as usize].deleted == 0;
            if was_live {
                if self.bulk_mode == 0 {
                    let nk = self.name_key_at(idx);
                    let pk = self.py_key_at(idx);
                    let fk = self.full_py_key_at(idx);
                    self.name_btree.remove(&nk);
                    self.py_btree.remove(&pk);
                    self.full_py_btree.remove(&fk);
                }
                self.records[idx as usize].deleted = 1;
                self.live_count = self.live_count.saturating_sub(1);
            }
        }
    }

    pub(crate) fn sort_indexes_valid(&self) -> bool {
        if self.live_count == 0 {
            return self.name_btree.is_empty()
                && self.py_btree.is_empty()
                && self.full_py_btree.is_empty();
        }
        if self.bulk_mode > 0 {
            return false;
        }
        self.name_btree.len() == self.live_count as usize
            && self.py_btree.len() == self.live_count as usize
            && self.full_py_btree.len() == self.live_count as usize
    }

    #[inline]
    pub(crate) fn name_sort_index_empty(&self) -> bool {
        self.name_btree.is_empty()
    }

    #[inline]
    pub(crate) fn py_sort_index_empty(&self) -> bool {
        self.py_btree.is_empty()
    }

    #[inline]
    pub(crate) fn full_py_sort_index_empty(&self) -> bool {
        self.full_py_btree.is_empty()
    }

    pub fn rebuild_indexes(&mut self) {
        self.name_btree.clear();
        self.py_btree.clear();
        self.full_py_btree.clear();
        if self.live_count == 0 {
            return;
        }
        let mut live: Vec<u32> = Vec::with_capacity(self.live_count as usize);
        for (i, r) in self.records.iter().enumerate() {
            if r.deleted == 0 {
                live.push(i as u32);
            }
        }
        if live.is_empty() {
            self.live_count = 0;
            return;
        }

        const PY_FILL_CHUNK: usize = 65536;
        for chunk in live.chunks(PY_FILL_CHUNK) {
            let updates: Vec<(u32, String, String)> = {
                let records = &self.records;
                let name_pool = &self.name_pool;
                chunk
                    .par_iter()
                    .map(|&idx| {
                        let r = &records[idx as usize];
                        let name = pool_utf8(name_pool, r.name_start, r.name_len);
                        (
                            idx,
                            compute_initials(name),
                            compute_full_pinyin_compact(name),
                        )
                    })
                    .collect()
            };
            for (idx, ini, full) in updates {
                let i = idx as usize;
                let (ps, pl) = Self::push_utf8(&mut self.py_pool, &ini);
                let (fs, fl) = Self::push_utf8(&mut self.full_py_pool, &full);
                self.records[i].py_start = ps;
                self.records[i].py_len = pl;
                self.records[i].full_py_start = fs;
                self.records[i].full_py_len = fl;
            }
        }

        for &idx in &live {
            self.name_btree.insert(self.name_key_at(idx));
            self.py_btree.insert(self.py_key_at(idx));
            self.full_py_btree.insert(self.full_py_key_at(idx));
        }
    }

    pub fn search_name_prefix(&self, prefix: &str, out: &mut Vec<u32>, max_results: usize) {
        out.clear();
        if max_results == 0 {
            return;
        }
        if self.live_count > 0 && self.name_btree.is_empty() {
            return;
        }
        let pool = &self.name_pool;
        let records = &self.records;
        let start = NameOrdKey {
            folded: fold_for_ord(prefix),
            idx: 0,
        };
        for key in self
            .name_btree
            .range((Bound::Included(start), Bound::Unbounded))
        {
            if out.len() >= max_results {
                break;
            }
            let idx = key.idx;
            let r = &records[idx as usize];
            if r.deleted != 0 {
                continue;
            }
            let name = pool_utf8(pool, r.name_start, r.name_len);
            if starts_with_ignore_case(name, prefix) {
                out.push(idx);
                continue;
            }
            if cmp_name_str_ignore_case(name, prefix) == Ordering::Greater {
                break;
            }
        }
    }

    pub fn search_pinyin_prefix(&self, prefix_lower: &str, out: &mut Vec<u32>, max_results: usize) {
        out.clear();
        if max_results == 0 {
            return;
        }
        if self.live_count > 0 && self.py_btree.is_empty() {
            return;
        }
        let py_pool = &self.py_pool;
        let records = &self.records;
        let start = PyOrdKey {
            py_folded: fold_for_ord(prefix_lower),
            name_folded: String::new(),
            idx: 0,
        };
        for key in self
            .py_btree
            .range((Bound::Included(start), Bound::Unbounded))
        {
            if out.len() >= max_results {
                break;
            }
            let idx = key.idx;
            let r = &records[idx as usize];
            if r.deleted != 0 {
                continue;
            }
            let py = pool_utf8(py_pool, r.py_start, r.py_len);
            if py.is_empty() && !prefix_lower.is_empty() {
                break;
            }
            if starts_with_ignore_case(py, prefix_lower) {
                out.push(idx);
                continue;
            }
            if cmp_name_str_ignore_case(py, prefix_lower) == Ordering::Greater {
                break;
            }
        }
    }

    /// 连续全拼串前缀（如 nihao → 含「你好」的文件名）。
    pub fn search_full_py_prefix(&self, prefix: &str, out: &mut Vec<u32>, max_results: usize) {
        out.clear();
        if max_results == 0 {
            return;
        }
        if self.live_count > 0 && self.full_py_btree.is_empty() {
            return;
        }
        let fp_pool = &self.full_py_pool;
        let records = &self.records;
        let start = NameOrdKey {
            folded: fold_for_ord(prefix),
            idx: 0,
        };
        for key in self
            .full_py_btree
            .range((Bound::Included(start), Bound::Unbounded))
        {
            if out.len() >= max_results {
                break;
            }
            let idx = key.idx;
            let r = &records[idx as usize];
            if r.deleted != 0 {
                continue;
            }
            let fp = pool_utf8(fp_pool, r.full_py_start, r.full_py_len);
            if starts_with_ignore_case(fp, prefix) {
                out.push(idx);
                continue;
            }
            if cmp_name_str_ignore_case(fp, prefix) == Ordering::Greater {
                break;
            }
        }
    }

    pub fn get_name_utf16(&self, idx: i32, buf: &mut [u16]) -> usize {
        if idx < 0 || idx as usize >= self.records.len() {
            return 0;
        }
        let r = &self.records[idx as usize];
        if r.deleted != 0 {
            return 0;
        }
        let name = pool_utf8(&self.name_pool, r.name_start, r.name_len);
        let mut out = Vec::with_capacity(name.encode_utf16().count());
        out.extend(name.encode_utf16());
        let n = out.len().min(buf.len());
        buf[..n].copy_from_slice(&out[..n]);
        n
    }

    pub fn build_path_utf16(&self, idx: i32, buf: &mut [u16]) -> usize {
        if idx < 0 || idx as usize >= self.records.len() {
            return 0;
        }
        let mut cur = idx as u32;
        let mut visited = HashSet::new();
        let mut parts: Vec<(u32, u32)> = Vec::new();
        loop {
            if !visited.insert(cur) {
                break;
            }
            let r = match self.records.get(cur as usize) {
                Some(x) if x.deleted == 0 => x,
                _ => return 0,
            };
            parts.push((r.name_start, r.name_len));
            let pkey = make_key(r.vol, r.parent_ref);
            match self.ref_to_idx.get(&pkey).copied() {
                Some(p) if p != cur => cur = p,
                _ => {
                    let ch = char::from_u32(r.vol as u32).unwrap_or('?');
                    let root = format!("{}:", ch);
                    let mut total: Vec<u16> = root.encode_utf16().collect();
                    for (start, len) in parts.into_iter().rev() {
                        total.push('\\' as u16);
                        let name = pool_utf8(&self.name_pool, start, len);
                        total.extend(name.encode_utf16());
                    }
                    let n = total.len().min(buf.len());
                    buf[..n].copy_from_slice(&total[..n]);
                    return n;
                }
            }
        }
        0
    }

    pub fn get_meta(&self, idx: i32, out_size: &mut i64, out_mtime: &mut i64, out_attr: &mut u32) -> bool {
        if idx < 0 || idx as usize >= self.records.len() {
            return false;
        }
        let r = &self.records[idx as usize];
        if r.deleted != 0 {
            return false;
        }
        *out_size = r.size;
        *out_mtime = r.mtime;
        *out_attr = r.attr;
        true
    }

    pub fn get_live_record(
        &self,
        idx: i32,
        out_fr: &mut u64,
        out_pr: &mut u64,
        out_vol: &mut u16,
        out_attr: &mut u32,
        out_size: &mut i64,
        out_mtime: &mut i64,
    ) -> bool {
        if idx < 0 || idx as usize >= self.records.len() {
            return false;
        }
        let r = &self.records[idx as usize];
        if r.deleted != 0 {
            return false;
        }
        *out_fr = r.file_ref;
        *out_pr = r.parent_ref;
        *out_vol = r.vol;
        *out_attr = r.attr;
        *out_size = r.size;
        *out_mtime = r.mtime;
        true
    }

    pub fn visit_live(&self, mut f: impl FnMut(i32) -> bool) {
        for (i, r) in self.records.iter().enumerate() {
            if r.deleted != 0 {
                continue;
            }
            if !f(i as i32) {
                break;
            }
        }
    }

    pub fn try_live_index(&self, vol: u16, file_ref: u64) -> Option<i32> {
        let idx = *self.ref_to_idx.get(&make_key(vol, file_ref))? as usize;
        let r = self.records.get(idx)?;
        if r.deleted != 0 {
            return None;
        }
        Some(idx as i32)
    }
}
