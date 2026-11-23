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

/// What the running OS can actually do. Probed once from ntdll
/// `RtlGetVersion` (no new crate features needed) and cached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OsBackdropSupport {
    /// Windows build number, e.g. 22621.
    pub build: u32,
    /// `DWMWA_MICA_EFFECT` (Windows 11 21H2 / 22000+).
    pub mica_effect: bool,
    /// `DWMWA_SYSTEMBACKDROP_TYPE` (22H2 / 22621+).
    pub system_backdrop: bool,
    /// `SetWindowCompositionAttribute` acrylic (Win10 1803 / 17134+).
    pub legacy_acrylic: bool,
}

/// Cached OS capability probe for the settings picker. Off Windows
/// everything reports unsupported (the card itself is Windows-gated).
pub fn os_support() -> OsBackdropSupport {
    #[cfg(windows)]
    {
        probe_os_support()
    }
    #[cfg(not(windows))]
    {
        OsBackdropSupport::for_build(0)
    }
}

impl OsBackdropSupport {
    pub fn for_build(build: u32) -> Self {
        Self {
            build,
            mica_effect: build >= 22000,
            system_backdrop: build >= 22621,
            legacy_acrylic: build >= 17134,
        }
    }

    /// Unsupported options are disabled in the picker instead of failing
    /// silently after the click.
    pub fn supports(self, material: WindowMaterial) -> bool {
        match material {
            WindowMaterial::Solid => true,
            WindowMaterial::Mica | WindowMaterial::MicaAlt => self.mica_effect,
            WindowMaterial::AcrylicThin | WindowMaterial::AcrylicStandard => {
                self.legacy_acrylic
            }
        }
    }
}

#[cfg(windows)]
fn probe_os_support() -> OsBackdropSupport {
    use std::sync::atomic::{AtomicU32, Ordering};
    static CACHED_BUILD: AtomicU32 = AtomicU32::new(0);
    let build = CACHED_BUILD.load(Ordering::Relaxed);
    if build != 0 {
        return OsBackdropSupport::for_build(build);
    }
    let build = os_build_number().unwrap_or(0);
    if build != 0 {
        CACHED_BUILD.store(build, Ordering::Relaxed);
    }
    OsBackdropSupport::for_build(build)
}

/// Raw build number via ntdll, bypassing the deprecated version helpers.
#[cfg(windows)]
fn os_build_number() -> Option<u32> {
    use windows::core::PCSTR;
    use windows::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};

    #[repr(C)]
    struct OsVersionInfo {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        csd: [u16; 128],
    }
    unsafe {
        type FnTy = unsafe extern "system" fn(*mut OsVersionInfo) -> i32;
        let ntdll =
            GetModuleHandleA(PCSTR::from_raw(c"ntdll.dll".as_ptr() as *const u8)).ok()?;
        if ntdll.is_invalid() {
            return None;
        }
        let proc = GetProcAddress(
            ntdll,
            PCSTR::from_raw(c"RtlGetVersion".as_ptr() as *const u8),
        )?;
        let get: FnTy = std::mem::transmute(proc);
        let mut info = OsVersionInfo {
            size: std::mem::size_of::<OsVersionInfo>() as u32,
            major: 0,
            minor: 0,
            build: 0,
            platform: 0,
            csd: [0; 128],
        };
        if get(&mut info) == 0 && info.build != 0 {
            Some(info.build)
        } else {
            None
        }
    }
}

/// Chrome translucency for backdrop materials, shared by the settings slider
/// and the theme hook. Solid ignores it entirely.
pub fn clamp_opacity(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.25, 1.0)
    } else {
        0.7
    }
}

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

/// Apply a material to one top-level window and return what actually stuck
/// (unsupported materials degrade to [`WindowMaterial::Solid`]). Safe to
/// call on any platform; off Windows it is a no-op. `dark` picks the
/// acrylic tint.
pub fn apply_to_window(
    window: &gpui_kit::Window,
    material: WindowMaterial,
    dark: bool,
) -> WindowMaterial {
    #[cfg(windows)]
    {
        windows_impl::apply(window, material, dark)
    }
    #[cfg(not(windows))]
    {
        let _ = (window, dark);
        material
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
        let result = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWINDOWATTRIBUTE(attr),
                &value as *const _ as *const _,
                std::mem::size_of_val(&value) as u32,
            )
        };
        let ok = result.is_ok();
        eprintln!(
            "[material] DwmSetWindowAttribute hwnd={:?} attr={} value={} ok={} err={:?}",
            hwnd,
            attr,
            value,
            ok,
            result.err()
        );
        ok
    }

    fn read_backdrop(hwnd: HWND) -> String {
        use windows::Win32::Graphics::Dwm::DwmGetWindowAttribute;
        let mut value: u32 = 0xFFFF_FFFF;
        let result = unsafe {
            DwmGetWindowAttribute(
                hwnd,
                DWMWINDOWATTRIBUTE(ATTR_SYSTEMBACKDROP),
                &mut value as *mut _ as *mut _,
                std::mem::size_of_val(&value) as u32,
            )
        };
        match result {
            Ok(()) => format!("ok=true value={}", value),
            Err(e) => format!("ok=false err={:?} value={}", e, value),
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

    /// Returns the material that actually stuck; anything unsupported
    /// degrades to solid so the UI can say so instead of failing silently.
    pub fn apply(window: &Window, material: Material, dark: bool) -> Material {
        let effective = apply_inner(window, material, dark);
        if let Some(hwnd) = hwnd_of(window) {
            eprintln!(
                "[material] effective={:?} readback=[{}]",
                effective,
                read_backdrop(hwnd)
            );
        }
        effective
    }

    fn apply_inner(window: &Window, material: Material, dark: bool) -> Material {
        use super::probe_os_support;
        let support = probe_os_support();
        eprintln!(
            "[material] apply requested={:?} dark={} build={} sys={} micafx={} legacy={}",
            material,
            dark,
            support.build,
            support.system_backdrop,
            support.mica_effect,
            support.legacy_acrylic
        );
        let Some(hwnd) = hwnd_of(window) else {
            eprintln!("[material] no HWND, staying solid");
            return Material::Solid;
        };
        // Skip calls the OS cannot honor; every remaining setter below is
        // still checked, so a lie here only costs a fallback, never a hang.
        match material {
            Material::Solid => {
                window.set_background_appearance(WindowBackgroundAppearance::Opaque);
                set_attr(hwnd, ATTR_SYSTEMBACKDROP, BACKDROP_NONE);
                legacy_accent(hwnd, 0, 0);
                Material::Solid
            }
            Material::Mica => {
                window.set_background_appearance(WindowBackgroundAppearance::MicaBackdrop);
                // gpui-pre-windows only drives SYSTEMBACKDROP on 22621+; the
                // MICA_EFFECT fallback covers 22000+, then solid.
                let ok = (support.system_backdrop
                    && set_attr(hwnd, ATTR_SYSTEMBACKDROP, BACKDROP_MICA))
                    || (support.mica_effect && set_attr(hwnd, ATTR_MICA_EFFECT, 1));
                if ok {
                    Material::Mica
                } else {
                    window.set_background_appearance(WindowBackgroundAppearance::Opaque);
                    Material::Solid
                }
            }
            Material::MicaAlt => {
                window.set_background_appearance(WindowBackgroundAppearance::MicaAltBackdrop);
                let ok = (support.system_backdrop
                    && set_attr(hwnd, ATTR_SYSTEMBACKDROP, BACKDROP_MICA_ALT))
                    || (support.mica_effect && set_attr(hwnd, ATTR_MICA_EFFECT, 1));
                if ok {
                    Material::MicaAlt
                } else {
                    window.set_background_appearance(WindowBackgroundAppearance::Opaque);
                    Material::Solid
                }
            }
            Material::AcrylicThin => {
                if !support.legacy_acrylic {
                    window.set_background_appearance(WindowBackgroundAppearance::Opaque);
                    return Material::Solid;
                }
                window.set_background_appearance(WindowBackgroundAppearance::Transparent);
                legacy_accent(hwnd, 4, argb(0x55, gray_tint(dark)));
                Material::AcrylicThin
            }
            Material::AcrylicStandard => {
                window.set_background_appearance(WindowBackgroundAppearance::Transparent);
                // Prefer the real system acrylic; the tinted legacy path is
                // the fallback for older builds.
                if support.system_backdrop
                    && set_attr(hwnd, ATTR_SYSTEMBACKDROP, BACKDROP_ACRYLIC)
                {
                    Material::AcrylicStandard
                } else if support.legacy_acrylic {
                    legacy_accent(hwnd, 4, argb(0xA6, gray_tint(dark)));
                    Material::AcrylicStandard
                } else {
                    window.set_background_appearance(WindowBackgroundAppearance::Opaque);
                    Material::Solid
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
    fn support_matrix_follows_windows_build_numbers() {
        // Below Win10 1803: solid only.
        let ancient = OsBackdropSupport::for_build(17133);
        assert!(ancient.supports(WindowMaterial::Solid));
        assert!(!ancient.supports(WindowMaterial::AcrylicThin));
        assert!(!ancient.supports(WindowMaterial::AcrylicStandard));
        assert!(!ancient.supports(WindowMaterial::Mica));
        assert!(!ancient.supports(WindowMaterial::MicaAlt));
        // Win10 1803+: legacy acrylic joins.
        let win10 = OsBackdropSupport::for_build(19045);
        assert!(win10.supports(WindowMaterial::AcrylicThin));
        assert!(win10.supports(WindowMaterial::AcrylicStandard));
        assert!(!win10.supports(WindowMaterial::Mica));
        assert!(!win10.supports(WindowMaterial::MicaAlt));
        // Win11 21H2: Mica effect, still no SYSTEMBACKDROP_TYPE.
        let win11_21h2 = OsBackdropSupport::for_build(22000);
        assert!(win11_21h2.supports(WindowMaterial::Mica));
        assert!(win11_21h2.supports(WindowMaterial::MicaAlt));
        assert!(!win11_21h2.system_backdrop);
        // Win11 22H2+: everything.
        let win11_22h2 = OsBackdropSupport::for_build(22621);
        assert!(win11_22h2.system_backdrop);
        for material in [
            WindowMaterial::Solid,
            WindowMaterial::Mica,
            WindowMaterial::MicaAlt,
            WindowMaterial::AcrylicThin,
            WindowMaterial::AcrylicStandard,
        ] {
            assert!(win11_22h2.supports(material));
        }
    }

    #[test]
    fn opacity_clamp_keeps_chrome_usable() {
        assert_eq!(clamp_opacity(0.7), 0.7);
        assert_eq!(clamp_opacity(0.0), 0.25);
        assert_eq!(clamp_opacity(2.0), 1.0);
        assert_eq!(clamp_opacity(f32::NAN), 0.7);
        assert_eq!(clamp_opacity(f32::INFINITY), 0.7);
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
