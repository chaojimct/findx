//! App-controlled file clipboard. No hidden system-clipboard reads and no implicit overwrites.
use crate::{
    browser::{self, FileAction},
    file_identity::{self, FileStamp},
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
#[derive(Clone, Debug)]
pub struct Source {
    pub path: PathBuf,
    pub stamp: FileStamp,
}
#[derive(Clone, Default)]
pub struct Clipboard {
    pub sources: Vec<Source>,
    pub cut: bool,
}
#[derive(Default, Debug)]
pub struct Outcome {
    pub completed: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
    pub cancelled: bool,
}
pub fn transfer(clipboard: &Clipboard, directory: &Path, cancel: &Arc<AtomicBool>) -> Outcome {
    let mut result = Outcome::default();
    for source in &clipboard.sources {
        if cancel.load(Ordering::Acquire) {
            result.cancelled = true;
            break;
        }
        let outcome = (|| {
            file_identity::revalidate(&source.path, source.stamp)?;
            if !directory.is_dir() {
                return Err("目标目录不可用；没有执行此项".to_string());
            }
            let name = source.path.file_name().ok_or("源位置没有文件名")?;
            browser::operate(
                if clipboard.cut {
                    FileAction::Move
                } else {
                    FileAction::Copy
                },
                &source.path,
                &directory.join(name),
                cancel,
            )
        })();
        match outcome {
            Ok(()) => result.completed.push(source.path.clone()),
            Err(e) => result.failed.push((source.path.clone(), e)),
        }
    }
    result.cancelled |= cancel.load(Ordering::Acquire);
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (PathBuf, PathBuf, Clipboard) {
        let root = std::env::temp_dir().join(format!(
            "findx-transfer-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("dst")).unwrap();
        for name in ["a.txt", "b.txt"] {
            std::fs::write(root.join(name), name).unwrap();
        }
        let c = Clipboard {
            sources: ["a.txt", "b.txt"]
                .iter()
                .map(|n| {
                    let path = root.join(n);
                    Source {
                        stamp: file_identity::capture(&path).unwrap(),
                        path,
                    }
                })
                .collect(),
            cut: false,
        };
        (root.clone(), root.join("dst"), c)
    }
    #[test]
    fn batch_conflict_keeps_original_and_reports_partial_success() {
        let (root, dst, c) = setup();
        std::fs::write(dst.join("a.txt"), "keep").unwrap();
        let out = transfer(&c, &dst, &Arc::new(AtomicBool::new(false)));
        assert_eq!(out.completed.len(), 1);
        assert_eq!(out.failed.len(), 1);
        assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "keep");
        assert!(root.join("b.txt").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn cancelled_batch_performs_no_work() {
        let (root, dst, c) = setup();
        let out = transfer(&c, &dst, &Arc::new(AtomicBool::new(true)));
        assert!(out.cancelled);
        assert!(out.completed.is_empty());
        assert!(!dst.join("a.txt").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn changed_source_is_not_moved() {
        let (root, dst, mut c) = setup();
        c.cut = true;
        std::fs::write(root.join("a.txt"), "changed source").unwrap();
        let out = transfer(&c, &dst, &Arc::new(AtomicBool::new(false)));
        assert_eq!(out.completed.len(), 1);
        assert_eq!(out.failed.len(), 1);
        assert!(root.join("a.txt").exists());
        assert!(!root.join("b.txt").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
