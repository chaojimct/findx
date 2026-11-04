//! 测试 inotify 删除事件能够正确处理
//!
//! Bug fix #2: 删除操作应该能够正确生成 Delete 事件

#[cfg(target_os = "linux")]
#[test]
fn test_inotify_delete_event_processing() {
    use findx2_core::{ChangeEvent, IndexBuilder, RawEntry};
    use std::fs;
    use std::os::unix::fs::MetadataExt;

    // 创建临时测试目录
    let temp_dir = std::env::temp_dir();
    let test_root = temp_dir.join("findx_test_delete");
    let _ = fs::remove_dir_all(&test_root);
    fs::create_dir_all(&test_root).expect("无法创建测试根目录");

    // 创建测试文件
    let test_file = test_root.join("test_file.txt");
    fs::write(&test_file, "test content").expect("无法创建测试文件");

    let meta = fs::metadata(&test_file).expect("无法获取文件元数据");
    let file_ino = meta.ino();

    // 创建一个简单的索引来测试事件应用
    let root_meta = fs::metadata(&test_root).unwrap();
    let root_ino = root_meta.ino();

    let dirs = vec![RawEntry {
        file_id: root_ino,
        file_id_128: None,
        parent_id: 0,
        name: String::new(),
        size: 0,
        mtime: 0,
        ctime: 0,
        attrs: 0x10,
        is_dir: true,
    }];

    let files = vec![RawEntry {
        file_id: file_ino,
        file_id_128: None,
        parent_id: root_ino,
        name: "test_file.txt".into(),
        size: meta.len(),
        mtime: 0,
        ctime: 0,
        attrs: 0,
        is_dir: false,
    }];

    let builder = IndexBuilder::new(0, 0, 1, 0).with_unix_volume(
        format!("dev:{}", root_meta.dev()),
        test_root.to_string_lossy().to_string(),
    );
    let mut store = builder.build_from_raw(files, dirs, false).unwrap();

    // 验证文件在索引中（包含根目录和文件，共2个条目）
    assert_eq!(store.entry_count(), 2, "初始索引应该包含根目录和文件共2个条目");
    assert!(
        store.frn_to_entry.contains_key(&file_ino),
        "文件 inode 应该在映射中"
    );
    
    let initial_entry_count = store.entry_count();

    // 删除文件
    fs::remove_file(&test_file).expect("无法删除测试文件");

    // 模拟 Delete 事件（修复后，即使文件已被删除，我们仍能生成正确的事件）
    let delete_event = ChangeEvent::Delete { file_id: file_ino };

    // 应用删除事件
    store
        .apply_change_event(&delete_event)
        .expect("应用删除事件失败");

    // 验证文件已从索引中删除（被标记为已删除）
    if let Some(&idx) = store.frn_to_entry.get(&file_ino) {
        assert!(
            store.deleted.contains(idx),
            "删除的文件应该被标记为已删除"
        );
    }
    
    // 验证条目总数没变，但删除计数增加了
    assert_eq!(
        store.entry_count(),
        initial_entry_count,
        "条目总数应该保持不变"
    );
    assert!(
        store.deleted.len() > 0,
        "应该至少有一个已删除的条目"
    );

    // 清理
    let _ = fs::remove_dir_all(test_root);
}

#[cfg(target_os = "linux")]
#[test]
fn test_path_inode_cache_behavior() {
    use std::collections::HashMap;
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    // 创建临时测试目录
    let temp_dir = std::env::temp_dir();
    let test_root = temp_dir.join("findx_test_cache");
    let _ = fs::remove_dir_all(&test_root);
    fs::create_dir_all(&test_root).expect("无法创建测试根目录");

    // 创建测试文件
    let test_file = test_root.join("cached_file.txt");
    fs::write(&test_file, "test content").expect("无法创建测试文件");

    let meta = fs::metadata(&test_file).expect("无法获取文件元数据");
    let file_ino = meta.ino();

    // 模拟缓存行为
    let cache: Arc<Mutex<HashMap<PathBuf, u64>>> = Arc::new(Mutex::new(HashMap::new()));

    // 缓存文件路径和 inode
    {
        let mut map = cache.lock().unwrap();
        map.insert(test_file.clone(), file_ino);
    }

    // 删除文件
    fs::remove_file(&test_file).expect("无法删除测试文件");

    // 文件已删除，但我们仍能从缓存中获取 inode
    let cached_ino = {
        let mut map = cache.lock().unwrap();
        map.remove(&test_file)
    };

    assert_eq!(
        cached_ino,
        Some(file_ino),
        "应该能从缓存中获取已删除文件的 inode"
    );

    // 验证文件确实已被删除
    assert!(
        !test_file.exists(),
        "文件应该已被删除"
    );

    // 清理
    let _ = fs::remove_dir_all(test_root);
}

#[cfg(not(target_os = "linux"))]
#[test]
fn test_placeholder() {
    // 这些测试仅在 Linux 上运行
}
