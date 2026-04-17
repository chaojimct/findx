//! 全拼 trigram 打包与有序 posting 交集（倒排查询用）。

use std::cmp::Ordering;

#[inline]
pub fn pack_trigram(a: u8, b: u8, c: u8) -> u32 {
    u32::from(a.to_ascii_lowercase())
        | (u32::from(b.to_ascii_lowercase()) << 8)
        | (u32::from(c.to_ascii_lowercase()) << 16)
}

/// 对有序 posting 列表求交集（升序）。
pub fn intersect_sorted_postings(lists: &[&[u32]]) -> Vec<u32> {
    if lists.is_empty() {
        return Vec::new();
    }
    if lists.iter().any(|l| l.is_empty()) {
        return Vec::new();
    }
    let mut lists_sorted: Vec<&[u32]> = lists.to_vec();
    lists_sorted.sort_by_key(|l| l.len());

    let mut out: Vec<u32> = lists_sorted[0].to_vec();
    for lst in lists_sorted.iter().skip(1) {
        out = intersect_two_sorted(&out, lst);
        if out.is_empty() {
            break;
        }
    }
    out
}

fn intersect_two_sorted(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut i = 0usize;
    let mut j = 0usize;
    let mut r = Vec::new();
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            Ordering::Less => i += 1,
            Ordering::Greater => j += 1,
            Ordering::Equal => {
                r.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    r
}
