//! 取消语义：progress 返回 false → Err(Cancelled)，新建目录被清理。

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

#[test]
fn cancelled_build_cleans_fresh_dir() {
    let case = tempfile::tempdir().unwrap();
    let root = case.path().join("vol");
    std::fs::create_dir(&root).unwrap();
    for name in ["a.txt", "b.txt"] {
        std::fs::File::create(root.join(name))
            .unwrap()
            .write_all("findx 全文搜索".as_bytes())
            .unwrap();
    }
    let rp = root.display().to_string().replace('\\', "/");
    let store = IndexBuilder::new(0, 1, 1, 1)
        .with_unix_volume("C", rp)
        .build_from_raw(
            vec![raw(1001, 100, "a.txt", false), raw(1002, 100, "b.txt", false)],
            vec![raw(100, 0, "", true)],
            true,
        )
        .unwrap();
    let index_bin = case.path().join("index.bin");
    findx2_core::save_index_bin(&index_bin, &store).unwrap();

    let cdir = content_dir_for(&index_bin);
    let err = build_from_store(&store, &cdir, true, |_| false).unwrap_err();
    assert!(
        matches!(err, findx2_content::Error::Cancelled),
        "expected Cancelled, got {err:?}"
    );
    assert!(!cdir.exists(), "新建目录应在取消后被清理");
}
