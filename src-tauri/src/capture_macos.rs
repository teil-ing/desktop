//! macOS-native capture via the TeilCapture Swift static library (swift/TeilCapture).
//!
//! The Swift side owns the ENTIRE interactive stack — selection overlays (region drag
//! with crosshair/marching ants, window hover-highlight picker), ScreenCaptureKit
//! capture with own-app exclusion, cross-monitor stitching, shadow-free window capture
//! with transparent corners, and flash/sound feedback. Each entry point blocks until
//! the user finishes or cancels, so ALWAYS call through `spawn_blocking` — calling on
//! the main thread would deadlock the overlay's main-thread work against the wait.

use std::ffi::{c_char, CStr};

// Status codes returned by the Swift entry points (see CaptureFFI.swift).
const STATUS_OK: i32 = 0;
const STATUS_CANCELLED: i32 = 1;

type CaptureFn = unsafe extern "C" fn(
    bool,             // show_flash
    bool,             // play_sound
    *mut *mut u8,     // out PNG buffer
    *mut usize,       // out PNG length
    *mut *mut c_char, // out error message
) -> i32;

extern "C" {
    fn teil_capture_region_interactive(
        show_flash: bool,
        play_sound: bool,
        out_ptr: *mut *mut u8,
        out_len: *mut usize,
        out_err: *mut *mut c_char,
    ) -> i32;
    fn teil_capture_window_interactive(
        show_flash: bool,
        play_sound: bool,
        out_ptr: *mut *mut u8,
        out_len: *mut usize,
        out_err: *mut *mut c_char,
    ) -> i32;
    fn teil_capture_fullscreen(
        show_flash: bool,
        play_sound: bool,
        out_ptr: *mut *mut u8,
        out_len: *mut usize,
        out_err: *mut *mut c_char,
    ) -> i32;
    fn teil_open_screen_settings();
    fn teil_buffer_free(ptr: *mut u8, len: usize);
    fn teil_string_free(ptr: *mut c_char);
}

// TCC status without triggering the system prompt (preflight) and the one-shot
// prompting request. Plain CoreGraphics C API — no Swift needed.
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

/// Runs one Swift capture entry point and marshals the result.
/// `Ok(Some(png))` on capture, `Ok(None)` on user cancel, `Err` on failure.
fn run_capture(f: CaptureFn) -> Result<Option<Vec<u8>>, String> {
    let mut ptr: *mut u8 = std::ptr::null_mut();
    let mut len: usize = 0;
    let mut err: *mut c_char = std::ptr::null_mut();

    let status = unsafe { f(true, true, &mut ptr, &mut len, &mut err) };
    match status {
        STATUS_OK => {
            if ptr.is_null() || len == 0 {
                return Err("Capture returned an empty image.".into());
            }
            let png = unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();
            unsafe { teil_buffer_free(ptr, len) };
            Ok(Some(png))
        }
        STATUS_CANCELLED => Ok(None),
        _ => {
            let message = if err.is_null() {
                "Unknown capture error.".to_string()
            } else {
                let m = unsafe { CStr::from_ptr(err) }.to_string_lossy().into_owned();
                unsafe { teil_string_free(err) };
                m
            };
            Err(message)
        }
    }
}

/// Blocking interactive capture. Mode: "region" | "window" | "fullscreen".
pub fn capture_interactive(mode: &str) -> Result<Option<Vec<u8>>, String> {
    match mode {
        "region" => run_capture(teil_capture_region_interactive),
        "window" => run_capture(teil_capture_window_interactive),
        "fullscreen" => run_capture(teil_capture_fullscreen),
        _ => Err(format!("Unknown capture mode: {mode}")),
    }
}

/// Instant, non-prompting Screen Recording permission check.
pub fn has_screen_permission() -> bool {
    unsafe { CGPreflightScreenCaptureAccess() }
}

/// Asks macOS for Screen Recording access. Shows the system prompt at most once
/// per app; afterwards it just returns the current status. A fresh grant only
/// takes effect after the app is relaunched.
pub fn request_screen_permission() -> bool {
    unsafe { CGRequestScreenCaptureAccess() }
}

/// Opens System Settings at the Screen Recording privacy pane.
pub fn open_screen_settings() {
    unsafe { teil_open_screen_settings() }
}
