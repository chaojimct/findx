use hashbrown::HashMap;
use pinyin::ToPinyin;
use rayon::prelude::*;
use std::cmp::Ordering;
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// time / size conversion helpers
// ---------------------------------------------------------------------------

const EPOCH_2000_TICKS: i64 = 630_822_816_000_000_000;
const TICKS_PER_SEC: i64 = 10_000_000;

#[inline]
fn mtime_to_compact(ticks: i64) -> u32 {
    if ticks <= EPOCH_2000_TICKS {
        return 0;
    }
    let secs = (ticks - EPOCH_2000_TICKS) / TICKS_PER_SEC;
    secs.min(u32::MAX as i64) as u32
}

#[inline]
fn compact_to_mtime(c: u32) -> i64 {
    EPOCH_2000_TICKS + (c as i64) * TICKS_PER_SEC
}

#[inline]
fn size_to_compact(s: i64) -> u32 {
    if s < 0 {
        return 0;
    }
    s.min(u32::MAX as i64) as u32
}

// ---------------------------------------------------------------------------
// Record – 40 bytes per entry (down from 72)
// Removed: py_start, py_len, full_py_start, full_py_len (computed on the fly)
// Shrunk:  size i64→u32, mtime i64→u32
// Added:   parent_idx (avoids HashMap lookup for path building)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
#[repr(C)]
pub struct Record {
    pub file_ref: u64,
    pub parent_ref: u64,
    pub name_start: u32,
    pub parent_idx: u32,
    pub size: u32,
    pub mtime: u32,
    pub name_len: u16,
    pub vol: u8,
    pub deleted: u8,
    pub attr: u32,
}

// ---------------------------------------------------------------------------
// string / pool utilities
// ---------------------------------------------------------------------------

#[inline]
fn make_key(vol: u16, file_ref: u64) -> u64 {
    ((vol as u64) << 48) | (file_ref & 0x0000_FFFF_FFFF_FFFF)
}

fn cmp_ignore_case(a: &[u8], b: &[u8]) -> Ordering {
    let la = a.len();
    let lb = b.len();
    let n = la.min(lb);
    for i in 0..n {
        let ca = a[i].to_ascii_lowercase();
        let cb = b[i].to_ascii_lowercase();
        match ca.cmp(&cb) {
            Ordering::Equal => continue,
            o => return o,
        }
    }
    la.cmp(&lb)
}

fn starts_with_ignore_case_bytes(hay: &[u8], needle: &[u8]) -> bool {
    if hay.len() < needle.len() {
        return false;
    }
    for i in 0..needle.len() {
        if hay[i].to_ascii_lowercase() != needle[i].to_ascii_lowercase() {
            return false;
        }
    }
    true
}

fn contains_ignore_case_bytes(hay: &[u8], needle: &[u8]) -> bool {
    let hl = hay.len();
    let nl = needle.len();
    if nl == 0 {
        return true;
    }
    if nl > hl {
        return false;
    }
    let first = needle[0].to_ascii_lowercase();
    'outer: for s in 0..=(hl - nl) {
        if hay[s].to_ascii_lowercase() != first {
            continue;
        }
        for j in 1..nl {
            if hay[s + j].to_ascii_lowercase() != needle[j].to_ascii_lowercase() {
                continue 'outer;
            }
        }
        return true;
    }
    false
}

fn contains_chars_lower(hay: &str, needle_low: &[char]) -> bool {
    if needle_low.is_empty() {
        return true;
    }
    let hay_low: Vec<char> = hay.chars().flat_map(|c| c.to_lowercase()).collect();
    if needle_low.len() > hay_low.len() {
        return false;
    }
    let first = needle_low[0];
    'outer: for s in 0..=(hay_low.len() - needle_low.len()) {
        if hay_low[s] != first {
            continue;
        }
        for j in 1..needle_low.len() {
            if hay_low[s + j] != needle_low[j] {
                continue 'outer;
            }
        }
        return true;
    }
    false
}

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

// ---------------------------------------------------------------------------
// pinyin helpers – zero-allocation stack-buffer versions for sort & search
// ---------------------------------------------------------------------------

#[inline]
fn is_cjk(c: char) -> bool {
    matches!(c, '\u{4E00}'..='\u{9FFF}')
}

fn name_contains_cjk(name: &str) -> bool {
    name.chars().any(|c| is_cjk(c))
}

fn compute_initials_stack(name: &str) -> ([u8; 256], usize) {
    let mut buf = [0u8; 256];
    let mut len = 0;
    let has_cjk = name_contains_cjk(name);
    for ch in name.chars() {
        if len >= 256 {
            break;
        }
        if ch.is_ascii_alphanumeric() {
            buf[len] = ch.to_ascii_lowercase() as u8;
            len += 1;
        } else if has_cjk {
            if let Some(py) = ch.to_pinyin() {
                if let Some(b) = py.plain().bytes().next() {
                    buf[len] = b;
                    len += 1;
                }
            }
        }
    }
    (buf, len)
}

fn compute_full_py_stack(name: &str) -> ([u8; 1024], usize) {
    let mut buf = [0u8; 1024];
    let mut len = 0;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            if len < 1024 {
                buf[len] = ch.to_ascii_lowercase() as u8;
                len += 1;
            }
        } else if let Some(py) = ch.to_pinyin() {
            for b in py.plain().bytes() {
                if len < 1024 {
                    buf[len] = b;
                    len += 1;
                }
            }
        }
    }
    (buf, len)
}


// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Engine {
    pub records: Vec<Record>,
    pub name_pool: Vec<u8>,

    // ref lookup: sorted parallel arrays (compact, steady state)
    ref_keys: Vec<u64>,
    ref_vals: Vec<u32>,
    // lazy HashMap for incremental ops (created on demand, freed on rebuild)
    ref_map: Option<HashMap<u64, u32>>,

    pub live_count: u32,
    pub bulk_mode: u32,
    name_sorted: Vec<u32>,
    py_sorted: Vec<u32>,
    full_py_sorted: Vec<u32>,
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    // -----------------------------------------------------------------------
    // ref lookup helpers
    // -----------------------------------------------------------------------

    fn find_ref(&self, key: u64) -> Option<u32> {
        if let Some(map) = &self.ref_map {
            return map.get(&key).copied();
        }
        let pos = self.ref_keys.partition_point(|&k| k < key);
        if pos < self.ref_keys.len() && self.ref_keys[pos] == key {
            Some(self.ref_vals[pos])
        } else {
            None
        }
    }

    fn ensure_ref_map(&mut self) {
        if self.ref_map.is_some() {
            return;
        }
        let mut map = HashMap::with_capacity(self.ref_keys.len());
        for i in 0..self.ref_keys.len() {
            map.insert(self.ref_keys[i], self.ref_vals[i]);
        }
        self.ref_map = Some(map);
        self.ref_keys = Vec::new();
        self.ref_vals = Vec::new();
    }

    fn insert_ref(&mut self, key: u64, val: u32) {
        self.ensure_ref_map();
        self.ref_map.as_mut().unwrap().insert(key, val);
    }

    fn remove_ref(&mut self, key: u64) -> Option<u32> {
        self.ensure_ref_map();
        self.ref_map.as_mut().unwrap().remove(&key)
    }

    fn compact_ref_lookup(&mut self) {
        let cap = self.live_count as usize;
        let mut keys = Vec::with_capacity(cap);
        let mut vals = Vec::with_capacity(cap);

        if let Some(map) = self.ref_map.take() {
            let mut pairs: Vec<(u64, u32)> = map.into_iter().collect();
            pairs.sort_unstable_by_key(|&(k, _)| k);
            for (k, v) in pairs {
                keys.push(k);
                vals.push(v);
            }
        }
        self.ref_keys = keys;
        self.ref_vals = vals;
    }

    // -----------------------------------------------------------------------
    // sorted-array helpers (binary insert / remove)
    // -----------------------------------------------------------------------

    fn sorted_insert_name(&mut self, idx: u32) {
        let records = &self.records;
        let pool = &self.name_pool;
        let pos = self.name_sorted.partition_point(|&o| {
            let ro = &records[o as usize];
            let ri = &records[idx as usize];
            let no = pool_utf8(pool, ro.name_start, ro.name_len as u32);
            let ni = pool_utf8(pool, ri.name_start, ri.name_len as u32);
            cmp_name_str_ignore_case(no, ni).then_with(|| o.cmp(&idx)) == Ordering::Less
        });
        self.name_sorted.insert(pos, idx);
    }

    fn sorted_remove_name(&mut self, idx: u32) {
        let records = &self.records;
        let pool = &self.name_pool;
        let pos = self.name_sorted.partition_point(|&o| {
            let ro = &records[o as usize];
            let ri = &records[idx as usize];
            let no = pool_utf8(pool, ro.name_start, ro.name_len as u32);
            let ni = pool_utf8(pool, ri.name_start, ri.name_len as u32);
            cmp_name_str_ignore_case(no, ni).then_with(|| o.cmp(&idx)) == Ordering::Less
        });
        if pos < self.name_sorted.len() && self.name_sorted[pos] == idx {
            self.name_sorted.remove(pos);
        }
    }

    fn sorted_insert_py(&mut self, idx: u32) {
        let records = &self.records;
        let pool = &self.name_pool;
        let ri = &records[idx as usize];
        let ni = pool_utf8(pool, ri.name_start, ri.name_len as u32);
        let (bi, li) = compute_initials_stack(ni);
        let pi = &bi[..li];
        let pos = self.py_sorted.partition_point(|&o| {
            let ro = &records[o as usize];
            let no = pool_utf8(pool, ro.name_start, ro.name_len as u32);
            let (bo, lo) = compute_initials_stack(no);
            let po = &bo[..lo];
            cmp_ignore_case(po, pi).then_with(|| o.cmp(&idx)) == Ordering::Less
        });
        self.py_sorted.insert(pos, idx);
    }

    fn sorted_remove_py(&mut self, idx: u32) {
        let records = &self.records;
        let pool = &self.name_pool;
        let ri = &records[idx as usize];
        let ni = pool_utf8(pool, ri.name_start, ri.name_len as u32);
        let (bi, li) = compute_initials_stack(ni);
        let pi = &bi[..li];
        let pos = self.py_sorted.partition_point(|&o| {
            let ro = &records[o as usize];
            let no = pool_utf8(pool, ro.name_start, ro.name_len as u32);
            let (bo, lo) = compute_initials_stack(no);
            let po = &bo[..lo];
            cmp_ignore_case(po, pi).then_with(|| o.cmp(&idx)) == Ordering::Less
        });
        if pos < self.py_sorted.len() && self.py_sorted[pos] == idx {
            self.py_sorted.remove(pos);
        }
    }

    fn sorted_insert_full_py(&mut self, idx: u32) {
        let records = &self.records;
        let pool = &self.name_pool;
        let ri = &records[idx as usize];
        let ni = pool_utf8(pool, ri.name_start, ri.name_len as u32);
        let (bi, li) = compute_full_py_stack(ni);
        let pi = &bi[..li];
        let pos = self.full_py_sorted.partition_point(|&o| {
            let ro = &records[o as usize];
            let no = pool_utf8(pool, ro.name_start, ro.name_len as u32);
            let (bo, lo) = compute_full_py_stack(no);
            let po = &bo[..lo];
            cmp_ignore_case(po, pi).then_with(|| o.cmp(&idx)) == Ordering::Less
        });
        self.full_py_sorted.insert(pos, idx);
    }

    fn sorted_remove_full_py(&mut self, idx: u32) {
        let records = &self.records;
        let pool = &self.name_pool;
        let ri = &records[idx as usize];
        let ni = pool_utf8(pool, ri.name_start, ri.name_len as u32);
        let (bi, li) = compute_full_py_stack(ni);
        let pi = &bi[..li];
        let pos = self.full_py_sorted.partition_point(|&o| {
            let ro = &records[o as usize];
            let no = pool_utf8(pool, ro.name_start, ro.name_len as u32);
            let (bo, lo) = compute_full_py_stack(no);
            let po = &bo[..lo];
            cmp_ignore_case(po, pi).then_with(|| o.cmp(&idx)) == Ordering::Less
        });
        if pos < self.full_py_sorted.len() && self.full_py_sorted[pos] == idx {
            self.full_py_sorted.remove(pos);
        }
    }

    // -----------------------------------------------------------------------
    // bulk lifecycle
    // -----------------------------------------------------------------------

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
        self.ref_keys.clear();
        self.ref_vals.clear();
        self.ref_map = None;
        self.name_sorted.clear();
        self.py_sorted.clear();
        self.full_py_sorted.clear();
        self.live_count = 0;
        self.bulk_mode = 0;
    }

    fn push_utf8(pool: &mut Vec<u8>, s: &str) -> (u32, u16) {
        let start = pool.len() as u32;
        pool.extend_from_slice(s.as_bytes());
        let len = s.len().min(u16::MAX as usize) as u16;
        (start, len)
    }

    // -----------------------------------------------------------------------
    // entry mutation
    // -----------------------------------------------------------------------

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
        let idx = self.records.len() as u32;
        self.records.push(Record {
            file_ref,
            parent_ref,
            name_start: ns,
            parent_idx: u32::MAX,
            size: size_to_compact(size),
            mtime: mtime_to_compact(mtime),
            name_len: nl,
            vol: vol as u8,
            deleted: 0,
            attr,
        });
        let key = make_key(vol, file_ref);
        self.insert_ref(key, idx);
        self.live_count += 1;
        if self.bulk_mode == 0 {
            self.sorted_insert_name(idx);
            self.sorted_insert_py(idx);
            self.sorted_insert_full_py(idx);
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
        if let Some(idx) = self.find_ref(key) {
            let was_live = self.records[idx as usize].deleted == 0;
            if was_live && self.bulk_mode == 0 {
                self.sorted_remove_name(idx);
                self.sorted_remove_py(idx);
                self.sorted_remove_full_py(idx);
            }
            let r = &mut self.records[idx as usize];
            if r.deleted != 0 {
                r.deleted = 0;
                self.live_count += 1;
            }
            let (ns, nl) = Self::push_utf8(&mut self.name_pool, &name);
            let r = &mut self.records[idx as usize];
            r.parent_ref = parent_ref;
            r.parent_idx = u32::MAX;
            r.name_start = ns;
            r.name_len = nl;
            r.attr = attr;
            r.size = size_to_compact(size);
            r.mtime = mtime_to_compact(mtime);
            r.vol = vol as u8;
            if self.bulk_mode == 0 {
                self.sorted_insert_name(idx);
                self.sorted_insert_py(idx);
                self.sorted_insert_full_py(idx);
            }
            return;
        }
        self.add_entry_utf16(vol, file_ref, parent_ref, name_utf16, attr, size, mtime);
    }

    pub fn remove_by_ref(&mut self, vol: u16, file_ref: u64) {
        let key = make_key(vol, file_ref);
        if let Some(idx) = self.remove_ref(key) {
            let was_live = self.records[idx as usize].deleted == 0;
            if was_live {
                if self.bulk_mode == 0 {
                    self.sorted_remove_name(idx);
                    self.sorted_remove_py(idx);
                    self.sorted_remove_full_py(idx);
                }
                self.records[idx as usize].deleted = 1;
                self.live_count = self.live_count.saturating_sub(1);
            }
        }
    }

    // -----------------------------------------------------------------------
    // index state queries
    // -----------------------------------------------------------------------

    pub(crate) fn sort_indexes_valid(&self) -> bool {
        if self.live_count == 0 {
            return self.name_sorted.is_empty()
                && self.py_sorted.is_empty()
                && self.full_py_sorted.is_empty();
        }
        if self.bulk_mode > 0 {
            return false;
        }
        self.name_sorted.len() == self.live_count as usize
            && self.py_sorted.len() == self.live_count as usize
            && self.full_py_sorted.len() == self.live_count as usize
    }

    #[inline]
    pub(crate) fn name_sort_index_empty(&self) -> bool {
        self.name_sorted.is_empty()
    }

    #[inline]
    pub(crate) fn py_sort_index_empty(&self) -> bool {
        self.py_sorted.is_empty()
    }

    #[inline]
    pub(crate) fn full_py_sort_index_empty(&self) -> bool {
        self.full_py_sorted.is_empty()
    }

    // -----------------------------------------------------------------------
    // rebuild_indexes: compact name_pool, resolve parent_idx, parallel sort
    // pinyin computed on-the-fly in comparators (py_pool/full_py_pool gone)
    // -----------------------------------------------------------------------

    pub fn rebuild_indexes(&mut self) {
        let t_total = std::time::Instant::now();
        self.name_sorted.clear();
        self.py_sorted.clear();
        self.full_py_sorted.clear();
        if self.live_count == 0 {
            self.compact_ref_lookup();
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
            self.compact_ref_lookup();
            return;
        }

        let t0 = std::time::Instant::now();
        // Phase 1: compact name_pool with deduplication
        let mut new_name_pool: Vec<u8> = Vec::new();
        let mut dedup: HashMap<&[u8], (u32, u16)> = HashMap::with_capacity(live.len() / 2);
        for &idx in &live {
            let r = &self.records[idx as usize];
            let name_bytes = &self.name_pool
                [r.name_start as usize..(r.name_start as usize + r.name_len as usize)];
            if let Some(&(start, len)) = dedup.get(name_bytes) {
                self.records[idx as usize].name_start = start;
                self.records[idx as usize].name_len = len;
            } else {
                let ns = new_name_pool.len() as u32;
                let nl = name_bytes.len().min(u16::MAX as usize) as u16;
                new_name_pool.extend_from_slice(name_bytes);
                dedup.insert(
                    &self.name_pool
                        [r.name_start as usize..(r.name_start as usize + r.name_len as usize)],
                    (ns, nl),
                );
                self.records[idx as usize].name_start = ns;
                self.records[idx as usize].name_len = nl;
            }
        }
        drop(dedup);
        self.name_pool = new_name_pool;

        let d1 = t0.elapsed();
        // Phase 2: resolve parent_idx using ref lookup
        let t1 = std::time::Instant::now();
        for &idx in &live {
            let r = &self.records[idx as usize];
            let pkey = make_key(r.vol as u16, r.parent_ref);
            let pidx = self.find_ref(pkey).unwrap_or(u32::MAX);
            self.records[idx as usize].parent_idx = pidx;
        }

        let d2 = t1.elapsed();
        // Phase 3: compact ref lookup (HashMap → sorted arrays, free HashMap)
        let t2 = std::time::Instant::now();
        self.compact_ref_lookup();
        let d3 = t2.elapsed();

        // Phase 4: parallel sort with on-the-fly pinyin computation
        let t3 = std::time::Instant::now();
        self.name_sorted = live.clone();
        self.py_sorted = live.clone();
        self.full_py_sorted = live;

        let records = &self.records;
        let name_pool = &self.name_pool;

        self.name_sorted.par_sort_unstable_by(|&a, &b| {
            let ra = &records[a as usize];
            let rb = &records[b as usize];
            let na = pool_utf8(name_pool, ra.name_start, ra.name_len as u32);
            let nb = pool_utf8(name_pool, rb.name_start, rb.name_len as u32);
            cmp_name_str_ignore_case(na, nb).then_with(|| a.cmp(&b))
        });

        self.py_sorted.par_sort_unstable_by(|&a, &b| {
            let ra = &records[a as usize];
            let rb = &records[b as usize];
            let na = pool_utf8(name_pool, ra.name_start, ra.name_len as u32);
            let nb = pool_utf8(name_pool, rb.name_start, rb.name_len as u32);
            let (ba, la) = compute_initials_stack(na);
            let (bb, lb) = compute_initials_stack(nb);
            let pa = &ba[..la];
            let pb = &bb[..lb];
            let ae = pa.is_empty();
            let be = pb.is_empty();
            if ae && be {
                return cmp_name_str_ignore_case(na, nb).then_with(|| a.cmp(&b));
            }
            if ae {
                return Ordering::Greater;
            }
            if be {
                return Ordering::Less;
            }
            cmp_ignore_case(pa, pb)
                .then_with(|| cmp_name_str_ignore_case(na, nb))
                .then_with(|| a.cmp(&b))
        });

        self.full_py_sorted.par_sort_unstable_by(|&a, &b| {
            let ra = &records[a as usize];
            let rb = &records[b as usize];
            let na = pool_utf8(name_pool, ra.name_start, ra.name_len as u32);
            let nb = pool_utf8(name_pool, rb.name_start, rb.name_len as u32);
            let (ba, la) = compute_full_py_stack(na);
            let (bb, lb) = compute_full_py_stack(nb);
            cmp_ignore_case(&ba[..la], &bb[..lb]).then_with(|| a.cmp(&b))
        });

        let d4 = t3.elapsed();
        eprintln!(
            "[findx_engine] rebuild_indexes: live={} P1_namepool={:.1}s P2_parent={:.1}s P3_reflookup={:.1}s P4_sort={:.1}s total={:.1}s",
            self.live_count, d1.as_secs_f64(), d2.as_secs_f64(), d3.as_secs_f64(), d4.as_secs_f64(), t_total.elapsed().as_secs_f64()
        );
        // Release excess capacity from all Vecs
        self.records.shrink_to_fit();
        self.name_pool.shrink_to_fit();
        self.ref_keys.shrink_to_fit();
        self.ref_vals.shrink_to_fit();
        self.name_sorted.shrink_to_fit();
        self.py_sorted.shrink_to_fit();
        self.full_py_sorted.shrink_to_fit();
    }

    // -----------------------------------------------------------------------
    // prefix search (pinyin computed on the fly from name_pool)
    // -----------------------------------------------------------------------

    pub fn search_name_prefix(&self, prefix: &str, out: &mut Vec<u32>, max_results: usize) {
        out.clear();
        if max_results == 0 || (self.live_count > 0 && self.name_sorted.is_empty()) {
            return;
        }
        let records = &self.records;
        let pool = &self.name_pool;
        let pos = self.name_sorted.partition_point(|&idx| {
            let r = &records[idx as usize];
            let name = pool_utf8(pool, r.name_start, r.name_len as u32);
            cmp_name_str_ignore_case(name, prefix) == Ordering::Less
        });
        for &idx in &self.name_sorted[pos..] {
            if out.len() >= max_results {
                break;
            }
            let r = &records[idx as usize];
            let name = pool_utf8(pool, r.name_start, r.name_len as u32);
            if starts_with_ignore_case(name, prefix) {
                out.push(idx);
            } else {
                break;
            }
        }
    }

    pub fn search_pinyin_prefix(
        &self,
        prefix_lower: &str,
        out: &mut Vec<u32>,
        max_results: usize,
    ) {
        out.clear();
        if max_results == 0 || (self.live_count > 0 && self.py_sorted.is_empty()) {
            return;
        }
        let prefix = prefix_lower.as_bytes();
        let records = &self.records;
        let name_pool = &self.name_pool;
        let pos = self.py_sorted.partition_point(|&idx| {
            let r = &records[idx as usize];
            let name = pool_utf8(name_pool, r.name_start, r.name_len as u32);
            let (buf, len) = compute_initials_stack(name);
            let py = &buf[..len];
            if py.is_empty() {
                return false;
            }
            cmp_ignore_case(py, prefix) == Ordering::Less
        });
        for &idx in &self.py_sorted[pos..] {
            if out.len() >= max_results {
                break;
            }
            let r = &records[idx as usize];
            let name = pool_utf8(name_pool, r.name_start, r.name_len as u32);
            let (buf, len) = compute_initials_stack(name);
            let py = &buf[..len];
            if py.is_empty() {
                break;
            }
            if starts_with_ignore_case_bytes(py, prefix) {
                out.push(idx);
            } else {
                break;
            }
        }
    }

    pub fn search_full_py_prefix(&self, prefix: &str, out: &mut Vec<u32>, max_results: usize) {
        out.clear();
        if max_results == 0 || (self.live_count > 0 && self.full_py_sorted.is_empty()) {
            return;
        }
        let prefix_bytes = prefix.as_bytes();
        let records = &self.records;
        let name_pool = &self.name_pool;
        let pos = self.full_py_sorted.partition_point(|&idx| {
            let r = &records[idx as usize];
            let name = pool_utf8(name_pool, r.name_start, r.name_len as u32);
            let (buf, len) = compute_full_py_stack(name);
            let fp = &buf[..len];
            cmp_ignore_case(fp, prefix_bytes) == Ordering::Less
        });
        for &idx in &self.full_py_sorted[pos..] {
            if out.len() >= max_results {
                break;
            }
            let r = &records[idx as usize];
            let name = pool_utf8(name_pool, r.name_start, r.name_len as u32);
            let (buf, len) = compute_full_py_stack(name);
            let fp = &buf[..len];
            if starts_with_ignore_case_bytes(fp, prefix_bytes) {
                out.push(idx);
            } else {
                break;
            }
        }
    }

    // -----------------------------------------------------------------------
    // data access
    // -----------------------------------------------------------------------

    pub fn get_name_utf16(&self, idx: i32, buf: &mut [u16]) -> usize {
        if idx < 0 || idx as usize >= self.records.len() {
            return 0;
        }
        let r = &self.records[idx as usize];
        if r.deleted != 0 {
            return 0;
        }
        let name = pool_utf8(&self.name_pool, r.name_start, r.name_len as u32);
        let mut i = 0;
        for u in name.encode_utf16() {
            if i >= buf.len() {
                break;
            }
            buf[i] = u;
            i += 1;
        }
        i
    }

    pub fn build_path_utf16(&self, idx: i32, buf: &mut [u16]) -> usize {
        if idx < 0 || idx as usize >= self.records.len() {
            return 0;
        }
        let mut cur = idx as u32;
        let mut visited = HashSet::new();
        let mut parts: Vec<(u32, u16)> = Vec::new();
        loop {
            if !visited.insert(cur) {
                break;
            }
            let r = match self.records.get(cur as usize) {
                Some(x) if x.deleted == 0 => x,
                _ => return 0,
            };
            parts.push((r.name_start, r.name_len));
            let pidx = r.parent_idx;
            if pidx != u32::MAX && pidx != cur {
                cur = pidx;
            } else {
                let ch = char::from_u32(r.vol as u32).unwrap_or('?');
                let root = format!("{}:", ch);
                let mut total: Vec<u16> = root.encode_utf16().collect();
                for (start, len) in parts.into_iter().rev() {
                    total.push('\\' as u16);
                    let name = pool_utf8(&self.name_pool, start, len as u32);
                    total.extend(name.encode_utf16());
                }
                let n = total.len().min(buf.len());
                buf[..n].copy_from_slice(&total[..n]);
                return n;
            }
        }
        0
    }

    pub fn get_meta(
        &self,
        idx: i32,
        out_size: &mut i64,
        out_mtime: &mut i64,
        out_attr: &mut u32,
    ) -> bool {
        if idx < 0 || idx as usize >= self.records.len() {
            return false;
        }
        let r = &self.records[idx as usize];
        if r.deleted != 0 {
            return false;
        }
        *out_size = r.size as i64;
        *out_mtime = compact_to_mtime(r.mtime);
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
        *out_vol = r.vol as u16;
        *out_attr = r.attr;
        *out_size = r.size as i64;
        *out_mtime = compact_to_mtime(r.mtime);
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

    pub fn search_name_contains(&self, needle: &str, out: &mut Vec<u32>, max_results: usize) {
        out.clear();
        if max_results == 0 || needle.is_empty() {
            return;
        }
        let needle_has_cjk = needle.chars().any(|c| is_cjk(c));
        let needle_low: Vec<char> = needle.chars().flat_map(|c| c.to_lowercase()).collect();
        for (i, r) in self.records.iter().enumerate() {
            if r.deleted != 0 {
                continue;
            }
            if out.len() >= max_results {
                break;
            }
            let name = pool_utf8(&self.name_pool, r.name_start, r.name_len as u32);
            if needle_has_cjk && !name_contains_cjk(name) {
                continue;
            }
            if contains_chars_lower(name, &needle_low) {
                out.push(i as u32);
            }
        }
    }

    pub fn search_full_py_contains(&self, needle: &str, out: &mut Vec<u32>, max_results: usize) {
        out.clear();
        if max_results == 0 || needle.is_empty() {
            return;
        }
        let needle_bytes = needle.as_bytes();
        for (i, r) in self.records.iter().enumerate() {
            if r.deleted != 0 {
                continue;
            }
            if out.len() >= max_results {
                break;
            }
            let name = pool_utf8(&self.name_pool, r.name_start, r.name_len as u32);
            if !name_contains_cjk(name) {
                continue;
            }
            let (buf, len) = compute_full_py_stack(name);
            let fp = &buf[..len];
            if contains_ignore_case_bytes(fp, needle_bytes) {
                out.push(i as u32);
            }
        }
    }

    pub fn search_initials_contains(&self, needle: &str, out: &mut Vec<u32>, max_results: usize) {
        out.clear();
        if max_results == 0 || needle.is_empty() {
            return;
        }
        let needle_bytes = needle.as_bytes();
        for (i, r) in self.records.iter().enumerate() {
            if r.deleted != 0 {
                continue;
            }
            if out.len() >= max_results {
                break;
            }
            let name = pool_utf8(&self.name_pool, r.name_start, r.name_len as u32);
            if !name_contains_cjk(name) {
                continue;
            }
            let (buf, len) = compute_initials_stack(name);
            let initials = &buf[..len];
            if contains_ignore_case_bytes(initials, needle_bytes) {
                out.push(i as u32);
            }
        }
    }

    pub fn try_live_index(&self, vol: u16, file_ref: u64) -> Option<i32> {
        let key = make_key(vol, file_ref);
        let idx = self.find_ref(key)? as usize;
        let r = self.records.get(idx)?;
        if r.deleted != 0 {
            return None;
        }
        Some(idx as i32)
    }

    // -------------------------------------------------------------------
    // binary persistence: save/load entire engine state to skip rebuild
    // Format: MAGIC(8) + header(20) + records + name_pool + ref + sorted×3
    // -------------------------------------------------------------------

    const BIN_MAGIC: &'static [u8; 8] = b"FXBIN02\0";

    pub fn save_to_file(&self, path: &str) -> std::io::Result<u64> {
        use std::io::{BufWriter, Write};

        let f = std::fs::File::create(path)?;
        let mut w = BufWriter::with_capacity(1 << 20, f);

        w.write_all(Self::BIN_MAGIC)?;

        let records_count = self.records.len() as u32;
        let name_pool_len = self.name_pool.len() as u32;
        let ref_keys_len = self.ref_keys.len() as u32;

        w.write_all(&records_count.to_le_bytes())?;
        w.write_all(&self.live_count.to_le_bytes())?;
        w.write_all(&name_pool_len.to_le_bytes())?;
        w.write_all(&ref_keys_len.to_le_bytes())?;
        w.write_all(&self.bulk_mode.to_le_bytes())?;

        let rec_bytes = unsafe {
            std::slice::from_raw_parts(
                self.records.as_ptr() as *const u8,
                self.records.len() * std::mem::size_of::<Record>(),
            )
        };
        w.write_all(rec_bytes)?;
        w.write_all(&self.name_pool)?;

        let rk_bytes = unsafe {
            std::slice::from_raw_parts(
                self.ref_keys.as_ptr() as *const u8,
                self.ref_keys.len() * 8,
            )
        };
        w.write_all(rk_bytes)?;
        let rv_bytes = unsafe {
            std::slice::from_raw_parts(
                self.ref_vals.as_ptr() as *const u8,
                self.ref_vals.len() * 4,
            )
        };
        w.write_all(rv_bytes)?;

        for arr in [&self.name_sorted, &self.py_sorted, &self.full_py_sorted] {
            let bytes = unsafe {
                std::slice::from_raw_parts(arr.as_ptr() as *const u8, arr.len() * 4)
            };
            w.write_all(bytes)?;
        }

        w.flush()?;
        let pos = w.into_inner()?.metadata()?.len();
        Ok(pos)
    }

    pub fn load_from_file(&mut self, path: &str) -> std::io::Result<i32> {
        use std::io::{BufReader, Read};

        let f = std::fs::File::open(path)?;
        let mut r = BufReader::with_capacity(1 << 20, f);

        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        if &magic != Self::BIN_MAGIC {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "bad magic"));
        }

        let mut hdr = [0u8; 20];
        r.read_exact(&mut hdr)?;
        let records_count = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
        let live_count = u32::from_le_bytes(hdr[4..8].try_into().unwrap());
        let name_pool_len = u32::from_le_bytes(hdr[8..12].try_into().unwrap());
        let ref_keys_len = u32::from_le_bytes(hdr[12..16].try_into().unwrap());
        let _bulk_mode = u32::from_le_bytes(hdr[16..20].try_into().unwrap());

        let rec_size = std::mem::size_of::<Record>();
        let mut rec_buf = vec![0u8; records_count as usize * rec_size];
        r.read_exact(&mut rec_buf)?;
        let mut records: Vec<Record> = Vec::with_capacity(records_count as usize);
        unsafe {
            std::ptr::copy_nonoverlapping(
                rec_buf.as_ptr(),
                records.as_mut_ptr() as *mut u8,
                rec_buf.len(),
            );
            records.set_len(records_count as usize);
        }
        drop(rec_buf);

        let mut name_pool = vec![0u8; name_pool_len as usize];
        r.read_exact(&mut name_pool)?;

        let mut rk_buf = vec![0u8; ref_keys_len as usize * 8];
        r.read_exact(&mut rk_buf)?;
        let mut ref_keys: Vec<u64> = Vec::with_capacity(ref_keys_len as usize);
        unsafe {
            std::ptr::copy_nonoverlapping(
                rk_buf.as_ptr(),
                ref_keys.as_mut_ptr() as *mut u8,
                rk_buf.len(),
            );
            ref_keys.set_len(ref_keys_len as usize);
        }
        drop(rk_buf);

        let mut rv_buf = vec![0u8; ref_keys_len as usize * 4];
        r.read_exact(&mut rv_buf)?;
        let mut ref_vals: Vec<u32> = Vec::with_capacity(ref_keys_len as usize);
        unsafe {
            std::ptr::copy_nonoverlapping(
                rv_buf.as_ptr(),
                ref_vals.as_mut_ptr() as *mut u8,
                rv_buf.len(),
            );
            ref_vals.set_len(ref_keys_len as usize);
        }
        drop(rv_buf);

        let live = live_count as usize;
        let mut name_sorted = vec![0u32; live];
        let mut py_sorted = vec![0u32; live];
        let mut full_py_sorted = vec![0u32; live];
        for arr in [&mut name_sorted, &mut py_sorted, &mut full_py_sorted] {
            let buf = unsafe {
                std::slice::from_raw_parts_mut(arr.as_mut_ptr() as *mut u8, live * 4)
            };
            r.read_exact(buf)?;
        }

        self.records = records;
        self.name_pool = name_pool;
        self.ref_keys = ref_keys;
        self.ref_vals = ref_vals;
        self.ref_map = None;
        self.live_count = live_count;
        self.bulk_mode = 0;
        self.name_sorted = name_sorted;
        self.py_sorted = py_sorted;
        self.full_py_sorted = full_py_sorted;

        Ok(live_count as i32)
    }
}
