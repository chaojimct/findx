//! scope 复现：对给定文件列表直调 build_from_plan（抽取+入库+commit 全链路），
//! 打印统计与失败样本。用于定位全量构建的系统性失败。
use std::io::Write;

use findx2_content::{build_from_plan, BuildStats, PlanItem};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: pool_scope <file...> | @<listfile>");
        std::process::exit(2);
    }
    let files: Vec<String> = if args.len() == 1 && args[0].starts_with('@') {
        let txt = std::fs::read_to_string(&args[0][1..]).unwrap();
        txt.lines().map(|s| s.to_string()).filter(|s| !s.is_empty()).collect()
    } else {
        args
    };
    let items: Vec<PlanItem> = files
        .iter()
        .enumerate()
        .map(|(i, p)| PlanItem {
            frn: i as u64 + 1,
            volume_key: "SCOPE".to_string(),
            path: p.clone(),
            mtime: 0,
            size: std::fs::metadata(p).map(|m| m.len()).unwrap_or(0),
        })
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let cdir = dir.path().join("scope.content");
    let mut st = BuildStats::default();
    match build_from_plan(&items, &cdir, true, &mut st, |s| {
        eprintln!(
            "progress ext={} skip={} fail={}",
            s.extracted, s.skipped, s.failed
        );
        true
    }) {
        Ok(docs) => {
            println!("DONE docs={} extracted={} skipped={} failed={}", docs, st.extracted, st.skipped, st.failed);
            for s in st.failed_samples.iter().take(25) {
                println!("FAIL: {s}");
            }
        }
        Err(e) => {
            println!("BUILD_ERR: {e}");
            for s in st.failed_samples.iter().take(25) {
                println!("FAIL: {s}");
            }
        }
    }
    let _ = std::io::stdout().flush();
}
