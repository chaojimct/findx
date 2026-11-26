//! 复现：WPS 云缓存 docx 的 hits 与 block 0 文本/映射
fn main() {
    let path = std::env::args().nth(1).expect("usage: probe_wps_hits <docx>");
    let needles: Vec<String> = std::env::args().skip(2).collect();
    let needles = if needles.is_empty() {
        vec!["马春天".to_string(), "我与青松".to_string()]
    } else {
        needles
    };
    let doc = match findx2_gui_native::docx_blocks::parse(&path) {
        Ok(d) => d,
        Err(e) => {
            println!("PARSE FAILED: {e}");
            return;
        }
    };
    println!("blocks={} plain_len={}", doc.blocks.len(), doc.plain.len());
    let hits = findx2_gui_native::docx_blocks::match_hits_multi(&doc.plain, &needles);
    println!("needles={needles:?} hits={}", hits.len());
    // 打印前 3 块的 plain 起点与文本
    for i in 0..doc.blocks.len().min(3) {
        let start = doc.block_starts.get(i).copied().unwrap_or(0);
        let text = match &doc.blocks[i] {
            findx2_gui_native::docx_blocks::DocxBlock::Paragraph(p) => p.text(),
            findx2_gui_native::docx_blocks::DocxBlock::Table(_) => "<table>".to_string(),
        };
        println!("  block[{i}] plain_start={start} text_len={} text={:?}", text.len(), text.chars().take(30).collect::<String>());
    }
    println!("hits detail:");
    for (i, r) in hits.iter().enumerate() {
        let ctx = doc.plain.get(r.clone()).unwrap_or("?");
        println!("  #{i} {}..{} = {:?}", r.start, r.end, ctx);
    }
}
