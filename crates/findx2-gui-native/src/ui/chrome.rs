//! Client-side Windows chrome built on supported GPUI hit regions and window APIs.
use crate::theme::FindxTokens;
use gpui_kit::component::{
    Icon,
    button::{Button, ButtonVariants as _},
    h_flex,
};
use gpui_kit::*;
/// Native WM_CLOSE is important: unlike remove_window(), it runs the settings dirty-close guard.
fn request_close(window: &mut Window) {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Ok(handle) = HasWindowHandle::window_handle(window) {
            if let RawWindowHandle::Win32(h) = handle.as_raw() {
                use windows::Win32::{
                    Foundation::{HWND, LPARAM, WPARAM},
                    UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE},
                };
                let _ = unsafe {
                    PostMessageW(
                        Some(HWND(h.hwnd.get() as *mut _)),
                        WM_CLOSE,
                        WPARAM(0),
                        LPARAM(0),
                    )
                };
            }
        }
    }
    #[cfg(not(windows))]
    window.remove_window();
}
fn toggle_maximize(window: &mut Window) {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Ok(handle) = HasWindowHandle::window_handle(window) {
            if let RawWindowHandle::Win32(h) = handle.as_raw() {
                use windows::Win32::{
                    Foundation::HWND,
                    UI::WindowsAndMessaging::{IsZoomed, SW_MAXIMIZE, SW_RESTORE, ShowWindowAsync},
                };
                let hwnd = HWND(h.hwnd.get() as *mut _);
                unsafe {
                    let command = if IsZoomed(hwnd).as_bool() {
                        SW_RESTORE
                    } else {
                        SW_MAXIMIZE
                    };
                    let _ = ShowWindowAsync(hwnd, command);
                }
            }
        }
    }
    #[cfg(not(windows))]
    window.zoom_window();
}
struct DragState {
    pressed: bool,
    native_pending: bool,
}
impl Render for DragState {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
fn begin_native_drag(window: &mut Window) -> bool {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Ok(handle) = HasWindowHandle::window_handle(window) {
            if let RawWindowHandle::Win32(h) = handle.as_raw() {
                use windows::Win32::{
                    Foundation::{HWND, LPARAM, POINT, WPARAM},
                    UI::{
                        Input::KeyboardAndMouse::{GetCapture, ReleaseCapture},
                        WindowsAndMessaging::{
                            GetCursorPos, HTCAPTION, PostMessageW, WM_NCLBUTTONDOWN,
                        },
                    },
                };
                let hwnd = HWND(h.hwnd.get() as *mut _);
                let mut p = POINT::default();
                unsafe {
                    if GetCursorPos(&mut p).is_err() {
                        return false;
                    }
                    if GetCapture() == hwnd {
                        let _ = ReleaseCapture();
                    }
                    let point = ((p.y as u16 as u32) << 16) | (p.x as u16 as u32);
                    return PostMessageW(
                        Some(hwnd),
                        WM_NCLBUTTONDOWN,
                        WPARAM(HTCAPTION as usize),
                        LPARAM(point as isize),
                    )
                    .is_ok();
                }
            }
        }
        false
    }
    #[cfg(not(windows))]
    {
        window.start_window_move();
        true
    }
}
pub fn title_bar(content: impl IntoElement, window: &mut Window, cx: &mut App) -> impl IntoElement {
    title_bar_with_tools(content, div(), window, cx)
}
pub fn title_bar_with_tools(
    content: impl IntoElement,
    tools: impl IntoElement,
    window: &mut Window,
    cx: &mut App,
) -> impl IntoElement {
    let tokens = *cx.global::<FindxTokens>();
    let state = window.use_state(cx, |_, _| DragState {
        pressed: false,
        native_pending: false,
    });
    h_flex()
        .w_full()
        .h(px(36.))
        .flex_shrink_0()
        .bg(tokens.surface)
        .text_color(tokens.text_primary)
        .border_b_1()
        .border_color(tokens.border)
        .child(content)
        .child(
            div()
                .id("title-drag-region")
                .flex_1()
                .min_w(px(24.))
                .h_full()
                .window_control_area(WindowControlArea::Drag)
                .on_mouse_down(
                    MouseButton::Left,
                    window.listener_for(&state, |state, event: &MouseDownEvent, window, cx| {
                        if state.native_pending {
                            state.native_pending = false;
                            state.pressed = false;
                            return;
                        }
                        if event.click_count == 2 {
                            state.pressed = false;
                            toggle_maximize(window);
                            window.prevent_default();
                            cx.stop_propagation();
                        } else {
                            state.pressed = true;
                        }
                    }),
                )
                .on_mouse_up(
                    MouseButton::Left,
                    window.listener_for(&state, |state, _, _, _| {
                        state.pressed = false;
                    }),
                )
                .on_mouse_move(window.listener_for(
                    &state,
                    |state, event: &MouseMoveEvent, window, _| {
                        if state.pressed && event.pressed_button == Some(MouseButton::Left) {
                            state.pressed = false;
                            state.native_pending = begin_native_drag(window);
                        }
                    },
                )),
        )
        .child(tools)
        .child(
            h_flex()
                .h_full()
                .flex_shrink_0()
                .child(
                    Button::new("caption-minimize")
                        .ghost()
                        .icon(Icon::default().path("icons/window-minimize.svg"))
                        .tooltip("最小化")
                        .accessibility_label("最小化")
                        .w(px(40.))
                        .h_full()
                        .window_control_area(WindowControlArea::Min)
                        .on_click(|_, w, _| w.minimize_window()),
                )
                .child(
                    Button::new("caption-maximize")
                        .ghost()
                        .icon(Icon::default().path(if window.is_maximized() {
                            "icons/minimize-2.svg"
                        } else {
                            "icons/window-maximize.svg"
                        }))
                        .tooltip(if window.is_maximized() {
                            "还原"
                        } else {
                            "最大化"
                        })
                        .accessibility_label(if window.is_maximized() {
                            "还原"
                        } else {
                            "最大化"
                        })
                        .w(px(40.))
                        .h_full()
                        .window_control_area(WindowControlArea::Max)
                        .on_click(|_, w, _| toggle_maximize(w)),
                )
                .child(
                    Button::new("caption-close")
                        .ghost()
                        .icon(Icon::default().path("icons/window-close.svg"))
                        .tooltip("关闭窗口")
                        .accessibility_label("关闭窗口")
                        .w(px(40.))
                        .h_full()
                        .window_control_area(WindowControlArea::Close)
                        .on_click(|_, w, _| request_close(w)),
                ),
        )
}

pub(super) fn capture_pointer(window: &Window) {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Ok(h) = HasWindowHandle::window_handle(window) {
            if let RawWindowHandle::Win32(h) = h.as_raw() {
                unsafe {
                    windows::Win32::UI::Input::KeyboardAndMouse::SetCapture(
                        windows::Win32::Foundation::HWND(h.hwnd.get() as *mut _),
                    );
                }
            }
        }
    }
}
pub(super) fn release_pointer(window: &Window) {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Ok(h) = HasWindowHandle::window_handle(window) {
            if let RawWindowHandle::Win32(h) = h.as_raw() {
                unsafe {
                    use windows::Win32::UI::Input::KeyboardAndMouse::{GetCapture, ReleaseCapture};
                    if GetCapture() == windows::Win32::Foundation::HWND(h.hwnd.get() as *mut _) {
                        let _ = ReleaseCapture();
                    }
                }
            }
        }
    }
}
