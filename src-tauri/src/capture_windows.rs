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

// ---- Screen recording (teil-capture-windows/src/recorder.rs) --------------

use crate::recording::{BeginOutcome, RecordMode, RecordOpts, RecordResult, RecordState, RecordStatus, StopReason};
use teil_capture_windows::recorder;

fn reason(r: recorder::RecReason) -> StopReason {
    match r {
        recorder::RecReason::User => StopReason::User,
        recorder::RecReason::SourceClosed => StopReason::SourceClosed,
        recorder::RecReason::StreamError => StopReason::StreamError,
        recorder::RecReason::WriterError => StopReason::WriterError,
        recorder::RecReason::NoFrames => StopReason::NoFrames,
        recorder::RecReason::Cancelled => StopReason::Cancelled,
        recorder::RecReason::None => StopReason::None,
    }
}

/// BLOCKING: runs the selection overlay and starts the capture. Call via
/// spawn_blocking (the overlay owns a Win32 message pump on this thread).
pub fn record_begin(mode: RecordMode, opts: &RecordOpts) -> Result<BeginOutcome, String> {
    let mode = match mode {
        RecordMode::Region => "region",
        RecordMode::Window => "window",
        RecordMode::Fullscreen => "fullscreen",
    };
    let rec_opts = recorder::RecOptions {
        fps: opts.fps,
        capture_audio: opts.capture_audio,
        show_cursor: opts.show_cursor,
        out_path: opts.out_path.clone(),
    };
    match recorder::record_begin(mode, rec_opts)? {
        recorder::RecBegin::Started { width, height } => Ok(BeginOutcome::Started { width, height }),
        recorder::RecBegin::Cancelled => Ok(BeginOutcome::Cancelled),
        recorder::RecBegin::Busy => Ok(BeginOutcome::Busy),
    }
}

pub fn record_pause() -> Result<(), String> {
    recorder::record_pause()
}

pub fn record_resume() -> Result<(), String> {
    recorder::record_resume()
}

/// BLOCKING: finalizes the MP4 (Media Foundation sink writer flush).
pub fn record_stop() -> Result<RecordResult, String> {
    let r = recorder::record_stop()?;
    Ok(RecordResult {
        duration_ms: r.duration_ms,
        bytes: r.bytes,
        width: r.width,
        height: r.height,
        reason: reason(r.reason),
    })
}

pub fn record_cancel() {
    recorder::record_cancel();
}

pub fn record_status() -> RecordStatus {
    let s = recorder::record_status();
    RecordStatus {
        state: match s.state {
            recorder::RecState::Recording => RecordState::Recording,
            recorder::RecState::Paused => RecordState::Paused,
            recorder::RecState::Finishing => RecordState::Finishing,
            recorder::RecState::Stopped => RecordState::Stopped,
            recorder::RecState::Failed => RecordState::Failed,
            recorder::RecState::Idle => RecordState::Idle,
        },
        reason: reason(s.reason),
        elapsed_ms: s.elapsed_ms,
        bytes: s.bytes,
        width: s.width,
        height: s.height,
    }
}
