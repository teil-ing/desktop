// Settings window — a standalone decorated dialog (Swift: the separate Preferences window),
// organized into tabs: General, Shortcuts, Upload, Account. Persists via the same Rust prefs
// commands; deleting the key notifies the popover via "auth-changed".

import { emit } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import * as ipc from "./ipc";
import { el, formatShortcut } from "./dom";
import type { ClipboardMode, Prefs } from "./types";

const root = document.getElementById("prefs-root")!;
let prefs: Prefs | null = null;

type TabId = "general" | "shortcuts" | "upload" | "account" | "about";
const TABS: { id: TabId; label: string }[] = [
  { id: "general", label: "General" },
  { id: "shortcuts", label: "Shortcuts" },
  { id: "upload", label: "Upload" },
  { id: "account", label: "Account" },
  { id: "about", label: "About" },
];
let activeTab: TabId = "general";

async function render() {
  root.innerHTML = "";
  prefs = await ipc.getPrefs().catch(() => null);
  if (!prefs) {
    root.appendChild(el("div", { class: "pane center", html: '<span class="spinner"></span>' }));
    return;
  }

  const bar = el("div", { class: "tabbar" });
  for (const t of TABS) {
    const b = el("button", { class: `tab${activeTab === t.id ? " active" : ""}`, text: t.label });
    b.onclick = () => {
      activeTab = t.id;
      render();
    };
    bar.appendChild(b);
  }
  root.appendChild(bar);

  const content = el("div", { class: "tab-content" });
  root.appendChild(content);

  switch (activeTab) {
    case "general":
      await renderGeneral(content);
      break;
    case "shortcuts":
      await renderShortcuts(content);
      break;
    case "upload":
      renderUpload(content);
      break;
    case "account":
      await renderAccount(content);
      break;
    case "about":
      await renderAbout(content);
      break;
  }
}

// ---- Tabs ----------------------------------------------------------------

async function renderGeneral(c: HTMLElement) {
  const p = prefs!;
  c.appendChild(toggle("Launch at Login", "Start teil.ing when you log in", p.launchAtLogin, (v) => save({ launchAtLogin: v })));
  c.appendChild(toggle("Auto-Check for Updates", "Check for new versions periodically", p.autoCheckForUpdates, (v) => save({ autoCheckForUpdates: v })));

  const version = await ipc.appVersion().catch(() => "?");
  const row = el("div", { class: "toggle-row" });
  row.appendChild(el("span", { class: "grow sub", text: `Current: v${version}` }));
  const checkBtn = el("button", { class: "bordered", text: "Check for Updates" });
  checkBtn.onclick = async () => {
    checkBtn.textContent = "Checking…";
    const res = await ipc.checkForUpdates().catch(() => null);
    if (res?.available) {
      checkBtn.textContent = `Install v${res.version} & Restart`;
      checkBtn.onclick = async () => {
        checkBtn.textContent = "Installing…";
        try {
          await ipc.installUpdate(); // relaunches on success
        } catch {
          checkBtn.textContent = "Update failed — try again";
          setTimeout(() => (checkBtn.textContent = "Check for Updates"), 3000);
        }
      };
    } else {
      checkBtn.textContent = res ? "Up to date" : "Could not check";
      setTimeout(() => (checkBtn.textContent = "Check for Updates"), 2000);
    }
  };
  row.appendChild(checkBtn);
  c.appendChild(row);
}

async function renderShortcuts(c: HTMLElement) {
  const shortcuts = await ipc.getShortcuts().catch(() => null);
  if (!shortcuts) {
    c.appendChild(el("div", { class: "sub", text: "Shortcuts unavailable." }));
    return;
  }
  const error = el("div", { class: "error-text", attrs: { style: "margin-top:6px;min-height:14px" } });
  for (const [mode, label] of [["region", "Region"], ["fullscreen", "Fullscreen"], ["window", "Window"]] as const) {
    const r = el("div", { class: "toggle-row" });
    r.appendChild(el("span", { class: "grow", text: label }));
    const btn = el("button", { class: "bordered shortcut", text: formatShortcut(shortcuts[mode]) }) as HTMLButtonElement;
    btn.onclick = () => recordShortcut(btn, mode, error);
    r.appendChild(btn);
    c.appendChild(r);
  }
  c.appendChild(error);
  c.appendChild(
    el("div", { class: "sub", text: "Click a shortcut, then press the new key combination. Esc cancels.", attrs: { style: "margin-top:4px" } }),
  );
  const reset = el("button", { class: "bordered", text: "Reset to Defaults", attrs: { style: "margin-top: 8px" } });
  reset.onclick = async () => {
    await ipc.resetShortcuts();
    render();
  };
  c.appendChild(reset);
}

// ---- Shortcut recorder -----------------------------------------------------

/** Puts the button into recording mode and captures the next key combination. */
function recordShortcut(btn: HTMLButtonElement, mode: "region" | "fullscreen" | "window", error: HTMLElement) {
  btn.textContent = "Press shortcut…";
  error.textContent = "";

  const stop = () => {
    window.removeEventListener("keydown", onKey, true);
    render(); // restore the (possibly updated) binding labels
  };

  const onKey = async (e: KeyboardEvent) => {
    e.preventDefault();
    e.stopPropagation();
    if (e.key === "Escape") return stop();
    // Bare modifier presses just arm the combo — wait for a real key.
    if (["Shift", "Control", "Alt", "Meta"].includes(e.key)) return;

    const accel = acceleratorFrom(e);
    if (!accel) {
      error.textContent = "Use at least one modifier key (⌘, Ctrl, or Alt) plus a key.";
      return;
    }
    try {
      await ipc.setShortcut(mode, accel);
    } catch (err) {
      error.textContent = String(err).replace(/^Error:\s*/, "");
    }
    stop();
  };
  window.addEventListener("keydown", onKey, true);
}

/** Builds a Tauri global-shortcut accelerator ("CmdOrCtrl+Shift+X") from a key event. */
function acceleratorFrom(e: KeyboardEvent): string | null {
  const isMac = /mac/i.test(navigator.userAgent);
  const mods: string[] = [];
  if (isMac ? e.metaKey : e.ctrlKey) mods.push("CmdOrCtrl");
  if (isMac && e.ctrlKey) mods.push("Ctrl");
  if (!isMac && e.metaKey) mods.push("Super");
  if (e.altKey) mods.push("Alt");
  if (e.shiftKey) mods.push("Shift");
  // Shift alone can't anchor a global hotkey — require ⌘/Ctrl/Alt.
  if (!mods.some((m) => m !== "Shift")) return null;

  const key = keyToken(e.code);
  return key ? [...mods, key].join("+") : null;
}

/** Maps a KeyboardEvent.code to an accelerator key token, or null if unsupported. */
function keyToken(code: string): string | null {
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit[0-9]$/.test(code)) return code.slice(5);
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(code)) return code;
  const named: Record<string, string> = {
    Space: "Space",
    Enter: "Enter",
    Tab: "Tab",
    Backspace: "Backspace",
    Delete: "Delete",
    Home: "Home",
    End: "End",
    PageUp: "PageUp",
    PageDown: "PageDown",
    ArrowUp: "Up",
    ArrowDown: "Down",
    ArrowLeft: "Left",
    ArrowRight: "Right",
    Comma: "Comma",
    Period: "Period",
    Slash: "Slash",
    Backslash: "Backslash",
    Minus: "Minus",
    Equal: "Equal",
    Semicolon: "Semicolon",
    Quote: "Quote",
    BracketLeft: "BracketLeft",
    BracketRight: "BracketRight",
    Backquote: "Backquote",
  };
  return named[code] ?? null;
}

function renderUpload(c: HTMLElement) {
  const p = prefs!;
  c.appendChild(toggle("Strip EXIF Metadata", "Remove location and camera info", p.stripExif, (v) => save({ stripExif: v })));
  c.appendChild(toggle("Private Upload", "Owner-only, not accessible via share link", p.privateUpload, (v) => save({ privateUpload: v })));
  c.appendChild(toggle("Open in Browser", "Open the share URL after each upload", p.openInBrowser, (v) => save({ openInBrowser: v })));
  // Re-render on change so the mode picker below shows/hides with the toggle.
  c.appendChild(toggle("Copy to Clipboard", "Copy after each upload", p.clipboardCopy, (v) => save({ clipboardCopy: v }).then(render)));
  if (p.clipboardCopy) {
    c.appendChild(clipboardModeRow(p.clipboardMode));
  }
}

async function renderAccount(c: HTMLElement) {
  const masked = await ipc.maskedApiKey().catch(() => null);
  const row = el("div", { class: "toggle-row" });
  row.appendChild(
    el("span", { class: "grow", html: `<code style="font-family:ui-monospace,monospace">${masked ?? "—"}</code>` }),
  );
  const del = el("button", { class: "bordered danger", text: "Delete" });
  del.onclick = async () => {
    await ipc.deleteApiKey();
    await emit("auth-changed"); // popover drops back to onboarding
    await getCurrentWindow().close();
  };
  row.appendChild(del);
  c.appendChild(row);
  c.appendChild(
    el("div", { class: "sub", text: "Your API key is stored in the system keychain.", attrs: { style: "margin-top: 8px" } }),
  );
}

const WEBSITE_URL = "https://teil.ing";
const SOURCE_URL = "https://github.com/teil-ing/desktop";

async function renderAbout(c: HTMLElement) {
  const version = await ipc.appVersion().catch(() => "?");

  const head = el("div", { class: "about-head" });
  head.appendChild(el("div", { class: "about-name", text: "teil.ing" }));
  head.appendChild(el("div", { class: "sub", text: `Version ${version}` }));
  head.appendChild(
    el("div", { class: "sub", text: "Fast screenshot capture and sharing.", attrs: { style: "margin-top:4px" } }),
  );
  c.appendChild(head);

  const links = el("div", { class: "about-links" });
  const website = el("button", { class: "bordered", text: "Website" });
  website.onclick = () => ipc.openExternal(WEBSITE_URL);
  links.appendChild(website);
  const source = el("button", { class: "bordered", text: "Source Code" });
  source.onclick = () => ipc.openExternal(SOURCE_URL);
  links.appendChild(source);
  c.appendChild(links);

  c.appendChild(
    el("div", { class: "sub", text: "© 2026 teil.ing · All rights reserved.", attrs: { style: "margin-top:10px" } }),
  );

  // Third-party licenses — legally required attribution for bundled OSS. The
  // text is large, so it is lazy-loaded (a separate chunk) only on demand.
  c.appendChild(el("div", { class: "divider", attrs: { style: "margin:12px 0" } }));
  c.appendChild(el("div", { class: "section-label", text: "Open-Source Licenses" }));
  c.appendChild(
    el("div", {
      class: "sub",
      text: "This app includes open-source components. Their licenses and copyright notices:",
      attrs: { style: "margin-bottom:6px" },
    }),
  );

  const viewer = el("pre", { class: "license-text", attrs: { style: "display:none" } });
  const btn = el("button", { class: "bordered", text: "Show Licenses" }) as HTMLButtonElement;
  btn.onclick = async () => {
    if (viewer.style.display === "none" && !viewer.textContent) {
      btn.disabled = true;
      btn.textContent = "Loading…";
      try {
        const mod = await import("../THIRD-PARTY-LICENSES.txt?raw");
        viewer.textContent = mod.default;
      } catch {
        viewer.textContent = "Could not load license information.";
      }
      btn.disabled = false;
    }
    const showing = viewer.style.display !== "none";
    viewer.style.display = showing ? "none" : "block";
    btn.textContent = showing ? "Show Licenses" : "Hide Licenses";
  };
  c.appendChild(btn);
  c.appendChild(viewer);
}

// ---- Shared controls -----------------------------------------------------

function toggle(title: string, sub: string, value: boolean, onChange: (v: boolean) => void) {
  const row = el("div", { class: "toggle-row" });
  const text = el("div", { class: "grow" });
  text.appendChild(el("div", { text: title }));
  text.appendChild(el("div", { class: "sub", text: sub }));
  row.appendChild(text);
  const sw = el("label", { class: "switch" });
  const input = el("input", { attrs: { type: "checkbox" } }) as HTMLInputElement;
  input.checked = value;
  input.onchange = () => onChange(input.checked);
  sw.appendChild(input);
  sw.appendChild(el("span", { class: "slider" }));
  row.appendChild(sw);
  return row;
}

/** Segmented URL/Image picker (Swift: the .segmented Picker under Copy to Clipboard). */
function clipboardModeRow(mode: ClipboardMode) {
  const wrap = el("div", { attrs: { style: "margin: 2px 0 10px 0" } });
  wrap.appendChild(
    el("div", { class: "sub", text: "Copy the share URL or the captured image.", attrs: { style: "margin-bottom: 4px" } }),
  );
  const seg = el("div", { class: "segmented" });
  for (const [val, label] of [["url", "Share URL"], ["image", "Image"]] as const) {
    const b = el("button", { class: `seg${mode === val ? " active" : ""}`, text: label });
    b.onclick = () => save({ clipboardMode: val }).then(render);
    seg.appendChild(b);
  }
  wrap.appendChild(seg);
  return wrap;
}

async function save(patch: Partial<Prefs>) {
  if (!prefs) return;
  prefs = { ...prefs, ...patch };
  await ipc.setPrefs(prefs);
}

// Close on Escape, like a standard dialog.
window.addEventListener("keydown", (e) => {
  if (e.key === "Escape") getCurrentWindow().close();
});

render();
