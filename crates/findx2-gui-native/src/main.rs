use std::time::Duration;

use findx2_gui_native::cli;
use findx2_gui_native::mock_backend;
use findx2_gui_native::ui;

fn main() {
    let raw: Vec<String> = std::env::args().collect();
    #[cfg(windows)]
    if raw.get(1).is_some_and(|s| s == "--office-preview-host") {
        findx2_gui_native::office_host::host_main(&raw);
        return;
    }
    if raw.len() == 2 && matches!(raw[1].as_str(), "--version" | "-V") {
        println!("FindX {} (native)", env!("CARGO_PKG_VERSION"));
        return;
    }
    if raw.get(1).is_some_and(|s| s == "--document-preview-worker") {
        findx2_gui_native::preview_worker::worker_main();
        return;
    }
    if raw.iter().any(|a| a == "-h" || a == "--help") {
        print!("{}", cli::help_text());
        return;
    }
    let opts = match cli::parse_args(raw) {
        Ok(opts) => opts,
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(2);
        }
    };

    if opts.mock {
        eprintln!(
            "findx2-gui-native: starting isolated mock backend on {}",
            opts.endpoint
        );
        let _handle = mock_backend::spawn_mock_server(opts.endpoint.clone());
        std::thread::sleep(Duration::from_millis(120));
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

    ui::start(opts);
}
