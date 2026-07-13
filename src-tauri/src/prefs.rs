//! User preferences + global-shortcut bindings, persisted as JSON in the app config dir.
//! Counterpart of the Swift PreferencesStore (UserDefaults) and the HotkeyMonitor defaults.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Prefs {
    pub strip_exif: bool,
    pub open_in_browser: bool,
    pub clipboard_copy: bool,
    pub launch_at_login: bool,
    pub auto_check_for_updates: bool,
    pub private_upload: bool,
    /// "url" | "image"
    pub clipboard_mode: String,
}

impl Default for Prefs {
    fn default() -> Self {
        // Matches PreferencesStore defaults (privacy-first, zero-friction).
        Self {
            strip_exif: true,
            open_in_browser: true,
            clipboard_copy: true,
            launch_at_login: false,
            auto_check_for_updates: true,
            private_upload: false,
            clipboard_mode: "url".into(),
        }
    }
}

pub fn load_prefs(path: &Path) -> Prefs {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_prefs(path: &Path, prefs: &Prefs) {
    if let Ok(json) = serde_json::to_string_pretty(prefs) {
        let _ = std::fs::write(path, json);
    }
}

// ---- Shortcuts -----------------------------------------------------------

/// Default accelerators. CmdOrCtrl maps to ⌘ on macOS and Ctrl on Windows —
/// so region=⌘/Ctrl+Shift+X, fullscreen=…+S, window=…+C (same as the macOS app).
pub fn default_shortcuts() -> HashMap<String, String> {
    HashMap::from([
        ("region".to_string(), "CmdOrCtrl+Shift+X".to_string()),
        ("fullscreen".to_string(), "CmdOrCtrl+Shift+S".to_string()),
        ("window".to_string(), "CmdOrCtrl+Shift+C".to_string()),
    ])
}

pub fn load_shortcuts(path: &Path) -> HashMap<String, String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(default_shortcuts)
}

pub fn save_shortcuts(path: &Path, shortcuts: &HashMap<String, String>) {
    if let Ok(json) = serde_json::to_string_pretty(shortcuts) {
        let _ = std::fs::write(path, json);
    }
}
