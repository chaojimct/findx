//! Table keyboard navigation is not file activation. SDK focus-visible uses any
//! key (including Space/Enter), which paints a clipped, full-panel blue ring.
//! Keep the SDK focus handles/tab stops/selected rows; only replace that paint.
use gpui_kit::component::v_flex;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Hsla, IntoElement, KeyDownEvent, Keystroke, ParentElement, Styled, div, px,
};

#[derive(Default)]
pub(super) struct TableNavigationFocus(bool);
impl TableNavigationFocus {
    pub(super) fn keystroke(&mut self, key: &Keystroke) -> bool {
        let next = self.0
            || (key.key == "tab"
                && !key.modifiers.control
                && !key.modifiers.alt
                && !key.modifiers.platform
                && !key.modifiers.function);
        std::mem::replace(&mut self.0, next) != next
    }
    pub(super) fn pointer(&mut self) -> bool {
        std::mem::replace(&mut self.0, false)
    }
    pub(super) fn visible(&self, table_focused: bool, window_active: bool) -> bool {
        self.0 && table_focused && window_active
    }
}

/// A bounded, neutral header mark, never a whole-panel accent outline. It has
/// no focus handle, handlers or tab stop, so it cannot intercept table input.
pub(super) fn table_frame(table: impl IntoElement, cue: bool, color: Hsla) -> AnyElement {
    v_flex()
        .relative()
        .size_full()
        .min_h_0()
        .min_w_0()
        .child(table)
        .when(cue, |this| {
            this.child(
                div()
                    .absolute()
                    .top(px(2.))
                    .left(px(10.))
                    .w(px(24.))
                    .h(px(2.))
                    .bg(color.opacity(0.55)),
            )
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(s: &'static str) -> Keystroke {
        Keystroke::parse(s).unwrap()
    }
    #[test]
    fn activation_and_arrows_do_not_promote_mouse_focus() {
        for s in ["space", "enter", "escape", "up", "down", "a"] {
            let mut focus = TableNavigationFocus::default();
            assert!(!focus.keystroke(&key(s)), "{s}");
            assert!(!focus.visible(true, true));
        }
    }
    #[test]
    fn tab_and_shift_tab_retain_discoverable_keyboard_focus() {
        for s in ["tab", "shift-tab"] {
            let mut focus = TableNavigationFocus::default();
            assert!(focus.keystroke(&key(s)));
            assert!(focus.visible(true, true));
            assert!(!focus.visible(false, true));
            assert!(!focus.visible(true, false));
            assert!(!focus.keystroke(&key("space")));
            assert!(focus.visible(true, true));
            assert!(focus.pointer());
            assert!(!focus.visible(true, true));
        }
    }
    #[test]
    fn app_switch_and_command_tabs_do_not_claim_table_navigation() {
        for s in ["ctrl-tab", "alt-tab", "cmd-tab"] {
            let mut focus = TableNavigationFocus::default();
            assert!(!focus.keystroke(&key(s)));
            assert!(!focus.visible(true, true));
        }
    }
}

/// Bare file commands are one-shot per physical press. Raw KeyDownEvent keeps
/// the OS repeat bit, including repeats after focus changes; no timeout/latch
/// can accidentally launch a file twice or lose the next genuine press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FileKey {
    Preview,
    Open,
    Dismiss,
}
impl FileKey {
    pub(super) fn parse(key: &Keystroke) -> Option<Self> {
        if key.modifiers.number_of_modifiers() != 0 {
            return None;
        }
        match key.key.as_str() {
            "space" => Some(Self::Preview),
            "enter" => Some(Self::Open),
            "escape" => Some(Self::Dismiss),
            _ => None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FileKeyRoute {
    Pass,
    ConsumeRepeat,
    Dispatch(FileKey),
}
pub(super) fn route_file_key(
    event: &KeyDownEvent,
    input_or_ime: bool,
    file_target: bool,
) -> FileKeyRoute {
    let Some(key) = FileKey::parse(&event.keystroke) else {
        return FileKeyRoute::Pass;
    };
    if event.prefer_character_input || input_or_ime || (!file_target && key != FileKey::Dismiss) {
        return FileKeyRoute::Pass;
    }
    if event.is_held {
        FileKeyRoute::ConsumeRepeat
    } else {
        FileKeyRoute::Dispatch(key)
    }
}

#[cfg(test)]
mod repeat_tests {
    use super::{FileKey, FileKeyRoute, route_file_key};
    use gpui_kit::{KeyDownEvent, Keystroke};
    fn event(key: &'static str, held: bool) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: Keystroke::parse(key).unwrap(),
            is_held: held,
            prefer_character_input: false,
        }
    }
    #[test]
    fn held_activation_never_reopens_or_retoggles_or_redismisses() {
        for name in ["space", "enter", "escape"] {
            let mut count = 0;
            for held in [false, true, true, true, false, true] {
                if matches!(
                    route_file_key(&event(name, held), false, true),
                    FileKeyRoute::Dispatch(_)
                ) {
                    count += 1;
                }
            }
            assert_eq!(count, 2, "{name}: two real presses only");
        }
    }
    #[test]
    fn held_event_after_window_or_directory_change_is_still_suppressed() {
        assert_eq!(
            route_file_key(&event("enter", true), false, true),
            FileKeyRoute::ConsumeRepeat
        );
        assert_eq!(
            route_file_key(&event("enter", false), false, true),
            FileKeyRoute::Dispatch(FileKey::Open)
        );
    }
    #[test]
    fn text_ime_and_native_buttons_keep_their_normal_events() {
        for name in ["space", "enter", "escape"] {
            for held in [false, true] {
                assert_eq!(
                    route_file_key(&event(name, held), true, true),
                    FileKeyRoute::Pass
                );
            }
        }
        for name in ["space", "enter"] {
            for held in [false, true] {
                assert_eq!(
                    route_file_key(&event(name, held), false, false),
                    FileKeyRoute::Pass
                );
            }
        }
    }
    #[test]
    fn platform_character_input_preference_is_never_stolen() {
        for name in ["space", "enter", "escape"] {
            for held in [false, true] {
                let mut e = event(name, held);
                e.prefer_character_input = true;
                assert_eq!(route_file_key(&e, false, true), FileKeyRoute::Pass);
            }
        }
    }
    #[test]
    fn modified_keys_and_navigation_repeat_are_untouched() {
        for name in [
            "shift-space",
            "ctrl-enter",
            "alt-enter",
            "shift-enter",
            "ctrl-escape",
            "up",
            "down",
            "home",
            "end",
            "backspace",
            "tab",
            "a",
        ] {
            for held in [false, true] {
                assert_eq!(
                    route_file_key(&event(name, held), false, true),
                    FileKeyRoute::Pass,
                    "{name}"
                );
            }
        }
    }
}
