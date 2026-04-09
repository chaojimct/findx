//! C ABI，供 FindX.Core P/Invoke。调用方需保证单线程或外加锁（与现版 C# FileIndex 一致）。

use crate::engine::{pool_utf8, Engine};
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
        let mtime_i64 = EPOCH_2000_TICKS + (r.mtime as i64) * TICKS_PER_SEC;
        let ctime_i64 = EPOCH_2000_TICKS + (r.ctime as i64) * TICKS_PER_SEC;
        let atime_i64 = EPOCH_2000_TICKS + (r.atime as i64) * TICKS_PER_SEC;
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
