//! 把现有 `design-tokens.css` / `themeColors.ts` 的色值映射到 GPUI Kit 主题。

use gpui_kit::component::{Colorize as _, Theme, ThemeMode};
use gpui_kit::*;

use crate::settings_logic::EffectiveTheme;
use serde::{Deserialize, Serialize};

/// Style is orthogonal to appearance. Old preference files without a stored
/// style fall back to the VS Code look.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ThemeStyle {
    Warm,
    #[default]
    VSCode,
    Neutral,
}
impl ThemeStyle {
    pub const ALL: [(Self, &'static str); 3] = [
        (Self::Warm, "暖白"),
        (Self::VSCode, "蓝白"),
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
                [0xc3cedb, 0xb8d8f5, 0xb9dcd0, 0xd5c2ef, 0xf3d29d, 0xf0bfc5]
            }
            (ThemeStyle::VSCode, false) => {
                [0xbacbdf, 0xaed5fa, 0xa8dacd, 0xcbb7ed, 0xf0cb92, 0xeeb1c1]
            }
            (ThemeStyle::Neutral, false) => {
                [0xc8d0dc, 0xbcd6ef, 0xbddbd1, 0xd2c2e8, 0xebcfaa, 0xe9c0c8]
            }
            (ThemeStyle::Warm, true) => {
                [0x3e5266, 0x325d87, 0x315f51, 0x594375, 0x6d542f, 0x754552]
            }
            (ThemeStyle::VSCode, true) => {
                [0x354a60, 0x28547c, 0x285747, 0x50396e, 0x634c28, 0x6b394b]
            }
            (ThemeStyle::Neutral, true) => {
                [0x415267, 0x365d82, 0x365e53, 0x584573, 0x695330, 0x704753]
            }
        };
        rgb(colors[category % colors.len()]).into()
    }

    /// DiskTree category fills, mixed toward the active FindX surface so Warm /
    /// Neutral / VS Code light and dark stay coherent. This does not replace the
    /// application chrome palette.
    pub fn usage_hue(class: crate::disktree::classify::Category) -> (f32, f32) {
        use crate::disktree::classify::Category::*;
        match class {
            Code => (0.605, 1.0),
            AgentScratch => (0.065, 1.0),
            Toolchain => (0.415, 1.0),
            Synced => (0.535, 1.0),
            Git => (0.955, 1.0),
            Media => (0.745, 1.0),
            Cache => (0.125, 0.95),
            Documents => (0.10, 0.65),
            Other => (0.10, 0.04),
        }
    }
    pub fn usage_fill(&self, class: crate::disktree::classify::Category, depth: usize) -> Hsla {
        let (h, chroma) = Self::usage_hue(class);
        let step = depth.min(4) as f32;
        let dark = self.bg.l < 0.4;
        let (s, l) = if dark {
            (0.30 * chroma, step.mul_add(0.026, 0.205))
        } else {
            (0.34 * chroma, step.mul_add(-0.026, 0.91))
        };
        self.mix_toward_surface(Hsla { h, s, l, a: 1.0 }, 0.12)
    }
    pub fn usage_accent(&self, class: crate::disktree::classify::Category) -> Hsla {
        let (h, chroma) = Self::usage_hue(class);
        let dark = self.bg.l < 0.4;
        let (s, l) = if dark {
            (0.42 * chroma, 0.52)
        } else {
            (0.45 * chroma, 0.46)
        };
        Hsla { h, s, l, a: 1.0 }
    }
    fn mix_toward_surface(&self, color: Hsla, amount: f32) -> Hsla {
        let t = amount.clamp(0., 1.);
        Hsla {
            h: color.h, // preserve category hue: legend and tiles must not drift toward different hues
            s: color.s + (self.surface.s - color.s) * t,
            l: color.l + (self.surface.l - color.l) * t,
            a: 1.0,
        }
    }
    pub fn usage_class_label(class: crate::disktree::classify::Category) -> &'static str {
        use crate::disktree::classify::Category::*;
        match class {
            Code => "代码",
            AgentScratch => "代理草稿",
            Toolchain => "工具链",
            Synced => "同步",
            Git => "Git",
            Media => "媒体",
            Documents => "文档",
            Cache => "缓存",
            Other => "未分类",
        }
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

/// Translucent chrome for backdrop materials. Runs right after
/// [`apply_findx_style`]; solid leaves every computed color untouched.
/// Only the window frame (title bar / sidebar / status bar) goes translucent —
/// content, text and controls stay opaque for readability. Off Windows this
/// is a no-op: without a compositor backdrop the alpha would only dull colors.
pub fn apply_material_translucency(
    material: crate::window_material::WindowMaterial,
    cx: &mut App,
) {
    if !(cfg!(windows) && material.translucent_chrome()) {
        return;
    }
    fn dim(color: Hsla, keep: f32) -> Hsla {
        Hsla {
            a: (color.a * keep).clamp(0.0, 1.0),
            ..color
        }
    }
    cx.update_global::<FindxTokens, _>(|t, _| {
        t.title_bar = dim(t.title_bar, 0.55);
        t.surface = dim(t.surface, 0.72);
    });
    Theme::update(cx, |theme| {
        let c = &mut theme.colors;
        c.title_bar = dim(c.title_bar, 0.55);
        c.sidebar = dim(c.sidebar, 0.72);
        c.status_bar = dim(c.status_bar, 0.72);
    });
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
    #[test]fn usage_category_hues_match_legend_and_all_depths_keep_readable_labels(){
        use crate::disktree::classify::Category;
        for (style,_) in ThemeStyle::ALL{for appearance in [EffectiveTheme::Light,EffectiveTheme::Dark]{
            let t=FindxTokens::for_style(style,appearance);
            for c in Category::LEGEND.into_iter().chain(std::iter::once(Category::Other)){for depth in 1..=6{
                let fill=t.usage_fill(c,depth);assert_eq!(fill.h,t.usage_accent(c).h);
                assert!(contrast(t.text_primary,fill)>=4.5);
                let mut header=fill;header.l=(header.l-0.035).clamp(0.,1.);
                assert!(contrast(t.text_primary,header)>=4.5,"header contrast {:?} {:?} depth{}: {}",style,c,depth,contrast(t.text_primary,header));
            }}
        }}
    }

}
