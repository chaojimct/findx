use std::time::Duration;

use findx2_gui_native::cli;
use findx2_gui_native::mock_backend;
use findx2_gui_native::ui;

fn main() {
    let raw: Vec<String> = std::env::args().collect();
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
    } else {
        eprintln!(
            "findx2-gui-native: connecting to isolated endpoint {} (will not use production pipe findx2)",
            opts.endpoint
        );
    }

    ui::start(opts);
}
