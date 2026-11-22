//! 详情/预览面板的三态控制：隐藏 / 自动 / 常显。
//!
//! 与 GPUI 的渲染解耦：状态机是纯函数，UI 只负责把当前状态传进来、把用户
//! 点击映射到 `next`，并按 `should_show` 决定是否渲染面板。这样三态的
//! 行为可以无窗口测试，也不需要在 GPUI 里跑 integration test。
//!
//! - `Hidden`：永远不显示，即使有选中项。
//! - `Auto`：有选中项/预览目标时显示，没有时折叠（默认）。
//! - `Visible`：总是显示面板骨架，即使没有选中项（便于拖拽/固定布局）。

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DetailPaneMode {
    Hidden,
    #[default]
    Auto,
    Visible,
}

impl DetailPaneMode {
    /// 用户点击「切换详情面板」时的循环顺序：自动 → 常显 → 隐藏 → 自动。
    pub fn cycle(self) -> Self {
        match self {
            Self::Auto => Self::Visible,
            Self::Visible => Self::Hidden,
            Self::Hidden => Self::Auto,
        }
    }

    /// 是否应该渲染面板内容。
    pub fn should_show(self, has_target: bool) -> bool {
        match self {
            Self::Hidden => false,
            Self::Visible => true,
            Self::Auto => has_target,
        }
    }

    /// 是否应该为面板保留布局空间（即使内容为空），避免其它区域在状态切换时
    /// 重新流式布局。
    pub fn reserves_space(self) -> bool {
        !matches!(self, Self::Hidden)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "自动",
            Self::Visible => "常显",
            Self::Hidden => "隐藏",
        }
    }

    pub fn tooltip(self) -> &'static str {
        match self {
            Self::Auto => "有选中项时显示详情（点击切换）",
            Self::Visible => "始终显示详情面板（点击切换）",
            Self::Hidden => "隐藏详情面板（点击切换）",
        }
    }

    /// 从本地偏好字符串解析；未知值回退到 Auto，绝不 panic。
    pub fn from_pref(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "hidden" | "off" | "false" | "0" => Self::Hidden,
            "visible" | "on" | "true" | "1" | "pinned" => Self::Visible,
            _ => Self::Auto,
        }
    }

    pub fn to_pref(self) -> &'static str {
        match self {
            Self::Hidden => "hidden",
            Self::Visible => "visible",
            Self::Auto => "auto",
        }
    }
}

/// 三态的最小描述，方便持久化与 UI 共享同一份真相。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetailPaneState {
    pub mode: DetailPaneMode,
}

impl Default for DetailPaneState {
    fn default() -> Self {
        Self { mode: DetailPaneMode::Auto }
    }
}

impl DetailPaneState {
    pub fn cycle(&mut self) {
        self.mode = self.mode.cycle();
    }
    pub fn should_show(&self, has_target: bool) -> bool {
        self.mode.should_show(has_target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_goes_auto_visible_hidden_auto() {
        assert_eq!(DetailPaneMode::Auto.cycle(), DetailPaneMode::Visible);
        assert_eq!(DetailPaneMode::Visible.cycle(), DetailPaneMode::Hidden);
        assert_eq!(DetailPaneMode::Hidden.cycle(), DetailPaneMode::Auto);
    }

    #[test]
    fn should_show_respects_mode_and_target() {
        assert!(!DetailPaneMode::Hidden.should_show(true));
        assert!(!DetailPaneMode::Hidden.should_show(false));
        assert!(DetailPaneMode::Visible.should_show(false));
        assert!(DetailPaneMode::Visible.should_show(true));
        assert!(!DetailPaneMode::Auto.should_show(false));
        assert!(DetailPaneMode::Auto.should_show(true));
    }

    #[test]
    fn reserves_space_only_when_not_hidden() {
        assert!(!DetailPaneMode::Hidden.reserves_space());
        assert!(DetailPaneMode::Auto.reserves_space());
        assert!(DetailPaneMode::Visible.reserves_space());
    }

    #[test]
    fn pref_roundtrips_and_unknown_defaults_to_auto() {
        for m in [DetailPaneMode::Auto, DetailPaneMode::Visible, DetailPaneMode::Hidden] {
            assert_eq!(DetailPaneMode::from_pref(m.to_pref()), m);
        }
        assert_eq!(DetailPaneMode::from_pref("garbage"), DetailPaneMode::Auto);
        assert_eq!(DetailPaneMode::from_pref("PINNED"), DetailPaneMode::Visible);
        assert_eq!(DetailPaneMode::from_pref("off"), DetailPaneMode::Hidden);
    }

    #[test]
    fn default_is_auto() {
        assert_eq!(DetailPaneState::default().mode, DetailPaneMode::Auto);
    }
}