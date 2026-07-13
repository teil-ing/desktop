// Thin typed wrappers over the Rust `#[tauri::command]` handlers (see src-tauri/src/commands.rs).
// This is the frontend's entire backend surface — capture, API, secure storage, prefs.

import { invoke } from "@tauri-apps/api/core";
import type {
  CaptureMode,
  ImageResponse,
  ImageUpdateRequest,
  Prefs,
  QuotaResponse,
  Region,
  WindowInfo,
} from "./types";

// ---- Auth / onboarding ---------------------------------------------------

/** True once an API key is stored in the OS keychain. */
export const hasApiKey = () => invoke<boolean>("has_api_key");

/** Validate a key against GET /images and, if valid, save it to the keychain. */
export const saveApiKey = (key: string) => invoke<void>("save_api_key", { key });

export const deleteApiKey = () => invoke<void>("delete_api_key");

/** Masked form of the stored key for the Account section (last 8 chars visible). */
export const maskedApiKey = () => invoke<string | null>("masked_api_key");

/**
 * Open the browser connect handshake (PKCE). Resolves once the browser is open;
 * completion arrives via the "auth-changed" / "auth-feedback" events.
 */
export const beginBrowserSignin = () => invoke<void>("begin_browser_signin");

// ---- Capture -------------------------------------------------------------
// On macOS the three begin/capture calls run fully native flows (Swift TeilCapture
// library: overlay + capture + feedback); the overlay/list/finish calls below the
// divider are only used by the Windows HTML-overlay flow.

/** Start region capture (native overlay on macOS; HTML drag overlay on Windows). */
export const beginRegionCapture = () => invoke<void>("begin_region_capture");

/** Capture the display under the cursor and upload immediately. */
export const captureFullscreen = () => invoke<void>("capture_fullscreen");

/** Start window capture (native hover-highlight picker on macOS; picker overlay on Windows). */
export const beginWindowCapture = () => invoke<void>("begin_window_capture");

/** macOS: whether Screen Recording is granted — instant, never prompts. Always true elsewhere. */
export const checkScreenPermission = () => invoke<boolean>("check_screen_permission");

/**
 * macOS: request Screen Recording access — shows the system prompt at most once
 * per app, then only reports status. A fresh grant needs an app relaunch.
 */
export const requestScreenPermission = () => invoke<boolean>("request_screen_permission");

/** macOS: open System Settings → Privacy & Security → Screen Recording. No-op elsewhere. */
export const openScreenSettings = () => invoke<void>("open_screen_settings");

/** Relaunch the app (applies a fresh Screen Recording grant). */
export const relaunchApp = () => invoke<void>("relaunch_app");

/** Mode ("region"|"window") + virtual-desktop origin for the overlay that just opened. */
export const overlayMode = () =>
  invoke<{ mode: string; originX: number; originY: number }>("overlay_mode");

/** Enumerate capturable windows (with thumbnails) for the picker. */
export const listWindows = () => invoke<WindowInfo[]>("list_windows");

/** Capture a specific window by id and upload. */
export const captureWindow = (windowId: number) =>
  invoke<void>("capture_window", { windowId });

/** Called by the overlay once the user finishes dragging (rect in virtual coords). */
export const finishRegionCapture = (region: Region | null) =>
  invoke<void>("finish_region_capture", { region });

// ---- Remote image API ----------------------------------------------------

export const listImages = (limit = 5, offset = 0) =>
  invoke<{ images: ImageResponse[]; limit: number; offset: number }>("list_images", {
    limit,
    offset,
  });

export const getQuota = () => invoke<QuotaResponse>("get_quota");

export const getImageDetails = (id: string) =>
  invoke<ImageResponse>("get_image_details", { id });

export const updateImage = (id: string, update: ImageUpdateRequest) =>
  invoke<void>("update_image", { id, update });

export const deleteImage = (id: string) => invoke<void>("delete_image", { id });

/** Retry the last failed upload. */
export const retryUpload = () => invoke<void>("retry_upload");

// ---- Preferences ---------------------------------------------------------

export const getPrefs = () => invoke<Prefs>("get_prefs");

export const setPrefs = (prefs: Prefs) => invoke<void>("set_prefs", { prefs });

// ---- Shortcuts -----------------------------------------------------------

/** Current global shortcut accelerators, keyed by capture mode. */
export const getShortcuts = () =>
  invoke<Record<CaptureMode, string>>("get_shortcuts");

/** Rebind one capture mode to a new accelerator (e.g. "CmdOrCtrl+Shift+X"). */
export const setShortcut = (mode: CaptureMode, accelerator: string) =>
  invoke<void>("set_shortcut", { mode, accelerator });

export const resetShortcuts = () => invoke<void>("reset_shortcuts");

// ---- Updates / meta ------------------------------------------------------

export const appVersion = () => invoke<string>("app_version");

/** Check the signed update endpoint (GitHub releases latest.json). */
export const checkForUpdates = () =>
  invoke<{ available: boolean; version: string | null }>("check_for_updates");

/** Download, verify, install the pending update, and relaunch the app. */
export const installUpdate = () => invoke<void>("install_update");

// ---- Window chrome -------------------------------------------------------

/** Hide the popover (mirrors NSPopover transient dismiss). */
export const hidePopover = () => invoke<void>("hide_popover");

/** Open (or focus) the Settings window. */
export const openPreferences = () => invoke<void>("open_preferences");

/** Open a URL in the default browser. */
export const openExternal = (url: string) => invoke<void>("open_external", { url });

export const quitApp = () => invoke<void>("quit_app");
