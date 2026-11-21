//! 2.4.x 多卷旧索引在新引擎下的加载回归。
//!
//! 旧约定：顶层目录 `parent_idx==0` 哨兵（0 同时是 `dirs[0]` 的真实下标，无法区分），
//! 卷根是建库条目而不在 `dirs` 表；全库唯一「自父根」是 `dirs[0]` 的下标巧合。
//! 2.5.7/2.5.8 的 `rebuild_scoped_identity_maps` 要求「自父根数 == 卷数」，对这种库直接
//! `ambiguous multi-volume root topology` 拒载——服务/安装器全数失败。
//! 现退回 2.4.x 全局表语义（slot 0 + `legacy_global_scope` 直通）。
//!
//! 夹具按本机真实 2.4.10 库（E/D/C 三卷、889 万条目）的字节形态手工构造：
//! - 三个卷的顶层目录全部 `parent_idx==0`；
//! - `dirs[0]` 是「Program Files」且下标为 0（巧合自父根）；
//! - `dirs[1]`「Windows NT」是 `dirs[0]` 的真实子目录，`parent_idx==0` 与哨兵不可区分
//!   （2.4.x 路径重建同样在此截断，本测试钉住该既有语义不回归）；
//! - D: 卷有一个空目录（无任何子条目）。

use findx2_core::index::{hash_ext8, FileEntry};
use findx2_core::persist_v6::{V6Layout, V6Manifest, V6Volume};
use findx2_core::{load_store_auto, v6_load, v6_save_full, SearchEngine};
use std::path::PathBuf;

/// 手工写一个 2.4.x 形态的 legacy v6 平铺目录（manifest.json + 六个分段文件）。
fn write_legacy_flat(root: &std::path::Path) -> V6Manifest {
    std::fs::create_dir_all(root).unwrap();

    // (frn, parent_idx, name)：parent_idx==0 = 卷根下一级（legacy 哨兵）。
    let dirs: &[(u64, u32, &str)] = &[
        (1000, 0, "Program Files"),   // idx 0：E: 顶层，巧合自父根
        (1001, 0, "Windows NT"),      // idx 1：dirs[0] 的真实子目录（碰撞用例）
        (1002, 0, "Windows"),         // idx 2：E: 顶层
        (1003, 2, "System32"),        // idx 3
        (1004, 0, "Docs"),            // idx 4：D: 顶层
        (1005, 4, "sub"),             // idx 5
        (1006, 0, "EmptyDir"),        // idx 6：D: 空目录，无任何子条目
        (1007, 0, "Tools"),           // idx 7：D: 顶层
        (1008, 0, "Users"),           // idx 8：C: 顶层
        (1009, 8, "alice"),           // idx 9
        (1010, 9, "Desktop"),         // idx 10
        (1011, 9, "Downloads"),       // idx 11
    ];

    // (frn, dir_idx, name, is_dir, size)：条目按卷扫描段排列 E[0,7) D[7,14) C[14,21)。
    // 保真：2.4.x 扫描为每个 MFT 记录建条目——每个目录都有自己的 dir-entry
    // （compact 的目录裁决靠「目录条目被墓碑」判死，缺 dir-entry 的目录是漏网孤儿）。
    let entries: &[(u64, u32, &str, bool, u64)] = &[
        (1000, 0, "Program Files", true, 0),   // 0  目录条目（E:）
        (1001, 1, "Windows NT", true, 0),      // 1  目录条目（dirs[0] 的真实子目录，碰撞用例）
        (1002, 2, "Windows", true, 0),         // 2  目录条目
        (1003, 3, "System32", true, 0),        // 3  目录条目
        (2000, 0, "e0.txt", false, 10),        // 4  E:\Program Files
        (2002, 3, "sys.txt", false, 20),      // 5  E:\Windows\System32
        (2003, 2, "win.txt", false, 30),      // 6  E:\Windows
        (1004, 4, "Docs", true, 0),            // 7  目录条目（D:）
        (1005, 5, "sub", true, 0),             // 8  目录条目
        (1006, 6, "EmptyDir", true, 0),        // 9  空目录条目
        (1007, 7, "Tools", true, 0),           // 10 目录条目
        (2004, 5, "d0.txt", false, 40),       // 11 D:\Docs\sub
        (2006, 7, "t0.txt", false, 50),       // 12 D:\Tools
        (2007, 4, "d1.txt", false, 60),       // 13 D:\Docs
        (1008, 8, "Users", true, 0),           // 14 目录条目（C:）
        (1009, 9, "alice", true, 0),           // 15 目录条目
        (1010, 10, "Desktop", true, 0),        // 16 目录条目
        (1011, 11, "Downloads", true, 0),      // 17 目录条目
        (2009, 10, "a0.txt", false, 70),      // 18 C:\Users\alice\Desktop
        (2010, 11, "b0.txt", false, 80),      // 19 C:\Users\alice\Downloads
        (2011, 8, "u0.txt", false, 90),        // 20 C:\Users
    ];

    // names_buf：名字顺序追加，记录每个名字的偏移。
    let mut names_buf: Vec<u8> = Vec::new();
    let mut dir_name_off = Vec::new();
    for (_, _, name) in dirs {
        dir_name_off.push(names_buf.len() as u32);
        names_buf.extend_from_slice(name.as_bytes());
        names_buf.push(0);
    }
    let mut entry_name_off = Vec::new();
    for (_, _, name, _, _) in entries {
        entry_name_off.push(names_buf.len() as u32);
        names_buf.extend_from_slice(name.as_bytes());
        names_buf.push(0);
    }

    // dirs.dat：24B 记录 frn|parent_idx|name_offset|name_len（6B padding）。
    let mut dirs_bytes = Vec::new();
    for (i, (frn, parent, name)) in dirs.iter().enumerate() {
        let mut rec = [0u8; 24];
        rec[0..8].copy_from_slice(&frn.to_le_bytes());
        rec[8..12].copy_from_slice(&parent.to_le_bytes());
        rec[12..16].copy_from_slice(&dir_name_off[i].to_le_bytes());
        rec[16..18].copy_from_slice(&(name.len() as u16).to_le_bytes());
        dirs_bytes.extend_from_slice(&rec);
    }

    // entries.dat：32B v5 紧凑布局。
    let mut entries_bytes = Vec::new();
    let mut frns = Vec::new();
    for (i, (frn, dir_idx, name, is_dir, size)) in entries.iter().enumerate() {
        let mut e = FileEntry::default();
        e.size = *size;
        e.name_offset = entry_name_off[i];
        e.dir_idx = *dir_idx;
        let eh = hash_ext8(name) as u32;
        e.attrs = eh << 8;
        if *is_dir {
            e.attrs |= FileEntry::ATTR_IS_DIR;
        }
        e.n_len = name.len() as u16;
        frns.push(*frn);
        entries_bytes.extend_from_slice(&e_to_disk_bytes(&e));
    }

    // frns.dat / ranges.dat / names.dat / dirpaths.dat
    let mut frns_bytes = Vec::new();
    for f in &frns {
        frns_bytes.extend_from_slice(&f.to_le_bytes());
    }
    let ranges_bytes = vec![0u8; dirs.len() * 8];
    std::fs::write(root.join("dirs.dat"), &dirs_bytes).unwrap();
    std::fs::write(root.join("entries.dat"), &entries_bytes).unwrap();
    std::fs::write(root.join("frns.dat"), &frns_bytes).unwrap();
    std::fs::write(root.join("ranges.dat"), &ranges_bytes).unwrap();
    std::fs::write(root.join("names.dat"), &names_buf).unwrap();
    std::fs::write(root.join("dirpaths.dat"), b"").unwrap();

    let manifest = V6Manifest {
        version: 6,
        generation: 1,
        entry_count: entries.len(),
        dir_count: dirs.len(),
        names_len: names_buf.len(),
        dir_paths_len: 0,
        metadata_ready: true,
        wal_seq: 0,
        volumes: vec![
            V6Volume {
                letter: b'E',
                serial: 1,
                journal_id: 11,
                last_usn: 0,
                first_entry_idx: 0,
                root_prefix: "E:\\".into(),
                volume_id: "E".into(),
            },
            V6Volume {
                letter: b'D',
                serial: 2,
                journal_id: 22,
                last_usn: 0,
                first_entry_idx: 7,
                root_prefix: "D:\\".into(),
                volume_id: "D".into(),
            },
            V6Volume {
                letter: b'C',
                serial: 3,
                journal_id: 33,
                last_usn: 0,
                first_entry_idx: 14,
                root_prefix: "C:\\".into(),
                volume_id: "C".into(),
            },
        ],
    };
    std::fs::write(
        root.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    manifest
}

/// v5 磁盘 32B 布局：size|mtime|ctime|name_offset|dir_idx|attrs|n_len|_pad。
fn e_to_disk_bytes(e: &FileEntry) -> [u8; 32] {
    let mut b = [0u8; 32];
    b[0..8].copy_from_slice(&e.size.to_le_bytes());
    b[8..12].copy_from_slice(&e.mtime.to_le_bytes());
    b[12..16].copy_from_slice(&e.ctime.to_le_bytes());
    b[16..20].copy_from_slice(&e.name_offset.to_le_bytes());
    b[20..24].copy_from_slice(&e.dir_idx.to_le_bytes());
    b[24..28].copy_from_slice(&e.attrs.to_le_bytes());
    b[28..30].copy_from_slice(&e.n_len.to_le_bytes());
    b
}

fn tmp_index(name: &str) -> (PathBuf, V6Layout) {
    let mut p = std::env::temp_dir();
    p.push(format!("findx2_legacy_{name}_{}.bin", std::process::id()));
    let layout = V6Layout::for_index(&p);
    let _ = std::fs::remove_dir_all(&layout.dir);
    (p, layout)
}

/// fixture 里的 FRN 常量（与 entries 表对齐）。
const FRN_E_FILE: u64 = 2000; // E:\Program Files\e0.txt
const FRN_D_FILE: u64 = 2004; // D:\Docs\sub\d0.txt
const FRN_C_FILE: u64 = 2009; // C:\Users\alice\Desktop\a0.txt
const FRN_C_DIR: u64 = 1009; // C:\Users\alice

#[test]
fn legacy_multivolume_flat_v6_loads_and_resolves() {
    let (p, layout) = tmp_index("flat");
    write_legacy_flat(&layout.dir);

    // 2.5.7/2.5.8 在这里直接 Err("ambiguous multi-volume root topology")。
    let store = v6_load(&layout).unwrap();
    assert_eq!(store.entry_count(), 21);
    assert_eq!(store.dirs.len(), 12);
    assert!(store.legacy_global_scope, "legacy 库必须置 legacy_global_scope");

    // USN 身份解析：entry_on_volume / dir_on_volume 走 slot-0 直通，三个卷都能解析。
    let e_idx = store.entry_on_volume(b'E', FRN_E_FILE).unwrap();
    assert_eq!(store.name_str(&store.entries[e_idx as usize]).unwrap(), "e0.txt");
    let d_idx = store.entry_on_volume(b'D', FRN_D_FILE).unwrap();
    assert_eq!(store.name_str(&store.entries[d_idx as usize]).unwrap(), "d0.txt");
    let c_idx = store.entry_on_volume(b'C', FRN_C_FILE).unwrap();
    assert_eq!(store.name_str(&store.entries[c_idx as usize]).unwrap(), "a0.txt");
    let c_dir = store.dir_on_volume(b'C', FRN_C_DIR).unwrap();
    assert_eq!(store.dirs[c_dir as usize].name_len, "alice".len() as u16);
    // 错卷查找不炸（2.4.x 全局表语义下可能命中同 FRN 的其他卷条目，这里 FRN 全局唯一，
    // 只断言不 panic 且返回类型正确）。
    let _ = store.entry_on_volume(b'D', FRN_E_FILE);

    // 显示路径：卷盘符按扫描段判定（与 2.4.x 一致）。
    let _ = p;
    let p_e = store.entry_full_path_lower(e_idx as usize);
    assert!(p_e.starts_with("e:\\program files\\"), "got {p_e}");
    let p_d = store.entry_full_path_lower(d_idx as usize);
    assert!(p_d.starts_with("d:\\docs\\sub\\"), "got {p_d}");
    let p_c = store.entry_full_path_lower(c_idx as usize);
    assert!(p_c.starts_with("c:\\users\\alice\\desktop\\"), "got {p_c}");
}

#[test]
fn legacy_multivolume_survives_v5_and_gen_roundtrips() {
    let (p, layout) = tmp_index("roundtrip");
    write_legacy_flat(&layout.dir);
    let store = v6_load(&layout).unwrap();

    // v5 路径：save_index_bin → load_index_bin 同样要能载入（persist.rs 的 rebuild 调用）。
    findx2_core::save_index_bin(&p, &store).unwrap();
    let v5 = findx2_core::load_index_bin(&p).unwrap();
    assert!(v5.legacy_global_scope);
    assert_eq!(v5.entry_count(), 21);
    let d_idx = v5.entry_on_volume(b'D', FRN_D_FILE).unwrap();
    assert_eq!(v5.name_str(&v5.entries[d_idx as usize]).unwrap(), "d0.txt");

    // gen 路径：v6_save_full 拷贝旧布局进 gen/N，重新加载后仍走 legacy 分支。
    v6_save_full(&layout, &v5).unwrap();
    let gen = v6_load(&layout).unwrap();
    assert!(gen.legacy_global_scope);
    assert_eq!(gen.entry_count(), 21);
    let c_idx = gen.entry_on_volume(b'C', FRN_C_FILE).unwrap();
    assert_eq!(gen.name_str(&gen.entries[c_idx as usize]).unwrap(), "a0.txt");
    // load_store_auto（service/CLI 入口）同样可用。
    let auto = load_store_auto(&p).unwrap();
    assert_eq!(auto.entry_count(), 21);
}

#[test]
fn legacy_multivolume_sequential_rebuild_upgrades_topology_for_usage() {
    // 模拟服务「legacy 拓扑自动升级」（run.rs 启动线程）：按卷顺序执行
    // remove_volume_entries + merge_index_stores（service::rebuild_volume 的两个
    // 核心原语，与服务代码同路径），旧拓扑库应被逐步换为新格式——每卷一个自父根，
    // usage（目录占用树）可构建，重载后走严格逐卷 scope。
    let (_p, layout) = tmp_index("upgrade");
    write_legacy_flat(&layout.dir);
    let mut merged = v6_load(&layout).unwrap();
    assert!(merged.legacy_global_scope);
    assert_eq!(merged.volumes.len(), 3);

    // fresh 卷扫描产物：IndexBuilder + build_from_raw（rebuild_volume 同路径，
    // metadata_ready=false 与 fast 首遍一致）。
    let fresh = |letter: u8| {
        let dirs = vec![findx2_core::platform::RawEntry {
            file_id: letter as u64 * 1000 + 1,
            file_id_128: None,
            parent_id: 0,
            name: format!("vol-{}", letter as char).into(),
            size: 0,
            mtime: 0,
            ctime: 0,
            attrs: 0x10,
            is_dir: true,
        }];
        let files = vec![findx2_core::platform::RawEntry {
            file_id: letter as u64 * 1000 + 2,
            file_id_128: None,
            parent_id: letter as u64 * 1000 + 1,
            name: format!("{}-data.txt", letter as char).into(),
            size: 42,
            mtime: 0,
            ctime: 0,
            attrs: 0,
            is_dir: false,
        }];
        findx2_core::IndexBuilder::new(letter, letter as u32, letter as u64, 1000)
            .build_from_raw(files, dirs, false)
            .unwrap()
    };

    for letter in [b'E', b'D', b'C'] {
        // 物理摘除旧卷：compact 可达性裁决对 legacy parent_idx==0 哨兵视为链完整，
        // 其他卷目录不受牵连；每卷都有 dir-entry（fixture 保真点）保证整卷摘净。
        // 自父根（新格式）由 directory_retention 视为链终结，先合并进来的卷不受牵连。
        merged.remove_volume_entries(letter as char);
        merged = findx2_core::index::merge_index_stores(vec![merged, fresh(letter)]).unwrap();
        assert_eq!(merged.volumes.len(), 3, "中间态卷数恒为 3");
    }

    // 全部完成后：自父根数 == 卷数（新格式），旧 legacy 目录全部随卷摘除。
    let roots = merged
        .dirs
        .iter()
        .enumerate()
        .filter(|(i, d)| d.parent_idx as usize == *i)
        .count();
    assert_eq!(roots, merged.volumes.len(), "每卷应有一个自父根");
    assert_eq!(merged.dirs.len(), 3, "旧 legacy 目录应全部随卷摘除");
    assert_eq!(merged.entry_count(), 6, "三卷 × (1 目录条目 + 1 文件)");

    // usage（目录占用树）可构建：服务启动 enable_usage 的同一入口。
    let engine = SearchEngine::new(merged);
    assert!(!engine.usage_available());
    engine.enable_usage(16 * 1024 * 1024).unwrap();
    assert!(engine.usage_available(), "新格式拓扑下占用树必须可用");
    let page = engine
        .usage_page("E:\\", None, None, 64 * 1024)
        .expect("usage page must resolve the volume root");
    assert!(
        page.children.iter().any(|c| c.name == "E-data.txt"),
        "卷根下应列出该卷文件，got {:?}",
        page.children.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
    );

    // 落盘 → 重载：严格逐卷 scope，legacy 标志不再置位，usage 复验可用。
    v6_save_full(&layout, &engine.index_store().clone()).unwrap();
    let reloaded = v6_load(&layout).unwrap();
    assert!(!reloaded.legacy_global_scope, "升级后重载不得再走 legacy 全局表");
    let engine2 = SearchEngine::new(reloaded);
    engine2.enable_usage(16 * 1024 * 1024).unwrap();
    assert!(engine2.usage_available());
}

#[test]
fn new_format_multivolume_keeps_strict_scoped_maps() {
    // 反向钉子：新引擎自己合并出的多卷库（merge +dir_base 产生每卷自父根）必须仍走
    // 严格逐卷 scope 路径，不能被 legacy 分支误吞。
    let (p, layout) = tmp_index("newformat");
    let mk = |letter: u8| {
        let dirs = vec![findx2_core::platform::RawEntry {
            file_id: letter as u64 * 10 + 1,
            file_id_128: None,
            parent_id: 0,
            name: "rootdir".into(),
            size: 0,
            mtime: 0,
            ctime: 0,
            attrs: 0x10,
            is_dir: true,
        }];
        let files = vec![findx2_core::platform::RawEntry {
            file_id: letter as u64 * 10 + 2,
            file_id_128: None,
            parent_id: letter as u64 * 10 + 1,
            name: format!("{}-file.txt", letter as char).into(),
            size: 5,
            mtime: 1,
            ctime: 1,
            attrs: 0,
            is_dir: false,
        }];
        findx2_core::IndexBuilder::new(letter, letter as u32, letter as u64, 1)
            .build_from_raw(files, dirs, true)
            .unwrap()
    };
    let merged = findx2_core::index::merge_index_stores(vec![mk(b'E'), mk(b'D'), mk(b'C')]).unwrap();
    v6_save_full(&layout, &merged).unwrap();
    let loaded = v6_load(&layout).unwrap();
    assert!(!loaded.legacy_global_scope, "新格式不得走 legacy 全局表");
    // 逐卷 scope 解析。
    let e = loaded.entry_on_volume(b'E', (b'E' as u64) * 10 + 2).unwrap();
    assert_eq!(loaded.name_str(&loaded.entries[e as usize]).unwrap(), "E-file.txt");
    let d = loaded.entry_on_volume(b'D', (b'D' as u64) * 10 + 2).unwrap();
    assert_eq!(loaded.name_str(&loaded.entries[d as usize]).unwrap(), "D-file.txt");
    assert!(loaded.entry_on_volume(b'D', (b'E' as u64) * 10 + 2).is_none());
    let _ = p;
}
