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

// ---- Screen recording (RecordingFFI.swift) --------------------------------

use crate::recording::{BeginOutcome, RecordMode, RecordOpts, RecordResult, RecordState, RecordStatus, StopReason};

extern "C" {
    fn teil_record_begin(
        mode: i32,
        fps: i32,
        capture_audio: bool,
        show_cursor: bool,
        max_bytes: u64,
        out_path: *const c_char,
        out_err: *mut *mut c_char,
    ) -> i32;
    fn teil_record_pause() -> i32;
    fn teil_record_resume() -> i32;
    fn teil_record_stop(
        out_duration_ms: *mut i64,
        out_bytes: *mut u64,
        out_width: *mut i32,
        out_height: *mut i32,
        out_reason: *mut i32,
        out_err: *mut *mut c_char,
    ) -> i32;
    fn teil_record_cancel() -> i32;
    fn teil_record_status(
        out_state: *mut i32,
        out_reason: *mut i32,
        out_elapsed_ms: *mut i64,
        out_bytes: *mut u64,
        out_width: *mut i32,
        out_height: *mut i32,
    ) -> i32;
}

const STATUS_BUSY: i32 = 3;

/// Consumes a Swift-allocated error string, freeing it.
fn take_err(err: *mut c_char, fallback: &str) -> String {
    if err.is_null() {
        fallback.to_string()
    } else {
        let m = unsafe { CStr::from_ptr(err) }.to_string_lossy().into_owned();
        unsafe { teil_string_free(err) };
        m
    }
}

fn stop_reason(raw: i32) -> StopReason {
    match raw {
        1 => StopReason::User,
        2 => StopReason::SourceClosed,
        3 => StopReason::SizeLimit,
        4 => StopReason::StreamError,
        5 => StopReason::WriterError,
        6 => StopReason::NoFrames,
        7 => StopReason::Cancelled,
        _ => StopReason::None,
    }
}

/// BLOCKING: runs the selection overlay and starts the stream. Call via spawn_blocking.
pub fn record_begin(mode: RecordMode, opts: &RecordOpts) -> Result<BeginOutcome, String> {
    let path = std::ffi::CString::new(opts.out_path.to_string_lossy().as_bytes())
        .map_err(|_| "Invalid recording path.".to_string())?;
    let mut err: *mut c_char = std::ptr::null_mut();
    let status = unsafe {
        teil_record_begin(
            mode as i32,
            opts.fps as i32,
            opts.capture_audio,
            opts.show_cursor,
            opts.max_bytes,
            path.as_ptr(),
            &mut err,
        )
    };
    match status {
        STATUS_OK => {
            // Dimensions come from the first status read (begin returns after start).
            let s = record_status();
            Ok(BeginOutcome::Started { width: s.width, height: s.height })
        }
        STATUS_CANCELLED => Ok(BeginOutcome::Cancelled),
        STATUS_BUSY => Ok(BeginOutcome::Busy),
        _ => Err(take_err(err, "Unknown recording error.")),
    }
}

pub fn record_pause() -> Result<(), String> {
    match unsafe { teil_record_pause() } {
        STATUS_OK => Ok(()),
        _ => Err("No active recording to pause.".into()),
    }
}

pub fn record_resume() -> Result<(), String> {
    match unsafe { teil_record_resume() } {
        STATUS_OK => Ok(()),
        _ => Err("No paused recording to resume.".into()),
    }
}

/// BLOCKING: finalizes the file (waits for the writer). Call via spawn_blocking.
pub fn record_stop() -> Result<RecordResult, String> {
    let mut duration_ms: i64 = 0;
    let mut bytes: u64 = 0;
    let mut width: i32 = 0;
    let mut height: i32 = 0;
    let mut reason: i32 = 0;
    let mut err: *mut c_char = std::ptr::null_mut();
    let status = unsafe {
        teil_record_stop(&mut duration_ms, &mut bytes, &mut width, &mut height, &mut reason, &mut err)
    };
    match status {
        STATUS_OK => Ok(RecordResult {
            duration_ms: duration_ms.max(0) as u64,
            bytes,
            width: width.max(0) as u32,
            height: height.max(0) as u32,
            reason: stop_reason(reason),
        }),
        _ => Err(take_err(err, "The recording could not be saved.")),
    }
}

pub fn record_cancel() {
    unsafe { teil_record_cancel() };
}

/// Cheap status poll; safe to call from any thread, including concurrently with stop.
pub fn record_status() -> RecordStatus {
    let mut state: i32 = 0;
    let mut reason: i32 = 0;
    let mut elapsed_ms: i64 = 0;
    let mut bytes: u64 = 0;
    let mut width: i32 = 0;
    let mut height: i32 = 0;
    unsafe {
        teil_record_status(&mut state, &mut reason, &mut elapsed_ms, &mut bytes, &mut width, &mut height)
    };
    RecordStatus {
        state: match state {
            1 => RecordState::Recording,
            2 => RecordState::Paused,
            3 => RecordState::Finishing,
            4 => RecordState::Stopped,
            5 => RecordState::Failed,
            _ => RecordState::Idle,
        },
        reason: stop_reason(reason),
        elapsed_ms: elapsed_ms.max(0) as u64,
        bytes,
        width: width.max(0) as u32,
        height: height.max(0) as u32,
    }
}
