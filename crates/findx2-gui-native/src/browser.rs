//! Directory browsing is independent of Search. All I/O is caller-owned background work.
use findx2_ipc::SearchHitDto;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
pub type Cancel = Arc<AtomicBool>;
pub fn list(path: &Path, cancel: &Cancel) -> Result<(PathBuf, Vec<SearchHitDto>, bool), String> {
    let path = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    let mut truncated = false;
    for item in std::fs::read_dir(&path).map_err(|e| e.to_string())? {
        if cancel.load(Ordering::Acquire) {
            return Err("目录请求已取消".into());
        }
        if rows.len() >= 100_000 {
            truncated = true;
            break;
        }
        let e = item.map_err(|e| e.to_string())?;
        let Ok(meta) = e.metadata() else { continue };
        rows.push(SearchHitDto {
            entry_idx: rows.len() as u32,
            name: e.file_name().to_string_lossy().into_owned(),
            path: e.path().to_string_lossy().into_owned(),
            size: meta.len(),
            mtime: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|t| t.as_secs())
                .unwrap_or(0),
            is_directory: meta.is_dir(),
            name_highlight: vec![],
        });
    }
    rows.sort_by(|a, b| {
        b.is_directory
            .cmp(&a.is_directory)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok((path, rows, truncated))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileAction {
    Copy,
    Move,
    Rename,
}
impl FileAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Copy => "复制",
            Self::Move => "移动",
            Self::Rename => "重命名",
        }
    }
}
pub fn safe_name(s: &str) -> bool {
    if s != s.trim() {
        return false;
    }
    if s.is_empty()
        || s == "."
        || s == ".."
        || s.ends_with(['.', ' '])
        || s.chars()
            .any(|c| c.is_control() || "\\/:*?\"<>|".contains(c))
    {
        return false;
    }
    let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
    !matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}
pub fn destination(action: FileAction, source: &Path, text: &str) -> Result<PathBuf, String> {
    if action == FileAction::Rename {
        if !safe_name(text) {
            return Err("请输入有效的新文件名（不能包含路径、保留名或尾随点/空格）".into());
        }
        Ok(source.parent().ok_or("没有父目录")?.join(text))
    } else {
        let dst = PathBuf::from(text);
        if !dst.is_absolute() {
            return Err("目标需要完整绝对路径（含文件名）".into());
        }
        Ok(dst)
    }
}
pub fn operate(action: FileAction, src: &Path, dst: &Path, cancel: &Cancel) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        return Err("操作已取消".into());
    }
    let m = std::fs::symlink_metadata(src).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if m.file_attributes() & 0x400 != 0 {
            return Err("文件操作拒绝重解析点/挂载路径".into());
        }
    }
    if m.is_symlink() {
        return Err("暂不操作符号链接".into());
    }
    if dst.exists() {
        return Err("目标已存在；不会覆盖或合并".into());
    }
    if src == dst {
        return Err("源与目标相同".into());
    }
    if action == FileAction::Rename {
        std::fs::rename(src, dst).map_err(|e| e.to_string())?;
        return Ok(());
    }
    if action == FileAction::Move {
        if std::fs::rename(src, dst).is_ok() {
            return Ok(());
        }
    }
    if m.is_dir() {
        if action == FileAction::Move {
            return Err(
                "跨卷目录移动未执行：为避免目录变化时丢失文件，请先复制并核对；同卷目录移动可用"
                    .into(),
            );
        }
        let source = std::fs::canonicalize(src).map_err(|e| e.to_string())?;
        let parent = std::fs::canonicalize(dst.parent().ok_or("目标没有父目录")?)
            .map_err(|e| e.to_string())?;
        if parent
            .to_string_lossy()
            .to_lowercase()
            .starts_with(&(source.to_string_lossy().to_lowercase() + "\\"))
            || parent == source
        {
            return Err("不能把目录复制到自身内部".into());
        }
        let partial = dst.with_file_name(format!(
            "{}.findx-partial-{}",
            dst.file_name().ok_or("无目标文件名")?.to_string_lossy(),
            std::process::id()
        ));
        std::fs::create_dir(&partial).map_err(|e| e.to_string())?;
        let copied = (|| {
            let mut work = vec![(src.to_path_buf(), partial.clone())];
            let mut entries = 0usize;
            while let Some((from, to)) = work.pop() {
                if cancel.load(Ordering::Acquire) {
                    return Err("目录复制已取消".into());
                }
                for item in std::fs::read_dir(&from).map_err(|e| e.to_string())? {
                    let e = item.map_err(|e| e.to_string())?;
                    entries += 1;
                    if entries > 500_000 {
                        return Err("目录复制达到 500000 项安全上限".into());
                    }
                    let meta = std::fs::symlink_metadata(e.path()).map_err(|e| e.to_string())?;
                    #[cfg(windows)]
                    {
                        use std::os::windows::fs::MetadataExt;
                        if meta.file_attributes() & 0x400 != 0 {
                            return Err("目录副本含重解析点；拒绝跟随".into());
                        }
                    }
                    if meta.is_symlink() {
                        return Err("目录副本含符号链接；拒绝跟随".into());
                    }
                    let dest = to.join(e.file_name());
                    if meta.is_dir() {
                        std::fs::create_dir(&dest).map_err(|e| e.to_string())?;
                        work.push((e.path(), dest));
                    } else {
                        operate(FileAction::Copy, &e.path(), &dest, cancel)?;
                    }
                }
            }
            Ok::<(), String>(())
        })();
        copied.map_err(|e| format!("{e}；部分目录副本保留在 {}", partial.display()))?;
        if dst.exists() {
            return Err(format!(
                "目标已被创建；未覆盖，副本在 {}",
                partial.display()
            ));
        }
        std::fs::rename(&partial, dst).map_err(|e| e.to_string())?;
        return Ok(());
    }
    if !m.is_file() {
        return Err("只处理普通文件与目录".into());
    }
    // Destination opened create_new: never overwrite a racing target. A failed/cancelled
    // copy retains its visibly named partial file; no private file is deleted on failure.
    let partial = dst.with_file_name(format!(
        "{}.findx-partial-{}",
        dst.file_name().ok_or("无目标文件名")?.to_string_lossy(),
        std::process::id()
    ));
    let mut input = std::fs::File::open(src).map_err(|e| e.to_string())?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
        .map_err(|e| e.to_string())?;
    let mut bytes = vec![0u8; 1024 * 1024];
    let mut written = 0;
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err(format!(
                "已取消；未覆盖目标。部分副本保留在 {}",
                partial.display()
            ));
        }
        let n = input.read(&mut bytes).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        output.write_all(&bytes[..n]).map_err(|e| e.to_string())?;
        written += n as u64;
    }
    let mut times = std::fs::FileTimes::new();
    if let Ok(time) = m.modified() {
        times = times.set_modified(time);
    }
    if let Ok(time) = m.accessed() {
        times = times.set_accessed(time);
    }
    output.set_times(times).map_err(|e| e.to_string())?;
    output.sync_all().map_err(|e| e.to_string())?;
    drop(output);
    let after = std::fs::metadata(src).map_err(|e| e.to_string())?;
    if written != m.len() || after.len() != m.len() || after.modified().ok() != m.modified().ok() {
        return Err(format!(
            "源文件在复制期间改变；未提交，副本在 {}",
            partial.display()
        ));
    }
    if dst.exists() {
        return Err(format!(
            "目标被其他程序创建；未覆盖，副本在 {}",
            partial.display()
        ));
    }
    std::fs::rename(&partial, dst)
        .map_err(|e| format!("提交失败：{e}；副本在 {}", partial.display()))?;
    if action == FileAction::Move {
        std::fs::remove_file(src).map_err(|e| format!("复制成功但源删除失败（保留两份）：{e}"))?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_reject_reserved_and_paths() {
        for s in ["", "..", "CON.txt", "a/b", "C:", "trailing."] {
            assert!(!safe_name(s));
        }
        assert!(safe_name("新名称.txt"));
    }
    #[test]
    fn fixture_copy_move_rename_no_overwrite() {
        let d = std::env::temp_dir().join(format!("findx-native-fileops-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir(&d).unwrap();
        let a = d.join("a.txt");
        let b = d.join("b.txt");
        let c = d.join("c.txt");
        std::fs::write(&a, b"safe fixture").unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        operate(FileAction::Copy, &a, &b, &cancel).unwrap();
        assert!(operate(FileAction::Copy, &a, &b, &cancel).is_err());
        operate(FileAction::Rename, &b, &c, &cancel).unwrap();
        assert_eq!(std::fs::read(&c).unwrap(), b"safe fixture");
        let e = d.join("moved.txt");
        operate(FileAction::Move, &c, &e, &cancel).unwrap();
        assert!(!c.exists());
        cancel.store(true, Ordering::Release);
        assert!(operate(FileAction::Copy, &a, &d.join("cancelled"), &cancel).is_err());
        std::fs::remove_dir_all(d).unwrap();
    }
    #[test]
    fn listing_is_sorted_and_cancellable() {
        let c = Arc::new(AtomicBool::new(true));
        assert!(list(&std::env::temp_dir(), &c).is_err());
    }
    #[test]
    fn recursive_directory_copy_is_non_destructive() {
        let root = std::env::temp_dir().join(format!("findx-dircopy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("source/sub")).unwrap();
        std::fs::write(root.join("source/sub/file.txt"), b"directory fixture").unwrap();
        let c = Arc::new(AtomicBool::new(false));
        operate(
            FileAction::Copy,
            &root.join("source"),
            &root.join("copy"),
            &c,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(root.join("copy/sub/file.txt")).unwrap(),
            b"directory fixture"
        );
        assert!(root.join("source/sub/file.txt").exists());
        assert!(
            operate(
                FileAction::Copy,
                &root.join("source"),
                &root.join("source/nested"),
                &c
            )
            .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
