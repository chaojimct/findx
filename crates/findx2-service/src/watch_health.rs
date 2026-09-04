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
