//! 把现有 `design-tokens.css` / `themeColors.ts` 的色值映射到 GPUI Kit 主题。

use gpui_kit::component::{Colorize as _, Theme, ThemeMode};
use gpui_kit::*;

use crate::settings_logic::EffectiveTheme;
use serde::{Deserialize, Serialize};

/// Style is orthogonal to appearance. Old preference files intentionally default to Warm.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ThemeStyle {
    #[default]
    Warm,
    VSCode,
    Neutral,
}
impl ThemeStyle {
    pub const ALL: [(Self, &'static str); 3] = [
        (Self::Warm, "暖白"),
        (Self::VSCode, "VS Code"),
        (Self::Neutral, "中性灰"),
    ];
}

impl Global for FindxTokens {}

#[derive(Clone, Copy)]
pub struct FindxTokens {
    pub style: ThemeStyle,
    pub bg: Hsla,
    pub surface: Hsla,
    pub surface_raised: Hsla,
    pub border: Hsla,
    pub border_soft: Hsla,
    pub text_primary: Hsla,
    pub text_secondary: Hsla,
    pub text_tertiary: Hsla,
    pub accent: Hsla,
    pub accent_hover: Hsla,
    pub hover: Hsla,
    pub row_selected: Hsla,
    pub search_box_bg: Hsla,
    pub hit_bg: Hsla,
    pub hit_fg: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
    pub error: Hsla,
    pub title_bar: Hsla,
}

impl FindxTokens {
    /// Categories keep their identity while each style supplies a coherent readable chart palette.
    pub fn allocation_color(&self, category: usize) -> Hsla {
        let colors = match (self.style, self.bg.l < 0.4) {
            (ThemeStyle::Warm, false) => {
                [0xc9d2d8, 0xe0c5bf, 0xe4d4ad, 0xc4d1bf, 0xd1c6db, 0xd9d2b7]
            }
            (ThemeStyle::VSCode, false) => {
                [0xc5d6e7, 0xe1c4cd, 0xe5d6b4, 0xbcd4cf, 0xd1cae3, 0xd8d9ba]
            }
            (ThemeStyle::Neutral, false) => {
                [0xcdd1d5, 0xdacdd0, 0xdddbc9, 0xcbd6d2, 0xd3d0dc, 0xd6d6cc]
            }
            (ThemeStyle::Warm, true) => {
                [0x484d53, 0x59474a, 0x5e5139, 0x40544c, 0x514959, 0x55543f]
            }
            (ThemeStyle::VSCode, true) => {
                [0x3b4b5c, 0x59434b, 0x5c503b, 0x3b5355, 0x4e465f, 0x51533d]
            }
            (ThemeStyle::Neutral, true) => {
                [0x494c50, 0x53484b, 0x545244, 0x45504c, 0x4c4955, 0x4e5047]
            }
        };
        rgb(colors[category % colors.len()]).into()
    }
    pub fn for_theme(theme: EffectiveTheme) -> Self {
        Self::for_style(ThemeStyle::default(), theme)
    }

    pub fn for_style(style: ThemeStyle, theme: EffectiveTheme) -> Self {
        let dark = theme.is_dark();
        // bg, rail/chrome, raised, border, primary, secondary, muted, accent, hover, selected
        let p: [Hsla; 10] = match (style, dark) {
            (ThemeStyle::Warm, false) => [
                0xfffcf7, 0xf7f3eb, 0xffffff, 0xe0d9cd, 0x29251f, 0x645e54, 0x70685d, 0x336a99,
                0xeee8dd, 0xe5edf5,
            ],
            (ThemeStyle::Warm, true) => [
                0x24211d, 0x2c2823, 0x353029, 0x4a4339, 0xeee7dc, 0xc8beae, 0xa2998a, 0x8db9d9,
                0x3c352c, 0x35444d,
            ],
            (ThemeStyle::VSCode, false) => [
                0xffffff, 0xf3f3f3, 0xffffff, 0xd5d5d5, 0x1e1e1e, 0x555555, 0x666666, 0x006bbd,
                0xe8e8e8, 0xe2effc,
            ],
            (ThemeStyle::VSCode, true) => [
                0x1e1e1e, 0x252526, 0x2d2d30, 0x3c3c3c, 0xd4d4d4, 0xb4b4b4, 0x969696, 0x69adff,
                0x303035, 0x253d54,
            ],
            (ThemeStyle::Neutral, false) => [
                0xf8f8f8, 0xeeeeee, 0xffffff, 0xd4d4d4, 0x252525, 0x606060, 0x686868, 0x4b657e,
                0xe5e5e5, 0xe1e8ef,
            ],
            (ThemeStyle::Neutral, true) => [
                0x202020, 0x292929, 0x303030, 0x444444, 0xe5e5e5, 0xbfbfbf, 0x999999, 0xa7bdd2,
                0x383838, 0x38444f,
            ],
        }
        .map(|v| rgb(v).into());
        Self {
            style,
            bg: p[0],
            surface: p[1],
            surface_raised: p[2],
            border: p[3],
            border_soft: p[3].opacity(0.55),
            text_primary: p[4],
            text_secondary: p[5],
            text_tertiary: p[6],
            accent: p[7],
            accent_hover: if dark {
                p[7].lighten(0.04)
            } else {
                p[7].darken(0.04)
            },
            hover: p[8],
            row_selected: p[9],
            search_box_bg: p[2],
            hit_bg: if dark {
                rgb(0x665122).into()
            } else {
                rgb(0xffebbc).into()
            },
            hit_fg: p[4],
            success: if dark {
                rgb(0x8acb96).into()
            } else {
                rgb(0x347d49).into()
            },
            warning: if dark {
                rgb(0xe4b96b).into()
            } else {
                rgb(0x986521).into()
            },
            error: if dark {
                rgb(0xf09c96).into()
            } else {
                rgb(0xb44136).into()
            },
            title_bar: p[1],
        }
    }
}

pub fn apply_findx_theme(theme: EffectiveTheme, cx: &mut App) {
    apply_findx_style(ThemeStyle::default(), theme, cx);
}

/// Supported Kit update synchronizes both semantic tokens and the Base projection.
/// Styling only the outer div leaves tables, inputs and menus in the previous palette.
pub fn apply_findx_style(style: ThemeStyle, appearance: EffectiveTheme, cx: &mut App) {
    Theme::change(
        if appearance.is_dark() {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        },
        None,
        cx,
    );
    let t = FindxTokens::for_style(style, appearance);
    Theme::update(cx, |theme| {
        let c = &mut theme.colors;
        c.background = t.bg;
        c.foreground = t.text_primary;
        c.border = t.border;
        c.accent = t.hover;
        c.accent_foreground = t.text_primary;
        c.primary = t.accent;
        c.primary_hover = t.accent_hover;
        c.primary_active = t.accent;
        c.primary_foreground = if appearance.is_dark() {
            rgb(0x17212a).into()
        } else {
            rgb(0xffffff).into()
        };
        c.button = t.surface_raised;
        c.button_foreground = t.text_primary;
        c.button_hover = t.hover;
        c.button_active = t.row_selected;
        c.button_primary = t.accent;
        c.button_primary_hover = t.accent_hover;
        c.button_primary_active = t.accent;
        c.button_primary_foreground = c.primary_foreground;
        c.button_secondary = t.surface;
        c.button_secondary_foreground = t.text_primary;
        c.button_secondary_hover = t.hover;
        c.button_secondary_active = t.row_selected;
        c.secondary = t.surface;
        c.secondary_foreground = t.text_secondary;
        c.secondary_hover = t.hover;
        c.secondary_active = t.row_selected;
        c.input = t.border;
        c.caret = t.text_primary;
        c.selection = t.row_selected;
        c.ring = t.accent;
        c.muted = t.surface;
        c.muted_foreground = t.text_tertiary;
        c.popover = t.surface_raised;
        c.popover_foreground = t.text_primary;
        c.list = t.bg;
        c.list_active = t.row_selected;
        c.list_active_border = t.accent;
        c.list_even = t.bg;
        c.list_head = t.surface;
        c.list_hover = t.hover;
        c.table = t.bg;
        c.table_head = t.surface;
        c.table_head_foreground = t.text_secondary;
        c.table_foot = t.surface;
        c.table_foot_foreground = t.text_secondary;
        c.table_active = t.row_selected;
        c.table_active_border = t.accent;
        c.table_even = t.bg;
        c.table_hover = t.hover;
        c.table_row_border = t.border_soft;
        c.sidebar = t.surface;
        c.sidebar_foreground = t.text_primary;
        c.sidebar_border = t.border;
        c.sidebar_accent = t.row_selected;
        c.sidebar_accent_foreground = t.text_primary;
        c.sidebar_primary = t.accent;
        c.sidebar_primary_foreground = c.primary_foreground;
        c.title_bar = t.title_bar;
        c.title_bar_border = t.border;
        c.status_bar = t.surface;
        c.status_bar_border = t.border;
        c.scrollbar = t.bg;
        c.scrollbar_thumb = t.border;
        c.scrollbar_thumb_hover = t.text_tertiary;
        c.link = t.accent;
        c.link_hover = t.accent_hover;
        c.link_active = t.accent;
        c.group_box = t.surface;
        c.group_box_foreground = t.text_primary;
        c.drag_border = t.accent;
        c.drop_target = t.row_selected;
    });
    Theme::set_scrollbar_mode(gpui_kit::component::scroll::ScrollbarMode::Always, cx);
    cx.set_global(t);
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    fn luminance(c: Hsla) -> f32 {
        let c: Rgba = c.into();
        let linear = |v: f32| {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(c.r) + 0.7152 * linear(c.g) + 0.0722 * linear(c.b)
    }
    fn contrast(a: Hsla, b: Hsla) -> f32 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }
    #[test]
    fn all_six_palettes_keep_readable_text_and_selected_links() {
        for (style, _) in ThemeStyle::ALL {
            for appearance in [EffectiveTheme::Light, EffectiveTheme::Dark] {
                let t = FindxTokens::for_style(style, appearance);
                for fg in [t.text_primary, t.text_secondary, t.text_tertiary] {
                    for bg in [t.bg, t.surface, t.surface_raised] {
                        assert!(
                            contrast(fg, bg) >= 4.5,
                            "text contrast below AA: {}",
                            contrast(fg, bg)
                        );
                    }
                }
                for category in 0..6 {
                    assert!(
                        contrast(t.text_primary, t.allocation_color(category)) >= 4.5,
                        "treemap label contrast below AA"
                    );
                }
                for bg in [t.bg, t.surface, t.row_selected] {
                    assert!(
                        contrast(t.accent, bg) >= 4.5,
                        "selected-link contrast below AA: {}",
                        contrast(t.accent, bg)
                    );
                }
                let button_fg = if appearance.is_dark() {
                    rgb(0x17212a).into()
                } else {
                    rgb(0xffffff).into()
                };
                assert!(contrast(button_fg, t.accent) >= 4.5);
                assert!(contrast(button_fg, t.accent_hover) >= 4.5);
            }
        }
    }
    #[test]
    fn each_style_has_distinct_light_and_dark_palettes() {
        for (style, _) in ThemeStyle::ALL {
            let light = FindxTokens::for_style(style, EffectiveTheme::Light);
            let dark = FindxTokens::for_style(style, EffectiveTheme::Dark);
            assert!(light.bg.l > 0.9 && dark.bg.l < 0.25);
            assert!(light.text_primary.l < 0.3 && dark.text_primary.l > 0.7);
            assert_ne!(light.bg, dark.bg);
        }
    }
}
