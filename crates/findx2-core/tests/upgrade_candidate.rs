//! Bounded public fixtures only; these tests never open installed indexes/watchers.
use findx2_core::{
    V6Layout,
    index::{IndexBuilder, IndexStore},
    load_index_bin, load_store_auto, merge_index_stores,
    platform::RawEntry,
    save_index_bin, v6_load, v6_save_full,
};
use std::{
    io::{Seek, SeekFrom, Write},
    path::PathBuf,
};
fn fixture(label: &str) -> PathBuf {
    let base = std::env::var_os("FINDX_UPGRADE_FIXTURE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = base.join(format!(
        "findx-upgrade-{label}-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    println!("public_upgrade_fixture={}", dir.display());
    dir
}
fn store() -> IndexStore {
    let mut all = Vec::new();
    for (letter, id) in [(b'C', 100), (b'D', 200)] {
        let raw = |file_id, parent_id, name: &str, is_dir| RawEntry {
            file_id,
            file_id_128: None,
            parent_id,
            name: name.into(),
            size: if is_dir { 0 } else { 42 },
            mtime: 1,
            ctime: 1,
            attrs: if is_dir { 0x10 } else { 0 },
            is_dir,
        };
        all.push(
            IndexBuilder::new(letter, id as u32, id * 10, id * 100)
                .build_from_raw(
                    vec![raw(id + 1, id, "public.txt", false)],
                    vec![raw(id, 0, "", true)],
                    true,
                )
                .unwrap(),
        );
    }
    let mut s = merge_index_stores(all).unwrap();
    s.metadata_ready = false;
    s
}
fn v5_bytes(current: &[u8]) -> Vec<u8> {
    assert_eq!(u32::from_le_bytes(current[4..8].try_into().unwrap()), 6);
    let n = u32::from_le_bytes(current[36..40].try_into().unwrap()) as usize;
    let start = 64 + n * 32;
    let mut end = start;
    for _ in 0..n {
        for _ in 0..2 {
            let len = u16::from_le_bytes(current[end..end + 2].try_into().unwrap()) as usize;
            end += 2 + len;
        }
    }
    let mut v5 = current.to_vec();
    v5.drain(start..end);
    v5[4..8].copy_from_slice(&5u32.to_le_bytes());
    v5
}
#[test]
fn v5_two_volume_copy_upgrade_and_exact_backup_rollback() {
    let dir = fixture("rollback");
    let source = store();
    let export = dir.join("export.bin");
    save_index_bin(&export, &source).unwrap();
    let old = v5_bytes(&std::fs::read(&export).unwrap());
    let backup = dir.join("immutable-backup.bin");
    std::fs::write(&backup, &old).unwrap();
    let candidate = dir.join("candidate.bin");
    std::fs::write(&candidate, &old).unwrap();
    let mut migrated = load_store_auto(&candidate).unwrap();
    assert_eq!(migrated.entry_count(), source.entry_count());
    assert_eq!(migrated.volumes.len(), 2);
    assert!(!migrated.metadata_ready);
    for (a, b) in migrated.volumes.iter().zip(&source.volumes) {
        assert_eq!(
            (a.volume_letter, a.usn_journal_id, a.last_usn),
            (b.volume_letter, b.usn_journal_id, b.last_usn)
        );
    }
    migrated.entries[0].size += 1;
    let layout = V6Layout::for_index(&candidate);
    v6_save_full(&layout, &migrated).unwrap();
    let restarted = v6_load(&layout).unwrap();
    assert_eq!(restarted.entries[0].size, migrated.entries[0].size);
    assert!(!restarted.metadata_ready);
    assert_eq!(restarted.volumes.len(), 2);
    // Rollback uses a matched immutable OLD snapshot, never candidate segments.
    let rollback = dir.join("restored-old.bin");
    std::fs::write(&rollback, std::fs::read(&backup).unwrap()).unwrap();
    let restored = load_index_bin(&rollback).unwrap();
    assert_eq!(restored.entries[0].size, source.entries[0].size);
    assert_eq!(restored.volumes.len(), 2);
    assert_eq!(std::fs::read(&backup).unwrap(), old);
    assert_eq!(std::fs::read(&rollback).unwrap(), old);
}
#[test]
fn release_blocker_old_manifest_does_not_undo_an_in_place_prefix_patch() {
    let dir = fixture("crash-prefix");
    let source = store();
    let layout = V6Layout::for_index(&dir.join("fixture.bin"));
    v6_save_full(&layout, &source).unwrap();
    let manifest = std::fs::read(layout.dir.join("manifest.json")).unwrap();
    let changed = source.entries[0].size + 123;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(layout.dir.join("entries.dat"))
        .unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(&changed.to_le_bytes()).unwrap();
    file.sync_all().unwrap();
    drop(file);
    assert_eq!(
        std::fs::read(layout.dir.join("manifest.json")).unwrap(),
        manifest
    );
    assert_eq!(v6_load(&layout).unwrap().entries[0].size, changed);
    println!(
        "EXPECTED_RELEASE_BLOCKER: unchanged old manifest cannot undo an uncommitted in-place prefix patch; this is NOT a passing power-loss-safety test"
    );
}

// Existing in both installed 2.4.10 CLI and candidate. Windows normalizes these
// redundant separators, so this is a canonical-path/UX gate, not proven loss.
#[test]
#[ignore = "P1 existing multi-volume root path contains redundant separators; release gate remains open"]
fn known_legacy_second_volume_path_is_not_canonical() {
    let dir = fixture("canonical-path");
    let p = dir.join("legacy.bin");
    save_index_bin(&p, &store()).unwrap();
    let bytes = v5_bytes(&std::fs::read(&p).unwrap());
    let legacy = dir.join("legacy-v5.bin");
    std::fs::write(&legacy, bytes).unwrap();
    let loaded = load_index_bin(&legacy).unwrap();
    assert_eq!(loaded.entry_display_path(2).unwrap(), r"D:\public.txt");
}
