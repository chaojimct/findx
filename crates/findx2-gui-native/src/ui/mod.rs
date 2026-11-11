pub mod main_window;
pub mod settings_window;
pub mod update_card;

use gpui_kit::*;

use crate::cli::LaunchOptions;
use crate::session::AppSession;
use crate::ui::main_window::MainWindow;
use crate::ui::settings_window::SettingsWindow;

pub fn start(opts: LaunchOptions) {
    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        let session = cx.new(|_| AppSession::load(&opts, false));
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

        let bounds = Bounds::centered(None, size(px(1100.), px(720.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(560.), px(400.))),
            titlebar: Some(TitlebarOptions {
                title: Some("FindX Native (isolated)".into()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let session_for_window = session.clone();
        let _ = gpui_kit::open_window(options, cx, |window, cx| {
            window.set_window_title("FindX Native (isolated)");
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

    let bounds = Bounds::centered(None, size(px(720.), px(820.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(480.), px(520.))),
        titlebar: Some(TitlebarOptions {
            title: Some("FindX2 设置 (isolated)".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let session_for_view = session.clone();
    match gpui_kit::open_window(options, cx, |window, cx| {
        window.set_window_title("FindX2 设置 (isolated)");
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
