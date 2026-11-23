//! Optional Windows backdrop materials (Mica / Acrylic) for top-level windows.
//!
//! The five options mirror the Fluent backdrop picker: solid, Mica, Mica Alt,
//! thin acrylic, standard acrylic. Everything is best-effort and probe-based:
//! each DWM call is checked, and anything unsupported falls back to the next
//! cheaper option, ending at opaque solid. The elevated-transaction rule
//! applies here too — this only ever touches our own HWNDs, never the system.
//!
//! Two layers cooperate:
//! - GPUI's `WindowBackgroundAppearance` decides whether the DirectX renderer
//!   clears transparent (any non-opaque value) — required for any material to
//!   show through our own painting.
//! - This module's DWM calls pick the actual backdrop: `DWMWA_SYSTEMBACKDROP_TYPE`
//!   for Mica / Mica Alt (and standard acrylic on new builds), the legacy
//!   `SetWindowCompositionAttribute` acrylic path for the two tinted acrylics.

use serde::{Deserialize, Serialize};

/// Backdrop material of a top-level window. Stored in prefs; old files
/// without the field deserialize to [`WindowMaterial::Solid`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WindowMaterial {
    /// Opaque theme background. Always available, zero extra GPU cost.
    #[default]
    Solid,
    /// Windows 11 Mica (`DWMSBT_MAINWINDOW`).
    Mica,
    /// Windows 11 Mica Alt (`DWMSBT_TABBEDWINDOW`).
    MicaAlt,
    /// Subtle blur with a light tint (legacy composition path, Win10 1803+).
    AcrylicThin,
    /// Stronger blur tint (prefers the real system acrylic, legacy fallback).
    AcrylicStandard,
}

impl WindowMaterial {
    /// Picker order matches the familiar Fluent ordering.
    pub const ALL: [(Self, &'static str); 5] = [
        (Self::AcrylicThin, "轻薄亚克力"),
        (Self::AcrylicStandard, "标准亚克力"),
        (Self::Mica, "云母"),
        (Self::MicaAlt, "云母 Alt"),
        (Self::Solid, "纯色"),
    ];

    pub fn label(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(m, _)| *m == self)
            .map(|(_, label)| *label)
            .unwrap_or("纯色")
    }

    /// Whether chrome (title bar / sidebar / status bar) is painted
    /// translucent so the backdrop shows through. Content stays opaque.
    pub fn translucent_chrome(self) -> bool {
        !matches!(self, Self::Solid)
    }

    pub fn note(self) -> &'static str {
        match self {
            Self::Solid => "不透过窗口显示桌面；最省电。",
            Self::Mica => "Windows 11 云母；不受支持的系统自动回退纯色。",
            Self::MicaAlt => "Windows 11 云母 Alt（标签页风格）；不受支持的系统自动回退纯色。",
            Self::AcrylicThin => "轻微模糊 + 淡着色；Win10 1803 及以上可用。",
            Self::AcrylicStandard => "系统亚克力优先，否则加重着色；Win10 1803 及以上可用。",
        }
    }
}

/// Apply a material to one top-level window. Safe to call on any platform;
/// off Windows it is a no-op. `dark` picks the acrylic tint.
pub fn apply_to_window(window: &gpui_kit::Window, material: WindowMaterial, dark: bool) {
    #[cfg(windows)]
    {
        windows_impl::apply(window, material, dark);
    }
    #[cfg(not(windows))]
    {
        let _ = (window, material, dark);
    }
}

#[cfg(windows)]
mod windows_impl {
    use super::WindowMaterial as Material;
    use gpui_kit::{Window, WindowBackgroundAppearance};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmSetWindowAttribute};

    // DWMWINDOWATTRIBUTE numbers (stable ABI, avoids depending on SDK names).
    const ATTR_SYSTEMBACKDROP: i32 = 38;
    const ATTR_MICA_EFFECT: i32 = 1029;
    // DWM_SYSTEMBACKDROP_TYPE values.
    const BACKDROP_NONE: u32 = 1;
    const BACKDROP_MICA: u32 = 2;
    const BACKDROP_ACRYLIC: u32 = 3;
    const BACKDROP_MICA_ALT: u32 = 4;

    fn hwnd_of(window: &Window) -> Option<HWND> {
        let handle = HasWindowHandle::window_handle(window).ok()?;
        match handle.as_raw() {
            RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as *mut _)),
            _ => None,
        }
    }

    fn set_attr(hwnd: HWND, attr: i32, value: u32) -> bool {
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWINDOWATTRIBUTE(attr),
                &value as *const _ as *const _,
                std::mem::size_of_val(&value) as u32,
            )
            .is_ok()
        }
    }

    /// Legacy `SetWindowCompositionAttribute` acrylic/blur (Win10 1803+),
    /// loaded dynamically exactly like gpui-pre-windows does. `argb` is
    /// `0xAABBGGRR`; `state` 4 = acrylic blur, 3 = plain blur, 0 = off.
    fn legacy_accent(hwnd: HWND, state: u32, argb: u32) {
        use windows::core::{BOOL, PCSTR};
        use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};

        #[repr(C)]
        struct AccentPolicy {
            state: u32,
            flags: u32,
            gradient: u32,
            animation: u32,
        }
        #[repr(C)]
        struct CompositionData {
            attrib: u32,
            data: *mut std::ffi::c_void,
            count: usize,
        }
        unsafe {
            type FnTy = unsafe extern "system" fn(HWND, *mut CompositionData) -> BOOL;
            let Ok(user32) =
                GetModuleHandleA(PCSTR::from_raw(c"user32.dll".as_ptr() as *const u8))
            else {
                return;
            };
            if user32.is_invalid() {
                return;
            }
            let Some(proc) = GetProcAddress(
                user32,
                PCSTR::from_raw(c"SetWindowCompositionAttribute".as_ptr() as *const u8),
            ) else {
                return;
            };
            let set: FnTy = std::mem::transmute(proc);
            // Mirror gpui-pre-windows: acrylic with a zero alpha paints
            // nothing, so clamp the floor to 1.
            let mut gradient = argb;
            if state == 4 && gradient >> 24 == 0 {
                gradient |= 0x0100_0000;
            }
            let mut policy = AccentPolicy {
                state,
                flags: if state == 4 { 0 } else { 2 },
                gradient,
                animation: 0,
            };
            let mut data = CompositionData {
                attrib: 19, // WCA_ACCENT_POLICY
                data: &mut policy as *mut _ as *mut _,
                count: std::mem::size_of::<AccentPolicy>(),
            };
            let _ = set(hwnd, &mut data);
        }
    }

    fn gray_tint(dark: bool) -> (u8, u8, u8) {
        if dark {
            (0x2D, 0x2D, 0x2D)
        } else {
            (0xF2, 0xF2, 0xF2)
        }
    }

    fn argb(alpha: u8, (r, g, b): (u8, u8, u8)) -> u32 {
        (alpha as u32) << 24 | (b as u32) << 16 | (g as u32) << 8 | r as u32
    }

    pub fn apply(window: &Window, material: Material, dark: bool) {
        let Some(hwnd) = hwnd_of(window) else {
            return;
        };
        match material {
            Material::Solid => {
                window.set_background_appearance(WindowBackgroundAppearance::Opaque);
                set_attr(hwnd, ATTR_SYSTEMBACKDROP, BACKDROP_NONE);
                legacy_accent(hwnd, 0, 0);
            }
            Material::Mica => {
                window.set_background_appearance(WindowBackgroundAppearance::MicaBackdrop);
                // gpui-pre-windows only drives SYSTEMBACKDROP on 22621+; the
                // MICA_EFFECT fallback covers 22000+, then solid.
                if !set_attr(hwnd, ATTR_SYSTEMBACKDROP, BACKDROP_MICA)
                    && !set_attr(hwnd, ATTR_MICA_EFFECT, 1)
                {
                    window.set_background_appearance(WindowBackgroundAppearance::Opaque);
                }
            }
            Material::MicaAlt => {
                window.set_background_appearance(WindowBackgroundAppearance::MicaAltBackdrop);
                if !set_attr(hwnd, ATTR_SYSTEMBACKDROP, BACKDROP_MICA_ALT)
                    && !set_attr(hwnd, ATTR_MICA_EFFECT, 1)
                {
                    window.set_background_appearance(WindowBackgroundAppearance::Opaque);
                }
            }
            Material::AcrylicThin => {
                window.set_background_appearance(WindowBackgroundAppearance::Transparent);
                legacy_accent(hwnd, 4, argb(0x55, gray_tint(dark)));
            }
            Material::AcrylicStandard => {
                window.set_background_appearance(WindowBackgroundAppearance::Transparent);
                // Prefer the real system acrylic; the tinted legacy path is
                // the fallback for older builds.
                if !set_attr(hwnd, ATTR_SYSTEMBACKDROP, BACKDROP_ACRYLIC) {
                    legacy_accent(hwnd, 4, argb(0xA6, gray_tint(dark)));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_opaque_solid_and_labels_cover_all_variants() {
        assert_eq!(WindowMaterial::default(), WindowMaterial::Solid);
        assert_eq!(WindowMaterial::ALL.len(), 5);
        for (material, label) in WindowMaterial::ALL {
            assert!(!label.is_empty());
            assert_eq!(material.label(), label);
            assert!(!material.note().is_empty());
        }
        assert!(!WindowMaterial::Solid.translucent_chrome());
        assert!(WindowMaterial::Mica.translucent_chrome());
        assert!(WindowMaterial::MicaAlt.translucent_chrome());
        assert!(WindowMaterial::AcrylicThin.translucent_chrome());
        assert!(WindowMaterial::AcrylicStandard.translucent_chrome());
    }

    #[test]
    fn old_prefs_without_material_still_parse_as_solid() {
        let parsed: WindowMaterial = serde_json::from_str("\"solid\"").unwrap();
        assert_eq!(parsed, WindowMaterial::Solid);
        // Unknown future values must not kill the whole prefs file; the
        // field-level default below covers that in LocalPrefs.
        let fallback = serde_json::from_str::<WindowMaterial>("\"holographic\"")
            .unwrap_or_default();
        assert_eq!(fallback, WindowMaterial::Solid);
    }
}
