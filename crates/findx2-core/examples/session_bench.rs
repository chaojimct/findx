//! P0-2 会话增量验收：真实 index.bin 上模拟打字续写，对比有/无会话总耗时。
//! 用法：`cargo run --release -p findx2-core --example session_bench -- <index.bin>`
//!
//! 验收（T121 P0-2）：连续输入总耗时 < 1.5×首查；各步命中数与无会话全量一致。
//! 注：用 `allow_pinyin=false`（纯字面路径，会话 sound 范围；见 search_paged 文档）。

use std::path::PathBuf;
use std::time::Instant;

use findx2_core::{load_index_bin, QueryParser, SearchEngine, SearchOptions};

fn chain(engine: &SearchEngine, texts: &[&str], session: Option<u64>, opt: &SearchOptions) -> (Vec<(String, u32, f64)>, f64) {
    let mut rows = Vec::new();
    let mut total_ms = 0.0;
    for t in texts {
        let q = QueryParser::parse(t).unwrap();
        let t0 = Instant::now();
        let (_, n) = engine
            .search_paged(t, &q, opt, 0, 500, None, session)
            .unwrap();
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        total_ms += ms;
        rows.push((t.to_string(), n, ms));
    }
    (rows, total_ms)
}

fn main() {
    let path: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("index.bin"));
    eprintln!("加载: {}", path.display());
    let store_a = load_index_bin(&path).expect("load_index_bin");
    eprintln!("条目: {}", store_a.entries.len());
    let store_b = load_index_bin(&path).expect("load_index_bin");
    let eng_none = SearchEngine::new(store_a);
    let eng_sess = SearchEngine::new(store_b);
    let opt = SearchOptions { allow_pinyin: false, ..Default::default() };

    // 打字链：从 trigram 帮不上的短 needle 起步（<3 字节无倒排），每步都是上一步的前缀扩展。
    // 这正是会话的价值区间：首查全表慢，后续只 filter 收缩集。
    let texts = ["wi", "win", "wind", "windo"];
    let (rows_none, tot_none) = chain(&eng_none, &texts, None, &opt);
    let (rows_sess, tot_sess) = chain(&eng_sess, &texts, Some(1), &opt);

    eprintln!("── 无会话（全量×4）──");
    for (t, n, ms) in &rows_none {
        eprintln!("  {t:<10} hits={n:<8} {ms:.1} ms");
    }
    eprintln!("  合计: {tot_none:.1} ms");
    eprintln!("── 有会话（1 全量 + 3 增量）──");
    for (t, n, ms) in &rows_sess {
        eprintln!("  {t:<10} hits={n:<8} {ms:.1} ms");
    }
    eprintln!("  合计: {tot_sess:.1} ms");
    // 一致性：每步命中数必须相同。
    for ((_, n0, _), (_, n1, _)) in rows_none.iter().zip(rows_sess.iter()) {
        assert_eq!(n0, n1, "会话与全量命中不一致");
    }
    eprintln!("一致性: OK（各步命中数相同）");
    let first = rows_sess[0].2;
    eprintln!(
        "验收: 会话总耗时 {:.1} ms vs 首查 {:.1} ms，倍数 {:.2}（要求 <1.5）→ {}",
        tot_sess,
        first,
        tot_sess / first.max(0.001),
        if tot_sess < 1.5 * first { "PASS" } else { "FAIL" }
    );
}
