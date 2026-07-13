//! App core — counterpart of teil_ing_clientApp + AppDelegate.
//! Owns shared state, the tray icon, global shortcuts, and the popover/overlay windows.

mod api;
mod auth;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod capture;
#[cfg(target_os = "macos")]
mod capture_macos;
#[cfg(target_os = "windows")]
mod capture_windows;
mod commands;
mod prefs;
mod secure;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewWindow, WindowEvent,
};
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
use tauri::{LogicalPosition, LogicalSize, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use prefs::Prefs;

/// Shared application state (Swift: the various @MainActor singletons).
pub struct AppState {
    pub prefs: Mutex<Prefs>,
    pub prefs_path: PathBuf,
    pub shortcuts: Mutex<HashMap<String, String>>,
    pub shortcuts_path: PathBuf,
    /// Last failed upload image, retained for "Retry Upload" (Swift: UploadService.failedCapture).
    pub last_failed: Mutex<Option<image::RgbaImage>>,
    /// Mode + virtual-desktop origin for the overlay about to open — the overlay reads these
    /// via the `overlay_mode` command (query strings + init scripts didn't survive reliably).
    pub overlay_mode: Mutex<String>,
    pub overlay_origin: Mutex<(i32, i32)>,
    /// In-flight browser sign-in, waiting for its teiling://connect callback.
    pub pending_signin: Mutex<Option<auth::PendingSignin>>,
}

pub fn run() {
    tauri::Builder::default()
        // Must be first: forwards a second launch (e.g. Windows teiling:// scheme
        // activation) to the running instance; the deep-link feature re-emits the
        // URL through the deep-link plugin below.
        .plugin(tauri_plugin_single_instance::init(|_app, _argv, _cwd| {}))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            setup(app)?;
            Ok(())
        })
        .on_page_load(|webview, payload| {
            eprintln!(
                "[teil.ing] page load ({:?}): {} {}",
                payload.event(),
                webview.label(),
                payload.url()
            );
        })
        .on_window_event(|window, event| {
            // Transient popover: hide the main window when it loses focus (Swift: NSPopover .transient).
            if window.label() == "main" {
                if let WindowEvent::Focused(false) = event {
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::has_api_key,
            commands::begin_browser_signin,
            commands::save_api_key,
            commands::delete_api_key,
            commands::masked_api_key,
            commands::get_prefs,
            commands::set_prefs,
            commands::get_shortcuts,
            commands::set_shortcut,
            commands::reset_shortcuts,
            commands::overlay_mode,
            commands::begin_region_capture,
            commands::begin_window_capture,
            commands::capture_fullscreen,
            commands::check_screen_permission,
            commands::request_screen_permission,
            commands::open_screen_settings,
            commands::relaunch_app,
            commands::finish_region_capture,
            commands::list_windows,
            commands::capture_window,
            commands::list_images,
            commands::get_quota,
            commands::get_image_details,
            commands::update_image,
            commands::delete_image,
            commands::retry_upload,
            commands::hide_popover,
            commands::open_preferences,
            commands::open_external,
            commands::quit_app,
            commands::app_version,
            commands::check_for_updates,
            commands::install_update,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    // macOS: run as a menu-bar accessory (no Dock icon) — Swift: LSUIElement + .accessory.
    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Accessory);

    let handle = app.handle().clone();

    // Config paths + persisted state.
    let config_dir = handle.path().app_config_dir()?;
    std::fs::create_dir_all(&config_dir).ok();
    let prefs_path = config_dir.join("prefs.json");
    let shortcuts_path = config_dir.join("shortcuts.json");

    let prefs = prefs::load_prefs(&prefs_path);
    let shortcuts = prefs::load_shortcuts(&shortcuts_path);

    // Background update probe (Swift: UpdateService periodic check).
    if prefs.auto_check_for_updates {
        commands::spawn_update_check(handle.clone());
    }

    app.manage(AppState {
        prefs: Mutex::new(prefs),
        prefs_path,
        shortcuts: Mutex::new(shortcuts.clone()),
        shortcuts_path,
        last_failed: Mutex::new(None),
        overlay_mode: Mutex::new(String::new()),
        overlay_origin: Mutex::new((0, 0)),
        pending_signin: Mutex::new(None),
    });

    // Browser sign-in callbacks (teiling://connect?code=…&state=…).
    {
        use tauri_plugin_deep_link::DeepLinkExt;
        // Dev convenience: register the scheme at runtime where the OS allows it
        // (Windows/Linux). On macOS the scheme comes from the bundle's Info.plist.
        #[cfg(any(windows, target_os = "linux"))]
        let _ = app.deep_link().register_all();

        let deep_link_handle = handle.clone();
        app.deep_link().on_open_url(move |event| {
            for url in event.urls() {
                auth::handle_callback(&deep_link_handle, &url);
            }
        });
    }

    // Startup permission audit — the popover shows a banner while this is false.
    #[cfg(target_os = "macos")]
    eprintln!(
        "[teil.ing] screen recording permission: {}",
        capture_macos::has_screen_permission()
    );

    build_tray(app)?;

    for (mode, accel) in &shortcuts {
        if let Err(e) = register_shortcut(&handle, mode, accel) {
            eprintln!("[teil.ing] could not register shortcut {mode} = {accel}: {e}");
        }
    }

    Ok(())
}

fn build_tray(app: &mut tauri::App) -> tauri::Result<()> {
    let quit = MenuItem::with_id(app, "quit", "Quit teil.ing", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&quit])?;

    let mut builder = TrayIconBuilder::with_id("main")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("teil.ing")
        .on_menu_event(|app, event| {
            if event.id().as_ref() == "quit" {
                app.exit(0);
            }
        })
        .on_tray_icon_event(|tray, event| {
            // Left-click toggles the popover near the click point (Swift: togglePopover).
            if let TrayIconEvent::Click {
                button,
                button_state,
                position,
                ..
            } = event
            {
                eprintln!("[teil.ing] tray click: {button:?} {button_state:?}");
                if button == MouseButton::Left && button_state == MouseButtonState::Up {
                    toggle_popover(tray.app_handle(), position);
                }
            }
        });

    // macOS: dashed-rectangle template image (Swift app: SF Symbol "rectangle.dashed") —
    // black + alpha, so the system tints it for light/dark menu bars and selection.
    #[cfg(target_os = "macos")]
    {
        let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
        builder = builder.icon(icon).icon_as_template(true);
    }
    // Windows: keep the colored app icon — a black template image would be
    // invisible on the dark taskbar.
    #[cfg(not(target_os = "macos"))]
    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }
    builder.build(app)?;
    Ok(())
}

fn toggle_popover(app: &AppHandle, click: PhysicalPosition<f64>) {
    let Some(win) = app.get_webview_window("main") else {
        eprintln!("[teil.ing] toggle_popover: main window not found");
        return;
    };
    let visible = win.is_visible().unwrap_or(false);
    eprintln!("[teil.ing] toggle_popover: click={click:?} visible={visible}");
    if visible {
        let _ = win.hide();
    } else {
        position_popover(&win, click);
        if let Err(e) = win.show() {
            eprintln!("[teil.ing] popover show failed: {e}");
        }
        if let Err(e) = win.set_focus() {
            eprintln!("[teil.ing] popover set_focus failed: {e}");
        }
        eprintln!(
            "[teil.ing] popover shown at {:?} size {:?} visible={:?}",
            win.outer_position(),
            win.outer_size(),
            win.is_visible()
        );
        // Refresh the recent-upload list on every open (Swift: showPopover → refreshAll).
        let _ = app.emit("popover-shown", ());
    }
}

/// Place the popover near the tray click, clamped on-screen. Approximate; the tray sits at
/// the top on macOS and bottom-right on Windows, so this flips above/below the click.
fn position_popover(win: &WebviewWindow, click: PhysicalPosition<f64>) {
    let size = win.outer_size().unwrap_or(PhysicalSize::new(336, 560));
    let click_x = click.x as i32;
    let click_y = click.y as i32;
    let (mw, mh) = win
        .current_monitor()
        .ok()
        .flatten()
        .map(|m| (m.size().width as i32, m.size().height as i32))
        .unwrap_or((1920, 1080));

    let mut x = click_x - size.width as i32 / 2;
    x = x.clamp(8, (mw - size.width as i32 - 8).max(8));

    // If the click is in the bottom half of the display (Windows tray), open above it.
    let y = if click_y > mh / 2 {
        (click_y - size.height as i32 - 8).max(8)
    } else {
        (click_y + 8).min((mh - size.height as i32 - 8).max(8))
    };
    let _ = win.set_position(PhysicalPosition::new(x, y));
}

// ---- Capture triggers (shared by tray/UI commands and global shortcuts) --

pub fn register_shortcut(app: &AppHandle, mode: &str, accel: &str) -> Result<(), String> {
    let mode = mode.to_string();
    let handle = app.clone();
    app.global_shortcut()
        .on_shortcut(accel, move |_app, _sc, event| {
            if event.state == ShortcutState::Pressed {
                trigger_capture(&handle, &mode);
            }
        })
        .map_err(|e| e.to_string())
}

pub fn trigger_capture(app: &AppHandle, mode: &str) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.hide();
    }
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        // Fully native flows (macOS: TeilCapture Swift library; Windows:
        // teil-capture-windows Win32 overlay) — no HTML overlay.
        let mode: &'static str = match mode {
            "region" => "region",
            "window" => "window",
            "fullscreen" => "fullscreen",
            _ => return,
        };
        commands::spawn_native_capture(app.clone(), mode);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    match mode {
        "region" => {
            let _ = open_overlay(app, "region");
        }
        "window" => {
            let _ = open_overlay(app, "window");
        }
        "fullscreen" => commands::spawn_fullscreen(app.clone()),
        _ => {}
    }
}

/// Open the transparent, fullscreen, always-on-top capture overlay covering all displays.
/// Not used on macOS/Windows — those have native overlays (TeilCapture Swift library /
/// teil-capture-windows). Transparent webview overlays were unreliable on Windows.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn open_overlay(app: &AppHandle, mode: &str) -> anyhow::Result<()> {
    if let Some(w) = app.get_webview_window("overlay") {
        let _ = w.close();
    }
    // xcap monitor bounds come from CGDisplayBounds → LOGICAL points on macOS. Tauri window
    // sizing is logical too, so size/position the overlay in logical units. (Using Physical here
    // made the overlay a fraction of the screen on Retina displays.)
    let (x, y, w, h) = capture::virtual_bounds()?;
    // Stash the mode + virtual origin BEFORE creating the overlay; the overlay reads them via the
    // `overlay_mode` command (query strings and init scripts didn't reach the webview reliably).
    {
        let st = app.state::<AppState>();
        *st.overlay_mode.lock().unwrap() = mode.to_string();
        *st.overlay_origin.lock().unwrap() = (x, y);
    }
    let win = WebviewWindowBuilder::new(app, "overlay", WebviewUrl::App("overlay.html".into()))
        .title("capture")
        .inner_size(w as f64, h as f64)
        .position(x as f64, y as f64)
        .transparent(true)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .shadow(false)
        .focused(true)
        .build()?;
    let _ = win.set_position(LogicalPosition::new(x as f64, y as f64));
    let _ = win.set_size(LogicalSize::new(w as f64, h as f64));
    let _ = win.set_focus();
    Ok(())
}
