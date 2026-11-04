//! 回归测试：多卷合并索引中，增量追加的条目应使用正确的卷前缀
//!
//! Bug: 在同设备多根场景（如 /home/box 和 /workspace），增量 CreatePending
//! 追加的条目会超出原始 first_entry_idx 范围，volume_for_entry 错误地选择
//! 最后一个卷，导致显示路径使用错误的 root_prefix。
//!
//! 修复：volume_for_entry_by_parent 从父目录推导卷，确保路径前缀正确。

use findx2_core::index::IndexBuilder;
use findx2_core::platform::{ChangeEvent, RawEntry};

#[test]
fn test_incremental_create_preserves_correct_volume_prefix() {
    // 构建两个卷的合并索引，模拟 /home/box 和 /workspace 在同一设备
    
    // 卷 0: /home/box
    let vol0_root_dir = RawEntry {
        file_id: 1000,
        file_id_128: None,
        parent_id: 0,
        name: String::new(),
        size: 0,
        mtime: 0,
        ctime: 0,
        attrs: 0x10,
        is_dir: true,
    };
    
    let vol0_subdir = RawEntry {
        file_id: 1001,
        file_id_128: None,
        parent_id: 1000,
        name: "FindX拼音测试".into(),
        size: 0,
        mtime: 0,
        ctime: 0,
        attrs: 0x10,
        is_dir: true,
    };
    
    let vol0_file = RawEntry {
        file_id: 1002,
        file_id_128: None,
        parent_id: 1001,
        name: "existing.txt".into(),
        size: 100,
        mtime: 1,
        ctime: 1,
        attrs: 0,
        is_dir: false,
    };
    
    let store0 = IndexBuilder::new(0, 0, 1, 0)
        .with_unix_volume("dev:65040:/home/box", "/home/box")
        .build_from_raw(vec![vol0_file], vec![vol0_root_dir, vol0_subdir], true)
        .unwrap();
    
    // 卷 1: /workspace
    let vol1_root_dir = RawEntry {
        file_id: 2000,
        file_id_128: None,
        parent_id: 0,
        name: String::new(),
        size: 0,
        mtime: 0,
        ctime: 0,
        attrs: 0x10,
        is_dir: true,
    };
    
    let vol1_file = RawEntry {
        file_id: 2001,
        file_id_128: None,
        parent_id: 2000,
        name: "workspace.txt".into(),
        size: 200,
        mtime: 1,
        ctime: 1,
        attrs: 0,
        is_dir: false,
    };
    
    let store1 = IndexBuilder::new(0, 0, 1, 0)
        .with_unix_volume("dev:65040:/workspace", "/workspace")
        .build_from_raw(vec![vol1_file], vec![vol1_root_dir], true)
        .unwrap();
    
    // 合并两个卷
    let mut merged = findx2_core::merge_index_stores(vec![store0, store1]).unwrap();
    
    // 验证初始状态：卷 0 的文件路径正确
    let initial_vol0_file_idx = 0;  // existing.txt
    let initial_vol0_path = merged.entry_display_path(initial_vol0_file_idx).unwrap();
    assert!(
        initial_vol0_path.starts_with("/home/box"),
        "初始文件应使用正确的卷前缀，实际路径: {}",
        initial_vol0_path
    );
    assert!(
        initial_vol0_path.contains("FindX拼音测试") || initial_vol0_path.contains("findx拼音测试"),
        "路径应包含子目录（大小写不敏感检查），实际路径: {}",
        initial_vol0_path
    );
    
    // 验证卷 1 的文件路径正确
    let initial_vol1_file_idx = merged.entry_count() - 1;  // workspace.txt
    let initial_vol1_path = merged.entry_display_path(initial_vol1_file_idx).unwrap();
    assert!(
        initial_vol1_path.starts_with("/workspace"),
        "卷 1 初始文件应使用 /workspace 前缀，实际路径: {}",
        initial_vol1_path
    );
    
    // *** 关键测试：增量 CreatePending 到卷 0 的子目录 ***
    // 新条目会被追加到 entries 末尾，超出卷 0 的 first_entry_idx 范围
    let new_file_event = ChangeEvent::CreatePending {
        file_id: 1003,
        file_id_128: None,
        parent_id: 1001,  // FindX拼音测试 的 frn
        name: "新建文件.txt".into(),
        attrs: 0,
        is_dir: false,
    };
    
    merged.apply_change_event(&new_file_event).unwrap();
    
    // 验证新条目的路径使用卷 0 的前缀（/home/box）
    let new_idx = merged.entry_count() - 1;
    let new_path = merged.entry_display_path(new_idx).unwrap();
    
    println!("新增文件路径: {}", new_path);
    
    // 核心断言：新文件应使用卷 0 的前缀，而不是卷 1 的
    assert!(
        new_path.starts_with("/home/box"),
        "增量创建的文件应使用父目录所属卷的前缀（/home/box），而不是错误地使用 /workspace\n实际路径: {}",
        new_path
    );
    
    // 验证路径包含正确的子目录名（保留原始大小写）
    assert!(
        new_path.contains("FindX拼音测试") || new_path.contains("findx拼音测试"),
        "路径应包含子目录名，实际路径: {}",
        new_path
    );
    
    // 验证路径包含文件名
    assert!(
        new_path.contains("新建文件.txt"),
        "路径应包含文件名，实际路径: {}",
        new_path
    );
    
    // 额外验证：路径不应包含错误的前缀
    assert!(
        !new_path.contains("/workspace"),
        "新文件路径不应包含 /workspace，实际路径: {}",
        new_path
    );
}

#[test]
fn test_incremental_path_case_sensitivity() {
    // 测试原始大小写保留（Linux 文件系统大小写敏感）
    
    let root_dir = RawEntry {
        file_id: 1000,
        file_id_128: None,
        parent_id: 0,
        name: String::new(),
        size: 0,
        mtime: 0,
        ctime: 0,
        attrs: 0x10,
        is_dir: true,
    };
    
    let mixed_case_dir = RawEntry {
        file_id: 1001,
        file_id_128: None,
        parent_id: 1000,
        name: "MixedCase拼音Test".into(),
        size: 0,
        mtime: 0,
        ctime: 0,
        attrs: 0x10,
        is_dir: true,
    };
    
    let mut store = IndexBuilder::new(0, 0, 1, 0)
        .with_unix_volume("dev:123:/test", "/test")
        .build_from_raw(vec![], vec![root_dir, mixed_case_dir], true)
        .unwrap();
    
    // 增量创建文件
    let create_event = ChangeEvent::CreatePending {
        file_id: 1002,
        file_id_128: None,
        parent_id: 1001,
        name: "NewFile.TXT".into(),
        attrs: 0,
        is_dir: false,
    };
    
    store.apply_change_event(&create_event).unwrap();
    
    let new_idx = store.entry_count() - 1;
    let path = store.entry_display_path(new_idx).unwrap();
    
    println!("大小写敏感路径: {}", path);
    
    // Unix 上应保留原始大小写
    #[cfg(unix)]
    {
        assert!(
            path.contains("MixedCase拼音Test"),
            "Unix 路径应保留原始大小写，实际路径: {}",
            path
        );
        assert!(
            path.contains("NewFile.TXT"),
            "文件名应保留原始大小写，实际路径: {}",
            path
        );
    }
}

#[test]
fn test_delete_after_incremental_create_still_works() {
    // 验证修复后删除功能仍正常
    
    let root_dir = RawEntry {
        file_id: 1000,
        file_id_128: None,
        parent_id: 0,
        name: String::new(),
        size: 0,
        mtime: 0,
        ctime: 0,
        attrs: 0x10,
        is_dir: true,
    };
    
    let mut store = IndexBuilder::new(0, 0, 1, 0)
        .with_unix_volume("dev:123:/test", "/test")
        .build_from_raw(vec![], vec![root_dir], true)
        .unwrap();
    
    // 增量创建
    store.apply_change_event(&ChangeEvent::CreatePending {
        file_id: 1001,
        file_id_128: None,
        parent_id: 1000,
        name: "temp.txt".into(),
        attrs: 0,
        is_dir: false,
    }).unwrap();
    
    let before_delete = store.entry_count();
    assert!(store.frn_to_entry.contains_key(&1001), "文件应在索引中");
    
    // 删除
    store.apply_change_event(&ChangeEvent::Delete {
        file_id: 1001,
    }).unwrap();
    
    // 验证删除生效
    if let Some(&idx) = store.frn_to_entry.get(&1001) {
        assert!(
            store.deleted.contains(idx),
            "删除的文件应被标记为已删除"
        );
    }
    
    assert_eq!(
        store.entry_count(),
        before_delete,
        "条目总数不变，但墓碑计数增加"
    );
}
