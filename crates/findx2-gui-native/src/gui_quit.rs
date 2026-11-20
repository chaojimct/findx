//! Stable names shared by the installed GUI and the installer handoff.
//! Shipped 2.5.3 has no newer quit message; its normal exit is the tray caption.

pub const MAIN_WINDOW_TITLE: &str = "FindX";
pub const SETTINGS_WINDOW_TITLE: &str = "FindX 设置";
pub const TRAY_MESSAGE: &str = "FindX.Native.Tray.v1";
pub const TRAY_QUIT_CAPTION: &str = "退出 FindX（不停止索引服务）";
pub const NORMAL_QUIT_MESSAGE: &str = "FindX.Native.NormalQuit.v1";

/// Down-arrow count before Enter when the menu opens with no selection.
pub fn menu_activation_downs(index: usize) -> usize {
    index + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_253_quit_names_stay_exact() {
        assert_eq!(MAIN_WINDOW_TITLE, "FindX");
        assert_eq!(SETTINGS_WINDOW_TITLE, "FindX 设置");
        assert_eq!(TRAY_MESSAGE, "FindX.Native.Tray.v1");
        assert_eq!(TRAY_QUIT_CAPTION, "退出 FindX（不停止索引服务）");
        assert_eq!(NORMAL_QUIT_MESSAGE, "FindX.Native.NormalQuit.v1");
        assert_ne!(TRAY_QUIT_CAPTION, "退出");
    }

    #[test]
    fn second_tray_item_is_the_shipped_quit_command() {
        assert_eq!(menu_activation_downs(1), 2);
        assert_eq!(menu_activation_downs(0), 1);
    }
}
