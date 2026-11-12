//! Public Windows API allocation/128-bit identity fixture.
//! Validates the production batch walker against the previous exact per-path implementation.
use super::*;
use std::io::Write as _;
use std::{
    collections::HashMap,
    os::windows::{ffi::OsStringExt, fs::OpenOptionsExt, io::AsRawHandle},
};
use windows::Win32::{
    Foundation::HANDLE,
    Storage::FileSystem::{
        FILE_ID_EXTD_DIR_INFO, FileIdExtdDirectoryInfo, FileIdExtdDirectoryRestartInfo,
        GetFileInformationByHandleEx,
    },
    System::IO::DeviceIoControl,
};
#[test]
fn bounded_batch_api_fixture_preserves_full_file_ids_and_measures_allocation_differences() {
    let root = std::env::temp_dir().join(format!(
        "findx-batch-probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(root.clone());
    for i in 0..512 {
        std::fs::write(
            root.join(format!("ordinary-{i:04}.bin")),
            vec![i as u8; 8192],
        )
        .unwrap();
    }
    std::fs::hard_link(root.join("ordinary-0000.bin"), root.join("alias.bin")).unwrap();
    let sparse = std::fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(root.join("sparse.bin"))
        .unwrap();
    let mut returned = 0;
    unsafe {
        DeviceIoControl(
            HANDLE(sparse.as_raw_handle()),
            0x900C4,
            None,
            0,
            None,
            0,
            Some(&mut returned),
            None,
        )
        .unwrap();
    }
    sparse.set_len(16 * 1024 * 1024).unwrap();
    drop(sparse);
    std::fs::write(root.join("resident.bin"), [7u8; 128]).unwrap();
    let mut compressed = std::fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(root.join("compressed.bin"))
        .unwrap();
    compressed.write_all(&vec![0x41u8; 1024 * 1024]).unwrap();
    compressed.sync_all().unwrap();
    let format = 2u16;
    let compression = unsafe {
        DeviceIoControl(
            HANDLE(compressed.as_raw_handle()),
            0x9C040,
            Some(&format as *const _ as *const _),
            2,
            None,
            0,
            Some(&mut returned),
            None,
        )
    };
    compressed.sync_all().unwrap();
    drop(compressed);
    let directory = std::fs::OpenOptions::new()
        .access_mode(0x81)
        .share_mode(7)
        .custom_flags(0x02200000)
        .open(&root)
        .unwrap();
    let mut buffer = vec![0u64; 8192];
    let capacity = buffer.len() * 8;
    let header = std::mem::offset_of!(FILE_ID_EXTD_DIR_INFO, FileName);
    let mut first = true;
    let mut batched = HashMap::new();
    let mut calls = 0;
    let started = Instant::now();
    loop {
        let class = if first {
            FileIdExtdDirectoryRestartInfo
        } else {
            FileIdExtdDirectoryInfo
        };
        first = false;
        calls += 1;
        match unsafe {
            GetFileInformationByHandleEx(
                HANDLE(directory.as_raw_handle()),
                class,
                buffer.as_mut_ptr().cast(),
                capacity as u32,
            )
        } {
            Ok(()) => {}
            Err(e) if e.code().0 as u32 == 0x80070012 => break,
            Err(e) => panic!("bounded fixture enumeration failed: {e}"),
        }
        let mut offset = 0;
        loop {
            assert!(offset + header + 2 <= capacity);
            let ptr = unsafe { buffer.as_ptr().cast::<u8>().add(offset) };
            let entry = unsafe { ptr.cast::<FILE_ID_EXTD_DIR_INFO>().read_unaligned() };
            let length = entry.FileNameLength as usize;
            assert_eq!(length % 2, 0);
            assert!(offset + header + length <= capacity);
            let name = std::ffi::OsString::from_wide(unsafe {
                std::slice::from_raw_parts(ptr.add(header).cast::<u16>(), length / 2)
            });
            if name != "." && name != ".." {
                batched.insert(
                    name,
                    (
                        entry.AllocationSize.max(0) as u64,
                        u128::from_le_bytes(entry.FileId.Identifier),
                    ),
                );
            }
            if entry.NextEntryOffset == 0 {
                break;
            }
            let next = entry.NextEntryOffset as usize;
            assert!(next >= header + length && offset + next < capacity);
            offset += next;
        }
    }
    let batch_elapsed = started.elapsed();
    let started = Instant::now();
    let mut traditional = HashMap::new();
    for path in std::fs::read_dir(&root).unwrap() {
        let path = path.unwrap();
        traditional.insert(path.file_name(), info(&path.path()).unwrap());
    }
    let traditional_elapsed = started.elapsed();
    assert_eq!(batched.len(), 516);
    assert_eq!(batched.len(), traditional.len());
    for (name, (allocation, id)) in &batched {
        let previous = &traditional[name];
        assert_eq!(*id, previous.identity.1, "full 128-bit identity mismatch");
        if name.to_string_lossy().starts_with("ordinary-") || name == "alias.bin" {
            assert_eq!(
                *allocation, previous.allocated,
                "ordinary allocated size mismatch"
            );
        }
    }
    assert_eq!(
        batched[std::ffi::OsStr::new("ordinary-0000.bin")].1,
        batched[std::ffi::OsStr::new("alias.bin")].1
    );
    println!(
        "PUBLIC FIXTURE ONLY: {} paths, batch {:?} / {} directory calls; existing per-path metadata+identity+actual allocation {:?}; not a whole-drive benchmark",
        batched.len(),
        batch_elapsed,
        calls,
        traditional_elapsed
    );
    for name in ["resident.bin", "sparse.bin", "compressed.bin"] {
        let key = std::ffi::OsStr::new(name);
        println!(
            "{}: enumeration allocation={} actual GetCompressedFileSize={} compression_supported={}",
            name,
            batched[key].0,
            traditional[key].allocated,
            compression.is_ok()
        );
    }
    let progress = Arc::new(Progress::default());
    let production = super::scan(
        &root,
        Arc::new(AtomicBool::new(false)),
        progress.clone(),
        Limits::default(),
    )
    .unwrap();
    assert!(!production.partial && production.unknown == 0);
    assert_eq!(production.hardlinks, 1);
    let mut seen = std::collections::HashSet::new();
    let expected = traditional
        .values()
        .filter(|i| seen.insert(i.identity))
        .map(|i| i.allocated)
        .sum::<u64>()
        + info(&root).unwrap().allocated;
    assert_eq!(
        production.nodes[0].allocated, expected,
        "production allocation must match actual refined/deduplicated allocation, not logical size"
    );
    for name in ["resident.bin", "sparse.bin", "compressed.bin"] {
        let node = production
            .nodes
            .iter()
            .find(|n| n.path.file_name().is_some_and(|x| x == name))
            .unwrap();
        assert!(!node.uncertain);
        assert_eq!(
            node.allocated,
            traditional[std::ffi::OsStr::new(name)].allocated
        );
    }
    assert!(progress.metadata_calls.load(Ordering::Relaxed) < 16);
    println!(
        "production mixed fixture: hardlinks={} unknown={} batch_calls={} metadata_queries={}",
        production.hardlinks,
        production.unknown,
        progress.directory_calls.load(Ordering::Relaxed),
        progress.metadata_calls.load(Ordering::Relaxed)
    );
}
