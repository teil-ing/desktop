//! Tauri command handlers — the frontend's backend surface (Swift: AppDelegate actions +
//! APIService + UploadService + KeychainService + PreferencesStore).

use image::RgbaImage;
use serde::Deserialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_global_shortcut::GlobalShortcutExt;
use tauri_plugin_opener::OpenerExt;

use crate::api::{self, ImageListResponse, ImageResponse, ImageUpdateRequest, QuotaResponse};
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
use crate::capture::{self, WindowInfo};
#[cfg(target_os = "macos")]
use crate::capture_macos;
#[cfg(target_os = "windows")]
use crate::capture_windows;
use crate::prefs::{self, Prefs};
use crate::{secure, AppState};

// ---- Small helpers -------------------------------------------------------

fn key() -> Result<String, String> {
    secure::get_api_key().ok_or_else(|| "No API key found. Please add your key in settings.".to_string())
}

fn set_tray_tooltip(app: &AppHandle, text: &str) {
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(text));
    }
}

/// Run a blocking capture off the async runtime so IPC/UI stays responsive.
async fn blocking<T, F>(f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tokio::task::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

// ---- Auth / onboarding ---------------------------------------------------

#[tauri::command]
pub fn has_api_key() -> bool {
    secure::get_api_key().is_some()
}

/// Opens the browser connect handshake (Swift: AuthService.signInViaBrowser).
/// Completion arrives asynchronously via the "auth-changed"/"auth-feedback" events.
#[tauri::command]
pub fn begin_browser_signin(app: AppHandle) -> Result<(), String> {
    crate::auth::begin(&app)
}

#[tauri::command]
pub async fn save_api_key(key: String) -> Result<(), String> {
    // Validate against GET /images before saving (Swift: APIValidationService).
    let valid = api::validate(&key).await.map_err(|e| e.to_string())?;
    if !valid {
        return Err("Invalid API key. Please check and try again.".into());
    }
    secure::set_api_key(&key).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_api_key() {
    secure::delete_api_key();
}

#[tauri::command]
pub fn masked_api_key() -> Option<String> {
    secure::masked()
}

// ---- Preferences ---------------------------------------------------------

#[tauri::command]
pub fn get_prefs(state: State<AppState>) -> Prefs {
    state.prefs.lock().unwrap().clone()
}

#[tauri::command]
pub fn set_prefs(state: State<AppState>, prefs: Prefs) {
    // NOTE: launch_at_login is persisted but not yet wired to an OS autostart mechanism
    // (macOS SMAppService / Windows registry Run key). See README TODO.
    *state.prefs.lock().unwrap() = prefs.clone();
    prefs::save_prefs(&state.prefs_path, &prefs);
}

// ---- Shortcuts -----------------------------------------------------------

#[tauri::command]
pub fn get_shortcuts(state: State<AppState>) -> std::collections::HashMap<String, String> {
    state.shortcuts.lock().unwrap().clone()
}

#[tauri::command]
pub fn set_shortcut(
    app: AppHandle,
    state: State<AppState>,
    mode: String,
    accelerator: String,
) -> Result<(), String> {
    let old = state.shortcuts.lock().unwrap().get(&mode).cloned();
    if let Some(old) = &old {
        let _ = app.global_shortcut().unregister(old.as_str());
    }
    if let Err(e) = crate::register_shortcut(&app, &mode, &accelerator) {
        // Roll back so the previous binding keeps working.
        if let Some(old) = &old {
            let _ = crate::register_shortcut(&app, &mode, old);
        }
        return Err(format!("Could not register that shortcut: {e}"));
    }
    let mut map = state.shortcuts.lock().unwrap();
    map.insert(mode, accelerator);
    prefs::save_shortcuts(&state.shortcuts_path, &map);
    Ok(())
}

#[tauri::command]
pub fn reset_shortcuts(app: AppHandle, state: State<AppState>) {
    let mut map = state.shortcuts.lock().unwrap();
    for accel in map.values() {
        let _ = app.global_shortcut().unregister(accel.as_str());
    }
    *map = prefs::default_shortcuts();
    for (mode, accel) in map.iter() {
        let _ = crate::register_shortcut(&app, mode, accel);
    }
    prefs::save_shortcuts(&state.shortcuts_path, &map);
}

// ---- Capture flows -------------------------------------------------------
//
// macOS + Windows: fully native (TeilCapture Swift library / teil-capture-windows
// Win32 overlay) — one blocking call runs the selection overlay + capture and
// returns a PNG. Other platforms: HTML overlay (overlay.html) + xcap, as before.

/// Runs a native interactive capture off the async runtime, then feeds the
/// existing upload pipeline. Mode: "region" | "window" | "fullscreen".
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn spawn_native_capture(app: AppHandle, mode: &'static str) {
    // Without the TCC grant every capture fails — surface the popover (which is
    // parked on the blocking permission screen) instead of starting the overlay.
    #[cfg(target_os = "macos")]
    if !capture_macos::has_screen_permission() {
        eprintln!("[teil.ing] capture blocked: screen recording permission missing");
        show_main(&app);
        return;
    }
    // Hide the popover AND the settings window right before capturing. hide()
    // completes asynchronously in the compositor, so when either was actually
    // visible, wait a beat — otherwise it is still on screen when the overlay
    // freezes it and ends up baked into the screenshot.
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
    tauri::async_runtime::spawn(async move {
        if was_visible {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        #[cfg(target_os = "macos")]
        let run = move || capture_macos::capture_interactive(mode);
        #[cfg(target_os = "windows")]
        let run = move || capture_windows::capture_interactive(mode);
        match blocking(run).await {
            Ok(Some(png)) => match image::load_from_memory(&png) {
                Ok(img) => upload_capture(app, img.to_rgba8(), Some(png)).await,
                Err(e) => report_failure(&app, &format!("Capture failed: {e}")),
            },
            Ok(None) => {} // user cancelled — no capture, no feedback
            Err(e) => report_failure(&app, &native_error_message(&e)),
        }
    });
}

/// Failure text for the native capture path. On macOS a missing TCC grant is the
/// most common failure — say so explicitly.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn native_error_message(e: &str) -> String {
    #[cfg(target_os = "macos")]
    if !capture_macos::has_screen_permission() {
        return "Screen Recording permission is required. Grant it in System Settings → Privacy & Security, then restart the app.".into();
    }
    format!("Capture failed: {e}")
}

/// Mode + virtual-desktop origin the overlay should use (set by open_overlay just before
/// the overlay window is created). The overlay calls this on startup. Unused on macOS.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayInfo {
    pub mode: String,
    pub origin_x: i32,
    pub origin_y: i32,
}

#[tauri::command]
pub fn overlay_mode(state: State<AppState>) -> OverlayInfo {
    let mode = state.overlay_mode.lock().unwrap().clone();
    let (origin_x, origin_y) = *state.overlay_origin.lock().unwrap();
    OverlayInfo { mode, origin_x, origin_y }
}

#[tauri::command]
pub fn begin_region_capture(app: AppHandle) -> Result<(), String> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        spawn_native_capture(app, "region");
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        crate::open_overlay(&app, "region").map_err(|e| e.to_string())
    }
}

#[tauri::command]
pub fn begin_window_capture(app: AppHandle) -> Result<(), String> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        spawn_native_capture(app, "window");
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        crate::open_overlay(&app, "window").map_err(|e| e.to_string())
    }
}

#[tauri::command]
pub fn capture_fullscreen(app: AppHandle) {
    spawn_fullscreen(app);
}

// ---- Screen Recording permission (macOS TCC; trivially true elsewhere) ----

/// Instant, non-prompting check (CGPreflightScreenCaptureAccess).
#[tauri::command]
pub fn check_screen_permission() -> bool {
    #[cfg(target_os = "macos")]
    {
        capture_macos::has_screen_permission()
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

/// Asks macOS for access — shows the system prompt at most once per app, then
/// only reports status. Returns the CURRENT status (a fresh grant needs a relaunch).
#[tauri::command]
pub fn request_screen_permission() -> bool {
    #[cfg(target_os = "macos")]
    {
        capture_macos::request_screen_permission()
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

#[tauri::command]
pub fn open_screen_settings() {
    #[cfg(target_os = "macos")]
    capture_macos::open_screen_settings();
}

/// Relaunch the app — needed after granting Screen Recording for TCC to apply.
#[tauri::command]
pub fn relaunch_app(app: AppHandle) {
    app.restart();
}

// Fields are only read by the HTML-overlay flow; native platforms keep the stub signature.
#[cfg_attr(any(target_os = "macos", target_os = "windows"), allow(dead_code))]
#[derive(Deserialize)]
pub struct Region {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Not used on macOS/Windows — the native overlay finishes its own capture.
#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tauri::command]
pub async fn finish_region_capture(_app: AppHandle, _region: Option<Region>) -> Result<(), String> {
    Err("Region capture is handled natively on this platform.".into())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[tauri::command]
pub async fn finish_region_capture(app: AppHandle, region: Option<Region>) -> Result<(), String> {
    if let Some(overlay) = app.get_webview_window("overlay") {
        let _ = overlay.close();
    }
    let region = match region {
        Some(r) => r,
        None => return Ok(()), // user cancelled — no capture, no feedback
    };

    // The overlay is sized in logical points at the virtual-desktop origin, and its CSS pixels
    // equal points. xcap's capture_region also works in points (CGDisplayBounds space) — so add
    // the region (points) to the virtual origin (points); no scale factor involved.
    let (vx, vy, _, _) = capture::virtual_bounds().map_err(|e| e.to_string())?;
    let gx = vx + region.x.round() as i32;
    let gy = vy + region.y.round() as i32;
    let gw = region.width.round() as u32;
    let gh = region.height.round() as u32;
    eprintln!("[teil.ing] region capture global=({gx},{gy},{gw},{gh})");

    // Give the compositor a frame to remove the just-closed overlay before capturing (Swift: 50ms).
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;

    let cap = match blocking(move || capture::capture_region_global(gx, gy, gw, gh).map_err(|e| e.to_string())).await {
        Ok(c) => c,
        Err(e) => {
            report_failure(&app, &format!("Capture failed: {e}"));
            return Err(e);
        }
    };
    upload_capture(app, cap.image, None).await;
    Ok(())
}

/// Not used on macOS/Windows — the native picker enumerates windows itself.
#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tauri::command]
pub async fn list_windows() -> Result<Vec<serde_json::Value>, String> {
    Err("Window selection is handled natively on this platform.".into())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[tauri::command]
pub async fn list_windows() -> Result<Vec<WindowInfo>, String> {
    blocking(|| capture::list_windows().map_err(|e| e.to_string())).await
}

/// Not used on macOS/Windows — the native picker captures the clicked window itself.
#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tauri::command]
pub async fn capture_window(_app: AppHandle, _window_id: u32) -> Result<(), String> {
    Err("Window capture is handled natively on this platform.".into())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[tauri::command]
pub async fn capture_window(app: AppHandle, window_id: u32) -> Result<(), String> {
    if let Some(overlay) = app.get_webview_window("overlay") {
        let _ = overlay.close();
    }
    // Let the compositor remove the picker overlay before capturing the window.
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    let cap = match blocking(move || capture::capture_window(window_id).map_err(|e| e.to_string())).await {
        Ok(c) => c,
        Err(e) => {
            report_failure(&app, &format!("Capture failed: {e}"));
            return Err(e);
        }
    };
    upload_capture(app, cap.image, None).await;
    Ok(())
}

/// Fullscreen capture, then upload. Native on macOS (display under cursor) and
/// Windows (primary display); xcap primary display elsewhere.
pub fn spawn_fullscreen(app: AppHandle) {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    spawn_native_capture(app, "fullscreen");
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    tauri::async_runtime::spawn(async move {
        match blocking(|| capture::capture_primary().map_err(|e| e.to_string())).await {
            Ok(cap) => upload_capture(app, cap.image, None).await,
            Err(msg) => report_failure(&app, &format!("Capture failed: {msg}")),
        }
    });
}

// ---- Upload orchestration (Swift: UploadService.performUpload) -----------

/// Show the popover and push a failure banner (Swift: reopen popover on error) + log to stderr.
fn report_failure(app: &AppHandle, message: &str) {
    eprintln!("[teil.ing] {message}");
    let _ = app.emit("upload-feedback", serde_json::json!({"kind":"failed","message":message}));
    show_main(app);
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// PNG-encode a captured image for upload.
fn encode_png(image: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut buf = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut buf, image::ImageFormat::Png)
        .map_err(|e| format!("PNG encode failed: {e}"))?;
    Ok(buf.into_inner())
}

/// `png`: pass the already-encoded bytes when the capture backend produced them
/// (native macOS path) to skip re-encoding; None encodes from `image`.
async fn upload_capture(app: AppHandle, image: RgbaImage, png: Option<Vec<u8>>) {
    eprintln!("[teil.ing] uploading {}x{} image", image.width(), image.height());
    let _ = app.emit("upload-feedback", serde_json::json!({"kind":"started"}));
    set_tray_tooltip(&app, "Uploading…");

    // Snapshot prefs (drop the lock before awaiting).
    let prefs = { app.state::<AppState>().prefs.lock().unwrap().clone() };

    // Image-mode clipboard copy happens BEFORE the upload: the capture is pasteable
    // immediately and survives an upload failure. URL mode can only copy afterwards.
    if prefs.clipboard_copy && prefs.clipboard_mode == "image" {
        let _ = copy_image(&app, &image);
    }

    let key = match secure::get_api_key() {
        Some(k) => k,
        None => return fail(&app, "No API key found. Please add your key in settings.", image),
    };
    let png = match png.map_or_else(|| encode_png(&image), Ok) {
        Ok(p) => p,
        Err(e) => return fail(&app, &e, image),
    };

    match api::upload(&key, png, prefs.strip_exif, prefs.private_upload).await {
        Ok((id, share_url)) => {
            if prefs.clipboard_copy && prefs.clipboard_mode != "image" {
                let _ = app.clipboard().write_text(share_url.clone());
            }
            if prefs.open_in_browser {
                let _ = app.opener().open_url(share_url.clone(), None::<&str>);
            }
            *app.state::<AppState>().last_failed.lock().unwrap() = None;
            set_tray_tooltip(&app, "teil.ing");
            eprintln!("[teil.ing] upload ok: {share_url}");
            let _ = app.emit(
                "upload-feedback",
                serde_json::json!({"kind":"succeeded","imageId":id,"shareUrl":share_url}),
            );
        }
        Err(e) => fail(&app, &e.to_string(), image),
    }
}

fn fail(app: &AppHandle, message: &str, image: RgbaImage) {
    *app.state::<AppState>().last_failed.lock().unwrap() = Some(image);
    set_tray_tooltip(app, "Upload failed");
    report_failure(app, message);
}

fn copy_image(app: &AppHandle, image: &RgbaImage) -> Result<(), String> {
    let (w, h) = (image.width(), image.height());
    let img = tauri::image::Image::new_owned(image.as_raw().clone(), w, h);
    app.clipboard().write_image(&img).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn retry_upload(app: AppHandle) {
    let image = { app.state::<AppState>().last_failed.lock().unwrap().clone() };
    if let Some(image) = image {
        upload_capture(app, image, None).await;
    }
}

// ---- Remote API (Swift: APIService) --------------------------------------

#[tauri::command]
pub async fn list_images(limit: i64, offset: i64) -> Result<ImageListResponse, String> {
    api::list_images(&key()?, limit, offset).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_quota() -> Result<QuotaResponse, String> {
    api::get_quota(&key()?).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_image_details(id: String) -> Result<ImageResponse, String> {
    api::get_image_details(&key()?, &id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn update_image(id: String, update: ImageUpdateRequest) -> Result<(), String> {
    api::update_image(&key()?, &id, &update).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_image(id: String) -> Result<(), String> {
    api::delete_image(&key()?, &id).await.map_err(|e| e.to_string())
}

// ---- Window chrome / meta ------------------------------------------------

#[tauri::command]
pub fn hide_popover(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
}

/// Open (or focus) the Settings window — a real decorated dialog, not the in-popover pane.
/// MUST be async: on Windows, building a webview window from a synchronous command
/// stalls WebView2 initialization (the window opens but stays white).
#[tauri::command]
pub async fn open_preferences(app: AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("preferences") {
        let _ = win.show();
        let _ = win.set_focus();
        return Ok(());
    }
    let win = tauri::WebviewWindowBuilder::new(
        &app,
        "preferences",
        tauri::WebviewUrl::App("preferences.html".into()),
    )
    .title("teil.ing Settings")
    .inner_size(460.0, 440.0)
    .min_inner_size(420.0, 380.0)
    .resizable(true)
    .maximizable(false)
    .center()
    .build()
    .map_err(|e| e.to_string())?;
    let _ = win.set_focus();
    Ok(())
}

#[tauri::command]
pub fn open_external(app: AppHandle, url: String) {
    let _ = app.opener().open_url(url, None::<&str>);
}

#[tauri::command]
pub fn quit_app(app: AppHandle) {
    app.exit(0);
}

#[tauri::command]
pub fn app_version(app: AppHandle) -> String {
    app.package_info().version.to_string()
}

/// Check the update endpoint (GitHub releases latest.json) via the updater plugin.
#[tauri::command]
pub async fn check_for_updates(app: AppHandle) -> Result<serde_json::Value, String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|e| e.to_string())?;
    match updater.check().await {
        Ok(Some(update)) => Ok(serde_json::json!({
            "available": true,
            "version": update.version,
        })),
        Ok(None) => Ok(serde_json::json!({ "available": false, "version": null })),
        Err(e) => Err(format!("Could not check for updates: {e}")),
    }
}

/// Download, verify (minisign), install the pending update, and relaunch.
#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "No update available.".to_string())?;
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| format!("Update failed: {e}"))?;
    app.restart();
}

/// Startup auto-check (pref-gated). Emits "update-available" {version} so the
/// popover can show its update pill. Failures are silent — this is a background probe.
pub fn spawn_update_check(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        use tauri_plugin_updater::UpdaterExt;
        // Give the network stack / app startup a moment.
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let Ok(updater) = app.updater() else { return };
        if let Ok(Some(update)) = updater.check().await {
            eprintln!("[teil.ing] update available: v{}", update.version);
            let _ = app.emit("update-available", serde_json::json!({ "version": update.version }));
        }
    });
}
