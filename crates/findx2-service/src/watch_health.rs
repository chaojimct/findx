//! 各卷增量监听健康状态（Windows USN / Unix FSEvents·fanotify 共用）。

use std::sync::{Mutex, OnceLock};

fn watch_health() -> &'static Mutex<std::collections::HashMap<String, String>> {
    static HEALTH: OnceLock<Mutex<std::collections::HashMap<String, String>>> = OnceLock::new();
    HEALTH.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

pub(crate) fn set_watch_error(volume_letter: char, err: Option<String>) {
    set_watch_error_id(&volume_letter.to_ascii_uppercase().to_string(), err);
}

pub(crate) fn set_watch_error_id(volume_id: &str, err: Option<String>) {
    if let Ok(mut g) = watch_health().lock() {
        match err {
            Some(msg) => {
                g.insert(volume_id.to_string(), msg);
            }
            None => {
                g.remove(volume_id);
            }
        }
    }
}

pub(crate) fn watch_error_summary() -> Option<String> {
    let g = watch_health().lock().ok()?;
    if g.is_empty() {
        return None;
    }
    let mut items: Vec<String> = g.iter().map(|(k, v)| format!("{k}: {v}")).collect();
    items.sort();
    Some(items.join("; "))
}

/// Shared production/fake-watcher namespace gate. A failed prefix fences all later
/// cursors until explicit watcher reconstruction; it never implies search shutdown.
#[derive(Default)]
pub(crate) struct NamespaceGate { pub blocked: bool }
impl NamespaceGate {
    pub fn apply(&mut self, engine: &findx2_core::SearchEngine, volume: u8,
        events: &[findx2_core::ChangeEvent], journal_id: u64, next_usn: u64) -> Result<(), String> {
        if self.blocked { return Err("namespace fenced; explicit restart/recovery required".into()); }
        match engine.apply_volume_batch(volume, events, journal_id, next_usn) {
            Ok(()) => {
                set_watch_error(volume as char, engine.has_pending_namespace(volume).then(||
                    format!("missing-parent namespace events pending; durable cursor pinned for restart replay; {}", engine.pending_namespace_diagnostics(volume))));
                Ok(())
            }
            Err(error) => {
                self.blocked = true;
                set_watch_error(volume as char, Some(format!("namespace fenced; cursor retained: {error}")));
                Err(error)
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn test_health_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|p|p.into_inner())
}
