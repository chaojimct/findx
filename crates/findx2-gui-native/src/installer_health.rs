//! Documented, isolated, newly-started GUI diagnostic. Never controls another process.
//! Three actual render/next-frame cycles, private settings write/read, then App::quit.
use std::{path::PathBuf, sync::{OnceLock, atomic::{AtomicBool, AtomicUsize, Ordering}}};
static ROOT: OnceLock<PathBuf> = OnceLock::new();
static FRAMES: AtomicUsize = AtomicUsize::new(0);
static QUEUED: AtomicBool = AtomicBool::new(false);
static READY: AtomicBool = AtomicBool::new(false);
pub fn active() -> bool { ROOT.get().is_some() }
pub fn prepare(root: &str) -> Result<Vec<String>, String> {
    let path=PathBuf::from(root);
    if !path.is_absolute() || !path.is_dir() || std::fs::read_dir(&path).map_err(|e|e.to_string())?.next().is_some() {return Err("installer health requires an existing empty private directory".into())}
    for name in ["local", "roaming", "profile"] {std::fs::create_dir(path.join(name)).map_err(|e|e.to_string())?;}
    // main calls this before starting any threads; never mutate the real user's profile.
    unsafe { std::env::set_var("LOCALAPPDATA",path.join("local")); std::env::set_var("APPDATA",path.join("roaming"));
        std::env::set_var("USERPROFILE",path.join("profile"));std::env::set_var("HOME",path.join("profile"));
        std::env::set_var("XDG_DATA_HOME",path.join("profile/data"));std::env::set_var("XDG_CONFIG_HOME",path.join("profile/config")); }
    ROOT.set(path).map_err(|_|"health already initialized")?;
    let previous=std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {if let Some(root)=ROOT.get(){let _=std::fs::write(root.join("panic.log"),format!("{info}"));} previous(info);}));
    checkpoint("prepared");
    Ok(vec!["FindX".into(),"--mock".into(),"--pipe".into(),format!("findx-installer-health-{}",std::process::id())])
}
pub fn rendered(window: &mut gpui_kit::Window, prefs: crate::prefs::LocalPrefs, settings: crate::settings_logic::FindxGuiSettings) {
    if !active() || QUEUED.swap(true, Ordering::SeqCst) {return}
    window.on_next_frame(move |window,cx| {
        QUEUED.store(false,Ordering::SeqCst);
        let frames=FRAMES.fetch_add(1,Ordering::SeqCst)+1;
        if frames>=3 {
            let result=crate::prefs::save_prefs(&prefs).and_then(|_|crate::prefs::save_settings(&settings));
            let ok=result.is_ok() && std::fs::read(crate::isolation::prefs_path()).ok().and_then(|b|serde_json::from_slice::<serde_json::Value>(&b).ok()) == serde_json::to_value(&prefs).ok();
            if let Some(root)=ROOT.get(){ let record=serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"frames":frames,"storage_ok":ok,"normal_quit_requested":true,"isolated":true,"error":result.err()});
                let _=std::fs::write(root.join("render-health.json"),record.to_string()); }
            READY.store(ok,Ordering::SeqCst);
            cx.quit();
        } else {window.refresh();}
    });
    // on_next_frame schedules delivery. Its callback refreshes the window only
    // after paint, without requiring GPUI current_view outside prepaint/paint.
}
pub fn finished() -> bool {
    let Some(root)=ROOT.get() else{return true};
    let ok=READY.load(Ordering::SeqCst) && FRAMES.load(Ordering::SeqCst)>=3;
    let record=serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"normal_event_loop_return":true,"passed":ok});
    ok && std::fs::write(root.join("normal-exit.json"),record.to_string()).is_ok()
}

/// Only the explicit isolated diagnostic writes startup evidence. Normal GUI is silent.
pub fn checkpoint(stage: &str) {
    use std::io::Write;
    if let Some(root)=ROOT.get(){if let Ok(mut file)=std::fs::OpenOptions::new().create(true).append(true).open(root.join("startup.log")){let _=writeln!(file,"{stage}");}}
}
