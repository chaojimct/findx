//! 从仓库 `files_for_test` 合成 `index.bin`（无需管理员卷扫描，供本地 IPC/GUI 联调）。
//! 用法：`cargo run -p findx2-core --example write_fixture_index`

use std::path::Path;

use findx2_core::index::IndexBuilder;
use findx2_core::platform::RawEntry;
use findx2_core::save_index_bin;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.join("../..");
    let fixture = repo_root.join("files_for_test");
    let out = repo_root.join("index.bin");

    if !fixture.is_dir() {
        eprintln!("缺少目录 {}", fixture.display());
        std::process::exit(1);
    }

    const ROOT_FRN: u64 = 100;
    let dirs = vec![RawEntry {
        file_id: ROOT_FRN,
        file_id_128: None,
        parent_id: 0,
        name: "fixture_root".into(),
        size: 0,
        mtime: 0,
        ctime: 0,
        attrs: 0x10,
        is_dir: true,
    }];

    let mut files: Vec<RawEntry> = Vec::new();
    let mut id: u64 = 10_000;
    for e in std::fs::read_dir(&fixture)? {
        let e = e?;
        let meta = e.metadata()?;
        if !meta.is_file() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        files.push(RawEntry {
            file_id: id,
            file_id_128: None,
            parent_id: ROOT_FRN,
            name,
            size: 1,
            mtime: 1,
            ctime: 1,
            attrs: 0,
            is_dir: false,
        });
        id += 1;
    }

    let store = IndexBuilder::new(b'C', 1, 1, 1).build_from_raw(files, dirs, true)?;
    save_index_bin(&out, &store)?;
    eprintln!("写入 {}，条目 {}", out.display(), store.entry_count());
    Ok(())
}
