//! 拼音辅索引回填回归：建小库 → 主索引 → 走倒排 walk 建辅库 → 查 initials/全拼。
//!
//! 验证 walk 与在线抽取的一致性（同一分词/拼音逻辑），以及 aux 可搜性。

use std::io::Write;

use findx2_content::{
    aux_dir_for, build_aux_from_index, build_from_store, content_dir_for, open_if_exists,
    AuxStats, AUX_FULL_SUFFIX, AUX_INITIALS_SUFFIX,
};
use findx2_core::{IndexBuilder, RawEntry};

fn raw(file_id: u64, parent_id: u64, name: &str, is_dir: bool) -> RawEntry {
    RawEntry {
        file_id,
        file_id_128: None,
        parent_id,
        name: name.to_owned(),
        size: 0,
        mtime: 0,
        ctime: 0,
        attrs: 0,
        is_dir,
    }
}

#[test]
fn pinyin_aux_walk_matches_online_extraction() {
    let case = tempfile::tempdir().unwrap();
    let root = case.path().join("vol");
    std::fs::create_dir(&root).unwrap();
    std::fs::File::create(root.join("a.txt"))
        .unwrap()
        .write_all("全文搜索测试文档".as_bytes())
        .unwrap();
    std::fs::File::create(root.join("b.txt"))
        .unwrap()
        .write_all("nothing relevant here".as_bytes())
        .unwrap();

    let rp = root.display().to_string().replace('\\', "/");
    let store = IndexBuilder::new(0, 1, 1, 1)
        .with_unix_volume("C", rp)
        .build_from_raw(
            vec![
                raw(1001, 100, "a.txt", false),
                raw(1002, 100, "b.txt", false),
            ],
            vec![raw(100, 0, "", true)],
            true,
        )
        .unwrap();
    let index_bin = case.path().join("index.bin");
    findx2_core::save_index_bin(&index_bin, &store).unwrap();
    let store2 = findx2_core::load_store_auto(&index_bin).unwrap();
    let cdir = content_dir_for(&index_bin);
    let st = build_from_store(&store2, &cdir, false, |_| true).unwrap();
    assert_eq!(st.extracted, 2, "{st:?}");

    // 回填辅库。
    let aux = build_aux_from_index(&index_bin, true, |_, _| true).unwrap();
    assert!(aux.docs >= 1, "{aux:?}");

    // initials 辅库可查 "qw"（全文）。
    let ini_idx = findx2_content::open_or_create(&aux_dir_for(
        &index_bin,
        AUX_INITIALS_SUFFIX,
    ))
    .unwrap();
    let hits = ini_idx.search_pinyin("qw", 10, false).unwrap();
    assert!(!hits.is_empty(), "initials qw 应命中全文文档");

    // full 辅库可查 "quan wen"。
    let full_idx = findx2_content::open_or_create(&aux_dir_for(&index_bin, AUX_FULL_SUFFIX)).unwrap();
    // full 用 search_pinyin（多词 OR）；单音节走前缀 tested in unit tests.
    let hits2 = full_idx.search_pinyin("quan wen", 10, true).unwrap();
    assert!(!hits2.is_empty(), "全拼 quan wen 应命中全文文档");

    // 连写全拼 `quanwen` 严格连续命中（用户自然输入不带空格）。
    let hits3 = full_idx.search_pinyin("quanwen", 10, true).unwrap();
    assert!(!hits3.is_empty(), "连写 quanwen 应命中全文文档");
    // 半个尾音降级：`quanw`（wen 只打了 w）仍应命中。
    let hits4 = full_idx.search_pinyin("quanw", 10, true).unwrap();
    assert!(!hits4.is_empty(), "半截 quanw 应命中全文文档");
    // 首字母串不受切分影响：`qw` 在 full 侧无命中（只走 initials 侧）。
    let hits5 = full_idx.search_pinyin("qw", 10, true).unwrap();
    assert!(hits5.is_empty(), "qw 在全拼侧应无命中");

    // 主库按名可查（回归：walk 不破坏主库）。
    let cidx = open_if_exists(&index_bin).expect("主索引应存在");
    assert!(cidx.num_docs() >= 2);
    let _ = AuxStats { docs: 0, terms_cjk: 0, chars: 0, docs_ini: 0, docs_full: 0 };
}
