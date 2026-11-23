use std::path::Path;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStamp {
    pub volume: u64,
    pub id: u128,
    pub size: u64,
    pub modified: u64,
    pub directory: bool,
}
pub fn capture(path: &Path) -> Result<FileStamp, String> {
    #[cfg(windows)]
    {
        use std::os::windows::{
            fs::{MetadataExt, OpenOptionsExt},
            io::AsRawHandle,
        };
        use windows::Win32::{
            Foundation::HANDLE,
            Storage::FileSystem::{FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx},
        };
        let f = std::fs::OpenOptions::new()
            .access_mode(0x80)
            .share_mode(7)
            .custom_flags(0x02000000)
            .open(path)
            .map_err(|e| e.to_string())?;
        let m = f.metadata().map_err(|e| e.to_string())?;
        let mut i = FILE_ID_INFO::default();
        unsafe {
            GetFileInformationByHandleEx(
                HANDLE(f.as_raw_handle()),
                FileIdInfo,
                &mut i as *mut _ as *mut _,
                std::mem::size_of_val(&i) as u32,
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(FileStamp {
            volume: i.VolumeSerialNumber,
            id: u128::from_le_bytes(i.FileId.Identifier),
            size: m.len(),
            modified: m.last_write_time(),
            directory: m.is_dir(),
        })
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(path).map_err(|e| e.to_string())?;
        Ok(FileStamp {
            volume: m.dev(),
            id: m.ino() as u128,
            size: m.len(),
            modified: m.mtime() as u64,
            directory: m.is_dir(),
        })
    }
}
pub fn revalidate(path: &Path, expected: FileStamp) -> Result<(), String> {
    match capture(path) {
        Ok(actual) if actual == expected => Ok(()),
        Ok(actual) => {
            // Opt-in isolated DEV evidence only; never relax equality to clear
            // a warning or silently refresh the operation's trusted baseline.
            if std::env::var_os("FINDX_NATIVE_IDENTITY_DIAGNOSTICS").is_some() {
                eprintln!(
                    "[file-identity] mismatch path={path:?} expected={expected:?} actual={actual:?}"
                );
            }
            Err("文件身份或内容已变化；已阻止操作，请刷新后重新选择。".into())
        }
        Err(_) => Err("文件不可访问：可能已删除、权限不足或磁盘已断开。".into()),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aliases_and_replacements_have_correct_identity() {
        let d = std::env::temp_dir().join(format!(
            "findx-id-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&d).unwrap();
        let a = d.join("a.txt");
        let b = d.join("alias.txt");
        std::fs::write(&a, b"fixture").unwrap();
        std::fs::hard_link(&a, &b).unwrap();
        let stamp = capture(&a).unwrap();
        assert_eq!(stamp, capture(&b).unwrap());
        // Preview/read/focus activity does not change a trusted identity.
        for _ in 0..8 {
            assert_eq!(std::fs::read(&a).unwrap(), b"fixture");
            revalidate(&a, stamp).unwrap();
        }
        std::fs::remove_file(&a).unwrap();
        std::fs::write(&a, b"fixture").unwrap();
        assert!(revalidate(&a, stamp).is_err());
        // A changed file must still be rejected; a new explicit selection
        // can capture a baseline, but an existing operation never refreshes it.
        let replacement = capture(&a).unwrap();
        std::fs::write(&a, b"changed length").unwrap();
        assert!(revalidate(&a, replacement).is_err());
        std::fs::remove_file(&b).unwrap();
        assert!(revalidate(&b, stamp).is_err());
        std::fs::remove_dir_all(d).unwrap();
    }
}
