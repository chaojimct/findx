//! C ABI，供 FindX.Core P/Invoke。调用方需保证单线程或外加锁（与现版 C# FileIndex 一致）。

use crate::engine::{highlight_query_ranges, match_query, pool_utf8, Engine};
use std::ffi::c_void;

const EPOCH_2000_TICKS: i64 = 630_822_816_000_000_000;
const TICKS_PER_SEC: i64 = 10_000_000;

pub struct EngineBox {
    pub inner: Engine,
    pub scratch_idx: Vec<u32>,
}

impl EngineBox {
    pub fn new() -> Self {
        Self {
            inner: Engine::new(),
            scratch_idx: Vec::with_capacity(64),
        }
    }
}

#[inline]
unsafe fn as_box<'a>(p: *mut EngineBox) -> Option<&'a mut EngineBox> {
    if p.is_null() {
        None
    } else {
        Some(&mut *p)
    }
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_create() -> *mut EngineBox {
    Box::into_raw(Box::new(EngineBox::new()))
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_destroy(p: *mut EngineBox) {
    if !p.is_null() {
        drop(Box::from_raw(p));
    }
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_clear(p: *mut EngineBox) {
    if let Some(b) = as_box(p) {
        b.inner.clear();
        b.scratch_idx.clear();
    }
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_try_get_index(
    p: *const EngineBox,
    vol: u16,
    file_ref: u64,
    out_idx: *mut i32,
) -> i32 {
    if p.is_null() || out_idx.is_null() {
        return 0;
    }
    match (&(*p).inner).try_live_index(vol, file_ref) {
        Some(i) => {
            *out_idx = i;
            1
        }
        None => {
            *out_idx = -1;
            0
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_live_count(p: *const EngineBox) -> i32 {
    if p.is_null() {
        return 0;
    }
    (&*p).inner.live_count as i32
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_add_entry_utf16(
    p: *mut EngineBox,
    vol: u16,
    file_ref: u64,
    parent_ref: u64,
    name_utf16: *const u16,
    name_len: i32,
    attr: u32,
    size: i64,
    mtime: i64,
    ctime: i64,
    atime: i64,
) -> i32 {
    let b = match as_box(p) {
        Some(x) => x,
        None => return -1,
    };
    if name_utf16.is_null() || name_len < 0 {
        return -2;
    }
    let name = std::slice::from_raw_parts(name_utf16, name_len as usize);
    b.inner
        .add_entry_utf16(vol, file_ref, parent_ref, name, attr, size, mtime, ctime, atime);
    0
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_upsert_entry_utf16(
    p: *mut EngineBox,
    vol: u16,
    file_ref: u64,
    parent_ref: u64,
    name_utf16: *const u16,
    name_len: i32,
    attr: u32,
    size: i64,
    mtime: i64,
    ctime: i64,
    atime: i64,
) -> i32 {
    let b = match as_box(p) {
        Some(x) => x,
        None => return -1,
    };
    if name_utf16.is_null() || name_len < 0 {
        return -2;
    }
    let name = std::slice::from_raw_parts(name_utf16, name_len as usize);
    b.inner
        .upsert_entry_utf16(vol, file_ref, parent_ref, name, attr, size, mtime, ctime, atime);
    0
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_remove_by_ref(p: *mut EngineBox, vol: u16, file_ref: u64) {
    if let Some(b) = as_box(p) {
        b.inner.remove_by_ref(vol, file_ref);
    }
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_rebuild_indexes(p: *mut EngineBox) {
    if let Some(b) = as_box(p) {
        b.inner.rebuild_indexes();
    }
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_begin_bulk(p: *mut EngineBox) {
    if let Some(b) = as_box(p) {
        b.inner.begin_bulk();
    }
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_end_bulk(p: *mut EngineBox) {
    if let Some(b) = as_box(p) {
        b.inner.end_bulk();
    }
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_is_sort_ready(p: *const EngineBox) -> i32 {
    if p.is_null() {
        return 0;
    }
    if (&(*p).inner).sort_indexes_valid() {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_is_in_bulk_load(p: *const EngineBox) -> i32 {
    if p.is_null() {
        return 0;
    }
    if (&(*p).inner).bulk_mode > 0 {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_search_name_prefix(
    p: *mut EngineBox,
    prefix_utf8: *const u8,
    prefix_len: i32,
    out_indices: *mut u32,
    out_cap: i32,
) -> i32 {
    let b = match as_box(p) {
        Some(x) => x,
        None => return -1,
    };
    if prefix_utf8.is_null() || prefix_len < 0 || out_indices.is_null() || out_cap <= 0 {
        return -2;
    }
    if b.inner.live_count > 0 && b.inner.name_sort_index_empty() {
        return -3;
    }
    let prefix =
        std::str::from_utf8(std::slice::from_raw_parts(prefix_utf8, prefix_len as usize))
            .unwrap_or("");
    b.inner
        .search_name_prefix(prefix, &mut b.scratch_idx, out_cap as usize);
    let n = b.scratch_idx.len();
    let cap = out_cap as usize;
    let ncpy = n.min(cap);
    std::ptr::copy_nonoverlapping(b.scratch_idx.as_ptr(), out_indices, ncpy);
    ncpy as i32
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_search_name_prefix_path_needle(
    p: *mut EngineBox,
    prefix_utf8: *const u8,
    prefix_len: i32,
    path_needle_utf8: *const u8,
    path_needle_len: i32,
    out_indices: *mut u32,
    out_cap: i32,
    max_scan: i32,
) -> i32 {
    let b = match as_box(p) {
        Some(x) => x,
        None => return -1,
    };
    if prefix_utf8.is_null()
        || prefix_len < 0
        || path_needle_utf8.is_null()
        || path_needle_len < 0
        || out_indices.is_null()
        || out_cap <= 0
        || max_scan <= 0
    {
        return -2;
    }
    if b.inner.live_count > 0 && b.inner.name_sort_index_empty() {
        return -3;
    }
    let prefix =
        std::str::from_utf8(std::slice::from_raw_parts(prefix_utf8, prefix_len as usize))
            .unwrap_or("");
    let path_needle =
        std::str::from_utf8(std::slice::from_raw_parts(
            path_needle_utf8,
            path_needle_len as usize,
        ))
        .unwrap_or("");
    b.inner.search_name_prefix_path_needle(
        prefix,
        path_needle,
        &mut b.scratch_idx,
        out_cap as usize,
        max_scan as usize,
    );
    let n = b.scratch_idx.len();
    let cap = out_cap as usize;
    let ncpy = n.min(cap);
    std::ptr::copy_nonoverlapping(b.scratch_idx.as_ptr(), out_indices, ncpy);
    ncpy as i32
}

/// 将规范化目录路径（UTF-8，与 `Engine::normalize_dir_path_key` 一致）解析为目录行号；未找到返回 -1。
#[no_mangle]
pub unsafe extern "C" fn findx_engine_resolve_dir_path_utf8(
    p: *const EngineBox,
    path_utf8: *const u8,
    path_len: i32,
) -> i32 {
    if p.is_null() || path_utf8.is_null() || path_len < 0 {
        return -2;
    }
    let path =
        std::str::from_utf8(std::slice::from_raw_parts(path_utf8, path_len as usize)).unwrap_or("");
    (&*p).inner.resolve_dir_path_lower(path)
}

/// 在 `root_dir_idx` 为根的子树内按文件名忽略大小写前缀收集索引，最多 `out_cap` 条、最多访问 `max_nodes` 个结点。
#[no_mangle]
pub unsafe extern "C" fn findx_engine_search_name_prefix_in_subtree(
    p: *mut EngineBox,
    root_dir_idx: u32,
    prefix_utf8: *const u8,
    prefix_len: i32,
    out_indices: *mut u32,
    out_cap: i32,
    max_nodes: i32,
) -> i32 {
    let b = match as_box(p) {
        Some(x) => x,
        None => return -1,
    };
    if prefix_utf8.is_null()
        || prefix_len < 0
        || out_indices.is_null()
        || out_cap <= 0
        || max_nodes <= 0
    {
        return -2;
    }
    if b.inner.live_count > 0 && b.inner.name_sort_index_empty() {
        return -3;
    }
    let prefix =
        std::str::from_utf8(std::slice::from_raw_parts(prefix_utf8, prefix_len as usize))
            .unwrap_or("");
    b.inner.search_name_prefix_in_subtree(
        root_dir_idx,
        prefix,
        &mut b.scratch_idx,
        out_cap as usize,
        max_nodes as usize,
    );
    let n = b.scratch_idx.len();
    let cap = out_cap as usize;
    let ncpy = n.min(cap);
    std::ptr::copy_nonoverlapping(b.scratch_idx.as_ptr(), out_indices, ncpy);
    ncpy as i32
}

/// 对 `indices[0..n)` 逐条写入 `out_mask[i]`：1 表示 `index_is_under_dir_root(indices[i], root_dir_idx)`。
#[no_mangle]
pub unsafe extern "C" fn findx_engine_mask_indices_under_dir_root(
    p: *const EngineBox,
    indices: *const u32,
    n: i32,
    root_dir_idx: u32,
    out_mask: *mut u8,
) -> i32 {
    if p.is_null() || indices.is_null() || out_mask.is_null() || n < 0 {
        return -2;
    }
    let eng = &(*p).inner;
    for i in 0..n as usize {
        let idx = *indices.add(i);
        *out_mask.add(i) = u8::from(eng.index_is_under_dir_root(idx, root_dir_idx));
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_search_pinyin_prefix(
    p: *mut EngineBox,
    prefix_utf8: *const u8,
    prefix_len: i32,
    out_indices: *mut u32,
    out_cap: i32,
) -> i32 {
    let b = match as_box(p) {
        Some(x) => x,
        None => return -1,
    };
    if prefix_utf8.is_null() || prefix_len < 0 || out_indices.is_null() || out_cap <= 0 {
        return -2;
    }
    if b.inner.live_count > 0 && b.inner.py_sort_index_empty() {
        return -3;
    }
    let prefix =
        std::str::from_utf8(std::slice::from_raw_parts(prefix_utf8, prefix_len as usize))
            .unwrap_or("");
    b.inner
        .search_pinyin_prefix(prefix, &mut b.scratch_idx, out_cap as usize);
    let n = b.scratch_idx.len();
    let cap = out_cap as usize;
    let ncpy = n.min(cap);
    std::ptr::copy_nonoverlapping(b.scratch_idx.as_ptr(), out_indices, ncpy);
    ncpy as i32
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_search_full_py_prefix(
    p: *mut EngineBox,
    prefix_utf8: *const u8,
    prefix_len: i32,
    out_indices: *mut u32,
    out_cap: i32,
) -> i32 {
    let b = match as_box(p) {
        Some(x) => x,
        None => return -1,
    };
    if prefix_utf8.is_null() || prefix_len < 0 || out_indices.is_null() || out_cap <= 0 {
        return -2;
    }
    if b.inner.live_count > 0 && b.inner.full_py_sort_index_empty() {
        return -3;
    }
    let prefix =
        std::str::from_utf8(std::slice::from_raw_parts(prefix_utf8, prefix_len as usize))
            .unwrap_or("");
    b.inner
        .search_full_py_prefix(prefix, &mut b.scratch_idx, out_cap as usize);
    let n = b.scratch_idx.len();
    let cap = out_cap as usize;
    let ncpy = n.min(cap);
    std::ptr::copy_nonoverlapping(b.scratch_idx.as_ptr(), out_indices, ncpy);
    ncpy as i32
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_search_name_contains(
    p: *mut EngineBox,
    needle_utf8: *const u8,
    needle_len: i32,
    out_indices: *mut u32,
    out_cap: i32,
) -> i32 {
    let b = match as_box(p) {
        Some(x) => x,
        None => return -1,
    };
    if needle_utf8.is_null() || needle_len < 0 || out_indices.is_null() || out_cap <= 0 {
        return -2;
    }
    let needle =
        std::str::from_utf8(std::slice::from_raw_parts(needle_utf8, needle_len as usize))
            .unwrap_or("");
    b.inner
        .search_name_contains(needle, &mut b.scratch_idx, out_cap as usize);
    let n = b.scratch_idx.len();
    let ncpy = n.min(out_cap as usize);
    std::ptr::copy_nonoverlapping(b.scratch_idx.as_ptr(), out_indices, ncpy);
    ncpy as i32
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_search_full_py_contains(
    p: *mut EngineBox,
    needle_utf8: *const u8,
    needle_len: i32,
    out_indices: *mut u32,
    out_cap: i32,
) -> i32 {
    let b = match as_box(p) {
        Some(x) => x,
        None => return -1,
    };
    if needle_utf8.is_null() || needle_len < 0 || out_indices.is_null() || out_cap <= 0 {
        return -2;
    }
    let needle =
        std::str::from_utf8(std::slice::from_raw_parts(needle_utf8, needle_len as usize))
            .unwrap_or("");
    b.inner
        .search_full_py_contains(needle, &mut b.scratch_idx, out_cap as usize);
    let n = b.scratch_idx.len();
    let ncpy = n.min(out_cap as usize);
    std::ptr::copy_nonoverlapping(b.scratch_idx.as_ptr(), out_indices, ncpy);
    ncpy as i32
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_search_initials_contains(
    p: *mut EngineBox,
    needle_utf8: *const u8,
    needle_len: i32,
    out_indices: *mut u32,
    out_cap: i32,
) -> i32 {
    let b = match as_box(p) {
        Some(x) => x,
        None => return -1,
    };
    if needle_utf8.is_null() || needle_len < 0 || out_indices.is_null() || out_cap <= 0 {
        return -2;
    }
    let needle =
        std::str::from_utf8(std::slice::from_raw_parts(needle_utf8, needle_len as usize))
            .unwrap_or("");
    b.inner
        .search_initials_contains(needle, &mut b.scratch_idx, out_cap as usize);
    let n = b.scratch_idx.len();
    let ncpy = n.min(out_cap as usize);
    std::ptr::copy_nonoverlapping(b.scratch_idx.as_ptr(), out_indices, ncpy);
    ncpy as i32
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_search_full_py_fuzzy(
    p: *mut EngineBox,
    needle_utf8: *const u8,
    needle_len: i32,
    out_indices: *mut u32,
    out_cap: i32,
) -> i32 {
    let b = match as_box(p) {
        Some(x) => x,
        None => return -1,
    };
    if needle_utf8.is_null() || needle_len < 0 || out_indices.is_null() || out_cap <= 0 {
        return -2;
    }
    let needle =
        std::str::from_utf8(std::slice::from_raw_parts(needle_utf8, needle_len as usize))
            .unwrap_or("");
    b.inner
        .search_full_py_fuzzy(needle, &mut b.scratch_idx, out_cap as usize);
    let n = b.scratch_idx.len();
    let ncpy = n.min(out_cap as usize);
    std::ptr::copy_nonoverlapping(b.scratch_idx.as_ptr(), out_indices, ncpy);
    ncpy as i32
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_search_match_query(
    p: *mut EngineBox,
    query_utf8: *const u8,
    query_len: i32,
    out_indices: *mut u32,
    out_cap: i32,
) -> i32 {
    let b = match as_box(p) {
        Some(x) => x,
        None => return -1,
    };
    if query_utf8.is_null() || query_len < 0 || out_indices.is_null() || out_cap <= 0 {
        return -2;
    }
    let query =
        std::str::from_utf8(std::slice::from_raw_parts(query_utf8, query_len as usize))
            .unwrap_or("");
    b.inner
        .search_query_matches(query, &mut b.scratch_idx, out_cap as usize);
    let n = b.scratch_idx.len();
    let ncpy = n.min(out_cap as usize);
    std::ptr::copy_nonoverlapping(b.scratch_idx.as_ptr(), out_indices, ncpy);
    ncpy as i32
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_search_simple_query(
    p: *mut EngineBox,
    query_utf8: *const u8,
    query_len: i32,
    prefer_pinyin: i32,
    out_indices: *mut u32,
    out_cap: i32,
) -> i32 {
    let b = match as_box(p) {
        Some(x) => x,
        None => return -1,
    };
    if query_utf8.is_null() || query_len < 0 || out_indices.is_null() || out_cap <= 0 {
        return -2;
    }
    let query =
        std::str::from_utf8(std::slice::from_raw_parts(query_utf8, query_len as usize))
            .unwrap_or("");
    b.inner
        .search_simple_query(query, prefer_pinyin != 0, &mut b.scratch_idx, out_cap as usize);
    let n = b.scratch_idx.len();
    let ncpy = n.min(out_cap as usize);
    std::ptr::copy_nonoverlapping(b.scratch_idx.as_ptr(), out_indices, ncpy);
    ncpy as i32
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_search_simple_terms(
    p: *mut EngineBox,
    terms_utf8: *const u8,
    terms_len: i32,
    term_count: i32,
    prefer_pinyin: i32,
    out_indices: *mut u32,
    out_cap: i32,
) -> i32 {
    let b = match as_box(p) {
        Some(x) => x,
        None => return -1,
    };
    if terms_utf8.is_null()
        || terms_len < 0
        || term_count <= 0
        || out_indices.is_null()
        || out_cap <= 0
    {
        return -2;
    }

    let raw = std::slice::from_raw_parts(terms_utf8, terms_len as usize);
    let mut terms = Vec::with_capacity(term_count as usize);
    for segment in raw.split(|b| *b == 0).take(term_count as usize) {
        let text = std::str::from_utf8(segment).unwrap_or("");
        if !text.is_empty() {
            terms.push(text);
        }
    }
    if terms.is_empty() {
        return 0;
    }

    b.inner.search_simple_terms(
        &terms,
        prefer_pinyin != 0,
        &mut b.scratch_idx,
        out_cap as usize,
    );
    let n = b.scratch_idx.len();
    let ncpy = n.min(out_cap as usize);
    std::ptr::copy_nonoverlapping(b.scratch_idx.as_ptr(), out_indices, ncpy);
    ncpy as i32
}

#[no_mangle]
pub unsafe extern "C" fn findx_match_name_utf8(
    query_utf8: *const u8,
    query_len: i32,
    candidate_utf8: *const u8,
    candidate_len: i32,
    out_kind: *mut i32,
    out_score: *mut i32,
    out_matched_chars: *mut i32,
) -> i32 {
    if query_utf8.is_null()
        || query_len < 0
        || candidate_utf8.is_null()
        || candidate_len < 0
        || out_kind.is_null()
        || out_score.is_null()
        || out_matched_chars.is_null()
    {
        return -1;
    }

    let query =
        std::str::from_utf8(std::slice::from_raw_parts(query_utf8, query_len as usize))
            .unwrap_or("");
    let candidate =
        std::str::from_utf8(std::slice::from_raw_parts(candidate_utf8, candidate_len as usize))
            .unwrap_or("");
    let result = match_query(query, candidate);

    *out_kind = result.kind as i32;
    *out_score = result.score;
    *out_matched_chars = result.matched_chars;
    if result.is_match() { 1 } else { 0 }
}

#[no_mangle]
pub unsafe extern "C" fn findx_highlight_name_utf8(
    query_utf8: *const u8,
    query_len: i32,
    candidate_utf8: *const u8,
    candidate_len: i32,
    out_ranges: *mut i32,
    out_pair_cap: i32,
) -> i32 {
    if query_utf8.is_null()
        || query_len < 0
        || candidate_utf8.is_null()
        || candidate_len < 0
        || out_ranges.is_null()
        || out_pair_cap < 0
    {
        return -1;
    }

    let query =
        std::str::from_utf8(std::slice::from_raw_parts(query_utf8, query_len as usize))
            .unwrap_or("");
    let candidate =
        std::str::from_utf8(std::slice::from_raw_parts(candidate_utf8, candidate_len as usize))
            .unwrap_or("");
    let ranges = highlight_query_ranges(query, candidate);
    let pair_cap = out_pair_cap as usize;
    let copy_count = ranges.len().min(pair_cap);
    let out = std::slice::from_raw_parts_mut(out_ranges, pair_cap.saturating_mul(2));

    for i in 0..copy_count {
        out[i * 2] = ranges[i].0;
        out[i * 2 + 1] = ranges[i].1;
    }

    copy_count as i32
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_get_name_utf16_len(p: *const EngineBox, idx: i32) -> i32 {
    if p.is_null() {
        return -1;
    }
    let eng = &(*p).inner;
    if idx < 0 || idx as usize >= eng.records.len() {
        return -1;
    }
    let r = &eng.records[idx as usize];
    if r.deleted != 0 {
        return -1;
    }
    let name = pool_utf8(&eng.name_pool, r.name_start, r.name_len as u32);
    name.encode_utf16().count() as i32
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_get_name_utf16(
    p: *const EngineBox,
    idx: i32,
    buf: *mut u16,
    buf_len: i32,
) -> i32 {
    if p.is_null() || buf.is_null() || buf_len <= 0 {
        return -1;
    }
    let slice = std::slice::from_raw_parts_mut(buf, buf_len as usize);
    let n = (*p).inner.get_name_utf16(idx, slice);
    n as i32
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_build_path_utf16_len(p: *const EngineBox, idx: i32) -> i32 {
    if p.is_null() {
        return -1;
    }
    let eng = &(*p).inner;
    if idx < 0 || idx as usize >= eng.records.len() {
        return -1;
    }
    let mut tmp = vec![0u16; 32768];
    let n = eng.build_path_utf16(idx, &mut tmp);
    n as i32
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_build_path_utf16(
    p: *const EngineBox,
    idx: i32,
    buf: *mut u16,
    buf_len: i32,
) -> i32 {
    if p.is_null() || buf.is_null() || buf_len <= 0 {
        return -1;
    }
    let slice = std::slice::from_raw_parts_mut(buf, buf_len as usize);
    let n = (*p).inner.build_path_utf16(idx, slice);
    n as i32
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_get_live_record(
    p: *const EngineBox,
    idx: i32,
    out_fr: *mut u64,
    out_pr: *mut u64,
    out_vol: *mut u16,
    out_attr: *mut u32,
    out_size: *mut i64,
    out_mtime: *mut i64,
    out_ctime: *mut i64,
    out_atime: *mut i64,
) -> i32 {
    if p.is_null()
        || out_fr.is_null()
        || out_pr.is_null()
        || out_vol.is_null()
        || out_attr.is_null()
        || out_size.is_null()
        || out_mtime.is_null()
        || out_ctime.is_null()
        || out_atime.is_null()
    {
        return -1;
    }
    let mut fr = 0u64;
    let mut pr = 0u64;
    let mut vol = 0u16;
    let mut at = 0u32;
    let mut sz = 0i64;
    let mut mt = 0i64;
    let mut ct = 0i64;
    let mut at_time = 0i64;
    let ok = (*p)
        .inner
        .get_live_record(idx, &mut fr, &mut pr, &mut vol, &mut at, &mut sz, &mut mt, &mut ct, &mut at_time);
    if !ok {
        return 0;
    }
    *out_fr = fr;
    *out_pr = pr;
    *out_vol = vol;
    *out_attr = at;
    *out_size = sz;
    *out_mtime = mt;
    *out_ctime = ct;
    *out_atime = at_time;
    1
}

#[no_mangle]
pub unsafe extern "C" fn findx_engine_get_path_depth(p: *const EngineBox, idx: i32) -> i32 {
    if p.is_null() {
        return -1;
    }
    (*p).inner.get_path_depth(idx)
}

pub type VisitLiveFn =
    Option<unsafe extern "system" fn(user: *mut c_void, idx: i32) -> i32>;

#[no_mangle]
pub unsafe extern "C" fn findx_engine_visit_live(
    p: *const EngineBox,
    user: *mut c_void,
    cb: VisitLiveFn,
) {
    if p.is_null() {
        return;
    }
    let Some(cb) = cb else { return };
    (*p).inner.visit_live(|idx| {
        let r = cb(user, idx);
        r != 0
    });
}

/// Save engine state to a binary file. Returns 0 on success, -1 on error.
#[no_mangle]
pub unsafe extern "C" fn findx_engine_save_file(
    p: *const EngineBox,
    path_utf16: *const u16,
    path_len: i32,
) -> i32 {
    if p.is_null() || path_utf16.is_null() || path_len < 0 {
        return -1;
    }
    let path = String::from_utf16_lossy(std::slice::from_raw_parts(path_utf16, path_len as usize));
    match (*p).inner.save_to_file(&path) {
        Ok(_) => 0,
        Err(e) => {
            eprintln!("[findx_engine] save_to_file error: {}", e);
            -1
        }
    }
}

/// Load engine state from a binary file. Returns live_count on success, -1 on error.
#[no_mangle]
pub unsafe extern "C" fn findx_engine_load_file(
    p: *mut EngineBox,
    path_utf16: *const u16,
    path_len: i32,
) -> i32 {
    let b = match as_box(p) {
        Some(x) => x,
        None => return -1,
    };
    if path_utf16.is_null() || path_len < 0 {
        return -1;
    }
    let path = String::from_utf16_lossy(std::slice::from_raw_parts(path_utf16, path_len as usize));
    match b.inner.load_from_file(&path) {
        Ok(live) => live,
        Err(e) => {
            eprintln!("[findx_engine] load_from_file error: {}", e);
            -1
        }
    }
}

pub type PersistRowFn = Option<
    unsafe extern "system" fn(
        user: *mut c_void,
        file_ref: u64,
        parent_ref: u64,
        name_utf16: *const u16,
        name_len: i32,
        attr: u32,
        size: i64,
        mtime: i64,
        ctime: i64,
        atime: i64,
        vol: u16,
    ) -> i32,
>;

#[no_mangle]
pub unsafe extern "C" fn findx_engine_for_each_persist(
    p: *const EngineBox,
    user: *mut c_void,
    cb: PersistRowFn,
) {
    if p.is_null() {
        return;
    }
    let Some(cb) = cb else { return };
    let eng = &(*p).inner;
    for r in eng.records.iter() {
        if r.deleted != 0 {
            continue;
        }
        let nm = pool_utf8(&eng.name_pool, r.name_start, r.name_len as u32);
        let name_utf16: Vec<u16> = nm.encode_utf16().collect();
        let size_i64 = r.size as i64;
        let mtime_i64 = if r.mtime == 0 {
            0
        } else {
            EPOCH_2000_TICKS + (r.mtime as i64) * TICKS_PER_SEC
        };
        let ctime_i64 = if r.ctime == 0 {
            0
        } else {
            EPOCH_2000_TICKS + (r.ctime as i64) * TICKS_PER_SEC
        };
        let atime_i64 = if r.atime == 0 {
            0
        } else {
            EPOCH_2000_TICKS + (r.atime as i64) * TICKS_PER_SEC
        };
        let _ = cb(
            user,
            r.file_ref,
            r.parent_ref,
            name_utf16.as_ptr(),
            name_utf16.len() as i32,
            r.attr,
            size_i64,
            mtime_i64,
            ctime_i64,
            atime_i64,
            r.vol as u16,
        );
    }
}
