//! Screen/window still-capture for platforms WITHOUT a native capture module
//! (currently only Linux), via xcap 0.9 + the HTML overlay. macOS uses the
//! TeilCapture Swift library (capture_macos.rs); Windows uses the
//! teil-capture-windows Win32 crate (capture_windows.rs).
//!
//! xcap re-exports `image` at the same version as our direct `image` dependency,
//! so the types unify. All xcap accessors return `XCapResult`, mapped to anyhow.

use anyhow::{anyhow, Result};
use serde::Serialize;
use xcap::image::RgbaImage;
use xcap::{Monitor, Window};

/// A capturable window + its on-screen geometry (points, top-left origin — the same space as
/// the overlay), used for macOS-style hover-highlight selection (Swift: SCWindow surfaced to
/// the overlay). NO thumbnail — capturing every window up front is slow and is what hung the picker.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    pub id: u32,
    pub title: String,
    pub app_name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// A finished capture (Swift: CaptureResult, minus coordinate metadata we no longer need).
pub struct Capture {
    pub image: RgbaImage,
}

fn monitor_geom(m: &Monitor) -> Result<(i32, i32, u32, u32)> {
    Ok((
        m.x().map_err(|e| anyhow!("{e}"))?,
        m.y().map_err(|e| anyhow!("{e}"))?,
        m.width().map_err(|e| anyhow!("{e}"))?,
        m.height().map_err(|e| anyhow!("{e}"))?,
    ))
}

/// Combined bounds of all displays, in xcap's coordinate space — used to size the overlay window.
pub fn virtual_bounds() -> Result<(i32, i32, u32, u32)> {
    let monitors = Monitor::all().map_err(|e| anyhow!("{e}"))?;
    if monitors.is_empty() {
        return Err(anyhow!("No displays found"));
    }
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for m in &monitors {
        let (x, y, w, h) = monitor_geom(m)?;
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x + w as i32);
        max_y = max_y.max(y + h as i32);
    }
    Ok((min_x, min_y, (max_x - min_x) as u32, (max_y - min_y) as u32))
}

/// Capture a rectangle given in GLOBAL coordinates: pick the display under the rect's centre,
/// then capture the intersecting sub-region from it (Swift: dominant-monitor selection).
/// NOTE: exact for single-display / primary regions; a mixed-DPI secondary display may need
/// per-monitor scale handling here.
pub fn capture_region_global(gx: i32, gy: i32, gw: u32, gh: u32) -> Result<Capture> {
    let cx = gx + gw as i32 / 2;
    let cy = gy + gh as i32 / 2;
    let monitor = Monitor::from_point(cx, cy).or_else(|_| {
        Monitor::all()
            .map_err(|e| anyhow!("{e}"))?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("No displays found"))
    })?;

    let (mx, my, mw, mh) = monitor_geom(&monitor)?;
    let lx = (gx - mx).clamp(0, mw as i32) as u32;
    let ly = (gy - my).clamp(0, mh as i32) as u32;
    let lw = gw.min(mw.saturating_sub(lx)).max(1);
    let lh = gh.min(mh.saturating_sub(ly)).max(1);

    let image = monitor
        .capture_region(lx, ly, lw, lh)
        .map_err(|e| anyhow!("capture: {e}"))?;
    Ok(Capture { image })
}

/// Fullscreen capture of the primary display (Swift: current display).
/// Uses the primary monitor to avoid cursor-coordinate-space ambiguity across displays.
pub fn capture_primary() -> Result<Capture> {
    let monitors = Monitor::all().map_err(|e| anyhow!("{e}"))?;
    let monitor = monitors
        .iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .or_else(|| monitors.first())
        .ok_or_else(|| anyhow!("No displays found"))?;
    let image = monitor.capture_image().map_err(|e| anyhow!("capture: {e}"))?;
    Ok(Capture { image })
}

/// Enumerate on-screen windows with geometry, front-to-back (Window::all's order = frontmost
/// first). Fast — no image capture. The overlay uses the geometry for hover-highlighting.
pub fn list_windows() -> Result<Vec<WindowInfo>> {
    let windows = Window::all().map_err(|e| anyhow!("{e}"))?;
    let own_pid = std::process::id();
    let mut out = Vec::new();
    for w in windows {
        // Skip our own windows (the overlay + popover).
        if w.pid().map(|p| p == own_pid).unwrap_or(false) {
            continue;
        }
        if w.is_minimized().unwrap_or(false) {
            continue;
        }
        let id = match w.id() {
            Ok(id) => id,
            Err(_) => continue,
        };
        let width = w.width().unwrap_or(0);
        let height = w.height().unwrap_or(0);
        if width == 0 || height == 0 {
            continue;
        }
        let title = w.title().unwrap_or_default();
        let app_name = w.app_name().unwrap_or_default();
        if title.is_empty() && app_name.is_empty() {
            continue;
        }
        out.push(WindowInfo {
            id,
            title,
            app_name,
            x: w.x().unwrap_or(0),
            y: w.y().unwrap_or(0),
            width,
            height,
        });
    }
    eprintln!("[teil.ing] list_windows -> {} windows", out.len());
    Ok(out)
}

/// Capture a specific window by id (Swift: captureWindow(SCWindow)).
pub fn capture_window(id: u32) -> Result<Capture> {
    let windows = Window::all().map_err(|e| anyhow!("{e}"))?;
    for w in windows {
        if w.id().map(|wid| wid == id).unwrap_or(false) {
            let image = w.capture_image().map_err(|e| anyhow!("capture: {e}"))?;
            return Ok(Capture { image });
        }
    }
    Err(anyhow!("Window not found"))
}
