# teil.ing — cross-platform client (Tauri)

A **macOS + Windows** port of the native Swift menu-bar app (`../teil.ing-client`), built with
**Tauri v2 + Rust + TypeScript**. Kept as close as possible to the macOS app's architecture and UX.

> Status: **runs on macOS with fully native capture** (Swift/ScreenCaptureKit static
> library), browser sign-in (PKCE + `teiling://`), a blocking Screen Recording permission
> gate, signed auto-updates, and a signed/notarized CI release pipeline. Windows uses the
> xcap + HTML-overlay path and still needs runtime validation on real hardware.

## Why Tauri

Same reasoning as the design discussion: with Linux dropped, Tauri gives small, native-feeling
binaries (tray, global shortcuts, secure storage) — matching the lean feel of the Swift app far
better than Electron.

## Capture backends (per platform)

- **macOS — fully native.** The entire interactive capture stack from the Swift app is compiled
  into the binary as a static library: `src-tauri/swift/TeilCapture` (SwiftPM package, macOS 14+,
  built + linked by `build.rs` via `swift-rs`). That includes the region-selection overlay
  (crosshair, marching ants, live dimensions, cross-screen drag), the hover-highlight window
  picker (camera cursor, AX window raising), ScreenCaptureKit capture with own-app exclusion,
  cross-monitor stitching, shadow-free window capture with transparent corners, and the
  flash + shutter-sound feedback. Rust calls it through a small C ABI (`src/capture_macos.rs`);
  each interactive call blocks a worker thread until the user finishes and returns a PNG.
  The HTML overlay is **not used** on macOS.
- **Windows — `xcap`** (Windows.Graphics.Capture) + the transparent HTML drag overlay
  (`overlay.html` + `src/overlay.ts`), as before (`src/capture.rs`).

## Architecture (all capture/API/secrets in Rust; webview is UI only)

The API key never enters the webview, and image buffers never round-trip to JS.

| Swift (macOS app) | This project |
| --- | --- |
| `AppDelegate` (tray, popover, capture orchestration) | `src-tauri/src/lib.rs` |
| `CaptureEngine` / `CrossMonitorStitcher` / overlays / `CaptureFeedback` | macOS: `src-tauri/swift/TeilCapture` (same code, statically linked) via `src-tauri/src/capture_macos.rs`; Windows: `src-tauri/src/capture.rs` (xcap) |
| `APIService` + `UploadService` + `APIValidationService` | `src-tauri/src/api.rs` (reqwest) |
| `KeychainService` | `src-tauri/src/secure.rs` (keyring) |
| `PreferencesStore` (UserDefaults) + `HotkeyMonitor` defaults | `src-tauri/src/prefs.rs` (JSON in config dir) |
| command surface (AppDelegate actions) | `src-tauri/src/commands.rs` |
| `PopoverRootView` + sections | `src/main.ts` + `src/styles.css` |
| `SelectionOverlayView` / `WindowSelectionOverlayView` | macOS: native (TeilCapture); Windows: `overlay.html` + `src/overlay.ts` |
| Models (`ImageResponse`, `QuotaResponse`, …) | `src/types.ts` ⇄ `src-tauri/src/api.rs` |

Frontend files: `src/ipc.ts` (typed `invoke` wrappers), `src/dom.ts` (helpers +
`relativeTime` with the "Just now" clamp), `src/icons.ts` (SF-Symbol-equivalent SVGs).

## Feature parity

**Implemented** (mirrors the Swift app):
- Tray icon; left-click toggles a transient popover (hides on blur).
- Region capture via a transparent fullscreen drag overlay (dim + marching-ants + live dimensions).
- Fullscreen capture of the display under the cursor.
- Window capture via a picker overlay of live window thumbnails.
- Global shortcuts ⌘/Ctrl+Shift+X / S / C, rebindable, persisted, reset-to-defaults.
- Upload pipeline: exact multipart shape (`file` + optional `stripExif`/`private`), 201 parse,
  status→message mapping, clipboard (URL **or** image) + open-in-browser per prefs, retry.
- History list (prefers remote images, refreshes on every popover open + after each upload),
  per-row **Edit (web `…/edit`) / Copy / Open** buttons, relative timestamps.
- Quota bar, preferences (Account, General, Shortcuts, Upload Settings), onboarding (validate+save),
  "Check for Updates" → GitHub releases with the **Download** fallback (matches the macOS fallback).
- Keychain-backed API key; prefs persisted to the OS config dir.

**Deviations / TODO** (documented on purpose):
- **macOS is at full fidelity** via the native TeilCapture library: hover-highlight window
  selection, capture flash + shutter sound, cross-monitor stitching, shadow-free window capture.
  The items below apply to **Windows only**:
- **Window selection (Windows)** uses a picker overlay instead of the macOS hover-highlight
  (hover-select is much harder cross-platform; the picker is the pragmatic equivalent).
- **Capture flash + shutter sound (Windows)** aren't replicated (tray tooltip changes to
  "Uploading…"/"Upload failed" instead).
- **Cross-monitor region (Windows)**: a selection spanning two displays is captured from the
  display it overlaps most (macOS stitches). Single-display regions are exact.
- **In-app image detail/edit sheet** is not ported — the Edit button opens the web `…/edit` page,
  matching the macOS app's current per-row Edit behavior.
- **Launch at Login** persists the pref but isn't wired to autostart yet (needs macOS `SMAppService`
  / Windows Run-key, e.g. `tauri-plugin-autostart`).
- **Auto-update install** is fallback-only (opens the release page). Add `tauri-plugin-updater`
  for signed in-place updates.

## Prerequisites

- Rust (stable) + the platform toolchain: Xcode CLT on macOS, MSVC Build Tools + WebView2 on Windows.
- Node 18+ and a package manager (`npm` shown here).
- Tauri CLI (installed via the dev dependency).

## Setup & run

```
cd crossplatform
npm install
npm run tauri icon path/to/teil-ing-1024.png   # required once — see src-tauri/icons/README.md
npm run tauri dev                               # run
npm run tauri build                             # package .dmg (macOS) / .msi+.exe (Windows)
```

macOS needs Screen Recording permission (System Settings → Privacy & Security) the first time,
same as the native app.

## Releases & auto-update

- `make release V=x.y.z` bumps `tauri.conf.json` + `Cargo.toml`, commits, tags `vx.y.z`,
  and pushes. The tag triggers `.github/workflows/release.yml`, which builds the DMG,
  signs (Developer ID) + notarizes it, generates the signed updater artifacts
  (`*.app.tar.gz` + minisign `.sig` + `latest.json`), uploads everything to a **draft**
  release, and publishes it as latest only when all assets are attached.
- The app checks `releases/latest/download/latest.json` via `tauri-plugin-updater` —
  on startup (pref-gated) with an install pill in the popover footer, and manually in
  Settings → General → "Check for Updates" → "Install & Restart".
- Required repo secrets: `DEVELOPER_ID_CERT_BASE64`, `DEVELOPER_ID_CERT_PASSWORD`,
  `APP_STORE_CONNECT_KEY_BASE64`, `APP_STORE_CONNECT_KEY_ID`, `APP_STORE_CONNECT_ISSUER_ID`
  (same values as the teil-ing/macos repo), plus `TAURI_SIGNING_PRIVATE_KEY` and
  `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` (updater keypair; local copy in `~/.tauri/`).
- The updater public key lives in `tauri.conf.json` (`plugins.updater.pubkey`). If the
  private key is ever lost, shipped apps can no longer be updated in place.

## Verify-first (expected first-build friction)

1. **`src-tauri/src/capture.rs`** — done: pinned to xcap 0.9.6, adapted to its `XCapResult`
   accessors, and uses `Monitor::from_point` + `Monitor::capture_region`. Runtime behaviour of
   region cropping across displays still needs a look (see next item).
2. **DPI / multi-monitor** — region coordinates are translated as `overlay_physical_origin +
   css_rect * scale_factor`. Exact at uniform scale; mixed-DPI multi-monitor setups may need a
   per-monitor scale lookup.
3. **Tauri v2 details** — a few calls (`AppHandle::cursor_position`, tray event `position`,
   clipboard `write_image`) should be checked against the exact `@tauri-apps` versions you install.
4. **Icons** must exist before the first build (see `src-tauri/icons/README.md`).
