//! 临时诊断：连接 `\\.\pipe\findx2` 发送 Status，打印响应 JSON（重点 tombstone_count）。
//! 用法: cargo run --release -p findx2-ipc --example tmp_ipc_status [-- <pipe_name>]

use std::io::{BufRead, BufReader, Write};

fn main() {
    let name = std::env::args().nth(1).unwrap_or_else(|| "findx2".into());
    let path = format!(r"\\.\pipe\{name}");
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("open pipe");
    f.write_all(b"{\"type\":\"status\"}\n").expect("write");
    let mut line = String::new();
    BufReader::new(f).read_line(&mut line).expect("read");
    println!("raw: {}", line.trim());
}
