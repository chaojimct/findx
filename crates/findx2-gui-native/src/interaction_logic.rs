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

/// A file command must never reinterpret editing/IME/modal input as a file action.
pub fn file_commands_blocked(editor_focus: bool, composing: bool, modal: bool) -> bool {
    editor_focus || composing || modal
}

/// Rows a context-menu action may use. A menu click keeps the rows captured
/// when the menu opened, even if focus has returned to the search box.
/// Keyboard shortcuts still honor the editor guard and the live selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextResolution<T> {
    Ignored,
    Blank,
    Rows(Vec<T>),
}

pub fn resolve_context_rows<T: Clone>(
    menu_snapshot: Option<(bool, Vec<T>)>,
    editor_focused: bool,
    scope_current: bool,
    live_rows: Vec<T>,
) -> ContextResolution<T> {
    if let Some((blank, rows)) = menu_snapshot {
        return if blank {
            ContextResolution::Blank
        } else {
            ContextResolution::Rows(rows)
        };
    }
    if editor_focused || !scope_current {
        ContextResolution::Ignored
    } else {
        ContextResolution::Rows(live_rows)
    }
}

/// Parent directory for the in-app 「在目录中定位」 (Ctrl+Enter). The directory filter is always cleared
/// so a stale Files filter cannot hide the target.
pub fn locate_parent(path: &str) -> Option<String> {
    let path = path.trim();
    let parent = std::path::Path::new(path).parent()?;
    if parent.as_os_str().is_empty() {
        return None;
    }
    Some(parent.to_string_lossy().into_owned())
}
#[cfg(test)]
mod shortcut_safety_tests {
    use super::*;
    #[test]
    fn composition_without_focus_still_blocks_file_commands() {
        for editor in [false, true] {
            for ime in [false, true] {
                for modal in [false, true] {
                    assert_eq!(
                        file_commands_blocked(editor, ime, modal),
                        editor || ime || modal
                    );
                }
            }
        }
        assert!(file_commands_blocked(false, true, false));
    }
    #[test]
    fn menu_snapshot_survives_search_focus_and_a_stale_live_selection() {
        let snap = vec!["C:\\Users\\a.pdf"];
        let resolved = resolve_context_rows(Some((false, snap.clone())), true, false, Vec::<&str>::new());
        assert_eq!(resolved, ContextResolution::Rows(snap));
        assert_eq!(
            resolve_context_rows(Some((true, vec!["ignored"])), true, true, vec!["live"]),
            ContextResolution::Blank
        );
        assert_eq!(
            resolve_context_rows::<&str>(None, true, true, vec!["live"]),
            ContextResolution::Ignored
        );
        assert_eq!(
            resolve_context_rows(None, false, true, vec!["live"]),
            ContextResolution::Rows(vec!["live"])
        );
        let file = std::path::Path::new("Users").join("chaoj").join("report.pdf");
        let parent = std::path::Path::new("Users").join("chaoj");
        assert_eq!(
            locate_parent(&file.to_string_lossy()).as_deref(),
            Some(parent.to_string_lossy().as_ref())
        );
        assert!(locate_parent("report.pdf").is_none());
    }
}

/// Toggle effective visibility, including the unforced default. Otherwise the
/// first Space on a default-open preview is a no-op (None -> Show).
pub fn toggle_preview_force(
    current: Option<crate::search_logic::ForceState>,
    preferred_open: bool,
) -> crate::search_logic::ForceState {
    use crate::search_logic::ForceState;
    match current.unwrap_or(if preferred_open {
        ForceState::Show
    } else {
        ForceState::Hide
    }) {
        ForceState::Show => ForceState::Hide,
        ForceState::Hide => ForceState::Show,
    }
}
#[cfg(test)]
mod preview_toggle_tests {
    use super::toggle_preview_force;
    use crate::search_logic::ForceState::{Hide, Show};
    #[test]
    fn first_space_toggles_the_effective_default_and_overrides() {
        assert_eq!(toggle_preview_force(None, true), Hide);
        assert_eq!(toggle_preview_force(None, false), Show);
        for pref in [true, false] {
            assert_eq!(toggle_preview_force(Some(Show), pref), Hide);
            assert_eq!(toggle_preview_force(Some(Hide), pref), Show);
        }
    }
}

/// Escape cancels intents still waiting for metadata/identity. It does not
/// weaken identity checks or pretend a launch already handed to the OS can
/// be revoked. The identity worker may finish normally without opening UI.
pub(crate) fn cancel_deferred_file_intents<T, U>(open: &mut Option<T>, shell: &mut Option<U>) {
    *open = None;
    *shell = None;
}
#[cfg(test)]
mod deferred_intent_tests {
    #[test]
    fn escape_before_identity_completion_cancels_both_intents() {
        let mut open = Some("open-file");
        let mut shell = Some("open-with");
        super::cancel_deferred_file_intents(&mut open, &mut shell);
        // Same consumption protocol as the successful identity callback.
        let mut launches = 0;
        if open.take().is_some() {
            launches += 1;
        }
        if shell.take().is_some() {
            launches += 1;
        }
        assert_eq!(launches, 0);
        // A later explicit press is valid; cancellation is not a sticky gate.
        open = Some("new-explicit-open");
        assert!(open.take().is_some());
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SearchKeyIntent {
    Next,
    Previous,
    Open,
}
/// Query edits invalidate both published rows and queued activation immediately,
/// including the debounce interval. A pending intent follows only its request's
/// generation; older IPC replies cannot open a different query's result.
#[derive(Default)]
pub(crate) struct SearchKeyboard {
    published: Option<u64>,
    pending: Option<(u64, SearchKeyIntent, String)>,
}
impl SearchKeyboard {
    pub(crate) fn ready(&self, generation: u64) -> bool {
        self.published == Some(generation)
    }
    pub(crate) fn invalidate(&mut self) {
        self.published = None;
        self.pending = None;
    }
    pub(crate) fn queue(&mut self, generation: u64, intent: SearchKeyIntent, query: String) {
        self.pending = Some((generation, intent, query));
    }
    pub(crate) fn begin(&mut self, old: u64, new: u64, query: &str) {
        self.published = None;
        self.pending = self
            .pending
            .take()
            .filter(|(g, _, q)| *g == old && q == query)
            .map(|(_, i, q)| (new, i, q));
    }
    pub(crate) fn publish(&mut self, generation: u64) -> Option<SearchKeyIntent> {
        self.published = Some(generation);
        self.pending
            .take()
            .filter(|(g, _, _)| *g == generation)
            .map(|(_, i, _)| i)
    }
    pub(crate) fn cancel(&mut self) {
        self.pending = None;
    }
}
#[cfg(test)]
mod search_keyboard_tests {
    use super::{SearchKeyIntent::*, SearchKeyboard};
    #[test]
    fn debounce_never_opens_old_rows_and_intent_follows_exact_request() {
        let mut k = SearchKeyboard::default();
        k.publish(1);
        assert!(k.ready(1));
        k.invalidate();
        assert!(!k.ready(2));
        k.queue(2, Open, "q".into());
        k.begin(2, 3, "q");
        assert!(!k.ready(3));
        assert_eq!(k.publish(3), Some(Open));
        assert!(k.ready(3));
        assert_eq!(k.publish(3), None);
    }
    #[test]
    fn edits_and_escape_cancel_deferred_activation() {
        let mut k = SearchKeyboard::default();
        k.queue(4, Open, "q".into());
        k.invalidate();
        k.begin(5, 6, "q");
        assert_eq!(k.publish(6), None);
        k.queue(6, Open, "q".into());
        k.cancel();
        assert_eq!(k.publish(6), None);
    }
    #[test]
    fn filter_or_scope_change_cannot_retarget_a_queued_open() {
        let mut k = SearchKeyboard::default();
        k.queue(1, Open, "old path:A".into());
        k.begin(1, 2, "new path:B");
        assert_eq!(k.publish(2), None);
    }
    #[test]
    fn repeated_query_enter_is_one_intent_and_navigation_remains_ordered() {
        let mut k = SearchKeyboard::default();
        for _ in 0..8 {
            k.queue(2, Open, "q".into())
        }
        k.begin(2, 3, "q");
        assert_eq!(k.publish(3), Some(Open));
        assert_eq!(k.publish(3), None);
        k.queue(4, Next, "q".into());
        k.queue(4, Previous, "q".into());
        k.begin(4, 5, "q");
        assert_eq!(k.publish(5), Some(Previous));
    }
}
