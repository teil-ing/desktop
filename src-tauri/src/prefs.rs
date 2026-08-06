//! User preferences + global-shortcut bindings, persisted as JSON in the app config dir.
//! Counterpart of the Swift PreferencesStore (UserDefaults) and the HotkeyMonitor defaults.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

// `default` at the container level: a prefs.json written by an older build is missing
// any newly added field, and without this serde rejects the whole file — silently
// resetting every setting the user had. Missing fields now fall back individually.
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct Prefs {
    pub strip_exif: bool,
    pub open_in_browser: bool,
    pub clipboard_copy: bool,
    pub launch_at_login: bool,
    pub auto_check_for_updates: bool,
    pub private_upload: bool,
    /// "url" | "image"
    pub clipboard_mode: String,
    /// Where "Download" writes images. None = the OS Downloads folder (see
    /// commands::download_dir_path).
    pub download_dir: Option<String>,
    /// Prompt with a save dialog on every download instead of writing straight
    /// into `download_dir` (which then only seeds the dialog).
    pub ask_where_to_save: bool,
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
            download_dir: None,
            // Zero-friction by default: one click saves, no dialog.
            ask_where_to_save: false,
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
