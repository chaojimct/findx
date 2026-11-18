#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::time::Duration;

use findx2_gui_native::cli;
use findx2_gui_native::mock_backend;
use findx2_gui_native::ui;

fn main() {
    let mut raw: Vec<String> = std::env::args().collect();
    if raw.get(1).is_some_and(|s|s=="--installer-health") {
        if raw.len()!=3 { std::process::exit(2); }
        raw=match findx2_gui_native::installer_health::prepare(&raw[2]) {Ok(v)=>v,Err(_)=>std::process::exit(2)};
    }
    #[cfg(windows)]
    if raw.get(1).is_some_and(|s| s == "--office-preview-host") {
        findx2_gui_native::office_host::host_main(&raw);
        return;
    }
    if raw.len() == 2 && matches!(raw[1].as_str(), "--version" | "-V") {
        attach_cli_console();
        println!("FindX {} (native)", env!("CARGO_PKG_VERSION"));
        return;
    }
    if raw.get(1).is_some_and(|s| s == "--document-preview-worker") {
        findx2_gui_native::preview_worker::worker_main();
        return;
    }
    if raw.iter().any(|a| a == "-h" || a == "--help") {
        attach_cli_console();
        print!("{}", cli::help_text());
        return;
    }
    let opts = match cli::parse_args(raw) {
        Ok(opts) => opts,
        Err(err) => {
            attach_cli_console();
            eprintln!("{err}");
            std::process::exit(2);
        }
    };

    // Release official/GUI starts must not attach a console or print diagnostics
    // that would surface in a console window. Keep traces on --diag or debug builds.
    if opts.diag || cfg!(debug_assertions) {
        attach_cli_console();
        if opts.mock {
            eprintln!(
                "findx2-gui-native: starting isolated mock backend on {}",
                opts.endpoint
            );
        } else if opts.live {
            eprintln!(
                "findx2-gui-native: live client mode, connecting to {} (read-only; will not start or restart findx2-service)",
                opts.endpoint
            );
        } else {
            eprintln!(
                "findx2-gui-native: connecting to isolated endpoint {} (will not use production pipe findx2)",
                opts.endpoint
            );
        }
    }

    if opts.mock {
        let _handle = mock_backend::spawn_mock_server(opts.endpoint.clone());
        std::thread::sleep(Duration::from_millis(120));
    }

    findx2_gui_native::installer_health::checkpoint("enter-ui-start");
    ui::start(opts);
    findx2_gui_native::installer_health::checkpoint("event-loop-returned");
    if !findx2_gui_native::installer_health::finished() {std::process::exit(82);}
}

fn attach_cli_console() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::Storage::FileSystem::{
            FILE_TYPE_DISK, FILE_TYPE_PIPE, GetFileType,
        };
        use windows::Win32::System::Console::{
            ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole, GetConsoleWindow,
            GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
        };

        // If stdout/stderr are already pipes or files (cmd redirection, CI),
        // print there and do not AllocConsole or freopen over the handles.
        let redirected = [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE].into_iter().any(|id| {
            GetStdHandle(id).ok().is_some_and(|h| {
                let kind = GetFileType(h);
                kind == FILE_TYPE_DISK || kind == FILE_TYPE_PIPE
            })
        });
        if redirected {
            return;
        }

        if GetConsoleWindow().0.is_null() {
            if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
                let _ = AllocConsole();
            }
        }
        // Reconnect the MSVC CRT so println! / eprintln! reach the console.
        unsafe extern "C" {
            fn freopen(filename: *const i8, mode: *const i8, stream: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
            fn __acrt_iob_func(index: i32) -> *mut std::ffi::c_void;
        }
        let r = b"r\0".as_ptr() as *const i8;
        let w = b"w\0".as_ptr() as *const i8;
        let conin = b"CONIN$\0".as_ptr() as *const i8;
        let conout = b"CONOUT$\0".as_ptr() as *const i8;
        let _ = freopen(conin, r, __acrt_iob_func(0));
        let _ = freopen(conout, w, __acrt_iob_func(1));
        let _ = freopen(conout, w, __acrt_iob_func(2));
    }
}
