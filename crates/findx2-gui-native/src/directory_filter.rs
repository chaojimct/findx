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
        #[cfg(feature = "pinyin")]
        let matched = matchers.iter().all(|m| m.is_match(row.name.as_str()));
        #[cfg(not(feature = "pinyin"))]
        let matched = {
            let name = row.name.to_lowercase();
            lower.iter().all(|t| name.contains(t))
        };
        if matched {
            out.push(row.clone())
        }
    }
    if cancel.load(Ordering::Acquire) {
        None
    } else {
        Some(out)
    }
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
}
