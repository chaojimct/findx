//! snippet 端到端验证：真索引上查 + 取 snippet（v2 按需回读路径）。
use findx2_content::{doc_key, open_or_create, ContentIndex};

fn main() {
    let dir = std::path::Path::new("C:/Users/chaoj/AppData/Local/FindX-native-dev/index.bin.content");
    let cidx: ContentIndex = open_or_create(dir).expect("open");
    println!("docs={}", cidx.num_docs());
    for q in ["全文搜索", "进出口银行", "2026"] {
        let hits = cidx.search_ids(q, 5).unwrap_or_default();
        println!("query={q} hits={}", hits.len());
        let keys: Vec<u64> = hits.iter().map(|(v, f, _)| doc_key(v, *f)).collect();
        match cidx.snippets_for_keys(q, &keys, 5) {
            Ok(m) => {
                for (k, h) in m.iter().take(2) {
                    let plain: String = html_to_text(h);
                    println!("  key={k} snippet={}", plain.chars().take(120).collect::<String>());
                }
                if m.is_empty() {
                    println!("  (no snippets)");
                }
            }
            Err(e) => println!("  snippet err: {e}"),
        }
    }
}

fn html_to_text(h: &str) -> String {
    h.replace("<b>", "[").replace("</b>", "]").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&").replace("&quot;", "\"")
}
