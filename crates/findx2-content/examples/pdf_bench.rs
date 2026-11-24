//! PDF 双引擎 A/B：pdfium 主路径 vs pdf-extract 兜底，在真实语料上计时对比。
//! 用法：cargo run -p findx2-content --example pdf_bench -- <pdf...>
use std::time::Instant;

use findx2_content::{extract_text, office::extract_pdf_fallback};

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    if files.is_empty() {
        eprintln!("usage: pdf_bench <pdf...>");
        std::process::exit(2);
    }
    let mut t_new = 0u128;
    let mut t_old = 0u128;
    let mut n_new = 0u32;
    let mut n_old = 0u32;
    for f in &files {
        let p = std::path::Path::new(f);
        let t0 = Instant::now();
        let r1 = extract_text(p).map(|e| e.text.len());
        let d1 = t0.elapsed();
        let t0 = Instant::now();
        let r0 = extract_pdf_fallback(p)
            .map(|t| t.len())
            .map_err(|_| ());
        let d0 = t0.elapsed();
        match r1 {
            Ok(len) => {
                n_new += 1;
                t_new += d1.as_millis();
            }
            Err(_) => {}
        }
        if r0.is_ok() {
            n_old += 1;
            t_old += d0.as_millis();
        }
        println!(
            "{} new={:?}({}ms) old={}({}ms)",
            f,
            r1.map(|l| l.to_string()),
            d1.as_millis(),
            if r0.is_ok() { "ok" } else { "err" },
            d0.as_millis()
        );
    }
    println!("new(pdfium first): {n_new} ok, {t_new}ms total");
    println!("old(pdf-extract): {n_old} ok, {t_old}ms total");
}
