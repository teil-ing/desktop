//! Screen-recording orchestration: session state, the recording tray icon with its
//! timer + menu, the status poll loop, and the video upload pipeline.
//!
//! Platform-neutral: the native engine lives behind the `native` alias (macOS:
//! capture_macos → TeilCapture Swift lib; elsewhere: stubs reporting "unsupported").

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};

use crate::commands::{blocking, report_failure, set_tray_tooltip};
use crate::AppState;

// ---- Contract with the native layer --------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RecordMode {
    Region = 0,
    Window = 1,
    Fullscreen = 2,
}

pub struct RecordOpts {
    pub fps: u32,
    pub capture_audio: bool,
    pub show_cursor: bool,
    /// Native auto-stops once the file reaches this size. 0 = unlimited.
    pub max_bytes: u64,
    pub out_path: PathBuf,
}

pub enum BeginOutcome {
    Started { width: u32, height: u32 },
    Cancelled,
    Busy,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RecordState {
    Idle,
    Recording,
    Paused,
    Finishing,
    /// Ended natively (user window closed, size cap, stream error); collect via record_stop.
    Stopped,
    /// Failed natively; the file is gone. Acknowledge via record_cancel.
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StopReason {
    None,
    User,
    SourceClosed,
    SizeLimit,
    StreamError,
    WriterError,
    NoFrames,
    Cancelled,
}

pub struct RecordStatus {
    pub state: RecordState,
    pub reason: StopReason,
    pub elapsed_ms: u64,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
}

pub struct RecordResult {
    pub duration_ms: u64,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
    pub reason: StopReason,
}

#[cfg(target_os = "macos")]
use crate::capture_macos as native;

#[cfg(target_os = "windows")]
use crate::capture_windows as native;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod native {
    use super::*;
    const UNSUPPORTED: &str = "Screen recording is not supported on this platform yet.";
    pub fn record_begin(_mode: RecordMode, _opts: &RecordOpts) -> Result<BeginOutcome, String> {
        Err(UNSUPPORTED.into())
    }
    pub fn record_pause() -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
    pub fn record_resume() -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
    pub fn record_stop() -> Result<RecordResult, String> {
        Err(UNSUPPORTED.into())
    }
    pub fn record_cancel() {}
    pub fn record_status() -> RecordStatus {
        RecordStatus {
            state: RecordState::Idle,
            reason: StopReason::None,
            elapsed_ms: 0,
            bytes: 0,
            width: 0,
            height: 0,
        }
    }
}

/// Auto-stop before the server's 500 MiB cap so the finalized MP4 stays uploadable.
pub const MAX_BYTES: u64 = 480 * 1024 * 1024;

pub const SUPPORTED: bool = cfg!(any(target_os = "macos", target_os = "windows"));

// ---- Session state --------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Starting,
    Recording,
    Paused,
    Stopping,
    Uploading,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Phase::Starting => "starting",
            Phase::Recording => "recording",
            Phase::Paused => "paused",
            Phase::Stopping => "stopping",
            Phase::Uploading => "uploading",
        }
    }
}

pub struct Session {
    pub mode: &'static str,
    pub phase: Phase,
    pub out_path: PathBuf,
    pub filename: String,
    pub elapsed_ms: u64,
    pub bytes: u64,
    tick: Option<tauri::async_runtime::JoinHandle<()>>,
    menu: Option<RecMenu>,
    pub upload: Option<(u64, u64)>,
}

struct RecMenu {
    stop: MenuItem<tauri::Wry>,
    pause: MenuItem<tauri::Wry>,
    cancel: MenuItem<tauri::Wry>,
}

/// A finished-but-unuploaded recording kept on disk for "Retry Upload".
#[derive(Clone)]
pub struct FailedVideo {
    pub path: PathBuf,
    pub filename: String,
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
}

/// What "Retry Upload" retries — the old RgbaImage buffer or a kept video file.
pub enum FailedUpload {
    Image(image::RgbaImage),
    Video(FailedVideo),
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RecordingStatus {
    pub supported: bool,
    pub phase: &'static str,
    pub mode: Option<&'static str>,
    pub elapsed_ms: u64,
    pub bytes_written: u64,
    pub upload_sent: Option<u64>,
    pub upload_total: Option<u64>,
}

// ---- Public API -----------------------------------------------------------

pub fn init(app: &AppHandle) {
    install_menu_handler(app);
    sweep_stale(app);
}

pub fn is_active(app: &AppHandle) -> bool {
    app.state::<AppState>().recording.lock().unwrap().is_some()
}

pub fn status(app: &AppHandle) -> RecordingStatus {
    let state = app.state::<AppState>();
    let guard = state.recording.lock().unwrap();
    match guard.as_ref() {
        Some(s) => RecordingStatus {
            supported: SUPPORTED,
            phase: s.phase.as_str(),
            mode: Some(s.mode),
            elapsed_ms: s.elapsed_ms,
            bytes_written: s.bytes,
            upload_sent: s.upload.map(|(sent, _)| sent),
            upload_total: s.upload.map(|(_, total)| total),
        },
        None => RecordingStatus {
            supported: SUPPORTED,
            phase: "idle",
            mode: None,
            elapsed_ms: 0,
            bytes_written: 0,
            upload_sent: None,
            upload_total: None,
        },
    }
}

fn emit_state(app: &AppHandle) {
    let _ = app.emit("recording-state", status(app));
}

/// Kicks off a recording. Sync checks here; the blocking selection + start runs on
/// a background task. Mode: "region" | "window" | "fullscreen".
pub fn begin(app: &AppHandle, mode: &str) -> Result<(), String> {
    if !SUPPORTED {
        return Err("Screen recording is not supported on this platform yet.".into());
    }
    let mode: &'static str = match mode {
        "region" => "region",
        "window" => "window",
        "fullscreen" => "fullscreen",
        _ => return Err(format!("Unknown recording mode: {mode}")),
    };
    #[cfg(target_os = "macos")]
    if !crate::capture_macos::has_screen_permission() {
        eprintln!("[teil.ing] recording blocked: screen recording permission missing");
        crate::commands::show_main(app);
        return Err("Screen Recording permission is required.".into());
    }

    {
        let state = app.state::<AppState>();
        let mut guard = state.recording.lock().unwrap();
        if guard.is_some() {
            return Err("A recording is already in progress.".into());
        }
        let filename = format!(
            "recording-{}.mp4",
            chrono::Local::now().format("%Y-%m-%d_%H-%M-%S")
        );
        let out_path = recordings_dir(app).join(&filename);
        *guard = Some(Session {
            mode,
            phase: Phase::Starting,
            out_path,
            filename,
            elapsed_ms: 0,
            bytes: 0,
            tick: None,
            menu: None,
            upload: None,
        });
    }
    emit_state(app);

    // Hide the popover + settings so they are not baked into a fullscreen recording
    // (same dance as spawn_native_capture).
    let was_visible = ["main", "preferences"]
        .into_iter()
        .filter_map(|label| app.get_webview_window(label))
        .fold(false, |any, w| {
            let visible = w.is_visible().unwrap_or(false);
            if visible {
                let _ = w.hide();
            }
            any || visible
        });

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if was_visible {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        run_begin(app, mode).await;
    });
    Ok(())
}

async fn run_begin(app: AppHandle, mode: &'static str) {
    let (opts, out_path) = {
        let state = app.state::<AppState>();
        let prefs = state.prefs.lock().unwrap().clone();
        let out_path = state
            .recording
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.out_path.clone())
            .unwrap_or_default();
        (
            RecordOpts {
                fps: if prefs.video_fps == 60 { 60 } else { 30 },
                capture_audio: prefs.video_capture_audio,
                show_cursor: prefs.video_show_cursor,
                max_bytes: MAX_BYTES,
                out_path: out_path.clone(),
            },
            out_path,
        )
    };
    let _ = std::fs::create_dir_all(out_path.parent().unwrap_or(&out_path));

    let record_mode = match mode {
        "window" => RecordMode::Window,
        "fullscreen" => RecordMode::Fullscreen,
        _ => RecordMode::Region,
    };

    match blocking(move || native::record_begin(record_mode, &opts)).await {
        Ok(BeginOutcome::Started { width, height }) => {
            eprintln!("[teil.ing] recording started ({mode}, {width}x{height})");
            let menu = create_tray(&app);
            {
                let state = app.state::<AppState>();
                let mut guard = state.recording.lock().unwrap();
                if let Some(s) = guard.as_mut() {
                    s.phase = Phase::Recording;
                    s.menu = menu;
                }
            }
            spawn_tick(&app);
            emit_state(&app);
        }
        Ok(BeginOutcome::Cancelled) => {
            clear_session(&app);
            emit_state(&app);
        }
        Ok(BeginOutcome::Busy) => {
            clear_session(&app);
            emit_state(&app);
            report_failure(&app, "A recording is already in progress.");
        }
        Err(e) => {
            clear_session(&app);
            emit_state(&app);
            report_failure(&app, &begin_error_message(&e));
        }
    }
}

fn begin_error_message(e: &str) -> String {
    #[cfg(target_os = "macos")]
    if !crate::capture_macos::has_screen_permission() {
        return "Screen Recording permission is required. Grant it in System Settings → Privacy & Security, then restart the app.".into();
    }
    format!("Recording failed: {e}")
}

pub async fn pause(app: AppHandle) -> Result<(), String> {
    {
        let state = app.state::<AppState>();
        let guard = state.recording.lock().unwrap();
        match guard.as_ref().map(|s| s.phase) {
            Some(Phase::Recording) => {}
            _ => return Err("No active recording to pause.".into()),
        }
    }
    blocking(native::record_pause).await?;
    with_session(&app, |s| s.phase = Phase::Paused);
    if let Some(m) = menu_items(&app) {
        let _ = m.pause.set_text("Resume");
    }
    update_tray_visuals(&app, true);
    emit_state(&app);
    Ok(())
}

pub async fn resume(app: AppHandle) -> Result<(), String> {
    {
        let state = app.state::<AppState>();
        let guard = state.recording.lock().unwrap();
        match guard.as_ref().map(|s| s.phase) {
            Some(Phase::Paused) => {}
            _ => return Err("No paused recording to resume.".into()),
        }
    }
    blocking(native::record_resume).await?;
    with_session(&app, |s| s.phase = Phase::Recording);
    if let Some(m) = menu_items(&app) {
        let _ = m.pause.set_text("Pause");
    }
    update_tray_visuals(&app, false);
    emit_state(&app);
    Ok(())
}

/// Stops the recording, finalizes the file and uploads it. First caller wins.
pub async fn stop(app: AppHandle) -> Result<(), String> {
    if !begin_stopping(&app) {
        return Err("No active recording to stop.".into());
    }
    if let Some(tray) = app.tray_by_id("recording") {
        let _ = tray.set_title(Some("Saving…"));
    }
    let result = blocking(native::record_stop).await;
    remove_tray(&app);
    match result {
        Ok(res) => {
            with_session(&app, |s| s.phase = Phase::Uploading);
            emit_state(&app);
            upload_recording(app, res).await;
        }
        Err(e) => {
            clear_session(&app);
            emit_state(&app);
            report_failure(&app, &format!("Recording failed: {e}"));
        }
    }
    Ok(())
}

/// Discards the recording — nothing is uploaded, the file is deleted.
pub async fn cancel(app: AppHandle) -> Result<(), String> {
    if !begin_stopping(&app) {
        return Err("No active recording to discard.".into());
    }
    let _ = blocking(|| {
        native::record_cancel();
        Ok::<(), String>(())
    })
    .await;
    remove_tray(&app);
    let out_path = {
        let state = app.state::<AppState>();
        let guard = state.recording.lock().unwrap();
        guard.as_ref().map(|s| s.out_path.clone())
    };
    if let Some(p) = out_path {
        let _ = std::fs::remove_file(p);
    }
    clear_session(&app);
    set_tray_tooltip(&app, "teil.ing");
    emit_state(&app);
    Ok(())
}

/// Phase → Stopping if a recording is live; aborts the tick. False if nothing to stop.
fn begin_stopping(app: &AppHandle) -> bool {
    let state = app.state::<AppState>();
    let mut guard = state.recording.lock().unwrap();
    let Some(s) = guard.as_mut() else { return false };
    if !matches!(s.phase, Phase::Recording | Phase::Paused) {
        return false;
    }
    s.phase = Phase::Stopping;
    if let Some(t) = s.tick.take() {
        t.abort();
    }
    if let Some(m) = &s.menu {
        let _ = m.stop.set_enabled(false);
        let _ = m.pause.set_enabled(false);
        let _ = m.cancel.set_enabled(false);
    }
    true
}

// ---- Poll loop ------------------------------------------------------------

fn spawn_tick(app: &AppHandle) {
    let tick_app = app.clone();
    let handle = tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_title = String::new();
        loop {
            interval.tick().await;
            if !tick_once(&tick_app, &mut last_title) {
                break;
            }
        }
    });
    let state = app.state::<AppState>();
    let mut guard = state.recording.lock().unwrap();
    if let Some(s) = guard.as_mut() {
        s.tick = Some(handle);
    }
}

/// One poll of the native status. Returns false to end the loop.
fn tick_once(app: &AppHandle, last_title: &mut String) -> bool {
    // Only poll while the session is live.
    {
        let state = app.state::<AppState>();
        let guard = state.recording.lock().unwrap();
        match guard.as_ref().map(|s| s.phase) {
            Some(Phase::Recording) | Some(Phase::Paused) => {}
            _ => return false,
        }
    }

    let status = native::record_status();
    let paused = status.state == RecordState::Paused;
    with_session(app, |s| {
        s.elapsed_ms = status.elapsed_ms;
        s.bytes = status.bytes;
    });

    match status.state {
        RecordState::Recording | RecordState::Paused | RecordState::Finishing => {
            let title = if paused {
                format!("⏸ {}", format_elapsed(status.elapsed_ms))
            } else {
                format_elapsed(status.elapsed_ms)
            };
            if title != *last_title {
                *last_title = title.clone();
                if let Some(tray) = app.tray_by_id("recording") {
                    // Windows has no tray title text — the tooltip carries the
                    // timer there; setting both everywhere keeps this uniform.
                    let _ = tray.set_tooltip(Some(format!(
                        "Recording {title} — click to stop, right-click for options"
                    )));
                    let _ = tray.set_title(Some(title));
                }
            }
            emit_state(app);
            true
        }
        RecordState::Stopped => {
            // Native auto-stop (window closed, size cap, stream error) — collect + upload.
            eprintln!("[teil.ing] recording ended natively: {:?}", status.reason);
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let _ = stop(app).await;
            });
            false
        }
        RecordState::Failed | RecordState::Idle => {
            let app = app.clone();
            let reason = status.reason;
            tauri::async_runtime::spawn(async move {
                fail_native(app, reason).await;
            });
            false
        }
    }
}

async fn fail_native(app: AppHandle, reason: StopReason) {
    let _ = blocking(|| {
        native::record_cancel();
        Ok::<(), String>(())
    })
    .await;
    remove_tray(&app);
    clear_session(&app);
    emit_state(&app);
    report_failure(&app, failure_message(reason));
}

fn failure_message(reason: StopReason) -> &'static str {
    match reason {
        StopReason::WriterError => "The recording could not be written to disk.",
        StopReason::NoFrames => "The recording captured no frames.",
        StopReason::StreamError => "The recording stopped unexpectedly.",
        _ => "The recording failed.",
    }
}

// ---- Upload ---------------------------------------------------------------

async fn upload_recording(app: AppHandle, res: RecordResult) {
    let (path, filename) = {
        let state = app.state::<AppState>();
        let guard = state.recording.lock().unwrap();
        match guard.as_ref() {
            Some(s) => (s.out_path.clone(), s.filename.clone()),
            None => return,
        }
    };
    eprintln!(
        "[teil.ing] recording finalized: {} ms, {} bytes",
        res.duration_ms, res.bytes
    );
    if matches!(res.reason, StopReason::SourceClosed) {
        eprintln!("[teil.ing] recording ended: source window closed");
    }
    if matches!(res.reason, StopReason::SizeLimit) {
        eprintln!("[teil.ing] recording stopped at the 480 MB upload limit");
    }
    let video = FailedVideo {
        path,
        filename,
        duration_ms: res.duration_ms,
        width: res.width,
        height: res.height,
    };
    upload_video_file(app, video, true).await;
}

/// Re-runs the upload of a kept recording (from "Retry Upload").
pub async fn retry_video(app: AppHandle, video: FailedVideo) {
    upload_video_file(app, video, false).await;
}

/// Shared preflight + streaming upload + post-success plumbing.
/// `in_session` — whether an active Session (phase Uploading) mirrors progress.
async fn upload_video_file(app: AppHandle, video: FailedVideo, in_session: bool) {
    let _ = app.emit("upload-feedback", serde_json::json!({"kind":"started"}));
    set_tray_tooltip(&app, "Uploading…");

    let prefs = { app.state::<AppState>().prefs.lock().unwrap().clone() };

    let finish_err = |app: &AppHandle, video: FailedVideo, msg: &str| {
        *app.state::<AppState>().last_failed.lock().unwrap() = Some(FailedUpload::Video(video));
        if in_session {
            clear_session(app);
            emit_state(app);
        }
        set_tray_tooltip(app, "Upload failed");
        report_failure(app, msg);
    };

    let key = match crate::secure::get_api_key() {
        Some(k) => k,
        None => {
            return finish_err(
                &app,
                video,
                "No API key found. Please add your key in settings.",
            )
        }
    };

    let total = match tokio::fs::metadata(&video.path).await {
        Ok(m) if m.len() > 0 => m.len(),
        _ => return finish_err(&app, video, "The recording file is empty or missing."),
    };

    let ticket = match crate::api::preflight_video(&key, total).await {
        Ok(t) => t,
        Err(e) => return finish_err(&app, video, &e.to_string()),
    };

    // Throttled progress: emit on ≥1% or ≥100 ms, always on completion.
    let progress_app = app.clone();
    let mut last_emit = Instant::now() - Duration::from_secs(1);
    let mut last_pct: u64 = u64::MAX;
    let on_progress = move |sent: u64, total: u64| {
        let pct = if total > 0 { sent * 100 / total } else { 0 };
        let due = pct != last_pct || last_emit.elapsed() >= Duration::from_millis(100);
        if !(due || sent == total) {
            return;
        }
        last_emit = Instant::now();
        if pct != last_pct {
            last_pct = pct;
            set_tray_tooltip(&progress_app, &format!("Uploading… {pct}%"));
        }
        if in_session {
            let state = progress_app.state::<AppState>();
            let mut guard = state.recording.lock().unwrap();
            if let Some(s) = guard.as_mut() {
                s.upload = Some((sent, total));
            }
        }
        let _ = progress_app.emit(
            "upload-progress",
            serde_json::json!({"sent": sent, "total": total}),
        );
    };

    let meta = crate::api::VideoMeta {
        duration_ms: video.duration_ms,
        width: video.width,
        height: video.height,
    };
    match crate::api::upload_video(&key, &video.path, &video.filename, &meta, &ticket, on_progress)
        .await
    {
        Ok((id, share_url)) => {
            if prefs.private_upload {
                let req = crate::api::ImageUpdateRequest {
                    private: Some(true),
                    ..Default::default()
                };
                if let Err(e) = crate::api::update_image(&key, &id, &req).await {
                    eprintln!("[teil.ing] could not mark recording private: {e}");
                }
            }
            if prefs.clipboard_copy {
                use tauri_plugin_clipboard_manager::ClipboardExt;
                let _ = app.clipboard().write_text(share_url.clone());
            }
            if prefs.open_in_browser {
                use tauri_plugin_opener::OpenerExt;
                let _ = app.opener().open_url(share_url.clone(), None::<&str>);
            }
            let _ = std::fs::remove_file(&video.path);
            *app.state::<AppState>().last_failed.lock().unwrap() = None;
            if in_session {
                clear_session(&app);
                emit_state(&app);
            }
            set_tray_tooltip(&app, "teil.ing");
            eprintln!("[teil.ing] recording upload ok: {share_url}");
            let _ = app.emit(
                "upload-feedback",
                serde_json::json!({"kind":"succeeded","imageId":id,"shareUrl":share_url}),
            );
        }
        Err(e) => finish_err(&app, video, &e.to_string()),
    }
}

// ---- Tray -----------------------------------------------------------------

/// Registers the app-level menu-event handler ONCE. Per-tray builder handlers leak
/// (tray-icon pushes them to a global Vec that removal never clears), so the
/// recording tray's menu is dispatched from here instead.
fn install_menu_handler(app: &AppHandle) {
    app.on_menu_event(|app, event| {
        let app = app.clone();
        match event.id().as_ref() {
            "rec-stop" => {
                tauri::async_runtime::spawn(async move {
                    let _ = stop(app).await;
                });
            }
            "rec-pause" => {
                tauri::async_runtime::spawn(async move {
                    let paused = {
                        let state = app.state::<AppState>();
                        let guard = state.recording.lock().unwrap();
                        matches!(guard.as_ref().map(|s| s.phase), Some(Phase::Paused))
                    };
                    let _ = if paused { resume(app).await } else { pause(app).await };
                });
            }
            "rec-cancel" => {
                tauri::async_runtime::spawn(async move {
                    let _ = cancel(app).await;
                });
            }
            _ => {}
        }
    });
}

/// Builds the transient recording tray icon. Returns the menu-item handles so the
/// session can toggle Pause⇄Resume and disable entries while stopping.
fn create_tray(app: &AppHandle) -> Option<RecMenu> {
    let stop_item = MenuItem::with_id(app, "rec-stop", "Stop Recording", true, None::<&str>).ok()?;
    let pause_item = MenuItem::with_id(app, "rec-pause", "Pause", true, None::<&str>).ok()?;
    let cancel_item =
        MenuItem::with_id(app, "rec-cancel", "Discard Recording", true, None::<&str>).ok()?;
    let separator = PredefinedMenuItem::separator(app).ok()?;
    let menu = Menu::with_items(app, &[&stop_item, &pause_item, &separator, &cancel_item]).ok()?;

    let tray = TrayIconBuilder::with_id("recording")
        .icon(recording_icon(false))
        .tooltip("Recording — click to stop, right-click for options")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle().clone();
                tauri::async_runtime::spawn(async move {
                    let _ = stop(app).await;
                });
            }
        })
        .build(app)
        .ok()?;
    let _ = tray.set_title(Some("0:00"));

    Some(RecMenu {
        stop: stop_item,
        pause: pause_item,
        cancel: cancel_item,
    })
}

fn update_tray_visuals(app: &AppHandle, paused: bool) {
    if let Some(tray) = app.tray_by_id("recording") {
        let _ = tray.set_icon(Some(recording_icon(paused)));
        let elapsed = {
            let state = app.state::<AppState>();
            let guard = state.recording.lock().unwrap();
            guard.as_ref().map(|s| s.elapsed_ms).unwrap_or(0)
        };
        let title = if paused {
            format!("⏸ {}", format_elapsed(elapsed))
        } else {
            format_elapsed(elapsed)
        };
        let _ = tray.set_title(Some(title));
    }
}

fn remove_tray(app: &AppHandle) {
    let app = app.clone();
    let _ = app.clone().run_on_main_thread(move || {
        let _ = app.remove_tray_by_id("recording");
    });
}

/// 44×44 RGBA recording glyph: red filled dot; paused = red ring. NOT a template
/// image — templates get tinted to the menu-bar text color, losing the red.
fn recording_icon(paused: bool) -> tauri::image::Image<'static> {
    const S: usize = 44;
    let center = (S as f32 - 1.0) / 2.0;
    let radius = 12.0f32;
    let mut px = vec![0u8; S * S * 4];
    for y in 0..S {
        for x in 0..S {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let d = (dx * dx + dy * dy).sqrt();
            let alpha = if paused {
                let ring_center = radius - 1.5;
                (1.5 - (d - ring_center).abs() + 0.5).clamp(0.0, 1.0)
            } else {
                (radius - d + 0.5).clamp(0.0, 1.0)
            };
            if alpha > 0.0 {
                let i = (y * S + x) * 4;
                px[i] = 0xFF;
                px[i + 1] = 0x3B;
                px[i + 2] = 0x30;
                px[i + 3] = (alpha * 255.0) as u8;
            }
        }
    }
    tauri::image::Image::new_owned(px, S as u32, S as u32)
}

// ---- Housekeeping ---------------------------------------------------------

pub fn recordings_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .app_cache_dir()
        .unwrap_or_else(|_| std::env::temp_dir())
        .join("recordings")
}

/// Deletes leftover recording files from a previous run (crash, kill -9).
pub fn sweep_stale(app: &AppHandle) {
    let dir = recordings_dir(app);
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let mut n = 0;
    for entry in entries.flatten() {
        if entry.file_type().map(|t| t.is_file()).unwrap_or(false)
            && std::fs::remove_file(entry.path()).is_ok()
        {
            n += 1;
        }
    }
    if n > 0 {
        eprintln!("[teil.ing] swept {n} stale recording file(s)");
    }
}

/// RunEvent::Exit — abandon a live recording without hanging quit (3 s watchdog).
pub fn shutdown(app: &AppHandle) {
    let live = {
        let state = app.state::<AppState>();
        let guard = state.recording.lock().unwrap();
        guard.as_ref().map(|s| s.out_path.clone())
    };
    let Some(out_path) = live else { return };
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        native::record_cancel();
        let _ = tx.send(());
    });
    let _ = rx.recv_timeout(Duration::from_secs(3));
    let _ = std::fs::remove_file(out_path);
}

// ---- Small helpers --------------------------------------------------------

fn with_session(app: &AppHandle, f: impl FnOnce(&mut Session)) {
    let state = app.state::<AppState>();
    let mut guard = state.recording.lock().unwrap();
    if let Some(s) = guard.as_mut() {
        f(s);
    }
}

fn menu_items(app: &AppHandle) -> Option<RecMenu> {
    let state = app.state::<AppState>();
    let guard = state.recording.lock().unwrap();
    guard.as_ref().and_then(|s| {
        s.menu.as_ref().map(|m| RecMenu {
            stop: m.stop.clone(),
            pause: m.pause.clone(),
            cancel: m.cancel.clone(),
        })
    })
}

fn clear_session(app: &AppHandle) {
    let state = app.state::<AppState>();
    let mut guard = state.recording.lock().unwrap();
    if let Some(s) = guard.as_mut() {
        if let Some(t) = s.tick.take() {
            t.abort();
        }
    }
    *guard = None;
}

fn format_elapsed(ms: u64) -> String {
    let total = ms / 1000;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}
