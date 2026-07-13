//! Windows-native interactive capture — the Win32 counterpart of the TeilCapture
//! Swift library. The selection UI is a raw Win32 overlay window (RegisterClassW +
//! CreateWindowExW + its own message loop), NOT a Tauri/WebView2 window: transparent
//! webview overlays proved unreliable on Windows (z-order, focus, compositor lag).
//!
//! Flow: freeze the whole virtual desktop into a GDI snapshot first, then show the
//! overlay painting that snapshot dimmed with the live selection undimmed. Region
//! captures crop the frozen snapshot (nothing to hide, no compositor wait); window
//! captures re-grab the picked window via xcap (PrintWindow-style, occlusion-free)
//! and fall back to a snapshot crop.
//!
//! Every entry point BLOCKS until the user finishes or cancels — call through
//! `spawn_blocking`, exactly like the macOS Swift FFI entry points.

#[cfg(windows)]
mod overlay;

/// Blocking interactive capture. Mode: "region" | "window" | "fullscreen".
/// `Ok(Some(png))` on capture, `Ok(None)` on user cancel, `Err` on failure.
#[cfg(windows)]
pub fn capture_interactive(mode: &str) -> Result<Option<Vec<u8>>, String> {
    match mode {
        "region" => capture_region(),
        "window" => capture_window_interactive(),
        "fullscreen" => capture_fullscreen(),
        _ => Err(format!("Unknown capture mode: {mode}")),
    }
}

#[cfg(not(windows))]
pub fn capture_interactive(_mode: &str) -> Result<Option<Vec<u8>>, String> {
    Err("teil-capture-windows is only functional on Windows.".into())
}

#[cfg(windows)]
use image::RgbaImage;

#[cfg(windows)]
fn encode_png(image: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut buf = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut buf, image::ImageFormat::Png)
        .map_err(|e| format!("PNG encode failed: {e}"))?;
    Ok(buf.into_inner())
}

/// Fullscreen: primary display, no overlay (matches the previous xcap behavior).
#[cfg(windows)]
fn capture_fullscreen() -> Result<Option<Vec<u8>>, String> {
    let monitors = xcap::Monitor::all().map_err(|e| e.to_string())?;
    let monitor = monitors
        .iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .or_else(|| monitors.first())
        .ok_or_else(|| "No displays found".to_string())?;
    let image = monitor.capture_image().map_err(|e| format!("capture: {e}"))?;
    Ok(Some(encode_png(&image)?))
}

#[cfg(windows)]
fn capture_region() -> Result<Option<Vec<u8>>, String> {
    let frozen = overlay::freeze_screen()?;
    match overlay::run(&frozen, overlay::Mode::Region, &[])? {
        overlay::Outcome::Cancelled => Ok(None),
        overlay::Outcome::Region { x, y, w, h } => {
            let image = frozen.crop_global(x, y, w, h)?;
            Ok(Some(encode_png(&image)?))
        }
        overlay::Outcome::Window(_) => Err("Unexpected window pick in region mode".into()),
    }
}

#[cfg(windows)]
fn capture_window_interactive() -> Result<Option<Vec<u8>>, String> {
    let (windows, rects) = pickable_windows()?;
    if windows.is_empty() {
        return Err("No windows available to capture.".into());
    }
    let frozen = overlay::freeze_screen()?;
    match overlay::run(&frozen, overlay::Mode::WindowPick, &rects)? {
        overlay::Outcome::Cancelled => Ok(None),
        overlay::Outcome::Window(idx) => {
            let rect = rects[idx];
            // Occlusion-free grab of the picked window; the frozen snapshot (what the
            // user actually saw highlighted) is the fallback.
            let image = windows[idx].capture_image().unwrap_or_else(|_| {
                frozen
                    .crop_global(
                        rect.left,
                        rect.top,
                        rect.right - rect.left,
                        rect.bottom - rect.top,
                    )
                    .unwrap_or_else(|_| RgbaImage::new(1, 1))
            });
            if image.width() <= 1 && image.height() <= 1 {
                return Err("Window capture failed.".into());
            }
            Ok(Some(encode_png(&image)?))
        }
        overlay::Outcome::Region { .. } => Err("Unexpected region in window-pick mode".into()),
    }
}

/// On-screen windows front-to-back (xcap's Window::all order), with the same filters
/// as the old HTML picker: skip our own process, minimized, zero-sized, and untitled
/// windows. Returns the xcap handles (for the final grab) plus their global rects
/// (for hover hit-testing in the overlay).
#[cfg(windows)]
fn pickable_windows() -> Result<(Vec<xcap::Window>, Vec<overlay::Rect>), String> {
    let own_pid = std::process::id();
    let mut wins = Vec::new();
    let mut rects = Vec::new();
    for w in xcap::Window::all().map_err(|e| e.to_string())? {
        if w.pid().map(|p| p == own_pid).unwrap_or(false) {
            continue;
        }
        if w.is_minimized().unwrap_or(false) {
            continue;
        }
        let width = w.width().unwrap_or(0) as i32;
        let height = w.height().unwrap_or(0) as i32;
        if width <= 0 || height <= 0 {
            continue;
        }
        let title = w.title().unwrap_or_default();
        let app_name = w.app_name().unwrap_or_default();
        if title.is_empty() && app_name.is_empty() {
            continue;
        }
        let x = w.x().unwrap_or(0);
        let y = w.y().unwrap_or(0);
        rects.push(overlay::Rect {
            left: x,
            top: y,
            right: x + width,
            bottom: y + height,
        });
        wins.push(w);
    }
    Ok((wins, rects))
}
