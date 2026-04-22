//! 真实 index.bin 测速：`cargo run --release -p findx2-core --example real_index_bench -- path\to\index.bin`

use std::path::PathBuf;
use std::time::Instant;

use findx2_core::{
    load_index_bin, ParsedQuery, QueryParser, SearchEngine, SearchOptions, SortField,
};

fn main() {
    let path: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("index.bin"));

    eprintln!("加载: {}", path.display());
    let t0 = Instant::now();
    let store = load_index_bin(&path).expect("load_index_bin");
    let load_ms = t0.elapsed().as_secs_f64() * 1000.0;
    eprintln!(
        "  load_index_bin: {load_ms:.1} ms | 条目 {}",
        store.entry_count()
    );

    let engine = SearchEngine::new(store);
    let opt = SearchOptions::default();

    let cases: &[(&str, &str)] = &[
        ("ext:txt", "ext:txt"),
        ("子串（示例）", "readme"),
        ("ext:txt + 子串", "ext:txt readme"),
    ];

    for (label, qstr) in cases {
        let pq: ParsedQuery = QueryParser::parse(qstr).unwrap();
        let t1 = Instant::now();
        let n = engine.search(&pq, &opt).map(|(v, _t)| v.len()).unwrap_or(0);
        let ms = t1.elapsed().as_secs_f64() * 1000.0;
        eprintln!("  [{label}] {qstr:?} -> {n} 条, search {ms:.1} ms");
    }

    let mut pq_sort = QueryParser::parse("ext:txt").unwrap();
    pq_sort.sort_by = SortField::Size;
    pq_sort.sort_desc = true;
    pq_sort.limit = 100;
    let t2 = Instant::now();
    let n = engine
        .search(&pq_sort, &opt)
        .map(|(v, _t)| v.len())
        .unwrap_or(0);
    let ms2 = t2.elapsed().as_secs_f64() * 1000.0;
    eprintln!("  [ext:txt sort:size desc 100] -> {n} 条, search {ms2:.1} ms");
}
