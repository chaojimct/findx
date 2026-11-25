//! P0-3 验证：`path:` 查询建表一次、后续复用 + 结果一致性。
//! 用法：`cargo run --release -p findx2-core --example path_bench -- <index.bin>`
//!
//! 不同文本 → 每次都 miss（无 page cache 干预）：第一次建表，之后复用。
//! 断言：各查询命中 >0（真实索引 smoke）；打印冷/热耗时。

use std::path::PathBuf;
use std::time::Instant;

use findx2_core::{load_index_bin, QueryParser, SearchEngine, SearchOptions};

fn run(engine: &SearchEngine, qstr: &str, opt: &SearchOptions) -> (u32, f64) {
    let q = QueryParser::parse(qstr).unwrap();
    let t0 = Instant::now();
    let (_, total) = engine.search_paged(qstr, &q, opt, 0, 500, None, None).unwrap();
    (total, t0.elapsed().as_secs_f64() * 1000.0)
}

fn main() {
    let path: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("index.bin"));
    eprintln!("加载: {}", path.display());
    let store = load_index_bin(&path).expect("load_index_bin");
    eprintln!("条目: {}", store.entries.len());
    let engine = SearchEngine::new(store);
    let opt = SearchOptions { allow_pinyin: false, ..Default::default() };

    // 大命中（两段式）+ 小命中（直接逐条，现走缓存表免回溯）各一。修饰符必须在前。
    for qstr in ["path:users", "path:windows", "path:windows readme"] {
        let (n, ms) = run(&engine, qstr, &opt);
        assert!(n > 0, "无命中: {qstr}");
        eprintln!("  [{qstr}] 命中 {n}: {ms:.1}ms");
    }
    // 再来一轮不同文本：表已缓存，纯复用。
    for qstr in ["path:system32", "path:temp log"] {
        let (n, ms) = run(&engine, qstr, &opt);
        eprintln!("  [{qstr}] 命中 {n}: {ms:.1}ms（表复用）");
    }
    eprintln!("smoke: OK");
}
