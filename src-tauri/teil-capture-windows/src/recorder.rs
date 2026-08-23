//! Screen recording via Windows.Graphics.Capture + Media Foundation
//! (the `windows-capture` crate), mirroring the macOS ScreenRecorder contract:
//! blocking `record_begin` (selection overlay + start), instant
//! pause/resume/cancel/status, blocking `record_stop` that finalizes the MP4.
//!
//! Selection reuses the existing screenshot overlay (frozen-desktop picker).
//! Region recordings capture the monitor with the largest intersection and
//! crop every frame to the selection (WGC captures whole items only); a
//! region spanning monitors records only that monitor, same as macOS.
//!
//! Pause keeps the session running and drops frames, subtracting the paused
//! wall time from every subsequent frame timestamp so the written timeline
//! has no gap. Known v1 limits: resizing a recorded window crops/drops
//! frames rather than rescaling, and a fully static tail may shorten the
//! written duration slightly (WGC only delivers frames on change).

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MonitorFromRect, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::encoder::{
    AudioSettingsBuilder, ContainerSettingsBuilder, VideoEncoder, VideoSettingsBuilder,
    VideoSettingsSubType,
};
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};
use windows_capture::window::Window as WcWindow;

use crate::overlay;
use crate::pickable_windows;

// ---- Contract types (mirrored by src-tauri/src/capture_windows.rs) --------

pub struct RecOptions {
    pub fps: u32,
    /// Accepted for contract parity but IGNORED: windows-capture's encoder only
    /// consumes audio buffers the caller pushes (there is no WASAPI loopback or
    /// microphone capture in the crate), and enabling the track without feeding
    /// samples can stall finalization. System-audio loopback is a follow-up.
    pub capture_audio: bool,
    pub show_cursor: bool,
    pub out_path: PathBuf,
}

pub enum RecBegin {
    Started { width: u32, height: u32 },
    Cancelled,
    Busy,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RecState {
    Idle,
    Recording,
    Paused,
    Finishing,
    Stopped,
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RecReason {
    None,
    User,
    SourceClosed,
    StreamError,
    WriterError,
    NoFrames,
    Cancelled,
}

pub struct RecStatus {
    pub state: RecState,
    pub reason: RecReason,
    pub elapsed_ms: u64,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
}

pub struct RecResult {
    pub duration_ms: u64,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
    pub reason: RecReason,
}

// ---- Shared recording state ------------------------------------------------

struct Inner {
    state: RecState,
    reason: RecReason,
    encoder: Option<VideoEncoder>,
    /// Crop rect in item-space physical px (x1, y1, x2, y2); None = full item.
    crop: Option<(u32, u32, u32, u32)>,
    dims: (u32, u32),
    fps: u32,
    out_path: PathBuf,
    first_ts: Option<i64>,
    last_out_ts: i64,
    /// Accumulated paused time in 100 ns units (frame-timestamp domain).
    paused_total_100ns: i64,
    /// Raw (unadjusted) timestamp of the last frame actually sent — pacing.
    last_sent_raw_ts: i64,
    pause_started: Option<Instant>,
    paused_accum: Duration,
    started: Instant,
    error: Option<String>,
}

pub struct Shared {
    inner: Mutex<Inner>,
}

type HandlerError = Box<dyn std::error::Error + Send + Sync>;

struct Session {
    shared: Arc<Shared>,
    control: Option<windows_capture::capture::CaptureControl<RecorderHandler, HandlerError>>,
}

fn active() -> &'static Mutex<Option<Session>> {
    static ACTIVE: OnceLock<Mutex<Option<Session>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(None))
}

// ---- Capture handler -------------------------------------------------------

pub struct RecorderHandler {
    shared: Arc<Shared>,
    scratch: Vec<u8>,
}

impl GraphicsCaptureApiHandler for RecorderHandler {
    type Flags = Arc<Shared>;
    type Error = HandlerError;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self {
            shared: ctx.flags.clone(),
            scratch: Vec::new(),
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut windows_capture::frame::Frame<'_>,
        capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let shared = self.shared.clone();
        let mut inner = shared.inner.lock().unwrap();

        match inner.state {
            RecState::Recording => {}
            RecState::Paused => return Ok(()),
            // Finishing/Stopped/Failed: the controlling side owns teardown.
            _ => {
                capture_control.stop();
                return Ok(());
            }
        }

        let ts = match frame.timestamp() {
            Ok(t) => t.Duration,
            Err(_) => return Ok(()),
        };
        // Pace to the encoder's frame rate (with 20% tolerance): WGC can
        // deliver at display refresh regardless of MinimumUpdateInterval, and
        // the encoder queue is unbounded — oversupplying a slow encoder grows
        // memory and lag for the whole recording. The i64::MIN sentinel means
        // "nothing sent yet" and must bypass the subtraction — `ts - i64::MIN`
        // wraps negative in release mode and silently rejected EVERY frame
        // ("no frames were captured").
        let min_gap = (10_000_000 / inner.fps.max(1) as i64) * 8 / 10;
        if inner.last_sent_raw_ts != i64::MIN
            && ts.saturating_sub(inner.last_sent_raw_ts) < min_gap
        {
            return Ok(());
        }

        let (x1, y1, x2, y2) = match inner.crop {
            Some(c) => c,
            None => (0, 0, inner.dims.0, inner.dims.1),
        };
        // A shrunk window makes the crop fall outside the frame — drop it.
        if frame.width() < x2 || frame.height() < y2 {
            return Ok(());
        }
        let fb = match frame.buffer_crop(x1, y1, x2, y2) {
            Ok(fb) => fb,
            Err(_) => return Ok(()),
        };
        let bytes: Vec<u8> = fb.as_nopadding_buffer(&mut self.scratch).to_vec();
        if bytes.is_empty() {
            return Ok(());
        }
        // send_frame_buffer expects the Windows DIB convention — BGRA
        // BOTTOM-UP ("Windows expects BGRA and bottom-to-top layout for this
        // path") — while WGC frames are top-down. Without this row reversal
        // the encoded video is upside down (glyphs read as if mirrored).
        let row = (x2 - x1) as usize * 4;
        let rows = bytes.len() / row.max(1);
        let mut flipped = vec![0u8; bytes.len()];
        for y in 0..rows {
            flipped[y * row..(y + 1) * row]
                .copy_from_slice(&bytes[(rows - 1 - y) * row..(rows - y) * row]);
        }
        let bytes = flipped;

        let first = *inner.first_ts.get_or_insert(ts);
        let out_ts = ts - first - inner.paused_total_100ns;
        if inner.first_ts != Some(ts) && out_ts <= inner.last_out_ts {
            return Ok(()); // non-monotonic — skip
        }

        if let Some(encoder) = inner.encoder.as_mut() {
            if let Err(e) = encoder.send_frame_buffer(&bytes, out_ts) {
                inner.state = RecState::Failed;
                inner.reason = RecReason::WriterError;
                inner.error = Some(format!("encoder error: {e}"));
                inner.encoder = None; // dropped unfinished; file is discarded
                let path = inner.out_path.clone();
                drop(inner);
                let _ = std::fs::remove_file(path);
                capture_control.stop();
                return Ok(());
            }
            inner.last_out_ts = out_ts;
            inner.last_sent_raw_ts = ts;
        }
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        // The recorded window closed — park in Stopped so the host collects
        // the finished portion via record_stop (same contract as macOS).
        let mut inner = self.shared.inner.lock().unwrap();
        if matches!(inner.state, RecState::Recording | RecState::Paused) {
            inner.state = RecState::Stopped;
            inner.reason = RecReason::SourceClosed;
        }
        Ok(())
    }
}

// ---- Target resolution -----------------------------------------------------

fn even_down(n: u32) -> u32 {
    (n & !1).max(2)
}

struct MonitorRect {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
}

fn monitor_info(hmon: windows::Win32::Graphics::Gdi::HMONITOR) -> Result<MonitorRect, String> {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let ok = unsafe { GetMonitorInfoW(hmon, &mut info) };
    if !ok.as_bool() {
        return Err("GetMonitorInfoW failed".into());
    }
    let r = info.rcMonitor;
    Ok(MonitorRect {
        left: r.left,
        top: r.top,
        width: (r.right - r.left).max(0) as u32,
        height: (r.bottom - r.top).max(0) as u32,
    })
}

enum Target {
    Monitor {
        monitor: Monitor,
        crop: Option<(u32, u32, u32, u32)>,
        dims: (u32, u32),
        /// Region recordings: the recorded rect in global virtual-desktop px,
        /// for the on-screen frame window.
        frame_rect: Option<(i32, i32, i32, i32)>,
    },
    Window {
        window: WcWindow,
        dims: (u32, u32),
    },
}

/// Runs the interactive selection (blocking; needs a message pump thread) and
/// resolves the WGC capture target. Ok(None) = user cancelled.
fn resolve_target(mode: &str) -> Result<Option<Target>, String> {
    match mode {
        "fullscreen" => {
            let mut pt = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut pt);
            }
            let hmon = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTOPRIMARY) };
            let info = monitor_info(hmon)?;
            let dims = (even_down(info.width), even_down(info.height));
            let crop = if dims == (info.width, info.height) {
                None
            } else {
                Some((0, 0, dims.0, dims.1))
            };
            Ok(Some(Target::Monitor {
                monitor: Monitor::from_raw_hmonitor(hmon.0 as *mut std::ffi::c_void),
                crop,
                dims,
                frame_rect: None,
            }))
        }
        "region" => {
            let frozen = overlay::freeze_screen()?;
            match overlay::run(&frozen, overlay::Mode::Region, &[])? {
                overlay::Outcome::Region { x, y, w, h } => {
                    // Monitor with the largest intersection (nearest == largest
                    // for an on-screen selection rect).
                    let rect = RECT {
                        left: x,
                        top: y,
                        right: x + w,
                        bottom: y + h,
                    };
                    let hmon = unsafe { MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST) };
                    let info = monitor_info(hmon)?;
                    // Clamp the selection to this monitor, in monitor-local px.
                    let lx = (x - info.left).max(0) as u32;
                    let ly = (y - info.top).max(0) as u32;
                    let lw = even_down(((x + w - info.left).min(info.width as i32) as u32).saturating_sub(lx));
                    let lh = even_down(((y + h - info.top).min(info.height as i32) as u32).saturating_sub(ly));
                    if lw < 2 || lh < 2 {
                        return Ok(None);
                    }
                    Ok(Some(Target::Monitor {
                        monitor: Monitor::from_raw_hmonitor(hmon.0 as *mut std::ffi::c_void),
                        crop: Some((lx, ly, lx + lw, ly + lh)),
                        dims: (lw, lh),
                        frame_rect: Some((
                            info.left + lx as i32,
                            info.top + ly as i32,
                            lw as i32,
                            lh as i32,
                        )),
                    }))
                }
                overlay::Outcome::Window(_) => Ok(None),
                overlay::Outcome::Cancelled => Ok(None),
            }
        }
        "window" => {
            let frozen = overlay::freeze_screen()?;
            let (windows, rects) = pickable_windows()?;
            match overlay::run(&frozen, overlay::Mode::WindowPick, &rects)? {
                overlay::Outcome::Window(idx) => {
                    let picked = windows.get(idx).ok_or("window pick out of range")?;
                    let wc = find_wc_window(picked)?;
                    let rect = wc.rect().map_err(|e| format!("window rect: {e}"))?;
                    let w = even_down((rect.right - rect.left).max(0) as u32);
                    let h = even_down((rect.bottom - rect.top).max(0) as u32);
                    if w < 2 || h < 2 {
                        return Err("The selected window is too small to record.".into());
                    }
                    Ok(Some(Target::Window {
                        window: wc,
                        dims: (w, h),
                    }))
                }
                _ => Ok(None),
            }
        }
        other => Err(format!("Unknown recording mode: {other}")),
    }
}

/// Matches the overlay's xcap pick to a windows-capture Window (which carries
/// the HWND that Windows.Graphics.Capture needs) by process id + geometry.
fn find_wc_window(picked: &xcap::Window) -> Result<WcWindow, String> {
    let pid = picked.pid().map_err(|e| e.to_string())?;
    let px = picked.x().unwrap_or(0);
    let py = picked.y().unwrap_or(0);
    let title = picked.title().unwrap_or_default();

    let candidates = WcWindow::enumerate().map_err(|e| format!("window enumeration: {e}"))?;
    let mut by_title: Option<WcWindow> = None;
    for w in candidates {
        let Ok(wpid) = w.process_id() else { continue };
        if wpid != pid {
            continue;
        }
        if let Ok(r) = w.rect() {
            if (r.left - px).abs() <= 2 && (r.top - py).abs() <= 2 {
                return Ok(w);
            }
        }
        if by_title.is_none() {
            if let Ok(t) = w.title() {
                if !title.is_empty() && t == title {
                    by_title = Some(w);
                }
            }
        }
    }
    by_title.ok_or_else(|| "Could not resolve the selected window for recording.".into())
}

// ---- Public API ------------------------------------------------------------

/// ~0.12 bits per pixel·frame, clamped — same heuristic as the macOS recorder.
fn bitrate(width: u32, height: u32, fps: u32) -> u32 {
    ((width as u64 * height as u64 * fps as u64) as f64 * 0.12)
        .clamp(3_000_000.0, 32_000_000.0) as u32
}

pub fn record_begin(mode: &str, opts: RecOptions) -> Result<RecBegin, String> {
    {
        let guard = active().lock().unwrap();
        if guard.is_some() {
            return Ok(RecBegin::Busy);
        }
    }

    let Some(target) = resolve_target(mode)? else {
        return Ok(RecBegin::Cancelled);
    };
    let (dims, crop, frame_rect) = match &target {
        Target::Monitor {
            dims, crop, frame_rect, ..
        } => (*dims, *crop, *frame_rect),
        Target::Window { dims, .. } => (*dims, Some((0, 0, dims.0, dims.1)), None),
    };

    let _ = std::fs::remove_file(&opts.out_path);
    if let Some(parent) = opts.out_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let fps = if opts.fps == 60 { 60 } else { 30 };
    let encoder = VideoEncoder::new(
        VideoSettingsBuilder::new(dims.0, dims.1)
            // The builder DEFAULTS to HEVC, which plain Windows installs (no
            // HEVC extension, VMs) cannot encode — MF_E_INVALIDMEDIATYPE at
            // encoder creation. H.264 is universally available and what the
            // pipeline expects.
            .sub_type(VideoSettingsSubType::H264)
            .frame_rate(fps)
            .bitrate(bitrate(dims.0, dims.1, fps)),
        // Always disabled — see RecOptions::capture_audio.
        AudioSettingsBuilder::default().disabled(true),
        ContainerSettingsBuilder::default(),
        &opts.out_path,
    )
    .map_err(|e| format!("Could not create the video encoder: {e}"))?;

    let shared = Arc::new(Shared {
        inner: Mutex::new(Inner {
            state: RecState::Recording,
            reason: RecReason::None,
            encoder: Some(encoder),
            crop,
            dims,
            fps,
            out_path: opts.out_path.clone(),
            first_ts: None,
            last_out_ts: -1,
            last_sent_raw_ts: i64::MIN,
            paused_total_100ns: 0,
            pause_started: None,
            paused_accum: Duration::ZERO,
            started: Instant::now(),
            error: None,
        }),
    });

    let cursor = if opts.show_cursor {
        CursorCaptureSettings::WithCursor
    } else {
        CursorCaptureSettings::WithoutCursor
    };
    // The OS capture border marks the WHOLE monitor (we capture the monitor
    // and crop) — misleading for a region. Our own frame window marks the
    // actual recorded rect instead.
    let os_border = DrawBorderSettings::WithoutBorder;
    // A steady frame cadence (instead of change-driven delivery) keeps the
    // timeline honest on mostly-static screens.
    let interval = MinimumUpdateIntervalSettings::Custom(Duration::from_millis(1000 / fps as u64));

    let control = match target {
        Target::Monitor { monitor, .. } => {
            let settings = Settings::new(
                monitor,
                cursor,
                os_border,
                SecondaryWindowSettings::Default,
                interval,
                DirtyRegionSettings::Default,
                ColorFormat::Bgra8,
                shared.clone(),
            );
            RecorderHandler::start_free_threaded(settings)
        }
        Target::Window { window, .. } => {
            let settings = Settings::new(
                window,
                cursor,
                os_border,
                SecondaryWindowSettings::Default,
                interval,
                DirtyRegionSettings::Default,
                ColorFormat::Bgra8,
                shared.clone(),
            );
            RecorderHandler::start_free_threaded(settings)
        }
    }
    .map_err(|e| format!("Could not start the recording: {e}"))?;

    if let Some((fx, fy, fw, fh)) = frame_rect {
        crate::border::show(fx, fy, fw, fh);
    }
    *active().lock().unwrap() = Some(Session {
        shared,
        control: Some(control),
    });
    Ok(RecBegin::Started {
        width: dims.0,
        height: dims.1,
    })
}

pub fn record_pause() -> Result<(), String> {
    let guard = active().lock().unwrap();
    let Some(session) = guard.as_ref() else {
        return Err("No active recording to pause.".into());
    };
    let mut inner = session.shared.inner.lock().unwrap();
    if inner.state != RecState::Recording {
        return Err("No active recording to pause.".into());
    }
    inner.state = RecState::Paused;
    inner.pause_started = Some(Instant::now());
    Ok(())
}

pub fn record_resume() -> Result<(), String> {
    let guard = active().lock().unwrap();
    let Some(session) = guard.as_ref() else {
        return Err("No paused recording to resume.".into());
    };
    let mut inner = session.shared.inner.lock().unwrap();
    if inner.state != RecState::Paused {
        return Err("No paused recording to resume.".into());
    }
    if let Some(started) = inner.pause_started.take() {
        let paused = started.elapsed();
        inner.paused_accum += paused;
        inner.paused_total_100ns += (paused.as_nanos() / 100) as i64;
    }
    inner.state = RecState::Recording;
    Ok(())
}

/// BLOCKING: stops the session and finalizes the MP4.
pub fn record_stop() -> Result<RecResult, String> {
    let session = {
        let mut guard = active().lock().unwrap();
        guard.take()
    };
    let Some(mut session) = session else {
        return Err("No active recording to stop.".into());
    };

    // Freeze the timeline BEFORE tearing the session down.
    let reason = {
        let mut inner = session.shared.inner.lock().unwrap();
        if let Some(started) = inner.pause_started.take() {
            let paused = started.elapsed();
            inner.paused_accum += paused;
            inner.paused_total_100ns += (paused.as_nanos() / 100) as i64;
        }
        let reason = match inner.reason {
            RecReason::None => RecReason::User,
            r => r,
        };
        inner.state = RecState::Finishing;
        inner.reason = reason;
        reason
    };

    crate::border::hide();
    if let Some(control) = session.control.take() {
        let _ = control.stop();
    }

    let mut inner = session.shared.inner.lock().unwrap();
    let out_path = inner.out_path.clone();
    let Some(encoder) = inner.encoder.take() else {
        let msg = inner
            .error
            .clone()
            .unwrap_or_else(|| "The recording could not be written to disk.".into());
        drop(inner);
        let _ = std::fs::remove_file(&out_path);
        return Err(msg);
    };
    if inner.first_ts.is_none() {
        drop(encoder);
        drop(inner);
        let _ = std::fs::remove_file(&out_path);
        return Err("No frames were captured.".into());
    }

    let frame_100ns = 10_000_000 / inner.fps.max(1) as i64;
    let duration_ms = ((inner.last_out_ts + frame_100ns).max(0) / 10_000) as u64;
    let dims = inner.dims;
    drop(inner);

    encoder
        .finish()
        .map_err(|e| format!("Could not finalize the recording: {e}"))?;

    let bytes = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
    Ok(RecResult {
        duration_ms,
        bytes,
        width: dims.0,
        height: dims.1,
        reason,
    })
}

pub fn record_cancel() {
    let session = {
        let mut guard = active().lock().unwrap();
        guard.take()
    };
    let Some(mut session) = session else { return };
    crate::border::hide();
    let out_path = {
        let mut inner = session.shared.inner.lock().unwrap();
        inner.state = RecState::Failed;
        inner.reason = RecReason::Cancelled;
        inner.encoder = None; // dropped without finish — file is garbage anyway
        inner.out_path.clone()
    };
    if let Some(control) = session.control.take() {
        let _ = control.stop();
    }
    let _ = std::fs::remove_file(out_path);
}

pub fn record_status() -> RecStatus {
    let guard = active().lock().unwrap();
    let Some(session) = guard.as_ref() else {
        return RecStatus {
            state: RecState::Idle,
            reason: RecReason::None,
            elapsed_ms: 0,
            bytes: 0,
            width: 0,
            height: 0,
        };
    };
    let inner = session.shared.inner.lock().unwrap();
    let mut elapsed = inner.started.elapsed().saturating_sub(inner.paused_accum);
    if let Some(p) = inner.pause_started {
        elapsed = elapsed.saturating_sub(p.elapsed());
    }
    RecStatus {
        state: inner.state,
        reason: inner.reason,
        elapsed_ms: elapsed.as_millis() as u64,
        bytes: std::fs::metadata(&inner.out_path).map(|m| m.len()).unwrap_or(0),
        width: inner.dims.0,
        height: inner.dims.1,
    }
}
