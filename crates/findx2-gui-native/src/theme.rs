//! 把现有 `design-tokens.css` / `themeColors.ts` 的色值映射到 GPUI Kit 主题。

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;

use crate::settings_logic::EffectiveTheme;

impl Global for FindxTokens {}

#[derive(Clone, Copy)]
pub struct FindxTokens {
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
    pub fn for_theme(theme: EffectiveTheme) -> Self {
        if theme.is_dark() {
            Self {
                bg: rgb(0x1b1b1b).into(),
                surface: rgb(0x242424).into(),
                surface_raised: rgb(0x2c2c2c).into(),
                border: rgb(0x3d3d3d).into(),
                border_soft: rgb(0x333333).into(),
                text_primary: rgb(0xf2f2f2).into(),
                text_secondary: rgb(0xc8c8c8).into(),
                text_tertiary: rgb(0x9a9a9a).into(),
                accent: rgb(0x4da3ff).into(),
                accent_hover: rgb(0x6bb4ff).into(),
                hover: rgb(0x303030).into(),
                row_selected: rgb(0x1e3a5f).into(),
                search_box_bg: rgb(0x2c2c2c).into(),
                hit_bg: rgb(0x6a5400).into(),
                hit_fg: rgb(0xfff8e1).into(),
                success: rgb(0x6ccb5f).into(),
                warning: rgb(0xf7b955).into(),
                error: rgb(0xe74856).into(),
                title_bar: rgb(0x242424).into(),
            }
        } else {
            Self {
                bg: rgb(0xfafafa).into(),
                surface: rgb(0xffffff).into(),
                surface_raised: rgb(0xffffff).into(),
                border: rgb(0xd8d8d8).into(),
                border_soft: rgb(0xe8e8e8).into(),
                text_primary: rgb(0x1f1f1f).into(),
                text_secondary: rgb(0x555555).into(),
                text_tertiary: rgb(0x888888).into(),
                accent: rgb(0x0078d4).into(),
                accent_hover: rgb(0x106ebe).into(),
                hover: rgb(0xf5f5f5).into(),
                row_selected: rgb(0xe3f2fd).into(),
                search_box_bg: rgb(0xf5f5f5).into(),
                hit_bg: rgb(0xffecb3).into(),
                hit_fg: rgb(0x0d47a1).into(),
                success: rgb(0x107c10).into(),
                warning: rgb(0xd28a3a).into(),
                error: rgb(0xc42b1c).into(),
                title_bar: rgb(0xebebeb).into(),
            }
        }
    }
}

pub fn apply_findx_theme(theme: EffectiveTheme, cx: &mut App) {
    let mode = if theme.is_dark() {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    };
    Theme::change(mode, None, cx);
    // A file table needs discoverable, draggable scrollbars even while idle.
    Theme::set_scrollbar_mode(gpui_kit::component::scroll::ScrollbarMode::Always, cx);
    cx.set_global(FindxTokens::for_theme(theme));
}
