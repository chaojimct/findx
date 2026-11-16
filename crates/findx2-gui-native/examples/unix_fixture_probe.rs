//! Headless probe of the actual GUI session + IPC code, ONLY with a fixture HOME/runtime.
//! Copy to a marked fixture prefix as FindX; this is NOT a macOS/GUI acceptance artifact.
#[cfg(unix)]
fn main() {
    use findx2_gui_native::{cli, isolation, prefs, session::AppSession};
    assert!(
        std::env::var_os("FINDX_TINY_FIXTURE_PROBE").is_some(),
        "fixture opt-in required"
    );
    let opts = cli::parse_args(std::env::args()).unwrap();
    assert!(opts.official_product);
    isolation::configure_storage(opts.official_product);
    let session = AppSession::load(&opts, false);
    assert_eq!(session.settings.pipe_name, "findx2");
    assert_eq!(
        prefs::settings_path().parent().unwrap(),
        isolation::official_data_dir()
    );
    assert_eq!(
        session.settings.index_path,
        isolation::official_data_dir()
            .join("index.bin")
            .to_string_lossy()
    );
    assert!(
        !prefs::settings_path()
            .to_string_lossy()
            .contains("FindX-native-dev")
    );
    session
        .ipc
        .ping(session.endpoint.clone())
        .blocking_recv()
        .unwrap()
        .unwrap();
    let status = session
        .ipc
        .status(session.endpoint.clone())
        .blocking_recv()
        .unwrap()
        .unwrap();
    let result = session
        .ipc
        .search(
            session.endpoint.clone(),
            "alpha-t162-fixture".into(),
            false,
            100,
            0,
        )
        .blocking_recv()
        .unwrap()
        .unwrap();
    assert!(!result.hits.is_empty());
    prefs::save_settings(&session.settings).unwrap();
    println!(
        "{}",
        serde_json::json!({"scope":"tiny fixture GUI session/IPC, no rendered window", "official_entry":true,"settings_path":prefs::settings_path(),"search_hits":result.hits.len(),"status":status})
    );
}
#[cfg(not(unix))]
fn main() {}
