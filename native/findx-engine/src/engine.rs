use crate::trigram::{intersect_sorted_postings, pack_trigram};
use hashbrown::HashMap;
use pinyin::ToPinyin;
use rayon::prelude::*;
use std::cmp::{Ordering, Reverse};
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

// ---------------------------------------------------------------------------
// time / size conversion helpers
// ---------------------------------------------------------------------------

const EPOCH_2000_TICKS: i64 = 630_822_816_000_000_000;
const TICKS_PER_SEC: i64 = 10_000_000;

#[inline]
fn engine_trace_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("FINDX_ENGINE_TRACE").is_some())
}

/// 拼音池 trigram 倒排快照（与对应 `*_pool` 字节长度绑定；池子增长后惰性重建）。
#[derive(Default)]
struct TrigramIndexSnap {
    pool_len: usize,
    postings: HashMap<u32, Vec<u32>>,
}

#[inline]
fn mtime_to_compact(ticks: i64) -> u32 {
    if ticks <= EPOCH_2000_TICKS {
        return 0;
    }
    let secs = (ticks - EPOCH_2000_TICKS) / TICKS_PER_SEC;
    secs.min(u32::MAX as i64) as u32
}

#[inline]
fn compact_to_ticks_or_zero(c: u32) -> i64 {
    if c == 0 {
        0
    } else {
        EPOCH_2000_TICKS + (c as i64) * TICKS_PER_SEC
    }
}

#[inline]
fn size_to_compact(s: i64) -> u32 {
    if s < 0 {
        return 0;
    }
    s.min(u32::MAX as i64) as u32
}

// ---------------------------------------------------------------------------
// Record – 48 bytes per entry (down from 72)
// Removed: py_start, py_len, full_py_start, full_py_len (computed on the fly)
// Shrunk:  size i64→u32, mtime/ctime/atime i64→u32
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
    pub ctime: u32,
    pub atime: u32,
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
    // 拼音池与常见 ASCII needle 均为 ASCII：用窗口 + eq_ignore_ascii_case，便于 LLVM 向量化
    if hay.iter().all(|b| b.is_ascii()) && needle.iter().all(|b| b.is_ascii()) {
        return hay.windows(nl).any(|w| w.eq_ignore_ascii_case(needle));
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

#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchKind {
    None = 0,
    Initials = 1,
    FullPinyin = 2,
    Mixed = 3,
    Exact = 4,
    Prefix = 5,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueryMatch {
    pub kind: MatchKind,
    pub score: i32,
    pub matched_chars: i32,
}

impl QueryMatch {
    pub const NO_MATCH: Self = Self {
        kind: MatchKind::None,
        score: 0,
        matched_chars: 0,
    };

    pub fn is_match(&self) -> bool {
        self.kind != MatchKind::None
    }
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

#[inline]
fn pool_bytes_slice<'a>(pool: &'a [u8], off: &[u32], len: &[u16], idx: usize) -> &'a [u8] {
    if idx >= off.len() {
        return &[];
    }
    let o = off[idx] as usize;
    let l = len[idx] as usize;
    if l == 0 || o > pool.len() {
        return &[];
    }
    let end = o.saturating_add(l);
    if end > pool.len() {
        // 理论上不应发生；全量测试并行收尾时偶发越界会拖垮进程，此处降级为空切片。
        return &[];
    }
    &pool[o..end]
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

fn is_ascii_alnum(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_alphanumeric())
}

const PINYIN_INITIALS: &[&str] = &[
    "zh", "ch", "sh", "b", "p", "m", "f", "d", "t", "n", "l", "g", "k", "h", "j", "q",
    "x", "r", "z", "c", "s", "y", "w",
];

const PINYIN_FINALS: &[&str] = &[
    "a", "ai", "an", "ang", "ao", "e", "ei", "en", "eng", "er", "i", "ia", "ian", "iang",
    "iao", "ie", "in", "ing", "iong", "iu", "o", "ong", "ou", "u", "ua", "uai", "uan",
    "uang", "ue", "ui", "un", "uo", "v", "van", "ve", "vn",
];

fn is_valid_pinyin_syllable(token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    if PINYIN_FINALS.contains(&token) {
        return true;
    }
    for initial in PINYIN_INITIALS {
        if !token.starts_with(initial) || token.len() == initial.len() {
            continue;
        }
        if PINYIN_FINALS.contains(&&token[initial.len()..]) {
            return true;
        }
    }
    false
}

fn try_consume_pinyin_token_length(text: &[u8]) -> usize {
    let max = text.len().min(6);
    for len in (1..=max).rev() {
        if let Ok(token) = std::str::from_utf8(&text[..len]) {
            if is_valid_pinyin_syllable(token) {
                return len;
            }
        }
    }

    if text.len() >= 2 && matches!(text[0], b'z' | b'c' | b's') && text[1] == b'h' {
        return 2;
    }

    if text.first().is_some_and(|b| b.is_ascii_alphabetic()) {
        1
    } else {
        0
    }
}

fn build_ascii_pinyin_initials_anchor(keyword: &str) -> Option<String> {
    if keyword.len() < 4 || !is_ascii_alnum(keyword) {
        return None;
    }

    let lower = keyword.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut pos = 0usize;
    let mut anchor = String::with_capacity(bytes.len().min(12));
    while pos < bytes.len() && anchor.len() < 12 {
        let len = try_consume_pinyin_token_length(&bytes[pos..]);
        if len == 0 {
            break;
        }

        anchor.push(bytes[pos] as char);
        pos += len;
    }

    if anchor.len() >= 2 && anchor.len() < keyword.len() {
        Some(anchor)
    } else {
        None
    }
}

fn build_ascii_pinyin_tail_token(keyword: &str) -> Option<String> {
    if keyword.len() < 4 || !is_ascii_alnum(keyword) {
        return None;
    }

    let lower = keyword.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut pos = 0usize;
    let mut tail: Option<String> = None;
    while pos < bytes.len() {
        let len = try_consume_pinyin_token_length(&bytes[pos..]);
        if len == 0 {
            break;
        }

        if len >= 2 {
            if let Ok(token) = std::str::from_utf8(&bytes[pos..pos + len]) {
                if is_valid_pinyin_syllable(token) {
                    tail = Some(token.to_string());
                }
            }
        }

        pos += len;
    }

    tail.filter(|token| token.len() >= 2 && token.len() < keyword.len())
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

/// 每个 CJK 字对应一条全拼（小写 ASCII）与首字母，用于按字混合匹配与高亮。
struct CjkSyllable {
    full: Vec<u8>,
    initial: u8,
    char_idx: usize,
}

#[inline]
fn bytes_eq_ignore_case(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len()
        && a
            .iter()
            .zip(b.iter())
            .all(|(x, y)| x.to_ascii_lowercase() == y.to_ascii_lowercase())
}

fn collect_cjk_syllables(candidate: &str) -> Vec<CjkSyllable> {
    let mut out = Vec::new();
    for (char_idx, ch) in candidate.chars().enumerate() {
        if ch.is_ascii_alphanumeric() {
            continue;
        }
        if let Some(py) = ch.to_pinyin() {
            let plain = py.plain().to_ascii_lowercase();
            let full: Vec<u8> = plain.bytes().collect();
            let initial = full.first().copied().unwrap_or(0);
            out.push(CjkSyllable {
                full,
                initial,
                char_idx,
            });
        }
    }
    out
}

/// 按字对齐：每个字仅消耗「全拼」或「首字母」之一，支持从任意字起算的局部匹配（部分首字母 / 部分全拼 / 混合）。
fn match_mixed_pinyin(query_bytes: &[u8], candidate: &str) -> bool {
    let syllables = collect_cjk_syllables(candidate);
    if syllables.is_empty() {
        return false;
    }
    let nq = query_bytes.len();
    for start in 0..syllables.len() {
        let slice = &syllables[start..];
        let ns = slice.len();
        let mut memo = vec![vec![false; ns + 1]; nq + 1];
        for j in 0..=ns {
            memo[nq][j] = true;
        }
        for i in (0..nq).rev() {
            for j in (0..ns).rev() {
                let t = &slice[j];
                let mut ok = false;
                let flen = t.full.len();
                if i + flen <= nq && bytes_eq_ignore_case(&query_bytes[i..i + flen], &t.full[..]) {
                    ok = memo[i + flen][j + 1];
                }
                if !ok && query_bytes[i].to_ascii_lowercase() == t.initial.to_ascii_lowercase() {
                    ok = memo[i + 1][j + 1];
                }
                memo[i][j] = ok;
            }
        }
        if memo[0][0] {
            return true;
        }
    }
    false
}

fn dfs_mixed_path(
    q: &[u8],
    slice: &[CjkSyllable],
    qi: usize,
    si: usize,
    start_offset: usize,
    path: &mut Vec<usize>,
) -> bool {
    if qi == q.len() {
        return true;
    }
    if si >= slice.len() {
        return false;
    }
    let global = start_offset + si;
    let s = &slice[si];
    if q.len() >= qi + s.full.len() && bytes_eq_ignore_case(&q[qi..qi + s.full.len()], &s.full[..]) {
        path.push(global);
        if dfs_mixed_path(q, slice, qi + s.full.len(), si + 1, start_offset, path) {
            return true;
        }
        path.pop();
    }
    if qi < q.len() && q[qi].to_ascii_lowercase() == s.initial.to_ascii_lowercase() {
        path.push(global);
        if dfs_mixed_path(q, slice, qi + 1, si + 1, start_offset, path) {
            return true;
        }
        path.pop();
    }
    false
}

fn add_mixed_pinyin_ranges(
    query: &[u8],
    candidate: &str,
    spans: &[Utf16CharSpan],
    ranges: &mut Vec<(i32, i32)>,
) -> bool {
    let syllables = collect_cjk_syllables(candidate);
    if syllables.is_empty() {
        return false;
    }
    for start in 0..syllables.len() {
        let mut path = Vec::new();
        if dfs_mixed_path(query, &syllables[start..], 0, 0, start, &mut path) {
            let char_indices: Vec<usize> = path.iter().map(|&si| syllables[si].char_idx).collect();
            add_owner_indices_as_ranges(&char_indices, spans, ranges);
            return true;
        }
    }
    false
}

/// 拼音相关打分（全拼子串 / 首字母子串 / 按字混合），与 `compute_full_py_stack` 结果一致。
fn match_pinyin_stages(
    query_bytes: &[u8],
    query_chars: i32,
    full_py: &[u8],
    initials: &[u8],
    candidate: &str,
) -> QueryMatch {
    if starts_with_ignore_case_bytes(full_py, query_bytes) {
        return QueryMatch {
            kind: MatchKind::FullPinyin,
            score: 520,
            matched_chars: query_chars,
        };
    }
    if contains_ignore_case_bytes(full_py, query_bytes) {
        return QueryMatch {
            kind: MatchKind::FullPinyin,
            score: 420,
            matched_chars: query_chars,
        };
    }

    if starts_with_ignore_case_bytes(initials, query_bytes) {
        return QueryMatch {
            kind: MatchKind::Initials,
            score: 400,
            matched_chars: query_chars,
        };
    }
    if contains_ignore_case_bytes(initials, query_bytes) {
        return QueryMatch {
            kind: MatchKind::Initials,
            score: 300,
            matched_chars: query_chars,
        };
    }

    if match_mixed_pinyin(query_bytes, candidate) {
        return QueryMatch {
            kind: MatchKind::Mixed,
            score: 380,
            matched_chars: query_chars,
        };
    }

    QueryMatch::NO_MATCH
}

/// `query` 已为小写；`candidate` 为 ASCII 文件名（字节级大小写无关）。
#[inline]
fn ascii_bytes_eq_lower(candidate: &[u8], query_lower: &[u8]) -> bool {
    candidate.len() == query_lower.len()
        && candidate
            .iter()
            .zip(query_lower.iter())
            .all(|(c, q)| c.to_ascii_lowercase() == *q)
}

#[inline]
fn ascii_bytes_has_prefix_lower(candidate: &[u8], query_lower: &[u8]) -> bool {
    candidate.len() >= query_lower.len()
        && candidate[..query_lower.len()]
            .iter()
            .zip(query_lower.iter())
            .all(|(c, q)| c.to_ascii_lowercase() == *q)
}

#[inline]
fn ascii_bytes_contains_lower(candidate: &[u8], query_lower: &[u8]) -> bool {
    if query_lower.is_empty() {
        return true;
    }
    if query_lower.len() > candidate.len() {
        return false;
    }
    candidate.windows(query_lower.len()).any(|win| {
        win.iter()
            .zip(query_lower.iter())
            .all(|(c, q)| c.to_ascii_lowercase() == *q)
    })
}

/// `pinyin_cache`: 若提供则跳过一次 `compute_full_py_stack` / `compute_initials_stack`（索引内搜索回退路径用）。
pub fn match_query_with_pinyin_cache(
    query_lower: &str,
    candidate: &str,
    pinyin_cache: Option<(&[u8], &[u8])>,
) -> QueryMatch {
    if query_lower.is_empty() || candidate.is_empty() {
        return QueryMatch::NO_MATCH;
    }

    let query_chars = query_lower.chars().count() as i32;

    // 热路径：ASCII 查询 + ASCII 文件名。避免每条候选 `to_lowercase()` 整串分配；
    // `search_simple_query` / `search_query_matches` 可对数十万条调用本函数。
    if query_lower.is_ascii() && candidate.is_ascii() {
        let qb = query_lower.as_bytes();
        let cb = candidate.as_bytes();
        if ascii_bytes_eq_lower(cb, qb) {
            return QueryMatch {
                kind: MatchKind::Exact,
                score: 1000,
                matched_chars: candidate.chars().count() as i32,
            };
        }
        if ascii_bytes_has_prefix_lower(cb, qb) {
            return QueryMatch {
                kind: MatchKind::Prefix,
                score: 800,
                matched_chars: query_chars,
            };
        }
        if ascii_bytes_contains_lower(cb, qb) {
            return QueryMatch {
                kind: MatchKind::Prefix,
                score: 600,
                matched_chars: query_chars,
            };
        }
        let fuzzy = fuzzy_match_bytes(qb, cb);
        return if fuzzy > 0 {
            QueryMatch {
                kind: MatchKind::Mixed,
                score: fuzzy,
                matched_chars: query_chars,
            }
        } else {
            QueryMatch::NO_MATCH
        };
    }

    let candidate_lower = candidate.to_lowercase();

    if candidate_lower == query_lower {
        return QueryMatch {
            kind: MatchKind::Exact,
            score: 1000,
            matched_chars: candidate.chars().count() as i32,
        };
    }

    if candidate_lower.starts_with(query_lower) {
        return QueryMatch {
            kind: MatchKind::Prefix,
            score: 800,
            matched_chars: query_chars,
        };
    }

    if candidate_lower.contains(query_lower) {
        return QueryMatch {
            kind: MatchKind::Prefix,
            score: 600,
            matched_chars: query_chars,
        };
    }

    let query_bytes = query_lower.as_bytes();
    if !name_contains_cjk(candidate) {
        let fuzzy = fuzzy_match_bytes(query_bytes, candidate_lower.as_bytes());
        return if fuzzy > 0 {
            QueryMatch {
                kind: MatchKind::Mixed,
                score: fuzzy,
                matched_chars: query_chars,
            }
        } else {
            QueryMatch::NO_MATCH
        };
    }

    if !query_lower.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return if candidate_lower.contains(query_lower) {
            QueryMatch {
                kind: MatchKind::Prefix,
                score: 700,
                matched_chars: query_chars,
            }
        } else {
            QueryMatch::NO_MATCH
        };
    }

    let mut full_buf = [0u8; 1024];
    let mut init_buf = [0u8; 256];
    let (full_py, initials): (&[u8], &[u8]) = match pinyin_cache {
        Some((f, i)) => (f, i),
        None => {
            let (fb, fl) = compute_full_py_stack(candidate);
            let fl = fl.min(1024);
            full_buf[..fl].copy_from_slice(&fb[..fl]);
            let (ib, il) = compute_initials_stack(candidate);
            let il = il.min(256);
            init_buf[..il].copy_from_slice(&ib[..il]);
            (&full_buf[..fl], &init_buf[..il])
        }
    };

    match_pinyin_stages(query_bytes, query_chars, full_py, initials, candidate)
}

pub fn match_query(query_lower: &str, candidate: &str) -> QueryMatch {
    match_query_with_pinyin_cache(query_lower, candidate, None)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Utf16CharSpan {
    start: i32,
    len: i32,
}

fn utf16_char_spans(value: &str) -> Vec<Utf16CharSpan> {
    let mut spans = Vec::with_capacity(value.chars().count());
    let mut start = 0i32;
    for ch in value.chars() {
        let len = ch.len_utf16() as i32;
        spans.push(Utf16CharSpan { start, len });
        start += len;
    }
    spans
}

fn lower_chars_with_owners(value: &str) -> (Vec<char>, Vec<usize>) {
    let mut lower = Vec::new();
    let mut owners = Vec::new();
    for (char_idx, ch) in value.chars().enumerate() {
        for lc in ch.to_lowercase() {
            lower.push(lc);
            owners.push(char_idx);
        }
    }
    (lower, owners)
}

fn merge_ranges(ranges: &mut Vec<(i32, i32)>) {
    if ranges.len() <= 1 {
        return;
    }

    ranges.sort_by_key(|&(start, _)| start);
    let mut merged: Vec<(i32, i32)> = Vec::with_capacity(ranges.len());
    for &(start, len) in ranges.iter() {
        if len <= 0 {
            continue;
        }

        if let Some(last) = merged.last_mut() {
            let last_end = last.0 + last.1;
            let cur_end = start + len;
            if start <= last_end {
                last.1 = last_end.max(cur_end) - last.0;
                continue;
            }
        }

        merged.push((start, len));
    }

    *ranges = merged;
}

fn add_owner_indices_as_ranges(owner_indices: &[usize], spans: &[Utf16CharSpan], ranges: &mut Vec<(i32, i32)>) {
    if owner_indices.is_empty() {
        return;
    }

    let mut owners = owner_indices.to_vec();
    owners.sort_unstable();
    owners.dedup();

    let mut run_start = spans[owners[0]].start;
    let mut run_end = spans[owners[0]].start + spans[owners[0]].len;

    for &owner in owners.iter().skip(1) {
        let span = spans[owner];
        if span.start <= run_end {
            run_end = run_end.max(span.start + span.len);
            continue;
        }

        ranges.push((run_start, run_end - run_start));
        run_start = span.start;
        run_end = span.start + span.len;
    }

    ranges.push((run_start, run_end - run_start));
}

fn add_literal_ranges(query_lower: &str, candidate: &str, spans: &[Utf16CharSpan], ranges: &mut Vec<(i32, i32)>) -> bool {
    let needle: Vec<char> = query_lower.chars().collect();
    if needle.is_empty() {
        return false;
    }

    let (hay_lower, owners) = lower_chars_with_owners(candidate);
    if needle.len() > hay_lower.len() {
        return false;
    }

    let mut found = false;
    'outer: for start in 0..=(hay_lower.len() - needle.len()) {
        for j in 0..needle.len() {
            if hay_lower[start + j] != needle[j] {
                continue 'outer;
            }
        }
        add_owner_indices_as_ranges(&owners[start..start + needle.len()], spans, ranges);
        found = true;
    }

    found
}

fn build_initials_with_owners(name: &str) -> (Vec<u8>, Vec<usize>) {
    let has_cjk = name_contains_cjk(name);
    let mut initials = Vec::with_capacity(256);
    let mut owners = Vec::with_capacity(256);

    for (char_idx, ch) in name.chars().enumerate() {
        if initials.len() >= 256 {
            break;
        }

        if ch.is_ascii_alphanumeric() {
            initials.push(ch.to_ascii_lowercase() as u8);
            owners.push(char_idx);
        } else if has_cjk {
            if let Some(py) = ch.to_pinyin() {
                if let Some(b) = py.plain().bytes().next() {
                    initials.push(b);
                    owners.push(char_idx);
                }
            }
        }
    }

    (initials, owners)
}

fn build_full_py_with_owners(name: &str) -> (Vec<u8>, Vec<usize>) {
    let mut full_py = Vec::with_capacity(1024);
    let mut owners = Vec::with_capacity(1024);

    for (char_idx, ch) in name.chars().enumerate() {
        if full_py.len() >= 1024 {
            break;
        }

        if ch.is_ascii_alphanumeric() {
            full_py.push(ch.to_ascii_lowercase() as u8);
            owners.push(char_idx);
        } else if let Some(py) = ch.to_pinyin() {
            for b in py.plain().bytes() {
                if full_py.len() >= 1024 {
                    break;
                }
                full_py.push(b);
                owners.push(char_idx);
            }
        }
    }

    (full_py, owners)
}

fn find_first_ignore_case_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    let hl = hay.len();
    let nl = needle.len();
    if nl == 0 || nl > hl {
        return None;
    }

    let first = needle[0].to_ascii_lowercase();
    'outer: for start in 0..=(hl - nl) {
        if hay[start].to_ascii_lowercase() != first {
            continue;
        }
        for j in 1..nl {
            if hay[start + j].to_ascii_lowercase() != needle[j].to_ascii_lowercase() {
                continue 'outer;
            }
        }
        return Some(start);
    }

    None
}

fn add_bytes_match_ranges(
    owners: &[usize],
    start: usize,
    len: usize,
    spans: &[Utf16CharSpan],
    ranges: &mut Vec<(i32, i32)>,
) {
    if len == 0 || start + len > owners.len() {
        return;
    }
    add_owner_indices_as_ranges(&owners[start..start + len], spans, ranges);
}

fn add_ascii_subsequence_ranges(query: &[u8], candidate: &str, spans: &[Utf16CharSpan], ranges: &mut Vec<(i32, i32)>) -> bool {
    let mut query_idx = 0usize;
    let mut owners = Vec::with_capacity(query.len());

    for (char_idx, ch) in candidate.chars().enumerate() {
        if query_idx >= query.len() {
            break;
        }
        if ch.is_ascii() && ch.to_ascii_lowercase() as u8 == query[query_idx] {
            owners.push(char_idx);
            query_idx += 1;
        }
    }

    if query_idx == query.len() {
        add_owner_indices_as_ranges(&owners, spans, ranges);
        true
    } else {
        false
    }
}

pub fn highlight_query_ranges(query_lower: &str, candidate: &str) -> Vec<(i32, i32)> {
    if query_lower.is_empty() || candidate.is_empty() {
        return Vec::new();
    }

    let spans = utf16_char_spans(candidate);
    let mut ranges = Vec::new();
    let literal_found = add_literal_ranges(query_lower, candidate, &spans, &mut ranges);
    if literal_found {
        merge_ranges(&mut ranges);
        return ranges;
    }

    let query_bytes = query_lower.as_bytes();
    if !query_lower.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return ranges;
    }

    if !name_contains_cjk(candidate) {
        add_ascii_subsequence_ranges(query_bytes, candidate, &spans, &mut ranges);
        merge_ranges(&mut ranges);
        return ranges;
    }

    let (full_py, full_owners) = build_full_py_with_owners(candidate);
    if let Some(start) = find_first_ignore_case_bytes(&full_py, query_bytes) {
        add_bytes_match_ranges(&full_owners, start, query_bytes.len(), &spans, &mut ranges);
        merge_ranges(&mut ranges);
        return ranges;
    }

    let (initials, initials_owners) = build_initials_with_owners(candidate);
    if let Some(start) = find_first_ignore_case_bytes(&initials, query_bytes) {
        add_bytes_match_ranges(&initials_owners, start, query_bytes.len(), &spans, &mut ranges);
        merge_ranges(&mut ranges);
        return ranges;
    }

    if add_mixed_pinyin_ranges(query_bytes, candidate, &spans, &mut ranges) {
        merge_ranges(&mut ranges);
        return ranges;
    }

    ranges
}


// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Engine {
    pub records: Vec<Record>,
    pub name_pool: Vec<u8>,

    /// 与 records 下标对齐：预计算拼音首字母/全拼，避免搜索时每条记录反复 `to_pinyin`。
    initials_pool: Vec<u8>,
    full_py_pool: Vec<u8>,
    initials_off: Vec<u32>,
    initials_len: Vec<u16>,
    full_py_off: Vec<u32>,
    full_py_len: Vec<u16>,

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

    /// 父行号 → 直属子行行号（`rebuild_indexes` / 加载后 `rebuild_path_navigation_indexes` 重建）。
    children_by_parent_idx: HashMap<u32, Vec<u32>>,
    /// 目录全路径（小写、`\`）→ 目录行号，用于 `parent:` 字面路径 O(1) 定位。
    dir_path_lower_to_idx: HashMap<String, u32>,

    /// 全拼 trigram 倒排；`full_py_pool` 变化后由 `ensure_full_py_trigram` 惰性重建。
    full_py_trigram: Mutex<TrigramIndexSnap>,
    /// 首字母串 trigram 倒排（`search_initials_contains` 热路径）。
    initials_trigram: Mutex<TrigramIndexSnap>,
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    fn ensure_pinyin_seg_len(&mut self) {
        let n = self.records.len();
        self.initials_off.resize(n, 0);
        self.initials_len.resize(n, 0);
        self.full_py_off.resize(n, 0);
        self.full_py_len.resize(n, 0);
    }

    fn initials_slice(&self, idx: usize) -> &[u8] {
        pool_bytes_slice(&self.initials_pool, &self.initials_off, &self.initials_len, idx)
    }

    fn full_py_slice(&self, idx: usize) -> &[u8] {
        pool_bytes_slice(&self.full_py_pool, &self.full_py_off, &self.full_py_len, idx)
    }

    fn assign_pinyin_aux_for_index(&mut self, idx: usize) {
        let old_ini = self.snapshot_initials_bytes(idx);
        let old_fp = self.snapshot_full_py_bytes(idx);

        let r = &self.records[idx];
        if r.deleted != 0 {
            if self.bulk_mode == 0 {
                self.incremental_remove_initials_trigram(idx as u32, old_ini.as_deref());
                self.incremental_remove_full_py_trigram(idx as u32, old_fp.as_deref());
            }
            if idx < self.initials_off.len() {
                self.initials_off[idx] = 0;
                self.initials_len[idx] = 0;
                self.full_py_off[idx] = 0;
                self.full_py_len[idx] = 0;
            }
            return;
        }
        let name = pool_utf8(&self.name_pool, r.name_start, r.name_len as u32);
        let (ib, il) = compute_initials_stack(name);
        let (fb, fl) = compute_full_py_stack(name);
        let il = il.min(256);
        let fl = fl.min(1024);
        let ios = self.initials_pool.len() as u32;
        self.initials_pool.extend_from_slice(&ib[..il]);
        self.initials_off[idx] = ios;
        self.initials_len[idx] = il as u16;
        let fps = self.full_py_pool.len() as u32;
        self.full_py_pool.extend_from_slice(&fb[..fl]);
        self.full_py_off[idx] = fps;
        self.full_py_len[idx] = fl as u16;

        if self.bulk_mode == 0 {
            self.incremental_remove_initials_trigram(idx as u32, old_ini.as_deref());
            self.incremental_remove_full_py_trigram(idx as u32, old_fp.as_deref());
            self.incremental_add_initials_trigram(idx as u32, &ib[..il]);
            self.incremental_add_full_py_trigram(idx as u32, &fb[..fl]);
        }
    }

    fn rebuild_pinyin_aux_pools(&mut self, live: &[u32]) {
        self.initials_pool.clear();
        self.full_py_pool.clear();
        let n = self.records.len();
        self.initials_off.resize(n, 0);
        self.initials_len.resize(n, 0);
        self.full_py_off.resize(n, 0);
        self.full_py_len.resize(n, 0);
        for &idx in live {
            self.assign_pinyin_aux_for_index(idx as usize);
        }
        self.refresh_full_py_trigram_locked();
        self.refresh_initials_trigram_locked();
    }

    fn build_trigram_postings_for_pinyin_pool(
        records: &[Record],
        name_pool: &[u8],
        pool: &[u8],
        off: &[u32],
        len: &[u16],
    ) -> HashMap<u32, Vec<u32>> {
        let mut map: HashMap<u32, Vec<u32>> = HashMap::new();
        for (i, r) in records.iter().enumerate() {
            if r.deleted != 0 {
                continue;
            }
            let name = pool_utf8(name_pool, r.name_start, r.name_len as u32);
            if !name_contains_cjk(name) {
                continue;
            }
            let seg = pool_bytes_slice(pool, off, len, i);
            if seg.len() < 3 {
                continue;
            }
            for w in seg.windows(3) {
                let key = pack_trigram(w[0], w[1], w[2]);
                map.entry(key).or_default().push(i as u32);
            }
        }
        for v in map.values_mut() {
            v.sort_unstable();
            v.dedup();
        }
        map
    }

    #[inline]
    fn trigram_keys_for_ascii_seg(seg: &[u8]) -> Vec<u32> {
        if seg.len() < 3 {
            return Vec::new();
        }
        let mut keys: Vec<u32> = seg
            .windows(3)
            .map(|w| pack_trigram(w[0], w[1], w[2]))
            .collect();
        keys.sort_unstable();
        keys.dedup();
        keys
    }

    fn snapshot_initials_bytes(&self, idx: usize) -> Option<Vec<u8>> {
        if idx >= self.initials_len.len() || self.initials_len[idx] == 0 {
            return None;
        }
        Some(self.initials_slice(idx).to_vec())
    }

    fn snapshot_full_py_bytes(&self, idx: usize) -> Option<Vec<u8>> {
        if idx >= self.full_py_len.len() || self.full_py_len[idx] == 0 {
            return None;
        }
        Some(self.full_py_slice(idx).to_vec())
    }

    /// 从倒排中去掉某条记录在「旧拼音段」里贡献的 posting（与 assign 前快照一致）。
    fn incremental_remove_initials_trigram(&mut self, idx: u32, old_seg: Option<&[u8]>) {
        let Some(seg) = old_seg else {
            let mut g = self.initials_trigram.lock().unwrap();
            g.pool_len = self.initials_pool.len();
            return;
        };
        if seg.len() < 3 {
            let mut g = self.initials_trigram.lock().unwrap();
            g.pool_len = self.initials_pool.len();
            return;
        }
        let keys = Self::trigram_keys_for_ascii_seg(seg);
        let mut g = self.initials_trigram.lock().unwrap();
        for k in keys {
            if let Some(v) = g.postings.get_mut(&k) {
                if let Ok(p) = v.binary_search(&idx) {
                    v.remove(p);
                }
            }
        }
        g.pool_len = self.initials_pool.len();
    }

    fn incremental_remove_full_py_trigram(&mut self, idx: u32, old_seg: Option<&[u8]>) {
        let Some(seg) = old_seg else {
            let mut g = self.full_py_trigram.lock().unwrap();
            g.pool_len = self.full_py_pool.len();
            return;
        };
        if seg.len() < 3 {
            let mut g = self.full_py_trigram.lock().unwrap();
            g.pool_len = self.full_py_pool.len();
            return;
        }
        let keys = Self::trigram_keys_for_ascii_seg(seg);
        let mut g = self.full_py_trigram.lock().unwrap();
        for k in keys {
            if let Some(v) = g.postings.get_mut(&k) {
                if let Ok(p) = v.binary_search(&idx) {
                    v.remove(p);
                }
            }
        }
        g.pool_len = self.full_py_pool.len();
    }

    fn incremental_add_initials_trigram(&mut self, idx: u32, new_seg: &[u8]) {
        let mut g = self.initials_trigram.lock().unwrap();
        if new_seg.len() >= 3 {
            for k in Self::trigram_keys_for_ascii_seg(new_seg) {
                let v = g.postings.entry(k).or_default();
                if let Err(p) = v.binary_search(&idx) {
                    v.insert(p, idx);
                }
            }
        }
        g.pool_len = self.initials_pool.len();
    }

    fn incremental_add_full_py_trigram(&mut self, idx: u32, new_seg: &[u8]) {
        let mut g = self.full_py_trigram.lock().unwrap();
        if new_seg.len() >= 3 {
            for k in Self::trigram_keys_for_ascii_seg(new_seg) {
                let v = g.postings.entry(k).or_default();
                if let Err(p) = v.binary_search(&idx) {
                    v.insert(p, idx);
                }
            }
        }
        g.pool_len = self.full_py_pool.len();
    }

    fn refresh_full_py_trigram_locked(&mut self) {
        let built = Self::build_trigram_postings_for_pinyin_pool(
            &self.records,
            &self.name_pool,
            &self.full_py_pool,
            &self.full_py_off,
            &self.full_py_len,
        );
        let g = self.full_py_trigram.get_mut().unwrap();
        g.postings = built;
        g.pool_len = self.full_py_pool.len();
    }

    fn refresh_initials_trigram_locked(&mut self) {
        let built = Self::build_trigram_postings_for_pinyin_pool(
            &self.records,
            &self.name_pool,
            &self.initials_pool,
            &self.initials_off,
            &self.initials_len,
        );
        let g = self.initials_trigram.get_mut().unwrap();
        g.postings = built;
        g.pool_len = self.initials_pool.len();
    }

    /// 仅在倒排尚未构建时全量生成。日常增量由 `assign_pinyin_aux_for_index` 维护 posting，
    /// 不再用 `pool.len()` 与快照比较触发全量重建（否则 USN 每条更新后下一次搜索会卡 ~1s）。
    fn ensure_full_py_trigram(&self) {
        let pool_len = self.full_py_pool.len();
        let need_build = {
            let g = self.full_py_trigram.lock().unwrap();
            g.postings.is_empty() && pool_len > 0
        };
        if !need_build {
            return;
        }
        let built = Self::build_trigram_postings_for_pinyin_pool(
            &self.records,
            &self.name_pool,
            &self.full_py_pool,
            &self.full_py_off,
            &self.full_py_len,
        );
        let mut g = self.full_py_trigram.lock().unwrap();
        if g.postings.is_empty() && self.full_py_pool.len() > 0 {
            g.postings = built;
            g.pool_len = self.full_py_pool.len();
        }
    }

    fn ensure_initials_trigram(&self) {
        let pool_len = self.initials_pool.len();
        let need_build = {
            let g = self.initials_trigram.lock().unwrap();
            g.postings.is_empty() && pool_len > 0
        };
        if !need_build {
            return;
        }
        let built = Self::build_trigram_postings_for_pinyin_pool(
            &self.records,
            &self.name_pool,
            &self.initials_pool,
            &self.initials_off,
            &self.initials_len,
        );
        let mut g = self.initials_trigram.lock().unwrap();
        if g.postings.is_empty() && self.initials_pool.len() > 0 {
            g.postings = built;
            g.pool_len = self.initials_pool.len();
        }
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
        let pi = self.initials_slice(idx as usize).to_vec();
        let pos = {
            let py_sorted = &self.py_sorted;
            let initials_pool = &self.initials_pool;
            let initials_off = &self.initials_off;
            let initials_len = &self.initials_len;
            py_sorted.partition_point(|&o| {
                let po = pool_bytes_slice(initials_pool, initials_off, initials_len, o as usize);
                cmp_ignore_case(po, pi.as_slice()).then_with(|| o.cmp(&idx)) == Ordering::Less
            })
        };
        self.py_sorted.insert(pos, idx);
    }

    fn sorted_remove_py(&mut self, idx: u32) {
        let pi = self.initials_slice(idx as usize).to_vec();
        let pos = {
            let py_sorted = &self.py_sorted;
            let initials_pool = &self.initials_pool;
            let initials_off = &self.initials_off;
            let initials_len = &self.initials_len;
            py_sorted.partition_point(|&o| {
                let po = pool_bytes_slice(initials_pool, initials_off, initials_len, o as usize);
                cmp_ignore_case(po, pi.as_slice()).then_with(|| o.cmp(&idx)) == Ordering::Less
            })
        };
        if pos < self.py_sorted.len() && self.py_sorted[pos] == idx {
            self.py_sorted.remove(pos);
        }
    }

    fn sorted_insert_full_py(&mut self, idx: u32) {
        let pi = self.full_py_slice(idx as usize).to_vec();
        let pos = {
            let full_py_sorted = &self.full_py_sorted;
            let full_py_pool = &self.full_py_pool;
            let full_py_off = &self.full_py_off;
            let full_py_len = &self.full_py_len;
            full_py_sorted.partition_point(|&o| {
                let po = pool_bytes_slice(full_py_pool, full_py_off, full_py_len, o as usize);
                cmp_ignore_case(po, pi.as_slice()).then_with(|| o.cmp(&idx)) == Ordering::Less
            })
        };
        self.full_py_sorted.insert(pos, idx);
    }

    fn sorted_remove_full_py(&mut self, idx: u32) {
        let pi = self.full_py_slice(idx as usize).to_vec();
        let pos = {
            let full_py_sorted = &self.full_py_sorted;
            let full_py_pool = &self.full_py_pool;
            let full_py_off = &self.full_py_off;
            let full_py_len = &self.full_py_len;
            full_py_sorted.partition_point(|&o| {
                let po = pool_bytes_slice(full_py_pool, full_py_off, full_py_len, o as usize);
                cmp_ignore_case(po, pi.as_slice()).then_with(|| o.cmp(&idx)) == Ordering::Less
            })
        };
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
        self.initials_pool.clear();
        self.full_py_pool.clear();
        self.initials_off.clear();
        self.initials_len.clear();
        self.full_py_off.clear();
        self.full_py_len.clear();
        self.ref_keys.clear();
        self.ref_vals.clear();
        self.ref_map = None;
        self.name_sorted.clear();
        self.py_sorted.clear();
        self.full_py_sorted.clear();
        self.children_by_parent_idx.clear();
        self.dir_path_lower_to_idx.clear();
        self.live_count = 0;
        self.bulk_mode = 0;
        *self.full_py_trigram.get_mut().unwrap() = TrigramIndexSnap::default();
        *self.initials_trigram.get_mut().unwrap() = TrigramIndexSnap::default();
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
        ctime: i64,
        atime: i64,
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
            ctime: mtime_to_compact(ctime),
            atime: mtime_to_compact(atime),
            name_len: nl,
            vol: vol as u8,
            deleted: 0,
            attr,
        });
        self.ensure_pinyin_seg_len();
        if self.bulk_mode == 0 {
            self.assign_pinyin_aux_for_index(idx as usize);
        }
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
        ctime: i64,
        atime: i64,
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
            r.ctime = mtime_to_compact(ctime);
            r.atime = mtime_to_compact(atime);
            r.vol = vol as u8;
            self.assign_pinyin_aux_for_index(idx as usize);
            if self.bulk_mode == 0 {
                self.sorted_insert_name(idx);
                self.sorted_insert_py(idx);
                self.sorted_insert_full_py(idx);
            }
            return;
        }
        self.add_entry_utf16(vol, file_ref, parent_ref, name_utf16, attr, size, mtime, ctime, atime);
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
                self.assign_pinyin_aux_for_index(idx as usize);
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
    // rebuild_indexes: compact name_pool, resolve parent_idx, 预计算拼音池, parallel sort
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

        // Phase 3b: 预计算拼音首字母/全拼串（后续排序与子串扫描均用池化字节，避免 O(n) 搜索时反复 to_pinyin）
        self.rebuild_pinyin_aux_pools(&live);

        // 目录路径解析 / 子树遍历：依赖 parent_idx 与 name_pool，须在 move `live` 进排序表之前重建。
        self.rebuild_path_navigation_indexes(&live);

        // Phase 4: parallel sort（比较器读拼音池）
        let t3 = std::time::Instant::now();
        self.name_sorted = live.clone();
        self.py_sorted = live.clone();
        self.full_py_sorted = live;

        let records = &self.records;
        let name_pool = &self.name_pool;
        let initials_pool = &self.initials_pool;
        let initials_off = &self.initials_off;
        let initials_len = &self.initials_len;
        let full_py_pool = &self.full_py_pool;
        let full_py_off = &self.full_py_off;
        let full_py_len = &self.full_py_len;

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
            let pa = pool_bytes_slice(initials_pool, initials_off, initials_len, a as usize);
            let pb = pool_bytes_slice(initials_pool, initials_off, initials_len, b as usize);
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
            let pa = pool_bytes_slice(full_py_pool, full_py_off, full_py_len, a as usize);
            let pb = pool_bytes_slice(full_py_pool, full_py_off, full_py_len, b as usize);
            cmp_ignore_case(pa, pb).then_with(|| a.cmp(&b))
        });

        let d4 = t3.elapsed();

        // 必须与三棵排序表长度一致，否则 `sort_indexes_valid` 永远为 false，
        // C# 每次搜索前 `EnsureSearchIndexesReady` 都会再跑一遍全量 rebuild（约秒级）。
        self.live_count = self.name_sorted.len() as u32;

        eprintln!(
            "[findx_engine] rebuild_indexes: live={} P1_namepool={:.1}s P2_parent={:.1}s P3_reflookup={:.1}s P4_sort={:.1}s total={:.1}s",
            self.live_count, d1.as_secs_f64(), d2.as_secs_f64(), d3.as_secs_f64(), d4.as_secs_f64(), t_total.elapsed().as_secs_f64()
        );
        // Release excess capacity from all Vecs
        self.records.shrink_to_fit();
        self.name_pool.shrink_to_fit();
        self.initials_pool.shrink_to_fit();
        self.full_py_pool.shrink_to_fit();
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

    /// 将记录 `idx` 的全路径以小写 UTF-8 写入 `out`（`X:\a\b`），不含 `build_path_utf16` 的 HashSet。
    fn record_full_path_lower_utf8_into(
        &self,
        idx: u32,
        chain: &mut Vec<(u32, u16)>,
        out: &mut Vec<u8>,
    ) -> bool {
        out.clear();
        chain.clear();
        let mut cur = idx;
        let mut guard = 0usize;
        let vol_byte: u8 = loop {
            guard += 1;
            if guard > 1024 {
                return false;
            }
            let r = match self.records.get(cur as usize) {
                Some(x) if x.deleted == 0 => x,
                _ => return false,
            };
            chain.push((r.name_start, r.name_len));
            let pidx = r.parent_idx;
            if pidx == u32::MAX || pidx == cur {
                break r.vol;
            }
            cur = pidx;
        };

        let ch = char::from_u32(vol_byte as u32).unwrap_or('?');
        if ch.is_ascii() {
            out.push(ch.to_ascii_lowercase() as u8);
        } else {
            out.push(b'?');
        }
        out.extend_from_slice(b":");
        let pool = &self.name_pool;
        for (st, ln) in chain.iter().rev() {
            out.push(b'\\');
            let name = pool_utf8(pool, *st, *ln as u32);
            if name.is_ascii() {
                for b in name.as_bytes() {
                    out.push(b.to_ascii_lowercase());
                }
            } else {
                out.extend(name.to_lowercase().bytes());
            }
        }
        true
    }

    /// 与 C# `PathFilter` / `OrdinalIgnoreCase` 路径比较用的规范化键（小写、`\`、去尾 `\`）。
    pub fn normalize_dir_path_key(path: &str) -> String {
        let mut s: String = path.trim().chars().map(|c| if c == '/' { '\\' } else { c }).collect();
        while s.len() > 3 && s.ends_with('\\') {
            s.pop();
        }
        s.to_lowercase()
    }

    /// 沿父链拼出 UTF-8 全路径后做子串匹配（`needle_lower` 已规范化小写 UTF-8）。
    fn record_path_lower_utf8_contains_needle(
        &self,
        idx: u32,
        needle_lower: &[u8],
        scratch: &mut Vec<u8>,
        chain: &mut Vec<(u32, u16)>,
    ) -> bool {
        if needle_lower.is_empty() {
            return true;
        }
        if !self.record_full_path_lower_utf8_into(idx, chain, scratch) {
            return false;
        }
        if needle_lower.len() > scratch.len() {
            return false;
        }
        scratch
            .windows(needle_lower.len())
            .any(|w| w == needle_lower)
    }

    fn rebuild_path_navigation_indexes(&mut self, live: &[u32]) {
        self.children_by_parent_idx.clear();
        self.dir_path_lower_to_idx.clear();
        let records = &self.records;
        let mut chain = Vec::<(u32, u16)>::with_capacity(64);
        let mut scratch = Vec::<u8>::with_capacity(512);

        for &idx in live {
            let r = &records[idx as usize];
            if r.deleted != 0 {
                continue;
            }
            let p = r.parent_idx;
            if p != u32::MAX {
                self.children_by_parent_idx.entry(p).or_default().push(idx);
            }
            if (r.attr & 0x10) == 0 {
                continue;
            }
            if !self.record_full_path_lower_utf8_into(idx, &mut chain, &mut scratch) {
                continue;
            }
            let key = match std::str::from_utf8(&scratch) {
                Ok(s) => Self::normalize_dir_path_key(s),
                Err(_) => continue,
            };
            self.dir_path_lower_to_idx.entry(key).or_insert(idx);
        }
    }

    /// 将规范化后的目录路径（与 `normalize_dir_path_key` 一致）解析为目录行号；未找到返回 -1。
    pub fn resolve_dir_path_lower(&self, path: &str) -> i32 {
        let key = Self::normalize_dir_path_key(path);
        self.dir_path_lower_to_idx
            .get(&key)
            .copied()
            .map(|i| i as i32)
            .unwrap_or(-1)
    }

    /// `idx` 与 `root_dir_idx` 相同，或沿 `parent_idx` 向上可达 `root_dir_idx`（均在存活记录上）。
    pub fn index_is_under_dir_root(&self, idx: u32, root_dir_idx: u32) -> bool {
        let mut cur = idx;
        for _ in 0..2048 {
            if cur == root_dir_idx {
                return true;
            }
            let r = match self.records.get(cur as usize) {
                Some(x) if x.deleted == 0 => x,
                _ => return false,
            };
            let p = r.parent_idx;
            if p == u32::MAX || p == cur {
                return false;
            }
            cur = p;
        }
        false
    }

    /// 在 `root_dir_idx` 为根的子树（含根）中收集「文件名忽略大小写前缀」匹配，最多 `max_results` 条，最多访问 `max_nodes` 个结点。
    pub fn search_name_prefix_in_subtree(
        &self,
        root_dir_idx: u32,
        prefix: &str,
        out: &mut Vec<u32>,
        max_results: usize,
        max_nodes: usize,
    ) {
        out.clear();
        if max_results == 0 || max_nodes == 0 || prefix.is_empty() {
            return;
        }
        let records = &self.records;
        let pool = &self.name_pool;
        let mut stack = vec![root_dir_idx];
        let mut seen = HashSet::new();
        let mut visited = 0usize;
        while let Some(cur) = stack.pop() {
            if !seen.insert(cur) {
                continue;
            }
            visited += 1;
            if visited > max_nodes {
                return;
            }
            let r = match records.get(cur as usize) {
                Some(x) if x.deleted == 0 => x,
                _ => continue,
            };
            let name = pool_utf8(pool, r.name_start, r.name_len as u32);
            if starts_with_ignore_case(name, prefix) {
                out.push(cur);
                if out.len() >= max_results {
                    return;
                }
            }
            if let Some(ch) = self.children_by_parent_idx.get(&cur) {
                for &c in ch.iter().rev() {
                    stack.push(c);
                }
            }
        }
    }

    /// 在「文件名忽略大小写前缀」连续区间内扫描，仅保留**全路径**包含 `path_needle`（忽略大小写）的条目。
    /// 用于 `parent:`/`path:` + 单字符等场景：全局前缀桶的前 `max_results` 条常全部落在其它目录外。
    ///
    /// 注意：索引按**文件名**排序，无法从路径直接二分；本函数只能沿「某前缀」的全局区间扫，
    /// 但路径匹配已优化为无 `HashSet`/无整串 UTF-16 的 UTF-8 子串比较，避免单字符前缀下每秒数万次堆分配。
    pub fn search_name_prefix_path_needle(
        &self,
        prefix: &str,
        path_needle: &str,
        out: &mut Vec<u32>,
        max_results: usize,
        max_scan: usize,
    ) {
        out.clear();
        if max_results == 0
            || max_scan == 0
            || path_needle.is_empty()
            || (self.live_count > 0 && self.name_sorted.is_empty())
        {
            return;
        }
        let needle_norm: String = path_needle.chars().map(|c| if c == '/' { '\\' } else { c }).collect();
        let needle_lower = needle_norm.to_lowercase();
        let needle_bytes = needle_lower.as_bytes();

        let records = &self.records;
        let pool = &self.name_pool;
        let pos = self.name_sorted.partition_point(|&idx| {
            let r = &records[idx as usize];
            let name = pool_utf8(pool, r.name_start, r.name_len as u32);
            cmp_name_str_ignore_case(name, prefix) == Ordering::Less
        });
        let mut scratch = Vec::<u8>::with_capacity(512);
        let mut chain_buf: Vec<(u32, u16)> = Vec::with_capacity(64);
        let mut scanned = 0usize;
        for &idx in &self.name_sorted[pos..] {
            scanned += 1;
            if scanned > max_scan {
                return;
            }
            let r = &records[idx as usize];
            let name = pool_utf8(pool, r.name_start, r.name_len as u32);
            if !starts_with_ignore_case(name, prefix) {
                break;
            }
            if self.record_path_lower_utf8_contains_needle(idx, needle_bytes, &mut scratch, &mut chain_buf) {
                out.push(idx);
                if out.len() >= max_results {
                    return;
                }
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

    pub fn get_path_depth(&self, idx: i32) -> i32 {
        if idx < 0 || idx as usize >= self.records.len() {
            return -1;
        }

        let mut cur = idx as u32;
        let mut visited = HashSet::new();
        let mut depth = 1i32; // drive root, e.g. C:

        loop {
            if !visited.insert(cur) {
                return -1;
            }

            let r = match self.records.get(cur as usize) {
                Some(x) if x.deleted == 0 => x,
                _ => return -1,
            };

            depth += 1; // current segment
            let pidx = r.parent_idx;
            if pidx != u32::MAX && pidx != cur {
                cur = pidx;
                continue;
            }

            return depth;
        }
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
        *out_mtime = compact_to_ticks_or_zero(r.mtime);
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
        out_ctime: &mut i64,
        out_atime: &mut i64,
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
        *out_mtime = compact_to_ticks_or_zero(r.mtime);
        *out_ctime = compact_to_ticks_or_zero(r.ctime);
        *out_atime = compact_to_ticks_or_zero(r.atime);
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

    /// 全拼子串线性扫描（短 needle 与单元测试对照路径）。
    pub(crate) fn search_full_py_contains_linear(&self, needle: &str, out: &mut Vec<u32>, max_results: usize) {
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
            let fp = self.full_py_slice(i);
            if contains_ignore_case_bytes(fp, needle_bytes) {
                out.push(i as u32);
            }
        }
    }

    /// 全拼子串：needle ≥3 走 trigram 倒排 + 池字节校验；否则走线性扫描。
    pub fn search_full_py_contains(&self, needle: &str, out: &mut Vec<u32>, max_results: usize) {
        out.clear();
        if max_results == 0 || needle.is_empty() {
            return;
        }
        let needle_bytes = needle.as_bytes();
        if needle_bytes.len() < 3 {
            self.search_full_py_contains_linear(needle, out, max_results);
            return;
        }

        self.ensure_full_py_trigram();
        let guard = self.full_py_trigram.lock().unwrap();

        let mut keys: Vec<u32> = needle_bytes
            .windows(3)
            .map(|w| pack_trigram(w[0], w[1], w[2]))
            .collect();
        keys.sort_unstable();
        keys.dedup();

        let mut lists: Vec<&[u32]> = Vec::with_capacity(keys.len());
        for k in &keys {
            match guard.postings.get(k) {
                Some(v) => lists.push(v.as_slice()),
                None => return,
            }
        }
        let ids = intersect_sorted_postings(&lists);
        drop(guard);
        self.append_py_contains_verified_ids(needle_bytes, &ids, out, max_results, true);
    }

    /// 对 trigram 交集 `ids` 做字节池子串校验；`full_py` 为 true 否则首字母池。
    /// 大交集时并行扫描，避免「真命中极少却要求凑满 max_results」时顺序扫完整个 ids（可达 1s+）。
    fn append_py_contains_verified_ids(
        &self,
        needle_bytes: &[u8],
        ids: &[u32],
        out: &mut Vec<u32>,
        max_results: usize,
        full_py: bool,
    ) {
        out.clear();
        if max_results == 0 || ids.is_empty() {
            return;
        }
        let max_out = max_results.min(8192).max(1);
        const PAR_THRESHOLD: usize = 6144;
        if ids.len() <= PAR_THRESHOLD {
            for &idx in ids {
                if out.len() >= max_out {
                    break;
                }
                let r = &self.records[idx as usize];
                if r.deleted != 0 {
                    continue;
                }
                let name = pool_utf8(&self.name_pool, r.name_start, r.name_len as u32);
                if !name_contains_cjk(name) {
                    continue;
                }
                let hit = if full_py {
                    contains_ignore_case_bytes(self.full_py_slice(idx as usize), needle_bytes)
                } else {
                    contains_ignore_case_bytes(self.initials_slice(idx as usize), needle_bytes)
                };
                if hit {
                    out.push(idx);
                }
            }
            return;
        }

        let name_pool = &self.name_pool;
        let records = &self.records;
        let mut hits: Vec<u32> = ids
            .par_iter()
            .filter_map(|&idx| {
                let r = &records[idx as usize];
                if r.deleted != 0 {
                    return None;
                }
                let name = pool_utf8(name_pool, r.name_start, r.name_len as u32);
                if !name_contains_cjk(name) {
                    return None;
                }
                let ok = if full_py {
                    contains_ignore_case_bytes(
                        pool_bytes_slice(
                            &self.full_py_pool,
                            &self.full_py_off,
                            &self.full_py_len,
                            idx as usize,
                        ),
                        needle_bytes,
                    )
                } else {
                    contains_ignore_case_bytes(
                        pool_bytes_slice(
                            &self.initials_pool,
                            &self.initials_off,
                            &self.initials_len,
                            idx as usize,
                        ),
                        needle_bytes,
                    )
                };
                if ok { Some(idx) } else { None }
            })
            .collect();
        hits.truncate(max_out);
        hits.sort_unstable();
        out.extend_from_slice(&hits);
    }

    fn search_initials_contains_linear(&self, needle: &str, out: &mut Vec<u32>, max_results: usize) {
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
            let initials = self.initials_slice(i);
            if contains_ignore_case_bytes(initials, needle_bytes) {
                out.push(i as u32);
            }
        }
    }

    /// 首字母子串：needle ≥3 走 trigram 倒排 + 池字节校验；否则线性扫描。
    pub fn search_initials_contains(&self, needle: &str, out: &mut Vec<u32>, max_results: usize) {
        out.clear();
        if max_results == 0 || needle.is_empty() {
            return;
        }
        let needle_bytes = needle.as_bytes();
        if needle_bytes.len() < 3 {
            self.search_initials_contains_linear(needle, out, max_results);
            return;
        }

        self.ensure_initials_trigram();
        let guard = self.initials_trigram.lock().unwrap();

        let mut keys: Vec<u32> = needle_bytes
            .windows(3)
            .map(|w| pack_trigram(w[0], w[1], w[2]))
            .collect();
        keys.sort_unstable();
        keys.dedup();

        let mut lists: Vec<&[u32]> = Vec::with_capacity(keys.len());
        for k in &keys {
            match guard.postings.get(k) {
                Some(v) => lists.push(v.as_slice()),
                None => return,
            }
        }
        let ids = intersect_sorted_postings(&lists);
        drop(guard);
        self.append_py_contains_verified_ids(needle_bytes, &ids, out, max_results, false);
    }

    /// 候选过少时的补全：与 `search_query_matches` 一致（按字混合 / 全拼子串 / 首字母子串），不再使用全拼子序列模糊。
    pub fn search_full_py_fuzzy(&self, needle: &str, out: &mut Vec<u32>, max_results: usize) {
        self.search_query_matches(needle, out, max_results);
    }

    pub fn search_query_matches(&self, query: &str, out: &mut Vec<u32>, max_results: usize) {
        out.clear();
        if max_results == 0 || query.is_empty() {
            return;
        }

        let name_pool = &self.name_pool;
        let fp_pool = &self.full_py_pool;
        let fp_off = &self.full_py_off;
        let fp_len = &self.full_py_len;
        let ini_pool = &self.initials_pool;
        let ini_off = &self.initials_off;
        let ini_len = &self.initials_len;

        let ql = query.len();
        let ascii_q = query.bytes().all(|b| b.is_ascii_alphanumeric());

        let mut hits: Vec<(i32, u32)> = self
            .records
            .par_iter()
            .enumerate()
            .filter_map(|(i, r)| {
                if r.deleted != 0 {
                    return None;
                }
                // 纯 ASCII 查询：全拼/首字母子串命中要求池长度至少覆盖 query，否则不可能匹配（大幅削减全盘扫描量）
                if ascii_q && ql >= 2 {
                    let il = ini_len.get(i).copied().unwrap_or(0) as usize;
                    let fl = fp_len.get(i).copied().unwrap_or(0) as usize;
                    if il < ql && fl < ql {
                        return None;
                    }
                }
                let name = pool_utf8(name_pool, r.name_start, r.name_len as u32);
                let fp = pool_bytes_slice(fp_pool, fp_off, fp_len, i);
                let ini = pool_bytes_slice(ini_pool, ini_off, ini_len, i);
                let matched = match_query_with_pinyin_cache(query, name, Some((fp, ini)));
                if !matched.is_match() {
                    return None;
                }
                Some((matched.score, i as u32))
            })
            .collect();

        hits.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        hits.truncate(max_results);
        out.extend(hits.into_iter().map(|(_, idx)| idx));
    }

    /// 维护按分数最高的前 K 条；禁止在「未满」时每插入一次全表排序（K=out_cap 可达 8192 时 O(K²) 会卡十几秒）。
    fn push_topk(best: &mut Vec<(i32, u32)>, score: i32, idx: u32, max_results: usize) {
        if max_results == 0 {
            return;
        }

        if best.len() < max_results {
            best.push((score, idx));
            return;
        }

        let (min_pos, &(min_s, min_i)) = best
            .iter()
            .enumerate()
            .min_by_key(|(_, &(s, i))| (s, Reverse(i)))
            .expect("best non-empty");
        if score > min_s || (score == min_s && idx < min_i) {
            best[min_pos] = (score, idx);
        }
    }

    fn record_path_depth(&self, idx: u32) -> i32 {
        let mut cur = idx;
        let mut depth = 1i32;
        let mut guard = 0usize;

        loop {
            guard += 1;
            if guard > 1024 {
                return 1024;
            }

            let Some(r) = self.records.get(cur as usize) else {
                return depth;
            };

            depth += 1;
            let pidx = r.parent_idx;
            if pidx == u32::MAX || pidx == cur {
                return depth;
            }
            cur = pidx;
        }
    }

    fn rust_match_score(&self, idx: u32, name: &str, matched: QueryMatch, prefer_pinyin: bool) -> i32 {
        let r = &self.records[idx as usize];
        let mut score = matched.score;

        if prefer_pinyin && name_contains_cjk(name) {
            score += match matched.kind {
                MatchKind::FullPinyin => 340,
                MatchKind::Initials => 280,
                MatchKind::Mixed => 420,
                _ => 0,
            };

            let ext = name.rsplit('.').next().unwrap_or_default();
            if matches!(
                ext,
                "doc" | "docx" | "pdf" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "md" | "csv" | "rtf"
            ) {
                score += 170;
            } else if matches!(ext, "lnk" | "png" | "jpg" | "jpeg" | "gif" | "webp" | "ico") {
                score -= 90;
            }

            if (r.attr & 0x10) != 0 {
                score -= 35;
            }
        }

        score
    }

    pub fn search_simple_query(&self, query: &str, prefer_pinyin: bool, out: &mut Vec<u32>, max_results: usize) {
        out.clear();
        if max_results == 0 || query.is_empty() {
            return;
        }

        let keyword = query.to_ascii_lowercase();
        let ascii = is_ascii_alnum(&keyword);
        let mut candidates: HashSet<u32> = HashSet::with_capacity(2048);
        let t_all = std::time::Instant::now();
        self.collect_simple_candidates(&keyword, query, max_results, &mut candidates);

        let cand_raw = candidates.len();
        let max_budget = max_results.saturating_mul(512).clamp(4096, 262_144);
        if candidates.len() > max_budget && ascii {
            let ql = keyword.len();
            candidates.retain(|&idx| {
                let il = self.initials_len.get(idx as usize).copied().unwrap_or(0) as usize;
                let fl = self.full_py_len.get(idx as usize).copied().unwrap_or(0) as usize;
                il >= ql || fl >= ql
            });
        }
        if candidates.len() > max_budget {
            let mut v: Vec<u32> = candidates.iter().copied().collect();
            v.sort_unstable();
            v.truncate(max_budget);
            candidates.clear();
            candidates.extend(v);
        }
        let cand_scored = candidates.len();

        let t0 = std::time::Instant::now();
        let mut best: Vec<(i32, u32)> = Vec::with_capacity(max_results.min(256));
        let prefer = prefer_pinyin && ascii;
        for idx in candidates {
            let r = &self.records[idx as usize];
            if r.deleted != 0 {
                continue;
            }
            let name = pool_utf8(&self.name_pool, r.name_start, r.name_len as u32);
            let fp = self.full_py_slice(idx as usize);
            let ini = self.initials_slice(idx as usize);
            let matched = match_query_with_pinyin_cache(&keyword, name, Some((fp, ini)));
            if !matched.is_match() {
                continue;
            }

            let mut score = self.rust_match_score(idx, name, matched, prefer);
            score -= self.record_path_depth(idx) * 2;
            score -= name.chars().count() as i32;
            if (r.attr & 0x10) != 0 {
                score += 5;
            }
            Self::push_topk(&mut best, score, idx, max_results);
        }

        if best.is_empty() && ascii && keyword.len() >= 3 {
            self.search_query_matches(&keyword, out, max_results);
            if engine_trace_on() {
                eprintln!(
                    "[findx-engine] search_simple_query q={:?} cand_raw={} cand_scored={} collect+score_ms={:.2} total_ms={:.2} path=search_query_matches out={}",
                    query,
                    cand_raw,
                    cand_scored,
                    t0.elapsed().as_secs_f64() * 1000.0,
                    t_all.elapsed().as_secs_f64() * 1000.0,
                    out.len(),
                );
            }
            return;
        }

        best.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        out.extend(best.into_iter().map(|(_, idx)| idx));

        if engine_trace_on() {
            eprintln!(
                "[findx-engine] search_simple_query q={:?} cand_raw={} cand_scored={} score_only_ms={:.2} total_ms={:.2} out={}",
                query,
                cand_raw,
                cand_scored,
                t0.elapsed().as_secs_f64() * 1000.0,
                t_all.elapsed().as_secs_f64() * 1000.0,
                out.len(),
            );
        }
    }

    fn collect_simple_candidates(
        &self,
        keyword: &str,
        original_query: &str,
        max_results: usize,
        candidates: &mut HashSet<u32>,
    ) {
        let ascii = is_ascii_alnum(keyword);
        let has_cjk = original_query.chars().any(is_cjk);
        let full_cap = 8192usize.min(max_results.saturating_mul(256).max(1024));
        // 子串倒排交集可能很大；若仍用 full_cap(≤8192) 截断，真命中排在 8192 名之后会漏检并触发全盘扫描。
        let substring_cap = max_results.saturating_mul(512).clamp(8192, 262_144);
        let short_ascii = ascii && keyword.len() <= 1;
        let cap = if short_ascii { 512usize.min(full_cap) } else { full_cap };
        let mut temp = Vec::with_capacity(2048);

        self.search_name_prefix(keyword, &mut temp, cap);
        candidates.extend(temp.iter().copied());

        if ascii && !short_ascii {
            self.search_pinyin_prefix(keyword, &mut temp, cap);
            candidates.extend(temp.iter().copied());

            self.search_full_py_prefix(keyword, &mut temp, cap);
            candidates.extend(temp.iter().copied());

            let py_init_max = keyword.len().saturating_sub(1).min(4);
            for plen in (2..=py_init_max).rev() {
                self.search_pinyin_prefix(&keyword[..plen], &mut temp, 512);
                candidates.extend(temp.iter().copied());
            }

            let full_py_max = keyword.len().saturating_sub(1).min(6);
            for plen in (2..=full_py_max).rev() {
                self.search_full_py_prefix(&keyword[..plen], &mut temp, 512);
                candidates.extend(temp.iter().copied());
            }

            if keyword.len() >= 3 {
                let bytes = keyword.as_bytes();
                let first = bytes[0] as char;
                let p_max = 6usize.min(keyword.len().saturating_sub(2));
                for p in 1..=p_max {
                    if !bytes[p].is_ascii_lowercase() {
                        continue;
                    }
                    let q_max = (p + 6).min(keyword.len().saturating_sub(1));
                    for q in p + 1..=q_max {
                        if !bytes[q].is_ascii_lowercase() {
                            continue;
                        }
                        let prefix = format!("{first}{}{}", bytes[p] as char, bytes[q] as char);
                        self.search_pinyin_prefix(&prefix, &mut temp, 512);
                        candidates.extend(temp.iter().copied());
                    }
                }
            }

            if keyword.len() >= 3 {
                self.search_full_py_contains(keyword, &mut temp, substring_cap);
                candidates.extend(temp.iter().copied());

                // 首字母子串（如 ybhz→月报汇总）
                if keyword.len() <= 64 {
                    self.search_initials_contains(keyword, &mut temp, substring_cap);
                    candidates.extend(temp.iter().copied());
                }
            } else if keyword.len() == 2 && candidates.len() < 64 {
                self.search_initials_contains(keyword, &mut temp, 128);
                candidates.extend(temp.iter().copied());
            }

            if candidates.len() < 32 {
                let add_cap = ((32usize.saturating_sub(candidates.len())) * 8).clamp(256, 2048);
                if let Some(anchor) = build_ascii_pinyin_initials_anchor(keyword) {
                    if let Some(tail) = build_ascii_pinyin_tail_token(keyword) {
                        self.search_initials_contains(&anchor, &mut temp, add_cap * 2);
                        let initials_hits = temp.clone();
                        self.search_full_py_contains(&tail, &mut temp, add_cap * 2);
                        let tail_set: HashSet<u32> = temp.iter().copied().collect();
                        let mut added = 0usize;
                        for idx in initials_hits {
                            if tail_set.contains(&idx) {
                                candidates.insert(idx);
                                added += 1;
                                if added >= add_cap {
                                    break;
                                }
                            }
                        }
                    }

                    if keyword.len() >= 5 && candidates.len() < 32 {
                        self.search_full_py_fuzzy(keyword, &mut temp, add_cap);
                        candidates.extend(temp.iter().copied());
                    }

                    self.search_initials_contains(&anchor, &mut temp, add_cap);
                    candidates.extend(temp.iter().copied());
                }
            }
        }

        if has_cjk {
            self.search_name_contains(original_query, &mut temp, cap);
            candidates.extend(temp.iter().copied());
        }
    }

    pub fn search_simple_terms(
        &self,
        terms: &[&str],
        prefer_pinyin: bool,
        out: &mut Vec<u32>,
        max_results: usize,
    ) {
        out.clear();
        if max_results == 0 || terms.is_empty() {
            return;
        }

        let normalized: Vec<String> = terms
            .iter()
            .map(|term| term.trim())
            .filter(|term| !term.is_empty())
            .map(|term| term.to_ascii_lowercase())
            .collect();
        if normalized.is_empty() {
            return;
        }

        let mut intersection: Option<HashSet<u32>> = None;
        for term in &normalized {
            let mut term_candidates: HashSet<u32> = HashSet::with_capacity(1024);
            self.collect_simple_candidates(term, term, max_results, &mut term_candidates);

            let max_budget = max_results.saturating_mul(512).clamp(4096, 262_144);
            if term_candidates.len() > max_budget && is_ascii_alnum(term) {
                let ql = term.len();
                term_candidates.retain(|&idx| {
                    let il = self.initials_len.get(idx as usize).copied().unwrap_or(0) as usize;
                    let fl = self.full_py_len.get(idx as usize).copied().unwrap_or(0) as usize;
                    il >= ql || fl >= ql
                });
            }
            if term_candidates.len() > max_budget {
                let mut v: Vec<u32> = term_candidates.iter().copied().collect();
                v.sort_unstable();
                v.truncate(max_budget);
                term_candidates.clear();
                term_candidates.extend(v);
            }

            if term_candidates.is_empty() && is_ascii_alnum(term) && term.len() >= 3 {
                let mut fallback = Vec::with_capacity(max_results.min(256));
                self.search_query_matches(term, &mut fallback, 2048);
                term_candidates.extend(fallback);
            }

            if term_candidates.is_empty() {
                return;
            }

            intersection = Some(match intersection.take() {
                None => term_candidates,
                Some(existing) => existing
                    .into_iter()
                    .filter(|idx| term_candidates.contains(idx))
                    .collect(),
            });

            if intersection.as_ref().is_some_and(|set| set.is_empty()) {
                return;
            }
        }

        let Some(candidates) = intersection else { return };
        let prefer = prefer_pinyin && normalized.iter().all(|term| is_ascii_alnum(term));
        let mut best: Vec<(i32, u32)> = Vec::with_capacity(max_results.min(256));
        for idx in candidates {
            let r = &self.records[idx as usize];
            if r.deleted != 0 {
                continue;
            }

            let name = pool_utf8(&self.name_pool, r.name_start, r.name_len as u32);
            let fp = self.full_py_slice(idx as usize);
            let ini = self.initials_slice(idx as usize);
            let mut total_score = 0i32;
            let mut matched_any = false;
            for term in &normalized {
                let matched = match_query_with_pinyin_cache(term, name, Some((fp, ini)));
                if !matched.is_match() {
                    matched_any = false;
                    break;
                }
                matched_any = true;
                total_score += self.rust_match_score(idx, name, matched, prefer);
            }

            if !matched_any {
                continue;
            }

            total_score -= self.record_path_depth(idx) * 2;
            total_score -= name.chars().count() as i32;
            if (r.attr & 0x10) != 0 {
                total_score += 5;
            }

            Self::push_topk(&mut best, total_score, idx, max_results);
        }

        best.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        out.extend(best.into_iter().map(|(_, idx)| idx));
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
    // binary persistence
    // FXBIN05：核心块；加载后重算拼音池 + trigram。
    // FXBIN06：核心块 + PINYAUX 尾块（拼音池 + trigram 倒排快照），冷启动跳过重建。
    // Format: MAGIC(8) + header(20) + records + name_pool + ref + sorted×3 [+ PINYAUX(8)+aux_len(8)+aux_blob]
    // -------------------------------------------------------------------

    const BIN_MAGIC_LEGACY: &'static [u8; 8] = b"FXBIN05\0";
    const BIN_MAGIC: &'static [u8; 8] = b"FXBIN06\0";
    const PINY_AUX_MARKER: &'static [u8; 8] = b"PINYAUX\0";

    fn serialize_trigram_postings(m: &HashMap<u32, Vec<u32>>) -> Vec<u8> {
        let mut keys: Vec<u32> = m.keys().copied().collect();
        keys.sort_unstable();
        let mut out = Vec::new();
        out.extend_from_slice(&(keys.len() as u32).to_le_bytes());
        for k in keys {
            let v = m.get(&k).unwrap();
            out.extend_from_slice(&k.to_le_bytes());
            let plen = v.len() as u32;
            out.extend_from_slice(&plen.to_le_bytes());
            for &id in v {
                out.extend_from_slice(&id.to_le_bytes());
            }
        }
        out
    }

    fn deserialize_trigram_postings(s: &mut &[u8]) -> std::io::Result<HashMap<u32, Vec<u32>>> {
        use std::io::{Error, ErrorKind};
        let read_u32 = |buf: &mut &[u8]| -> std::io::Result<u32> {
            if buf.len() < 4 {
                return Err(Error::new(ErrorKind::UnexpectedEof, "trigram eof"));
            }
            let v = u32::from_le_bytes(buf[0..4].try_into().unwrap());
            *buf = &buf[4..];
            Ok(v)
        };
        let n = read_u32(s)? as usize;
        let mut m = HashMap::with_capacity(n);
        for _ in 0..n {
            let k = read_u32(s)?;
            let plen = read_u32(s)? as usize;
            if s.len() < plen * 4 {
                return Err(Error::new(ErrorKind::UnexpectedEof, "trigram postings"));
            }
            let mut vec = Vec::with_capacity(plen);
            for _ in 0..plen {
                vec.push(read_u32(s)?);
            }
            m.insert(k, vec);
        }
        Ok(m)
    }

    fn build_aux_persist_blob(&self) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&1u32.to_le_bytes()); // format version

        let il = self.initials_pool.len() as u64;
        v.extend_from_slice(&il.to_le_bytes());
        v.extend_from_slice(&self.initials_pool);

        let fl = self.full_py_pool.len() as u64;
        v.extend_from_slice(&fl.to_le_bytes());
        v.extend_from_slice(&self.full_py_pool);

        let n = self.records.len() as u64;
        v.extend_from_slice(&n.to_le_bytes());

        let nrec = self.records.len();
        let noff = nrec * 4;
        let nlen = nrec * 2;
        unsafe {
            v.extend_from_slice(std::slice::from_raw_parts(
                self.initials_off.as_ptr() as *const u8,
                noff,
            ));
            v.extend_from_slice(std::slice::from_raw_parts(
                self.initials_len.as_ptr() as *const u8,
                nlen,
            ));
            v.extend_from_slice(std::slice::from_raw_parts(
                self.full_py_off.as_ptr() as *const u8,
                noff,
            ));
            v.extend_from_slice(std::slice::from_raw_parts(
                self.full_py_len.as_ptr() as *const u8,
                nlen,
            ));
        }

        let fp = self.full_py_trigram.lock().unwrap();
        v.extend_from_slice(&Self::serialize_trigram_postings(&fp.postings));
        let ip = self.initials_trigram.lock().unwrap();
        v.extend_from_slice(&Self::serialize_trigram_postings(&ip.postings));

        v
    }

    fn restore_aux_persist_blob(&mut self, blob: &[u8]) -> std::io::Result<()> {
        use std::io::{Error, ErrorKind};
        let mut s = blob;
        if s.len() < 4 {
            return Err(Error::new(ErrorKind::InvalidData, "aux blob too short"));
        }
        let ver = u32::from_le_bytes(s[0..4].try_into().unwrap());
        if ver != 1 {
            return Err(Error::new(ErrorKind::InvalidData, "unknown aux version"));
        }
        s = &s[4..];

        let read_u64 = |buf: &mut &[u8]| -> std::io::Result<usize> {
            if buf.len() < 8 {
                return Err(Error::new(ErrorKind::UnexpectedEof, "aux u64"));
            }
            let v = u64::from_le_bytes(buf[0..8].try_into().unwrap()) as usize;
            *buf = &buf[8..];
            Ok(v)
        };

        let ilen = read_u64(&mut s)?;
        if s.len() < ilen {
            return Err(Error::new(ErrorKind::UnexpectedEof, "initials_pool"));
        }
        let initials_pool = s[..ilen].to_vec();
        s = &s[ilen..];

        let flen = read_u64(&mut s)?;
        if s.len() < flen {
            return Err(Error::new(ErrorKind::UnexpectedEof, "full_py_pool"));
        }
        let full_py_pool = s[..flen].to_vec();
        s = &s[flen..];

        let nrec = read_u64(&mut s)?;
        if nrec != self.records.len() {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "aux record count mismatch",
            ));
        }

        let need_off = nrec * 4;
        let need_len = nrec * 2;
        if s.len() < need_off + need_len + need_off + need_len {
            return Err(Error::new(ErrorKind::UnexpectedEof, "off/len arrays"));
        }

        let mut initials_off = vec![0u32; nrec];
        for i in 0..nrec {
            let o = i * 4;
            initials_off[i] = u32::from_le_bytes(s[o..o + 4].try_into().unwrap());
        }
        s = &s[need_off..];

        let mut initials_len = vec![0u16; nrec];
        for i in 0..nrec {
            let o = i * 2;
            initials_len[i] = u16::from_le_bytes(s[o..o + 2].try_into().unwrap());
        }
        s = &s[need_len..];

        let mut full_py_off = vec![0u32; nrec];
        for i in 0..nrec {
            let o = i * 4;
            full_py_off[i] = u32::from_le_bytes(s[o..o + 4].try_into().unwrap());
        }
        s = &s[need_off..];

        let mut full_py_len = vec![0u16; nrec];
        for i in 0..nrec {
            let o = i * 2;
            full_py_len[i] = u16::from_le_bytes(s[o..o + 2].try_into().unwrap());
        }
        s = &s[need_len..];

        let fp_post = Self::deserialize_trigram_postings(&mut s)?;
        let ini_post = Self::deserialize_trigram_postings(&mut s)?;
        if !s.is_empty() {
            return Err(Error::new(ErrorKind::InvalidData, "trailing aux bytes"));
        }

        self.initials_pool = initials_pool;
        self.full_py_pool = full_py_pool;
        self.initials_off = initials_off;
        self.initials_len = initials_len;
        self.full_py_off = full_py_off;
        self.full_py_len = full_py_len;

        *self.full_py_trigram.get_mut().unwrap() = TrigramIndexSnap {
            pool_len: self.full_py_pool.len(),
            postings: fp_post,
        };
        *self.initials_trigram.get_mut().unwrap() = TrigramIndexSnap {
            pool_len: self.initials_pool.len(),
            postings: ini_post,
        };

        Ok(())
    }

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

        let aux = self.build_aux_persist_blob();
        w.write_all(Self::PINY_AUX_MARKER)?;
        w.write_all(&(aux.len() as u64).to_le_bytes())?;
        w.write_all(&aux)?;

        w.flush()?;
        let pos = w.into_inner()?.metadata()?.len();
        Ok(pos)
    }

    pub fn load_from_file(&mut self, path: &str) -> std::io::Result<i32> {
        use std::io::{BufReader, Error, ErrorKind, Read};

        let f = std::fs::File::open(path)?;
        let mut r = BufReader::with_capacity(1 << 20, f);

        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        let is_v6 = &magic == Self::BIN_MAGIC;
        if !is_v6 && &magic != Self::BIN_MAGIC_LEGACY {
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

        if is_v6 {
            let mut tag = [0u8; 8];
            r.read_exact(&mut tag)?;
            if &tag != Self::PINY_AUX_MARKER {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "FXBIN06 missing PINYAUX trailer",
                ));
            }
            let mut lb = [0u8; 8];
            r.read_exact(&mut lb)?;
            let aux_len = u64::from_le_bytes(lb) as usize;
            let mut aux = vec![0u8; aux_len];
            r.read_exact(&mut aux)?;
            self.restore_aux_persist_blob(&aux)?;
        }

        let mut live_indices: Vec<u32> = Vec::new();
        for (i, rec) in self.records.iter().enumerate() {
            if rec.deleted == 0 {
                live_indices.push(i as u32);
            }
        }
        if !is_v6 {
            self.rebuild_pinyin_aux_pools(&live_indices);
        }
        self.rebuild_path_navigation_indexes(&live_indices);

        Ok(live_count as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::{compute_full_py_stack, match_query, Engine, MatchKind};

    #[test]
    fn full_pinyin_of_yuebao_contains_bao() {
        let (buf, len) = compute_full_py_stack("月报");
        let s = std::str::from_utf8(&buf[..len]).unwrap();
        assert_eq!(s, "yuebao");
    }

    #[test]
    fn match_query_finds_yuebao_by_bao() {
        let result = match_query("bao", "【彩石智能月报】马春天+3月.docx");
        assert!(result.is_match());
        assert_eq!(result.kind, MatchKind::FullPinyin);
    }

    #[test]
    fn match_query_windows_ascii_casefold() {
        let exact = match_query("windows", "Windows");
        assert!(exact.is_match());
        assert_eq!(exact.kind, MatchKind::Exact);

        let prefix = match_query("wind", "WindowsApps");
        assert!(prefix.is_match());
        assert_eq!(prefix.kind, MatchKind::Prefix);
    }

    #[test]
    fn full_py_contains_inverted_matches_linear() {
        let mut e = Engine::new();
        e.begin_bulk();
        let name = "月报汇总.md";
        let utf16: Vec<u16> = name.encode_utf16().collect();
        e.add_entry_utf16(1, 1, 0, &utf16, 0x20, 1, 1, 1, 1);
        let name2 = "退场申请.txt";
        let u2: Vec<u16> = name2.encode_utf16().collect();
        e.add_entry_utf16(1, 2, 0, &u2, 0x20, 1, 1, 1, 1);
        e.end_bulk();

        for needle in ["yuebao", "bao", "tui", "chang"] {
            let mut a = Vec::new();
            let mut b = Vec::new();
            e.search_full_py_contains(needle, &mut a, 100);
            e.search_full_py_contains_linear(needle, &mut b, 100);
            a.sort_unstable();
            b.sort_unstable();
            assert_eq!(a, b, "needle={needle}");
        }
    }

    #[test]
    fn search_simple_query_ybhz_uses_candidates_not_full_scan() {
        let mut e = Engine::new();
        e.begin_bulk();
        for i in 0..3000u32 {
            let name = format!("noise{i}.txt");
            let utf16: Vec<u16> = name.encode_utf16().collect();
            e.add_entry_utf16(1, 10 + i as u64, 0, &utf16, 0x20, 1, 1, 1, 1);
        }
        let utf16: Vec<u16> = "月报汇总.md".encode_utf16().collect();
        e.add_entry_utf16(1, 99, 0, &utf16, 0x20, 1, 1, 1, 1);
        e.end_bulk();

        let mut out = Vec::new();
        e.search_simple_query("ybhz", true, &mut out, 10);
        assert_eq!(out.len(), 1, "expected single hit among 3001 files");
        let mut verify = Vec::new();
        e.search_query_matches("ybhz", &mut verify, 10);
        assert_eq!(verify, out);
    }

    #[test]
    fn initials_contains_inverted_matches_linear() {
        let mut e = Engine::new();
        e.begin_bulk();
        let utf16: Vec<u16> = "月报汇总.md".encode_utf16().collect();
        e.add_entry_utf16(1, 1, 0, &utf16, 0x20, 1, 1, 1, 1);
        let u2: Vec<u16> = "退场申请.txt".encode_utf16().collect();
        e.add_entry_utf16(1, 2, 0, &u2, 0x20, 1, 1, 1, 1);
        e.end_bulk();

        for needle in ["ybhz", "ybh", "tcsq"] {
            let mut a = Vec::new();
            let mut b = Vec::new();
            e.search_initials_contains(needle, &mut a, 100);
            e.search_initials_contains_linear(needle, &mut b, 100);
            a.sort_unstable();
            b.sort_unstable();
            assert_eq!(a, b, "needle={needle}");
        }
    }

    #[test]
    fn fxb06_save_load_skips_pinyin_rebuild_matches_search() {
        let mut e = Engine::new();
        e.begin_bulk();
        let utf16: Vec<u16> = "月报汇总.md".encode_utf16().collect();
        e.add_entry_utf16(1, 1, 0, &utf16, 0x20, 1, 1, 1, 1);
        let u2: Vec<u16> = "退场申请.txt".encode_utf16().collect();
        e.add_entry_utf16(1, 2, 0, &u2, 0x20, 1, 1, 1, 1);
        e.end_bulk();

        let dir = std::env::temp_dir();
        let path = dir.join("findx_fxb06_roundtrip.bin");
        let path_s = path.to_str().unwrap();
        e.save_to_file(path_s).unwrap();

        let mut e2 = Engine::new();
        let n = e2.load_from_file(path_s).unwrap();
        assert_eq!(n, 2);

        let mut a = Vec::new();
        let mut b = Vec::new();
        e2.search_full_py_contains("bao", &mut a, 50);
        e2.search_full_py_contains_linear("bao", &mut b, 50);
        a.sort_unstable();
        b.sort_unstable();
        assert_eq!(a, b);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn match_query_nhao_is_mixed_nihao() {
        let r = match_query("nhao", "你好");
        assert!(r.is_match());
        assert_eq!(r.kind, MatchKind::Mixed);
    }

    #[test]
    fn match_query_nihao_full_pinyin() {
        let r = match_query("nihao", "你好");
        assert!(r.is_match());
        assert_eq!(r.kind, MatchKind::FullPinyin);
    }

    #[test]
    fn match_query_nzdaom_mixed() {
        let r = match_query("nzdaom", "你知道吗");
        assert!(r.is_match());
        assert_eq!(r.kind, MatchKind::Mixed);
    }

    #[test]
    fn match_query_nzdm_initials() {
        let r = match_query("nzdm", "你知道吗");
        assert!(r.is_match());
        assert_eq!(r.kind, MatchKind::Initials);
    }

    #[test]
    fn match_query_jt_initials_in_sentence() {
        let r = match_query("jt", "今天是什么日子");
        assert!(r.is_match());
        assert_eq!(r.kind, MatchKind::Initials);
    }

    #[test]
    fn match_query_nizhidaoma_partial_full() {
        let r = match_query("nizhidaoma", "你知道吗");
        assert!(r.is_match());
        assert_eq!(r.kind, MatchKind::FullPinyin);
    }

    #[test]
    fn match_query_no_subsequence_fuzzy_across_chars() {
        let r = match_query("nihao", "拟承担中");
        assert!(!r.is_match());
    }
}

fn fuzzy_match_bytes(query: &[u8], candidate: &[u8]) -> i32 {
    let mut qi = 0usize;
    let mut matched = 0i32;
    let mut start = usize::MAX;
    let mut end = 0usize;
    let mut prev = usize::MAX;
    let mut gaps = 0i32;
    let mut streak = 0i32;
    let mut best_streak = 0i32;

    for (idx, &b) in candidate.iter().enumerate() {
        if qi >= query.len() {
            break;
        }
        if b.to_ascii_lowercase() == query[qi] {
            if start == usize::MAX {
                start = idx;
            }
            if prev != usize::MAX {
                if idx == prev + 1 {
                    streak += 1;
                    if streak > best_streak {
                        best_streak = streak;
                    }
                } else {
                    gaps += (idx - prev - 1) as i32;
                    streak = 0;
                }
            }
            prev = idx;
            end = idx;
            matched += 10;
            qi += 1;
        }
    }
    if qi != query.len() {
        return 0;
    }

    let span = (end - start + 1) as i32;
    matched + best_streak * 8 - gaps - span
}
