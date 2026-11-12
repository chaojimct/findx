//! Ownership-safe transient menu gate; independent from OS preview handlers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MenuToken(u64);
#[derive(Default)]
pub(crate) struct MenuGate {
    serial: u64,
    active: Option<MenuToken>,
}
impl MenuGate {
    pub(crate) fn begin(&mut self) -> MenuToken {
        self.serial = self.serial.wrapping_add(1);
        let token = MenuToken(self.serial);
        self.active = Some(token);
        token
    }
    pub(crate) fn finish(&mut self, token: MenuToken) -> bool {
        if self.active != Some(token) {
            return false;
        }
        self.active = None;
        true
    }
    pub(crate) fn is_open(&self) -> bool {
        self.active.is_some()
    }
}
pub(crate) fn surface_allowed(
    mounted: bool,
    menu_open: bool,
    address_popup: bool,
    keyboard_popup: bool,
) -> bool {
    mounted && !(menu_open || address_popup || keyboard_popup)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn late_blur_cannot_release_replacement_menu() {
        let mut g = MenuGate::default();
        let old = g.begin();
        let current = g.begin();
        assert!(!g.finish(old));
        assert!(g.is_open());
        assert!(g.finish(current));
        assert!(!g.is_open());
    }
    #[test]
    fn old_dismiss_cannot_release_replacement_menu() {
        let mut g = MenuGate::default();
        let old = g.begin();
        assert!(g.finish(old));
        let current = g.begin();
        assert!(!g.finish(old));
        assert!(g.is_open());
        assert!(g.finish(current));
    }
    #[test]
    fn owned_popup_remains_suppressed_after_focus_gate_closes() {
        assert!(!surface_allowed(true, false, true, false));
        assert!(!surface_allowed(true, false, false, true));
    }
    #[test]
    fn mode_settings_and_preview_close_never_restore_surface() {
        for menu in [false, true] {
            assert!(!surface_allowed(false, menu, false, false));
        }
        assert!(surface_allowed(true, false, false, false));
        assert!(!surface_allowed(true, true, false, false));
    }
}
