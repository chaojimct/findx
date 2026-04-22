//! 使用仓库根目录 `files_for_test` 下的真实文件名，覆盖拼音全拼、首字母、中英混合、多词 AND、OR、`!` 排除、`ext:` 等。
//! 运行：`cargo test -p findx2-core --features pinyin --test pinyin_files_for_test`
//! 合成大集上的拼音耗时：`cargo bench -p findx2-core --features pinyin --bench pinyin_perf`

use std::path::Path;

use findx2_core::index::IndexBuilder;
use findx2_core::platform::RawEntry;
use findx2_core::{QueryParser, SearchEngine, SearchOptions};

fn engine_from_fixture() -> SearchEngine {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../files_for_test");
    assert!(
        dir.is_dir(),
        "缺少目录 {}（与 README 中 files_for_test 约定一致）",
        dir.display()
    );

    const ROOT_FRN: u64 = 100;
    let dirs = vec![RawEntry {
        file_id: ROOT_FRN,
        file_id_128: None,
        parent_id: 0,
        name: "fixture_root".into(),
        size: 0,
        mtime: 0,
        ctime: 0,
        attrs: 0x10,
        is_dir: true,
    }];

    let mut files: Vec<RawEntry> = Vec::new();
    let mut id: u64 = 10_000;
    for e in std::fs::read_dir(&dir).expect("read_dir") {
        let e = e.expect("dirent");
        let meta = e.metadata().expect("metadata");
        if !meta.is_file() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        files.push(RawEntry {
            file_id: id,
            file_id_128: None,
            parent_id: ROOT_FRN,
            name,
            size: 1,
            mtime: 1,
            ctime: 1,
            attrs: 0,
            is_dir: false,
        });
        id += 1;
    }
    assert!(
        !files.is_empty(),
        "{} 下应有若干测试文件",
        dir.display()
    );

    let store = IndexBuilder::new(b'C', 1, 1, 1)
        .build_from_raw(files, dirs, true)
        .expect("build_from_raw");
    SearchEngine::new(store)
}

fn names(engine: &SearchEngine, q: &str, allow_pinyin: bool) -> Vec<String> {
    let pq = QueryParser::parse(q).expect("parse");
    let (hits, _total) = engine
        .search(
            &pq,
            &SearchOptions {
                allow_pinyin,
                ..Default::default()
            },
        )
        .expect("search");
    hits.into_iter().map(|h| h.name).collect()
}

fn assert_hit_contains(names: &[String], needle: &str) {
    assert!(
        names.iter().any(|n| n.contains(needle)),
        "期望命中文件名包含 {needle:?}，实际: {names:?}"
    );
}

#[test]
fn pinyin_full_spelling() {
    let engine = engine_from_fixture();
    let n = names(&engine, "beijing", true);
    assert_hit_contains(&n, "北京");
    let n = names(&engine, "chaoyang", true);
    assert_hit_contains(&n, "朝阳");
    let n = names(&engine, "shanghai", true);
    assert_hit_contains(&n, "上海");
    let n = names(&engine, "lujiazui", true);
    assert_hit_contains(&n, "陆家嘴");
}

#[test]
fn pinyin_initials() {
    let engine = engine_from_fixture();
    // ib-matcher 默认含 AsciiFirstLetter；「sh」应对「上海」等
    let n = names(&engine, "sh", true);
    assert_hit_contains(&n, "上海");
    // 「bj」→ 北京 常见首字母简拼
    let n = names(&engine, "bj", true);
    assert_hit_contains(&n, "北京");
}

#[test]
fn pinyin_and_combine_two_terms() {
    let engine = engine_from_fixture();
    let n = names(&engine, "shanghai lujiazui", true);
    assert_hit_contains(&n, "陆家嘴");
    assert_eq!(n.len(), 1, "应唯一命中 上海陆家嘴.md");
}

#[test]
fn pinyin_quanpin_and_file_word() {
    let engine = engine_from_fixture();
    let n = names(&engine, "quanpin", true);
    assert_hit_contains(&n, "全拼");
    let n = names(&engine, "pinyin", true);
    assert_hit_contains(&n, "拼音");
}

#[test]
fn ascii_substring_and_chinese_literal() {
    let engine = engine_from_fixture();
    let n = names(&engine, "english", true);
    assert_hit_contains(&n, "English");
    let n = names(&engine, "zi", true);
    assert_hit_contains(&n, "zi串");
    let n = names(&engine, "中文", true);
    assert_hit_contains(&n, "中文");
}

#[test]
fn ext_modifier_with_pinyin() {
    let engine = engine_from_fixture();
    let n = names(&engine, "ext:md shanghai", true);
    assert_hit_contains(&n, "陆家嘴");
    assert!(
        n.iter().all(|s| s.ends_with(".md")),
        "ext:md 应只保留 .md：{n:?}"
    );
}

#[test]
fn pinyin_only_and_no_pinyin_modifiers() {
    let engine = engine_from_fixture();
    let n = names(&engine, "beijing;py", true);
    assert_hit_contains(&n, "北京");
    let n = names(&engine, "english;en", true);
    assert_hit_contains(&n, "English");
}

#[test]
fn pinyin_disabled_no_match_on_chinese_name() {
    let engine = engine_from_fixture();
    let n = names(&engine, "beijing", false);
    assert!(
        n.is_empty(),
        "关闭拼音时不应仅靠 ascii 拼音命中中文名：{n:?}"
    );
}

/// 多裸词 AND：拼音 + 拼音（文件名「拼音全拼测试」）
#[test]
fn pinyin_two_terms_both_ascii_pinyin() {
    let engine = engine_from_fixture();
    let n = names(&engine, "pinyin quanpin", true);
    assert_hit_contains(&n, "拼音全拼");
}

/// 顶层 OR：任一分支命中即可
#[test]
fn pinyin_or_branch_ascii_pinyin() {
    let engine = engine_from_fixture();
    let n = names(&engine, "beijing | english", true);
    assert!(n.len() >= 2, "OR 应同时覆盖中文与英文文件名：{n:?}");
    assert_hit_contains(&n, "北京");
    assert_hit_contains(&n, "English");
}

/// `ext:` 缩小候选后，再用 `!` 排除英文名
#[test]
fn ext_filter_then_not_substring() {
    let engine = engine_from_fixture();
    let n = names(&engine, "ext:txt !english", true);
    assert!(
        !n.iter().any(|s| s.contains("English")),
        "应排除 English_only.txt：{n:?}"
    );
    assert_hit_contains(&n, "北京");
}

/// 字面「北京」：非 ASCII 关键词不走拼音分支，仅 memmem（UTF-8）
#[test]
fn chinese_literal_name_term_no_ascii_pinyin_path() {
    let engine = engine_from_fixture();
    let n = names(&engine, "北京", true);
    assert_hit_contains(&n, "北京");
    assert_eq!(n.len(), 1);
}

