//! Current-level name filtering only. Same ib-matcher pinyin table/config as search;
//! no index IPC, transliteration cache, or main-thread per-key directory walk.
use findx2_ipc::SearchHitDto;
use std::sync::atomic::{AtomicBool, Ordering};

pub fn filter_rows(
    rows: &[SearchHitDto],
    query: &str,
    cancel: &AtomicBool,
) -> Option<Vec<SearchHitDto>> {
    let terms: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
    #[cfg(feature = "pinyin")]
    let config = ib_matcher::matcher::MatchConfig::builder()
        .pinyin(ib_matcher::matcher::PinyinMatchConfig::notations(
            ib_matcher::pinyin::PinyinNotation::Ascii
                | ib_matcher::pinyin::PinyinNotation::AsciiFirstLetter,
        ))
        .build();
    // Literal matcher: punctuation stays literal, unlike a regex query. Terms
    // use the product's case-insensitive full-pinyin + initials AND convention.
    #[cfg(feature = "pinyin")]
    let matchers: Vec<_> = terms
        .iter()
        .map(|t| config.matcher::<str>(t.as_str()))
        .collect();
    #[cfg(not(feature = "pinyin"))]
    let lower: Vec<_> = terms.iter().map(|t| t.to_lowercase()).collect();
    let mut out = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        if i % 32 == 0 && cancel.load(Ordering::Acquire) {
            return None;
        }
        let mut byte_ranges = Vec::new();
        let mut matched = true;
        #[cfg(feature = "pinyin")]
        for matcher in &matchers {
            // find() returns ORIGINAL UTF-8 byte positions, including when the
            // input is full pinyin, initials, mixed text or a polyphonic alias.
            // Advance one scalar after the match START to retain repeated and
            // overlapping matches, without ever slicing inside a UTF-8 scalar.
            let mut offset = 0;
            let mut found_term = false;
            while offset < row.name.len() {
                if cancel.load(Ordering::Acquire) {
                    return None;
                }
                let Some(m) = matcher.find(&row.name[offset..]) else {
                    break;
                };
                let start = offset + m.start();
                let end = offset + m.end();
                if start >= end
                    || !row.name.is_char_boundary(start)
                    || !row.name.is_char_boundary(end)
                {
                    break;
                }
                found_term = true;
                byte_ranges.push(start..end);
                offset = start + row.name[start..].chars().next().unwrap().len_utf8();
            }
            if !found_term {
                matched = false;
                break;
            }
        }
        #[cfg(not(feature = "pinyin"))]
        for term in &lower {
            let ranges = literal_ranges(&row.name, term);
            if ranges.is_empty() {
                matched = false;
                break;
            }
            byte_ranges.extend(ranges);
        }
        if matched {
            let mut hit = row.clone();
            // Rows and their highlights travel together through the existing
            // cancel/generation/scope/source gate. Empty queries overwrite any
            // old marks rather than allowing highlights to leak across refresh.
            hit.name_highlight = scalar_ranges(&row.name, byte_ranges);
            out.push(hit);
        }
    }
    if cancel.load(Ordering::Acquire) {
        None
    } else {
        Some(out)
    }
}

/// Single publication gate for both membership and marks; late work cannot
/// paint a newer query, directory, refresh snapshot, or another workspace.
pub(crate) fn can_publish(
    cancelled: bool,
    issued: u64,
    current: u64,
    directory: bool,
    same_scope: bool,
    same_source: bool,
) -> bool {
    !cancelled && issued == current && directory && same_scope && same_source
}

/// Match positions are bytes; IPC SearchHitDto uses Unicode scalar offsets.
/// Merge terms/overlaps so the renderer gets ordered, disjoint ranges.
fn scalar_ranges(name: &str, ranges: Vec<std::ops::Range<usize>>) -> Vec<[u32; 2]> {
    if ranges.is_empty() {
        return Vec::new();
    }
    let boundaries: Vec<_> = name
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(name.len()))
        .collect();
    let mut ranges: Vec<_> = ranges
        .into_iter()
        .filter_map(|r| {
            let start = boundaries.binary_search(&r.start).ok()? as u32;
            let end = boundaries.binary_search(&r.end).ok()? as u32;
            (start < end).then_some([start, end])
        })
        .collect();
    ranges.sort_unstable();
    let mut merged: Vec<[u32; 2]> = Vec::new();
    for range in ranges {
        if let Some(last) = merged.last_mut().filter(|last| range[0] <= last[1]) {
            last[1] = last[1].max(range[1]);
        } else {
            merged.push(range);
        }
    }
    merged
}

#[cfg(not(feature = "pinyin"))]
fn literal_ranges(name: &str, term: &str) -> Vec<std::ops::Range<usize>> {
    // Unicode lowercasing can change byte length. Map the
    // folded bytes back to source scalars, not offsets into the folded string.
    let mut folded = String::new();
    let mut source = Vec::new();
    for (start, c) in name.char_indices() {
        let fold_start = folded.len();
        folded.extend(c.to_lowercase());
        source.push((fold_start, folded.len(), start, start + c.len_utf8()));
    }
    let mut ranges = Vec::new();
    let mut offset = 0;
    while offset < folded.len() {
        let Some(relative) = folded[offset..].find(term) else {
            break;
        };
        let start = offset + relative;
        let end = start + term.len();
        let from = source.iter().find(|s| s.1 > start).unwrap().2;
        let to = source.iter().rev().find(|s| s.0 < end).unwrap().3;
        ranges.push(from..to);
        offset = start + folded[start..].chars().next().unwrap().len_utf8();
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rows() -> Vec<SearchHitDto> {
        [
            "北京旅游攻略.docx",
            "测试报告.txt",
            "北京资料",
            "README.MD",
            "a[b]+(1).txt",
            "北京 Report2026.txt",
            "重庆报告.pdf",
        ]
        .into_iter()
        .enumerate()
        .map(|(i, name)| SearchHitDto {
            entry_idx: i as u32,
            name: name.into(),
            path: format!("fixture/{name}"),
            size: 1,
            mtime: 1,
            is_directory: i == 2,
            name_highlight: vec![],
            snippet: String::new(),
        })
        .collect()
    }
    fn names(q: &str) -> Vec<String> {
        filter_rows(&rows(), q, &AtomicBool::new(false))
            .unwrap()
            .into_iter()
            .map(|r| r.name)
            .collect()
    }
    #[test]
    fn literal_chinese_english_extension_and_specials() {
        assert_eq!(names("北京").len(), 3);
        assert_eq!(names("readme.md"), vec!["README.MD"]);
        assert_eq!(names("a[b]+"), vec!["a[b]+(1).txt"]);
        assert!(names(".*").is_empty());
        assert_eq!(names("").len(), 7);
    }
    #[cfg(feature = "pinyin")]
    #[test]
    fn full_initials_mixed_and_terms_follow_existing_matcher() {
        assert_eq!(names("beijing").len(), 3);
        assert_eq!(names("bj").len(), 3);
        assert_eq!(names("ce shi"), vec!["测试报告.txt"]);
        assert_eq!(names("bj report2026.txt"), vec!["北京 Report2026.txt"]);
        assert_eq!(names("BEIJING .DOCX"), vec!["北京旅游攻略.docx"]);
        assert_eq!(names("北京lvyou"), vec!["北京旅游攻略.docx"]);
    }
    #[test]
    fn cancellation_never_publishes_partial_rows() {
        assert!(filter_rows(&rows(), "", &AtomicBool::new(true)).is_none());
    }
    #[test]
    fn only_supplied_level_and_stable_sort_identity() {
        let r = rows();
        let scoped = &r[2..5];
        let out = filter_rows(scoped, "", &AtomicBool::new(false)).unwrap();
        assert_eq!(
            out.iter().map(|r| r.entry_idx).collect::<Vec<_>>(),
            vec![2, 3, 4]
        );
    }
    #[cfg(feature = "pinyin")]
    #[test]
    fn renamed_source_is_rematched_without_stale_pinyin_cache() {
        let mut r = rows();
        assert_eq!(
            filter_rows(&r, "beijing", &AtomicBool::new(false))
                .unwrap()
                .len(),
            3
        );
        r[0].name = "上海.docx".into();
        assert_eq!(
            filter_rows(&r, "beijing", &AtomicBool::new(false))
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            filter_rows(&r, "shanghai", &AtomicBool::new(false))
                .unwrap()
                .len(),
            1
        );
    }
    #[cfg(feature = "pinyin")]
    #[test]
    fn large_owned_fixture_and_polyphonic_observations() {
        let source = rows();
        let rows: Vec<_> = (0..20000)
            .map(|i| source[i % source.len()].clone())
            .collect();
        let t = std::time::Instant::now();
        let a = filter_rows(&rows, "beijing", &AtomicBool::new(false)).unwrap();
        let first = t.elapsed();
        let t = std::time::Instant::now();
        let b = filter_rows(&rows, "bj", &AtomicBool::new(false)).unwrap();
        assert_eq!(a.len(), b.len());
        println!(
            "local_filter rows={} full_ms={} initials_ms={} matches={} cq={:?} zq={:?}",
            rows.len(),
            first.as_millis(),
            t.elapsed().as_millis(),
            a.len(),
            names("chongqing"),
            names("zhongqing")
        );
    }
    fn one(name: &str) -> SearchHitDto {
        SearchHitDto {
            name: name.into(),
            path: format!("fixture/{name}"),
            name_highlight: vec![[99, 100]],
            ..rows().remove(0)
        }
    }
    fn marks(name: &str, q: &str) -> Vec<[u32; 2]> {
        let hits = filter_rows(&[one(name)], q, &AtomicBool::new(false)).unwrap();
        assert_eq!(hits.len(), 1, "{name} / {q}");
        hits[0].name_highlight.clone()
    }
    #[test]
    #[cfg(feature = "pinyin")]
    fn lvsqs_full_pinyin_and_initials_mark_only_matched_chinese() {
        let name = "绿水青山绿证核算绿皮书.pdf";
        for q in ["lvsqs", "lvshuiqingshan", "lsqs", "LVSHUIQINGSHAN"] {
            assert_eq!(marks(name, q), vec![[0, 4]], "{q}");
        }
    }
    #[test]
    fn literal_chinese_ascii_case_extension_and_punctuation_positions() {
        for (name, q, expected) in [
            ("绿水青山绿证核算绿皮书.pdf", "绿水青山", vec![[0, 4]]),
            ("README.MD", "readme", vec![[0, 6]]),
            ("北京报告.DOCX", ".docx", vec![[4, 9]]),
            ("前缀a[b]+(1).txt", "a[b]+", vec![[2, 7]]),
            ("🟢—绿水青山.txt", "绿水", vec![[2, 4]]),
            ("哈哈哈.txt", "哈哈", vec![[0, 3]]),
            ("Alpha AAAAA.txt", "aaa", vec![[6, 11]]),
        ] {
            assert_eq!(marks(name, q), expected, "{name} / {q}");
        }
    }
    #[test]
    #[cfg(feature = "pinyin")]
    fn repeated_mixed_polyphonic_and_and_tokens_have_real_positions() {
        for (name, q, expected) in [
            ("🟢—绿水青山与绿水青山.txt", "lvsqs", vec![[2, 6], [7, 11]]),
            ("北京Report绿水青山2026.PDF", "bjREPORTlvsqs", vec![[0, 12]]),
            (
                "北京旅游攻略.docx",
                "  BEIJING   .DOCX  ",
                vec![[0, 2], [6, 11]],
            ),
            ("绿水青山.txt", "lvsqs 绿水", vec![[0, 4]]),
            ("重庆报告.pdf", "chongqing", vec![[0, 2]]),
            ("重庆报告.pdf", "zhongqing", vec![[0, 2]]),
            ("重庆报告.pdf", "cq", vec![[0, 2]]),
            ("重庆报告.pdf", "zq", vec![[0, 2]]),
        ] {
            assert_eq!(marks(name, q), expected, "{name} / {q}");
        }
    }
    #[test]
    fn clearing_refresh_and_sort_keep_marks_with_rows_not_old_positions() {
        let source = vec![one("第二个测试.txt"), one("测试报告.pdf"), one("README.MD")];
        let cancel = AtomicBool::new(false);
        let filtered = filter_rows(&source, "测试", &cancel).unwrap();
        assert_eq!(filtered[0].name_highlight, vec![[3, 5]]);
        assert_eq!(filtered[1].name_highlight, vec![[0, 2]]);
        assert!(source.iter().all(|r| r.name_highlight == vec![[99, 100]]));
        let mut sorted = source.clone();
        sorted.sort_by(|a, b| a.name.cmp(&b.name));
        let result = filter_rows(&sorted, "测试", &cancel).unwrap();
        for row in &result {
            assert_eq!(
                crate::search_logic::highlight_segments(&row.name, &row.name_highlight)
                    .into_iter()
                    .filter(|(_, on)| *on)
                    .map(|(s, _)| s)
                    .collect::<String>(),
                "测试"
            );
        }
        let cleared = filter_rows(&result, "  ", &cancel).unwrap();
        assert!(cleared.iter().all(|r| r.name_highlight.is_empty()));
        assert!(
            filter_rows(&source, "not-present", &cancel)
                .unwrap()
                .is_empty()
        );
        cancel.store(true, Ordering::Release);
        assert!(filter_rows(&source, "测试", &cancel).is_none());
        assert!(filter_rows(&source, "", &cancel).is_none());
        cancel.store(false, Ordering::Release);
        assert_eq!(
            filter_rows(&[one("新测试条目.txt")], "测试", &cancel).unwrap()[0].name_highlight,
            vec![[1, 3]]
        );
    }
    #[test]
    fn scalar_mapping_rejects_invalid_bytes_and_merges_terms() {
        assert_eq!(
            scalar_ranges("🟢—绿水青山.txt", vec![7..13, 10..19, 1..4]),
            vec![[2, 6]]
        );
        assert_eq!(
            crate::search_logic::highlight_byte_ranges("🟢—绿水青山.txt", &[[2, 6]]),
            vec![7..19]
        );
    }
    #[test]
    fn late_cancelled_or_different_scope_results_cannot_publish_marks() {
        assert!(can_publish(false, 2, 2, true, true, true));
        assert!(!can_publish(true, 2, 2, true, true, true));
        assert!(!can_publish(false, 1, 2, true, true, true));
        assert!(!can_publish(false, 2, 2, false, true, true));
        assert!(!can_publish(false, 2, 2, true, false, true));
        assert!(!can_publish(false, 2, 2, true, true, false));
    }
}
