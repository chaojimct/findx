//! 测试同设备上多个根路径的 volume_id 唯一性
//!
//! Bug fix #1: 同设备多根路径不应产生相同的 volume_id

#[cfg(target_os = "linux")]
#[test]
fn test_volume_id_uniqueness_for_different_roots() {
    use findx2_linux::volume_id_for_path;
    use std::fs;
    use std::os::unix::fs::MetadataExt;

    // 创建两个临时目录（它们会在同一个设备上）
    let temp_dir = std::env::temp_dir();
    let root1 = temp_dir.join("findx_test_root1");
    let root2 = temp_dir.join("findx_test_root2");

    let _ = fs::create_dir_all(&root1);
    let _ = fs::create_dir_all(&root2);

    // 验证它们确实在同一个设备上
    let meta1 = fs::metadata(&root1).expect("无法获取 root1 的元数据");
    let meta2 = fs::metadata(&root2).expect("无法获取 root2 的元数据");
    assert_eq!(meta1.dev(), meta2.dev(), "测试要求两个根在同一设备上");

    // 获取它们的 volume_id
    let vol_id1 = volume_id_for_path(root1.to_str().unwrap());
    let vol_id2 = volume_id_for_path(root2.to_str().unwrap());

    // volume_id 应该不同（修复前会相同）
    assert_ne!(
        vol_id1, vol_id2,
        "同设备上的不同路径应该产生不同的 volume_id\nvol_id1={}\nvol_id2={}",
        vol_id1, vol_id2
    );

    // volume_id 应该包含路径信息
    assert!(
        vol_id1.contains("root1") || vol_id1.contains(&root1.to_string_lossy().to_string()),
        "volume_id 应该包含路径信息: {}",
        vol_id1
    );
    assert!(
        vol_id2.contains("root2") || vol_id2.contains(&root2.to_string_lossy().to_string()),
        "volume_id 应该包含路径信息: {}",
        vol_id2
    );

    // 清理
    let _ = fs::remove_dir_all(root1);
    let _ = fs::remove_dir_all(root2);
}

#[cfg(target_os = "linux")]
#[test]
fn test_volume_id_stability() {
    use findx2_linux::volume_id_for_path;
    use std::fs;

    // 创建临时目录
    let temp_dir = std::env::temp_dir();
    let test_root = temp_dir.join("findx_test_stability");
    let _ = fs::create_dir_all(&test_root);

    // 多次调用应该返回相同的 volume_id
    let vol_id1 = volume_id_for_path(test_root.to_str().unwrap());
    let vol_id2 = volume_id_for_path(test_root.to_str().unwrap());

    assert_eq!(
        vol_id1, vol_id2,
        "同一路径的 volume_id 应该保持稳定"
    );

    // 清理
    let _ = fs::remove_dir_all(test_root);
}

#[cfg(not(target_os = "linux"))]
#[test]
fn test_placeholder() {
    // 这些测试仅在 Linux 上运行
}
