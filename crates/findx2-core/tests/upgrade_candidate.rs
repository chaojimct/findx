//! Bounded public fixtures only; these tests never open installed indexes/watchers.
use findx2_core::{
    V6Layout,
    index::{IndexBuilder, IndexStore},
    load_index_bin, load_store_auto, merge_index_stores,
    platform::RawEntry,
    generation_dir, published_path, read_published, save_index_bin, v6_load, v6_save_full,
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
fn legacy_flat_in_place_prefix_is_still_unsafe() {
    let dir = fixture("legacy-prefix");
    let source = store();
    let layout = V6Layout::for_index(&dir.join("fixture.bin"));
    v6_save_full(&layout, &source).unwrap();
    let ptr = read_published(&layout).unwrap().unwrap();
    let gen = generation_dir(&layout, ptr.manifest.generation);
    for name in ["entries.dat","frns.dat","dirs.dat","ranges.dat","names.dat","dirpaths.dat","manifest.json"] {
        std::fs::copy(gen.join(name), layout.dir.join(name)).unwrap();
    }
    std::fs::remove_file(published_path(&layout)).unwrap();
    std::fs::rename(layout.dir.join("gen"), dir.join("set-aside-gen")).unwrap();
    let manifest = std::fs::read(layout.dir.join("manifest.json")).unwrap();
    let changed = source.entries[0].size + 123;
    let mut file = std::fs::OpenOptions::new().write(true).open(layout.dir.join("entries.dat")).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(&changed.to_le_bytes()).unwrap();
    file.sync_all().unwrap();
    drop(file);
    assert_eq!(std::fs::read(layout.dir.join("manifest.json")).unwrap(), manifest);
    assert_eq!(v6_load(&layout).unwrap().entries[0].size, changed);
    println!("legacy flat v6 remains unsafe under in-place prefix patches; transactional save must copy, not patch, this format");
}

#[test]
fn transactional_save_copies_legacy_and_ignores_later_prefix_patches() {
    let dir = fixture("cow-protect");
    let source = store();
    let layout = V6Layout::for_index(&dir.join("fixture.bin"));
    v6_save_full(&layout, &source).unwrap();
    let original = source.entries[0].size;
    // A leftover or pre-migration flat file must not change the published generation.
    std::fs::write(layout.dir.join("entries.dat"), [0xFFu8; 32]).unwrap();
    assert_eq!(v6_load(&layout).unwrap().entries[0].size, original);
    // In-place mutation of the live generation is detected, not mixed with leftover files.
    let ptr = read_published(&layout).unwrap().unwrap();
    let live = generation_dir(&layout, ptr.manifest.generation).join("entries.dat");
    let mut file = std::fs::OpenOptions::new().write(true).open(&live).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(&(original + 123).to_le_bytes()).unwrap();
    file.sync_all().unwrap();
    drop(file);
    assert!(v6_load(&layout).is_err(), "corrupt published generation must not load mixed leftover bytes");
}

// Read-compatible correction of the historical ignored D:\\public.txt case.
// No loader or format migration is involved: old cached paths remain untouched.
#[test]
fn legacy_second_volume_paths_are_canonical_without_rewriting_indexes() {
    let dir = fixture("canonical-path");
    let p = dir.join("legacy.bin");
    save_index_bin(&p, &store()).unwrap();
    let bytes = v5_bytes(&std::fs::read(&p).unwrap());
    let legacy = dir.join("legacy-v5.bin");
    std::fs::write(&legacy, &bytes).unwrap();
    let assert_paths = |loaded: &IndexStore| {
        for (idx, expected) in [(0, r"C:\public.txt"), (2, r"D:\public.txt")] {
            assert_eq!(loaded.entry_display_path(idx).unwrap(), expected);
            assert_eq!(loaded.entry_full_path_lower(idx), expected.to_ascii_lowercase());
        }
        assert_eq!(loaded.entry_display_path(1).unwrap(), r"C:\");
        assert_eq!(loaded.entry_display_path(3).unwrap(), r"D:\");
        assert_eq!(loaded.volumes[0].last_usn, 10_000);
        assert_eq!(loaded.volumes[1].last_usn, 20_000);
    };
    let loaded = load_index_bin(&legacy).unwrap();
    assert_paths(&loaded);
    assert_eq!(std::fs::read(&legacy).unwrap(), bytes, "read must not rewrite legacy v5");

    let layout = V6Layout::for_index(&dir.join("canonical-v6.bin"));
    v6_save_full(&layout, &loaded).unwrap();
    let published = std::fs::read(published_path(&layout)).unwrap();
    let ptr = read_published(&layout).unwrap().unwrap();
    let gen = generation_dir(&layout, ptr.manifest.generation);
    let names = ["entries.dat", "frns.dat", "dirs.dat", "ranges.dat", "names.dat", "dirpaths.dat", "manifest.json"];
    let original: Vec<_> = names.iter().map(|name| std::fs::read(gen.join(name)).unwrap()).collect();
    for _ in 0..2 {
        let restarted = v6_load(&layout).unwrap();
        assert_paths(&restarted);
        for (name, before) in names.iter().zip(&original) {
            assert_eq!(&std::fs::read(gen.join(name)).unwrap(), before, "read changed {name}");
        }
        assert_eq!(std::fs::read(published_path(&layout)).unwrap(), published);
    }
    assert_eq!(std::fs::read(&legacy).unwrap(), bytes);
}

fn store_with_pinyin() -> IndexStore {
    let mut all = Vec::new();
    for (letter, id, extra) in [
        (b'C', 100, Some("\u{7eff}\u{6c34}\u{9752}\u{5c71}.txt")),
        (b'D', 200, None),
    ] {
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
        let mut files = vec![raw(id + 1, id, "public.txt", false)];
        if let Some(name) = extra {
            files.push(raw(id + 2, id, name, false));
        }
        all.push(
            IndexBuilder::new(letter, id as u32, id * 10, id * 100)
                .build_from_raw(files, vec![raw(id, 0, "", true)], true)
                .unwrap(),
        );
    }
    let mut s = merge_index_stores(all).unwrap();
    s.metadata_ready = false;
    s
}

/// Writes a public v5 two-volume fixture when FINDX_EXPORT_LEGACY_FIXTURE is set.
/// Never opens an installed or experimental live index.
#[test]
fn export_stage_c_legacy_fixtures_if_requested() {
    let Some(out) = std::env::var_os("FINDX_EXPORT_LEGACY_FIXTURE") else {
        return;
    };
    let out = PathBuf::from(out);
    std::fs::create_dir_all(&out).unwrap();
    let source = store_with_pinyin();
    let current = out.join("current-v6.bin");
    save_index_bin(&current, &source).unwrap();
    let v5 = v5_bytes(&std::fs::read(&current).unwrap());
    let dest = out.join("index.bin");
    std::fs::write(&dest, &v5).unwrap();
    let loaded = load_index_bin(&dest).unwrap();
    assert_eq!(loaded.volumes.len(), 2);
    assert!(!loaded.metadata_ready);
    let needle = "\u{7eff}\u{6c34}\u{9752}\u{5c71}.txt".as_bytes();
    assert!(
        loaded
            .entries
            .iter()
            .any(|e| loaded.name_bytes(e) == needle),
        "pinyin probe entry must be present"
    );
    println!("exported_legacy_v5={}", dest.display());
}

#[test]
fn second_volume_self_parent_root_and_nested_paths_match_cached_and_uncached() {
    let mut stores = Vec::new();
    for (letter, id) in [(b'C', 100u64), (b'D', 200)] {
        let raw = |file_id, parent_id, name: &str, is_dir| RawEntry {
            file_id, file_id_128: None, parent_id, name: name.into(),
            size: 0, mtime: 0, ctime: 0, attrs: if is_dir { 0x10 } else { 0 }, is_dir,
        };
        stores.push(IndexBuilder::new(letter, id as u32, id, id).build_from_raw(
            vec![raw(id+1,id,"root.txt",false),raw(id+3,id+2,"child.txt",false)],
            vec![raw(id,0,"",true),raw(id+2,id,"nested",true)],true).unwrap());
    }
    let mut merged = merge_index_stores(stores).unwrap();
    let check = |store: &IndexStore| {
        let paths: Vec<_> = (0..store.entry_count()).map(|idx| {
            let path = store.entry_display_path(idx).unwrap();
            assert!(!path.contains(r"\\"), "noncanonical merged root: {path:?}");
            assert_eq!(store.entry_full_path_lower(idx),path.to_ascii_lowercase());
            path
        }).collect();
        for expected in [r"C:\root.txt",r"D:\root.txt",r"C:\nested\child.txt",r"D:\nested\child.txt",r"C:\nested",r"D:\nested",r"C:\",r"D:\"] {
            assert!(paths.iter().any(|p|p==expected),"missing {expected:?} in {paths:?}");
        }
    };
    check(&merged);
    merged.rebuild_dir_paths();
    check(&merged);
    // A legacy root cache is interpreted canonically without being repaired in place.
    let mut old_cache = merged.dir_paths_buf.clone();
    old_cache.extend_from_slice(br"\\");
    let offset=(old_cache.len()-2) as u32;
    let root=merged.dir_root_idx(merged.entries.iter().enumerate().find(|(i,e)| !e.is_dir_entry() && merged.volume_letter_for_entry(*i)=='D').unwrap().1.dir_idx);
    merged.dir_paths_buf=old_cache;
    merged.dir_path_ranges[root as usize]=(offset,2);
    let before=merged.dir_paths_buf.clone();
    check(&merged);
    assert_eq!(merged.dir_paths_buf,before,"read must not mutate old cached bytes");
    let engine = findx2_core::SearchEngine::new(merged);
    for (text, expected) in [
        ("parent:C:/ file:",r"C:\root.txt"),
        ("parent:D:/ file:",r"D:\root.txt"),
        ("parent:C:/nested file:",r"C:\nested\child.txt"),
        ("parent:D:/nested file:",r"D:\nested\child.txt"),
        (r"path:D:\nested child.txt",r"D:\nested\child.txt"),
    ] {
        let query=findx2_core::QueryParser::parse(text).unwrap();
        let options=findx2_core::SearchOptions::default();
        let (hits,total)=engine.search(&query,&options).unwrap();
        assert_eq!(total,1,"canonical direct query: {text}");
        assert_eq!(hits[0].path,expected,"canonical direct query: {text}");
        let (page,n)=engine.search_paged(text,&query,&options,0,10).unwrap();
        assert_eq!(n,1,"canonical paged query: {text}");
        assert_eq!(page[0].path,expected,"canonical paged query: {text}");
    }
}
