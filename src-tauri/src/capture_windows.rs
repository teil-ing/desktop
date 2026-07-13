//! Windows-native capture via the teil-capture-windows crate (raw Win32 overlay +
//! selection, xcap/GDI pixel grabs) — the Windows counterpart of capture_macos.rs.
//!
//! Each entry point blocks until the user finishes or cancels (the overlay runs its
//! own message loop on the calling thread), so ALWAYS call through `spawn_blocking` —
//! never on the Tauri main thread.

/// Blocking interactive capture. Mode: "region" | "window" | "fullscreen".
/// `Ok(Some(png))` on capture, `Ok(None)` on user cancel, `Err` on failure.
pub fn capture_interactive(mode: &str) -> Result<Option<Vec<u8>>, String> {
    teil_capture_windows::capture_interactive(mode)
}
