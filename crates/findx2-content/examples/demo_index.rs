//! 生成固定演示库：demo/vol（txt/md + 真实 docx/xlsx/pdf fixture 拷贝）
//! → demo/index.bin → demo/index.bin.content/。
//! 用法：先跑本 example，再用 CLI 验证：
//!   findx2 status --index <demo>/index.bin
//!   findx2 search --index <demo>/index.bin "content:xxx"

use std::io::Write;

use findx2_content::{build_from_store, content_dir_for};
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!("用法：demo_index <demo-dir>");
        std::process::exit(2);
    }
    let demo = std::path::PathBuf::from(&args[1]);
    let vol = demo.join("vol");
    let _ = std::fs::create_dir_all(&vol);

    std::fs::File::create(vol.join("alpha.txt"))
        .unwrap()
        .write_all("findx 全文搜索演示：本地磁盘的 Google".as_bytes())
        .unwrap();
    std::fs::File::create(vol.join("notes.md"))
        .unwrap()
        .write_all("# beta\nnothing relevant here\n".as_bytes())
        .unwrap();
    // 真实 Office/PDF 样本（gui-native 自带公开 fixture）。
    let fx = r"C:\Users\chaoj\dev\tools\findx-content\crates\findx2-gui-native\tests\fixtures";
    for f in [
        "findx-public-layout.docx",
        "findx-public-layout.xlsx",
        "findx-public-three-pages.pdf",
    ] {
        let _ = std::fs::copy(
            std::path::Path::new(fx).join(f),
            vol.join(f),
        );
    }

    let rp = vol.display().to_string().replace('\\', "/");
    let files: Vec<String> = std::fs::read_dir(&vol)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let mut raws = Vec::new();
    let mut frn = 1000u64;
    for n in &files {
        frn += 1;
        raws.push(raw(frn, 100, n, false));
    }
    let store = IndexBuilder::new(0, 1, 1, 1)
        .with_unix_volume("C", rp)
        .build_from_raw(raws, vec![raw(100, 0, "", true)], true)
        .unwrap();
    let index_bin = demo.join("index.bin");
    findx2_core::save_index_bin(&index_bin, &store).unwrap();
    println!("[demo] index.bin：{} 条目", store.entry_count());

    let cdir = content_dir_for(&index_bin);
    let st = build_from_store(&store, &cdir, true, |s| {
        println!(
            "[demo] 扫描 {} / 抽取 {} / 跳过 {} / 失败 {}",
            s.scanned, s.extracted, s.skipped, s.failed
        );
        true
    })
    .unwrap();
    println!("[demo] 内容索引：{st:?} → {}", cdir.display());
}
