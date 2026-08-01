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
    AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, PhysicalSize, WebviewWindow,
    WindowEvent,
};
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
use tauri::{LogicalPosition, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use prefs::Prefs;

/// Where the tray icon sat at the last click, in physical px. Placement is recomputed
/// from this every time the popover's content height changes, so the popover keeps
/// hugging the icon as it grows and shrinks.
#[derive(Clone, Copy)]
pub struct TrayAnchor {
    /// Horizontal center of the icon.
    pub center_x: i32,
    pub icon_top: i32,
    pub icon_bottom: i32,
    /// Click point, used to pick the monitor the tray lives on.
    pub probe: (f64, f64),
}

/// Popover card width in logical px — 320px card + 8px body padding either side.
pub const POPOVER_WIDTH: f64 = 336.0;
/// Height used until the frontend reports its first measurement.
const POPOVER_DEFAULT_HEIGHT: u32 = 560;

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
    /// Tray-icon anchor from the last tray click; None until the popover is first opened.
    pub popover_anchor: Mutex<Option<TrayAnchor>>,
    /// Popover content height in LOGICAL px, reported by the frontend after each render.
    pub popover_height: Mutex<u32>,
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
            commands::set_popover_height,
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
        popover_anchor: Mutex::new(None),
        popover_height: Mutex::new(POPOVER_DEFAULT_HEIGHT),
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
            // Left-click toggles the popover attached to the tray icon (Swift: togglePopover).
            if let TrayIconEvent::Click {
                button,
                button_state,
                position,
                rect,
                ..
            } = event
            {
                eprintln!("[teil.ing] tray click: {button:?} {button_state:?} rect={rect:?}");
                if button == MouseButton::Left && button_state == MouseButtonState::Up {
                    toggle_popover(tray.app_handle(), position, rect);
                }
            }
        });

    // macOS: dashed-rectangle template image (Swift app: SF Symbol "rectangle.dashed") —
    // black + alpha, so the system itself tints it for light AND dark menu bars (and
    // for the selected/highlighted state). No manual theme tracking needed.
    #[cfg(target_os = "macos")]
    {
        let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
        builder = builder.icon(icon).icon_as_template(true);
    }
    // Windows and the rest: no template images, so the glyph is recolored to contrast
    // with the taskbar and re-applied whenever the system theme flips.
    #[cfg(not(target_os = "macos"))]
    {
        match tray_icon_for_theme(light_taskbar()) {
            Some(icon) => builder = builder.icon(icon),
            // Fall back to the colored app icon if the glyph ever fails to decode.
            None => {
                if let Some(icon) = app.default_window_icon().cloned() {
                    builder = builder.icon(icon);
                }
            }
        }
    }
    builder.build(app)?;

    #[cfg(target_os = "windows")]
    watch_taskbar_theme(app.handle().clone());

    Ok(())
}

/// True when the taskbar/tray is light and the glyph must be drawn dark.
///
/// Windows exposes this as SystemUsesLightTheme, which is separate from the app theme
/// (AppsUseLightTheme) — under "Choose your mode: Custom" the two disagree, and it's
/// the system one that colors the taskbar. Defaults to a dark taskbar, the Windows
/// out-of-the-box look.
#[cfg(target_os = "windows")]
fn light_taskbar() -> bool {
    winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|k| k.get_value::<u32, _>("SystemUsesLightTheme"))
        .map(|v| v == 1)
        .unwrap_or(false)
}

/// Linux and friends: no tray theme to read, assume a dark panel.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn light_taskbar() -> bool {
    false
}

/// tray.png is a black + alpha glyph; recolor it to contrast with the tray background.
/// Alpha is left untouched so the antialiased dashes keep their shape.
#[cfg(not(target_os = "macos"))]
fn tray_icon_for_theme(light_taskbar: bool) -> Option<tauri::image::Image<'static>> {
    let rgba = image::load_from_memory(include_bytes!("../icons/tray.png"))
        .ok()?
        .to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    let mut px = rgba.into_raw();
    let lum = if light_taskbar { 0 } else { 255 };
    for p in px.chunks_exact_mut(4) {
        p[0] = lum;
        p[1] = lum;
        p[2] = lum;
    }
    Some(tauri::image::Image::new_owned(px, w, h))
}

/// Repaint the tray glyph when the user flips Windows between light and dark.
///
/// Polled rather than event-driven: WM_SETTINGCHANGE/ThemeChanged track the APP theme,
/// which "Custom" mode lets you set independently of the taskbar, so the registry value
/// is the only reliable signal. Theme flips are rare and the check is a registry read.
#[cfg(target_os = "windows")]
fn watch_taskbar_theme(app: AppHandle) {
    std::thread::spawn(move || {
        let mut current = light_taskbar();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let light = light_taskbar();
            if light == current {
                continue;
            }
            current = light;
            if let (Some(tray), Some(icon)) = (app.tray_by_id("main"), tray_icon_for_theme(light)) {
                let _ = tray.set_icon(Some(icon));
            }
        }
    });
}

fn toggle_popover(app: &AppHandle, click: PhysicalPosition<f64>, icon_rect: tauri::Rect) {
    let Some(win) = app.get_webview_window("main") else {
        eprintln!("[teil.ing] toggle_popover: main window not found");
        return;
    };
    let visible = win.is_visible().unwrap_or(false);
    eprintln!("[teil.ing] toggle_popover: click={click:?} visible={visible}");
    if visible {
        let _ = win.hide();
    } else {
        // Anchor to the icon's bounds, not the click point — clicks land anywhere inside
        // the icon (or on a flyout icon well above the taskbar), and anchoring to the
        // bounds is what makes the popover hug the icon like a menu. A zero-sized rect
        // (platform didn't report one) falls back to the click point.
        let scale = monitor_scale_at(&win, click.x, click.y);
        let ipos: PhysicalPosition<i32> = icon_rect.position.to_physical(scale);
        let isize: PhysicalSize<u32> = icon_rect.size.to_physical(scale);
        let anchor = if isize.width > 0 && isize.height > 0 {
            TrayAnchor {
                center_x: ipos.x + isize.width as i32 / 2,
                icon_top: ipos.y,
                icon_bottom: ipos.y + isize.height as i32,
                probe: (click.x, click.y),
            }
        } else {
            TrayAnchor {
                center_x: click.x as i32,
                icon_top: click.y as i32,
                icon_bottom: click.y as i32,
                probe: (click.x, click.y),
            }
        };

        let state = app.state::<AppState>();
        *state.popover_anchor.lock().unwrap() = Some(anchor);
        let height = *state.popover_height.lock().unwrap();
        place_popover(&win, anchor, height);

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

/// Scale factor of the monitor containing a physical point.
fn monitor_scale_at(win: &WebviewWindow, x: f64, y: f64) -> f64 {
    win.monitor_from_point(x, y)
        .ok()
        .flatten()
        .or_else(|| win.primary_monitor().ok().flatten())
        .map(|m| m.scale_factor())
        .unwrap_or(1.0)
}

/// Size the popover to `height` (logical px of content) and place it attached to the
/// tray ICON — centered on it, opening upward flush with its top edge when the tray is
/// at the bottom (Windows) and downward below it when the tray is at the top (macOS),
/// like a native tray menu. Clamped to the monitor's work area.
///
/// Sizing to the content is what makes the bottom anchor land: the window is
/// transparent and the card is laid out from its TOP, so a window taller than its card
/// left the card floating above the tray by the unused remainder (and the empty part
/// still swallowed clicks). All coordinates physical px.
pub(crate) fn place_popover(win: &WebviewWindow, anchor: TrayAnchor, height: u32) {
    // current_monitor() is unreliable for a still-hidden window (None → a 1920x1080
    // fallback that dragged the popover toward screen-center on larger displays, and
    // it ignored the monitor origin on multi-monitor setups). Locate the monitor
    // from the click point instead, and position within its WORK AREA (excludes the
    // taskbar/menu bar).
    let monitor = win
        .monitor_from_point(anchor.probe.0, anchor.probe.1)
        .ok()
        .flatten()
        .or_else(|| win.primary_monitor().ok().flatten());
    let scale = monitor.as_ref().map(|m| m.scale_factor()).unwrap_or(1.0);
    let (wx, wy, ww, wh) = monitor
        .map(|m| {
            let r = m.work_area();
            (r.position.x, r.position.y, r.size.width as i32, r.size.height as i32)
        })
        .unwrap_or((0, 0, 1920, 1040));

    let _ = win.set_size(LogicalSize::new(POPOVER_WIDTH, height as f64));
    // Derive the physical box from the size we just asked for — outer_size() can still
    // report the pre-resize value this early.
    let w = (POPOVER_WIDTH * scale).round() as i32;
    let h = (height as f64 * scale).round() as i32;

    let mut x = anchor.center_x - w / 2;
    x = x.clamp(wx + 8, (wx + ww - w - 8).max(wx + 8));

    // Icon in the lower half (Windows taskbar / overflow flyout): open upward, flush
    // above the icon. Upper half (macOS menu bar): open below the icon.
    const GAP: i32 = 6;
    let y = if (anchor.icon_top + anchor.icon_bottom) / 2 > wy + wh / 2 {
        (anchor.icon_top - h - GAP).max(wy + 8)
    } else {
        (anchor.icon_bottom + GAP).min((wy + wh - h - 8).max(wy + 8))
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
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        // Fully native flows (macOS: TeilCapture Swift library; Windows:
        // teil-capture-windows Win32 overlay) — no HTML overlay. Hiding the
        // popover (plus the settle delay that keeps it out of the frozen
        // screenshot) is owned by spawn_native_capture.
        let mode: &'static str = match mode {
            "region" => "region",
            "window" => "window",
            "fullscreen" => "fullscreen",
            _ => return,
        };
        commands::spawn_native_capture(app.clone(), mode);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.hide();
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
