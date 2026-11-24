//! 诊断：16 线程并发压 pdf 进程池，看是否 hang。
//! 用法：cargo run -p findx2-content --example pool_stress -- <pdf...>
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    if files.is_empty() {
        eprintln!("usage: pool_stress <pdf...>");
        std::process::exit(2);
    }
    let pool = match findx2_content::pdf_pool::global() {
        Some(p) => p,
        None => {
            eprintln!("no pool (worker missing?)");
            std::process::exit(3);
        }
    };
    // 预热：主线程先跑一个，确认单线程路径通。
    let t0 = Instant::now();
    let first = std::path::Path::new(&files[0]);
    match pool.extract(first) {
        Ok(t) => println!("warmup ok: {} chars", t.len()),
        Err(e) => println!("warmup err: {e}"),
    }
    println!("warmup took {:?}", t0.elapsed());
    let ok = AtomicU64::new(0);
    let err = AtomicU64::new(0);
    let t0 = Instant::now();
    let ok_ref = &ok;
    let err_ref = &err;
    std::thread::scope(|s| {
        for chunk in files.chunks(files.len().div_ceil(16)) {
            let chunk: Vec<String> = chunk.to_vec();
            s.spawn(move || {
                for f in &chunk {
                    match pool.extract(std::path::Path::new(f)) {
                        Ok(_) => {
                            ok_ref.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(_) => {
                            err_ref.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            });
        }
    });
    println!(
        "done in {:?}: ok={} err={}",
        t0.elapsed(),
        ok.load(Ordering::Relaxed),
        err.load(Ordering::Relaxed)
    );
}
