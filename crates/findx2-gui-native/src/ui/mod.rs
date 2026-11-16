mod browser_table;
pub(super) mod chrome;
mod file_icon;
pub mod main_window;
mod result_table;
pub mod settings_window;
pub mod update_card;

use gpui_kit::*;

use crate::cli::LaunchOptions;
use crate::session::AppSession;
use crate::ui::main_window::MainWindow;
use crate::ui::settings_window::SettingsWindow;

fn launch_window_size() -> Size<Pixels> {
    if let Ok(raw) = std::env::var("FINDX_NATIVE_WINDOW_SIZE") {
        let lowered = raw.to_ascii_lowercase();
        let mut parts = lowered.split('x');
        if let (Some(w), Some(h)) = (parts.next(), parts.next()) {
            if let (Ok(w), Ok(h)) = (w.trim().parse::<f32>(), h.trim().parse::<f32>()) {
                if (560.0..4000.0).contains(&w) && (400.0..3000.0).contains(&h) {
                    return size(px(w), px(h));
                }
            }
        }
    }
    size(px(1100.), px(720.))
}

#[cfg(windows)]
fn set_official_app_user_model_id() {
    use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
    use windows::core::w;
    unsafe {
        let _ = SetCurrentProcessExplicitAppUserModelID(w!("FindX.FindX"));
    }
}

pub fn start(opts: LaunchOptions) {
    let title = opts.window_title().to_string();
    if opts.official_product {
        #[cfg(windows)]
        set_official_app_user_model_id();
    }
    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            let dark = matches!(
                cx.window_appearance(),
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            );
            let session = cx.new(|_| AppSession::load(&opts, dark));
            session.update(cx, |session, cx| {
                session.apply_theme(cx);
            });

            let roots_session = session.clone();
            cx.spawn(async move |cx| {
                let roots = cx
                    .background_spawn(async { crate::prefs::list_available_roots() })
                    .await;
                let _ = roots_session.update(cx, |session, cx| {
                    session.available_roots = roots;
                    cx.notify();
                });
            })
            .detach();

            let window_size = launch_window_size();
            let bounds = Bounds::centered(None, window_size, cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                focus: std::env::var_os("FINDX_NATIVE_USAGE_OPEN").is_none(),
                window_min_size: Some(size(px(560.), px(400.))),
                ..gpui_kit::component::TitleBar::window_options()
            };

            let session_for_window = session.clone();
            let title_for_window = title.clone();
            let _ = gpui_kit::open_window(options, cx, |window, cx| {
                window.set_window_title(&title_for_window);
                cx.new(|cx| MainWindow::new(session_for_window, window, cx))
            })
            .expect("open FindX native window");

            cx.on_window_closed(|cx, _id| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
        });
}

pub fn open_settings_window(session: Entity<AppSession>, cx: &mut App) {
    if let Some(handle) = session.read(cx).settings_window {
        if handle
            .update(cx, |_, window, _| {
                window.activate_window();
            })
            .is_ok()
        {
            return;
        }
    }

    let official = session.read(cx).official_product;
    let live = session.read(cx).live;
    let title = if official {
        "FindX 设置"
    } else if live {
        "FindX2 设置 (native)"
    } else {
        "FindX2 设置 (isolated)"
    };

    let bounds = Bounds::centered(None, size(px(760.), px(680.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(480.), px(520.))),
        ..gpui_kit::component::TitleBar::window_options()
    };
    let session_for_view = session.clone();
    match gpui_kit::open_window(options, cx, |window, cx| {
        window.set_window_title(title);
        cx.new(|cx| SettingsWindow::new(session_for_view, window, cx))
    }) {
        Ok((handle, _)) => {
            session.update(cx, |session, _| {
                session.settings_window = Some(handle);
            });
        }
        Err(err) => eprintln!("findx2-gui-native: failed to open settings: {err:?}"),
    }
}
