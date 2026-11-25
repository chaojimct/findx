use findx2_core::{
    index::IndexBuilder,
    platform::{ChangeEvent, RawEntry},
    QueryParser, SearchEngine, SearchOptions, SortField,
};
fn raw(id: u64, parent: u64, name: &str, dir: bool) -> RawEntry {
    RawEntry {
        file_id: id,
        file_id_128: None,
        parent_id: parent,
        name: name.into(),
        size: id + 1,
        mtime: 116444736000000000 + id * 10000000,
        ctime: 116444736000000000 + id * 10000000,
        attrs: if dir { 0x10 } else { 0 },
        is_dir: dir,
    }
}
fn store(letter: u8) -> findx2_core::IndexStore {
    let dirs = vec![
        raw(10, 0, "", true),
        raw(20, 10, "dev", true),
        raw(30, 10, "other", true),
        raw(40, 20, "deep", true),
        raw(50, 10, "DEV", true),
    ];
    let mut files: Vec<_> = (0..240)
        .map(|i| {
            raw(
                1000 + i,
                match i % 4 {
                    0 => 10,
                    1 => 20,
                    2 => 40,
                    _ => 50,
                },
                &format!("item_{i:04}.log"),
                false,
            )
        })
        .collect();
    files.push(raw(800, 20, "hardlink_a.bin", false));
    files.push(raw(800, 30, "hardlink_b.bin", false));
    IndexBuilder::new(letter, 1, 2, 3)
        .build_from_raw(files, dirs, true)
        .unwrap()
}
fn engine() -> SearchEngine {
    let e = SearchEngine::new(store(b'C'));
    e.enable_usage(256 * 1024 * 1024).unwrap();
    e
}
fn compare(e: &SearchEngine, text: &str) {
    let mut q = QueryParser::parse(text).unwrap();
    q.limit = u32::MAX;
    let (all, total) = e.search(&q, &SearchOptions::default()).unwrap();
    assert_eq!(all.len(), total as usize);
    for (offset, limit) in [(0, 13), (7, 31), (0, 10000), (9999, 20), (0, 0)] {
        let (page, n) = e
            .search_paged(text, &q, &SearchOptions::default(), offset, limit, None, None)
            .unwrap();
        assert_eq!(n, total, "{text}");
        let expected: Vec<_> = all.iter().skip(offset).take(limit).collect();
        // Name order has no secondary identity key. Equal-key boundary membership
        // may differ; every non-boundary member and primary key position must agree.
        let effective = if !e.index_store().metadata_ready
            && matches!(
                q.sort_by,
                SortField::Size | SortField::Modified | SortField::Created
            ) {
            SortField::Name
        } else {
            q.sort_by
        };
        let key = |h: &findx2_core::SearchHit| match effective {
            SortField::Name => h.name.to_ascii_lowercase(),
            SortField::Path => h.path.to_ascii_lowercase(),
            SortField::Size => format!("{:020}", h.size),
            SortField::Modified => format!("{:020}", h.mtime),
            SortField::Created => format!(
                "{:020}",
                e.index_store().entries[h.entry_idx as usize].ctime
            ),
        };
        let expected_keys: Vec<_> = expected.iter().map(|h| key(h)).collect();
        let got_keys: Vec<_> = page.iter().map(&key).collect();
        assert_eq!(got_keys, expected_keys, "{text} offset {offset}");
        let mut got_ids: Vec<_> = page.iter().map(|h| h.entry_idx).collect();
        got_ids.sort_unstable();
        let before = got_ids.len();
        got_ids.dedup();
        assert_eq!(got_ids.len(), before);
        let boundary = |h: &findx2_core::SearchHit| {
            expected_keys.first() == Some(&key(h)) || expected_keys.last() == Some(&key(h))
        };
        let mut expected_ids: Vec<_> = expected
            .iter()
            .filter(|h| !boundary(h) || page.len() == all.len())
            .map(|h| h.entry_idx)
            .collect();
        let mut strict_got: Vec<_> = page
            .iter()
            .filter(|h| !boundary(h) || page.len() == all.len())
            .map(|h| h.entry_idx)
            .collect();
        expected_ids.sort_unstable();
        strict_got.sort_unstable();
        assert_eq!(strict_got, expected_ids, "{text} nonboundary membership");
        for h in page {
            let reference = all.iter().find(|x| x.entry_idx == h.entry_idx).unwrap();
            assert_eq!(h.path, reference.path);
            assert_eq!(h.size, reference.size);
        }
    }
}
#[test]
fn root_deep_case_collisions_types_sort_and_paging() {
    let e = engine();
    for text in [
        "parent:C:/",
        "parent:c:/dev",
        "parent:C:/DEV/deep",
        "parent:C:/other",
        "parent:C:/ file:",
        "parent:C:/ folder:",
        "parent:C:/dev sort:path",
        "parent:C:/dev sort:size",
        "parent:C:/dev sort:modified",
        "parent:C:/dev sort:created",
        "parent:C:/dev sort:path:desc",
        "parent:C:/dev sort:name:desc",
        "parent:C:/does-not-exist",
        "parent:dev",
        "parentcontains:dev",
        "parent:C:/dev ext:log",
        "parent:C:/dev size:>100",
        "parent:C:/dev item_",
        "parent:C:/dev | parent:C:/other",
    ] {
        compare(&e, text);
    }
}
#[test]
fn create_rename_move_delete_rebuild_and_stale_descendants() {
    let e = engine();
    e.apply_volume_change(
        b'C',
        &ChangeEvent::Create {
            entry: raw(9000, 20, "new.bin", false),
        },
    )
    .unwrap();
    compare(&e, "parent:C:/dev");
    e.apply_volume_change(
        b'C',
        &ChangeEvent::Rename {
            file_id: 9000,
            new_parent_id: 30,
            new_name: "moved.bin".into(),
        },
    )
    .unwrap();
    compare(&e, "parent:C:/dev");
    compare(&e, "parent:C:/other");
    e.apply_volume_change(
        b'C',
        &ChangeEvent::Rename {
            file_id: 20,
            new_parent_id: 30,
            new_name: "moved-dir".into(),
        },
    )
    .unwrap();
    compare(&e, "parent:C:/other/moved-dir");
    e.apply_volume_change(b'C', &ChangeEvent::Delete { file_id: 20 })
        .unwrap();
    compare(&e, "parent:C:/other/moved-dir");
    e.apply_volume_change(
        b'C',
        &ChangeEvent::Create {
            entry: raw(60, 30, "moved-dir", true),
        },
    )
    .unwrap();
    e.apply_volume_change(
        b'C',
        &ChangeEvent::Create {
            entry: raw(9001, 60, "rebuilt.bin", false),
        },
    )
    .unwrap();
    compare(&e, "parent:C:/other/moved-dir");
    compare(&e, "parent:C:/");
}
#[test]
fn missing_parent_parking_and_external_epoch_fallback() {
    let e = engine();
    e.apply_volume_batch(
        b'C',
        &[ChangeEvent::Create {
            entry: raw(9100, 8888, "parked.bin", false),
        }],
        2,
        4,
    )
    .unwrap();
    assert!(e.has_pending_namespace(b'C'));
    compare(&e, "parent:C:/");
    e.apply_volume_batch(
        b'C',
        &[ChangeEvent::Create {
            entry: raw(8888, 10, "late", true),
        }],
        2,
        5,
    )
    .unwrap();
    assert!(!e.has_pending_namespace(b'C'));
    compare(&e, "parent:C:/late");
    {
        let mut g = e.index_store_mut();
        g.apply_change_event(&ChangeEvent::Create {
            entry: raw(9200, 20, "external.bin", false),
        })
        .unwrap();
    }
    e.note_external_mutation();
    compare(&e, "parent:C:/dev");
}
#[test]
fn volume_identity_and_same_file_id_links() {
    let e =
        SearchEngine::new(findx2_core::merge_index_stores(vec![store(b'C'), store(b'D')]).unwrap());
    e.enable_usage(256 * 1024 * 1024).unwrap();
    for t in [
        "parent:C:/",
        "parent:D:/",
        "parent:C:/dev",
        "parent:D:/dev",
        "parent:E:/",
        "parent:dev",
    ] {
        compare(&e, t);
    }
    e.apply_volume_change(
        b'D',
        &ChangeEvent::Create {
            entry: raw(9800, 20, "d-only.bin", false),
        },
    )
    .unwrap();
    compare(&e, "parent:D:/dev");
    compare(&e, "parent:C:/dev");
}
#[test]
fn metadata_overlay_and_unsupported_filters_stay_coherent() {
    let e = engine();
    let q = QueryParser::parse("hardlink_a").unwrap();
    let (h, _) = e.search(&q, &SearchOptions::default()).unwrap();
    let ticket = e.metadata_ticket(h[0].entry_idx as usize).unwrap();
    e.commit_metadata(ticket, 999999, 116444736999999999, 116444736999999999)
        .unwrap();
    compare(&e, "parent:C:/dev");
    compare(&e, "parent:C:/dev size:>900000");
}

#[test]
fn empty_parent_delete_hidden_dir_then_recreate() {
    let e = engine();
    e.apply_volume_change(
        b'C',
        &ChangeEvent::Create {
            entry: raw(9900, 10, "empty", true),
        },
    )
    .unwrap();
    e.apply_volume_change(b'C', &ChangeEvent::Delete { file_id: 9900 })
        .unwrap();
    e.apply_volume_change(
        b'C',
        &ChangeEvent::Create {
            entry: raw(9901, 9900, "hidden", true),
        },
    )
    .unwrap();
    e.apply_volume_change(
        b'C',
        &ChangeEvent::Create {
            entry: raw(9902, 10, "empty", true),
        },
    )
    .unwrap();
    compare(&e, "parent:C:/empty");
}

#[test]
fn hardlink_overlay_move_and_delete_keep_legacy_membership() {
    let e = engine();
    compare(&e, "parent:C:/dev");
    compare(&e, "parent:C:/other");
    e.apply_volume_change(
        b'C',
        &ChangeEvent::Rename {
            file_id: 800,
            new_parent_id: 20,
            new_name: "moved-link.bin".into(),
        },
    )
    .unwrap();
    compare(&e, "parent:C:/dev");
    compare(&e, "parent:C:/other");
    e.apply_volume_change(b'C', &ChangeEvent::Delete { file_id: 800 })
        .unwrap();
    compare(&e, "parent:C:/dev");
    compare(&e, "parent:C:/other");
    compare(&e, "parent:C:/");
}
