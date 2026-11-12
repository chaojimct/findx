//! Search/file interaction policy; no filesystem, windows or service side effects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileMenuContext {
    Blank,
    File,
    Directory,
    Multiple,
}
pub(crate) fn file_menu_context(blank: bool, count: usize, directory: bool) -> FileMenuContext {
    match (blank, count, directory) {
        (true, _, _) | (_, 0, _) => FileMenuContext::Blank,
        (_, 1, true) => FileMenuContext::Directory,
        (_, 1, false) => FileMenuContext::File,
        _ => FileMenuContext::Multiple,
    }
}
pub(crate) fn notice_can_expire(
    ticket: u64,
    current: u64,
    message: &str,
    summary: &str,
    busy: bool,
) -> bool {
    ticket == current && message == summary && !busy
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blank_never_inherits_selected_file_commands() {
        assert_eq!(file_menu_context(true, 1, true), FileMenuContext::Blank);
        assert_eq!(file_menu_context(true, 5, false), FileMenuContext::Blank);
        assert_eq!(file_menu_context(false, 0, false), FileMenuContext::Blank);
    }
    #[test]
    fn file_directory_and_multi_selection_are_distinct() {
        assert_eq!(file_menu_context(false, 1, false), FileMenuContext::File);
        assert_eq!(
            file_menu_context(false, 1, true),
            FileMenuContext::Directory
        );
        assert_eq!(file_menu_context(false, 2, true), FileMenuContext::Multiple);
    }
    #[test]
    fn old_timer_cannot_clear_a_new_notice_even_with_identical_text() {
        assert!(!notice_can_expire(1, 2, "done", "done", false));
        assert!(notice_can_expire(2, 2, "done", "done", false));
    }
    #[test]
    fn expiry_preserves_error_and_active_operation_status() {
        assert!(!notice_can_expire(2, 2, "done", "error", false));
        assert!(!notice_can_expire(2, 2, "done", "done", true));
    }
}
