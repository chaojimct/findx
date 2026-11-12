//! Pure navigation transactions and bounded sibling enumeration. No scan/index side effects.
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Default, Debug)]
pub struct AddressEdit {
    revision: u64,
    active: bool,
    pending: bool,
}
impl AddressEdit {
    pub fn begin(&mut self) {
        self.invalidate();
        self.active = true;
    }
    pub fn change(&mut self) {
        if self.active {
            self.revision = self.revision.wrapping_add(1);
            self.pending = false;
        }
    }
    pub fn invalidate(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.active = false;
        self.pending = false;
    }
    pub fn submit(&mut self) -> Option<u64> {
        if !self.active {
            return None;
        }
        self.change();
        self.pending = true;
        Some(self.revision)
    }
    pub fn accepts(&self, ticket: u64) -> bool {
        self.active && self.pending && self.revision == ticket
    }
    pub fn finish(&mut self, ticket: u64) -> bool {
        if !self.accepts(ticket) {
            return false;
        }
        self.pending = false;
        true
    }
    pub fn pending(&self) -> bool {
        self.pending
    }
}

/// Content-width planning; only the terminal crumb can shrink. Hidden middle crumbs remain actionable.
#[derive(Debug, PartialEq)]
pub struct CrumbPlan {
    pub visible: Vec<usize>,
    pub hidden: Vec<usize>,
    pub terminal_width: f32,
}
pub fn plan_crumbs(widths: &[f32], available: f32) -> CrumbPlan {
    if widths.is_empty() {
        return CrumbPlan {
            visible: vec![],
            hidden: vec![],
            terminal_width: 0.,
        };
    }
    let last = widths.len() - 1;
    let mut visible = (0..widths.len()).collect::<Vec<_>>();
    let mut hidden = Vec::new();
    while visible.len() > 2 {
        let need = visible.iter().map(|&i| widths[i]).sum::<f32>()
            + if hidden.is_empty() { 0. } else { 28. };
        if need <= available {
            break;
        }
        hidden.push(visible.remove(1));
    }
    let prefix = visible
        .iter()
        .filter(|&&i| i != last)
        .map(|&i| widths[i])
        .sum::<f32>()
        + if hidden.is_empty() { 0. } else { 28. };
    CrumbPlan {
        visible,
        hidden,
        terminal_width: widths[last].min((available - prefix).max(24.)),
    }
}
#[derive(Clone, Debug)]
pub struct Sibling {
    pub path: PathBuf,
    pub name: String,
    pub directory: bool,
    pub bytes: Option<u64>,
    pub uncertain: bool,
}
#[derive(Debug)]
pub struct Siblings {
    pub entries: Vec<Sibling>,
    pub partial: bool,
}
/// Bounded, cancelable enumeration; no allocation measurement and no child recursion.
pub fn list_siblings(parent: &Path, cancel: &AtomicBool) -> std::io::Result<Siblings> {
    if cancel.load(Ordering::Acquire) {
        return Err(std::io::ErrorKind::Interrupted.into());
    }
    let mut entries = Vec::new();
    let mut partial = false;
    for entry in std::fs::read_dir(parent)? {
        if cancel.load(Ordering::Acquire) {
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        if entries.len() >= 256 {
            partial = true;
            break;
        }
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                partial = true;
                continue;
            }
        };
        let directory = match entry.file_type() {
            Ok(t) => t.is_dir(),
            Err(_) => {
                partial = true;
                continue;
            }
        };
        entries.push(Sibling {
            path: entry.path(),
            name: entry.file_name().to_string_lossy().into_owned(),
            directory,
            bytes: None,
            uncertain: false,
        });
    }
    entries.sort_by(|a, b| {
        b.directory
            .cmp(&a.directory)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(Siblings { entries, partial })
}
pub fn validate_directory(draft: &str, current: &Path) -> std::io::Result<PathBuf> {
    let text = draft.trim();
    let text = text
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(text);
    if text.is_empty() {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    let p = PathBuf::from(text);
    let p = if p.is_absolute() { p } else { current.join(p) };
    if !std::fs::metadata(&p)?.is_dir() {
        return Err(std::io::ErrorKind::NotADirectory.into());
    }
    drop(std::fs::read_dir(&p)?);
    std::fs::canonicalize(p)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancel_never_accepts_pending_validation() {
        let mut e = AddressEdit::default();
        e.begin();
        let t = e.submit().unwrap();
        e.invalidate();
        assert!(!e.accepts(t));
    }
    #[test]
    fn changing_text_rejects_previous_validation() {
        let mut e = AddressEdit::default();
        e.begin();
        let t = e.submit().unwrap();
        e.change();
        assert!(!e.accepts(t));
    }
    #[test]
    fn repeated_submit_keeps_only_latest() {
        let mut e = AddressEdit::default();
        e.begin();
        let a = e.submit().unwrap();
        let b = e.submit().unwrap();
        assert!(!e.accepts(a));
        assert!(e.finish(b));
        assert!(!e.pending());
    }
    #[test]
    fn failed_validation_keeps_edit_and_can_retry() {
        let mut e = AddressEdit::default();
        e.begin();
        let a = e.submit().unwrap();
        assert!(e.finish(a));
        assert!(e.submit().is_some());
        e.invalidate();
        assert!(e.submit().is_none());
    }
    #[test]
    fn middle_collapses_before_terminal() {
        let p = plan_crumbs(&[48., 90., 100., 120.], 230.);
        assert_eq!(p.visible, vec![0, 3]);
        assert_eq!(p.hidden, vec![1, 2]);
        assert_eq!(p.terminal_width, 120.);
    }
    #[test]
    fn short_path_uses_natural_width() {
        let p = plan_crumbs(&[48., 60., 80.], 700.);
        assert_eq!(p.visible, vec![0, 1, 2]);
        assert!(p.hidden.is_empty());
        assert_eq!(p.terminal_width, 80.);
    }
    #[test]
    fn long_current_name_uses_remaining_width() {
        let p = plan_crumbs(&[48., 90., 800.], 300.);
        assert_eq!(p.terminal_width, 224.);
        assert_eq!(p.hidden, vec![1]);
    }
    #[test]
    fn relative_validation_and_cancellation_use_fixture_only() {
        let root = std::env::temp_dir().join(format!("findx-nav-{}", std::process::id()));
        std::fs::create_dir_all(root.join("child")).unwrap();
        std::fs::write(root.join("file"), b"fixture").unwrap();
        assert!(
            validate_directory("child", &root)
                .unwrap()
                .ends_with("child")
        );
        assert!(validate_directory("file", &root).is_err());
        assert!(validate_directory("", &root).is_err());
        let entries = list_siblings(&root, &AtomicBool::new(false)).unwrap();
        assert_eq!(entries.entries.len(), 2);
        assert!(entries.entries[0].directory);
        assert!(entries.entries.iter().all(|e| e.bytes.is_none()));
        assert!(list_siblings(&root, &AtomicBool::new(true)).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
