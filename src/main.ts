// Popover controller — the TS counterpart of PopoverRootView + AppDelegate's popover side.
// Renders one of: onboarding, main. Settings open in a separate window (preferences.ts).
// Listens for upload feedback, "popover-shown", and "auth-changed" events.

import { listen } from "@tauri-apps/api/event";
import * as ipc from "./ipc";
import { el, relativeTime, formatBytes, editUrl, formatShortcut } from "./dom";
import { icons } from "./icons";
import type { ImageResponse, QuotaResponse, UploadFeedback } from "./types";

type View = "loading" | "permission" | "onboarding" | "main";

const root = document.getElementById("root")!;

const state = {
  view: "loading" as View,
  images: [] as ImageResponse[],
  quota: null as QuotaResponse | null,
  /** Capture-mode → accelerator, shown as a hint on each capture row. */
  shortcuts: {} as Record<string, string>,
  loadingRemote: false,
  remoteError: null as string | null,
  uploadError: null as string | null,
  /** Browser sign-in: waiting for the teiling:// callback. */
  authWaiting: false,
  authError: null as string | null,
  /** macOS Screen Recording TCC; always true on Windows. */
  screenPermission: true,
  /** Version string when the background probe found an update. */
  updateVersion: null as string | null,
  /** Permission was granted while running — a relaunch is required to apply it. */
  permissionGrantedMidRun: false,
  /** Image id with the inline delete confirmation open (one at a time). */
  confirmingDelete: null as string | null,
  /** Image id whose deletion is in flight. */
  deleting: null as string | null,
};

/**
 * Blocking permission gate. When Screen Recording is missing, the app is
 * unusable until it is granted AND the app restarts (macOS applies fresh TCC
 * grants only on relaunch), so we park on the "permission" view and poll —
 * the screen flips to its "restart" state the moment the grant appears.
 */
let permissionPoll: number | null = null;

function enterPermissionGate() {
  state.view = "permission";
  render();
  if (permissionPoll !== null) return;
  permissionPoll = window.setInterval(async () => {
    const granted = await ipc.checkScreenPermission().catch(() => false);
    if (granted) {
      if (permissionPoll !== null) window.clearInterval(permissionPoll);
      permissionPoll = null;
      state.permissionGrantedMidRun = true;
      render();
    }
  }, 2000);
}

// ---- Boot ----------------------------------------------------------------

async function applyAuthGate() {
  const onboarded = await ipc.hasApiKey().catch(() => false);
  state.view = onboarded ? "main" : "onboarding";
  render();
  if (onboarded) {
    loadShortcuts();
    refreshAll();
  }
}

/** Refresh the capture-mode shortcut hints (they can change in the Settings window). */
async function loadShortcuts() {
  state.shortcuts = (await ipc.getShortcuts().catch(() => ({}))) ?? {};
  if (state.view === "main") render();
}

async function boot() {
  // Permission gate first: on macOS a missing Screen Recording grant blocks the
  // whole app; on Windows the check always passes and this is a no-op.
  state.screenPermission = await ipc.checkScreenPermission().catch(() => true);
  if (!state.screenPermission) {
    enterPermissionGate();
  } else {
    await applyAuthGate();
  }

  // Re-run the onboarding gate when the key changes (e.g. deleted in the Settings window,
  // or a browser sign-in just stored a fresh key).
  await listen("auth-changed", () => applyAuthGate());

  // Browser sign-in outcome (Rust auth.rs). Success also fires "auth-changed" above.
  await listen<{ kind: string; message?: string }>("auth-feedback", (e) => {
    state.authWaiting = false;
    state.authError = e.payload.kind === "failed" ? (e.payload.message ?? "Sign-in failed.") : null;
    if (state.view === "onboarding") render();
  });

  // Rust emits this whenever the tray popover is shown → refresh like showPopover() does.
  await listen("popover-shown", () => {
    if (state.view === "main") {
      loadShortcuts();
      refreshAll();
    }
  });

  // Background update probe (Rust: spawn_update_check) found a newer version.
  await listen<{ version: string }>("update-available", (e) => {
    state.updateVersion = e.payload.version;
    if (state.view === "main") render();
  });

  await listen<UploadFeedback>("upload-feedback", (e) => {
    const f = e.payload;
    if (f.kind === "started") {
      state.uploadError = null;
    } else if (f.kind === "succeeded") {
      state.uploadError = null;
      refreshAll(); // new upload appears at the top (list prefers remote images)
    } else if (f.kind === "failed") {
      state.uploadError = f.message;
    }
    if (state.view === "main") render();
  });
}

async function refreshAll() {
  state.loadingRemote = true;
  state.remoteError = null;
  state.confirmingDelete = null; // a fresh open/refresh discards pending confirms
  if (state.view === "main") render();
  try {
    const [list, quota] = await Promise.all([
      ipc.listImages(5, 0),
      ipc.getQuota().catch(() => null),
    ]);
    state.images = list.images;
    state.quota = quota;
  } catch (err) {
    state.remoteError = String(err);
  } finally {
    state.loadingRemote = false;
    if (state.view === "main") render();
  }
}

// ---- Render dispatch -----------------------------------------------------

function render() {
  root.innerHTML = "";
  switch (state.view) {
    case "permission":
      renderPermissionGate();
      break;
    case "onboarding":
      renderOnboarding();
      break;
    case "main":
      renderMain();
      break;
    default:
      root.appendChild(el("div", { class: "pane center", html: '<span class="spinner"></span>' }));
  }
}

// ---- Permission gate (blocks everything until granted + relaunched) -------

function renderPermissionGate() {
  const pane = el("div", { class: "pane center" });
  pane.appendChild(el("div", { class: "gate-icon", html: icons.warn, attrs: { style: "margin-bottom:8px" } }));
  pane.appendChild(el("h3", { class: "sec", text: "Permission Required" }));

  if (state.permissionGrantedMidRun) {
    pane.appendChild(
      el("div", {
        text: "Screen Recording is granted. Restart teil.ing to start capturing.",
        attrs: { style: "color:var(--secondary);margin-bottom:12px" },
      }),
    );
    const restart = el("button", { class: "primary", text: "Restart teil.ing" });
    restart.onclick = () => ipc.relaunchApp();
    pane.appendChild(restart);
  } else {
    pane.appendChild(
      el("div", {
        text: "teil.ing needs Screen Recording access to capture your screen. Nothing works without it.",
        attrs: { style: "color:var(--secondary);margin-bottom:12px" },
      }),
    );
    const grant = el("button", { class: "primary", text: "Grant Permission" }) as HTMLButtonElement;
    grant.onclick = async () => {
      // First click shows the system prompt (macOS asks once per app); afterwards
      // it only reports status, so fall back to opening System Settings.
      const granted = await ipc.requestScreenPermission().catch(() => false);
      if (!granted) await ipc.openScreenSettings().catch(() => {});
      // The poll started by enterPermissionGate() flips this screen to its
      // restart state as soon as the grant lands.
    };
    pane.appendChild(grant);
    pane.appendChild(
      el("div", {
        text: "System Settings → Privacy & Security → Screen Recording",
        attrs: { style: "color:var(--secondary);font-size:11px;margin-top:8px" },
      }),
    );
  }

  root.appendChild(pane);
}

// ---- Onboarding ----------------------------------------------------------

function renderOnboarding() {
  const pane = el("div", { class: "pane center" });
  pane.appendChild(el("h3", { class: "sec", text: "Welcome to teil.ing" }));
  pane.appendChild(
    el("div", { class: "sub", html: "Sign in with your browser to connect this device", attrs: { style: "color:var(--secondary);margin-bottom:12px" } }),
  );

  // Primary: browser connect flow (Swift: AuthService.signInViaBrowser).
  const signinBtn = el("button", {
    class: "primary",
    text: state.authWaiting ? "Waiting for browser…" : "Sign in with teil.ing",
  }) as HTMLButtonElement;
  signinBtn.disabled = state.authWaiting;
  signinBtn.onclick = async () => {
    state.authError = null;
    state.authWaiting = true;
    render();
    try {
      await ipc.beginBrowserSignin();
    } catch (err) {
      state.authWaiting = false;
      state.authError = String(err).replace(/^Error:\s*/, "");
      render();
    }
  };
  pane.appendChild(signinBtn);

  const authError = el("div", { class: "error-text", attrs: { style: "margin-top:6px" } });
  if (state.authError) authError.textContent = state.authError;
  pane.appendChild(authError);

  // Fallback: manual API key paste (kept for parity with the Swift onboarding).
  pane.appendChild(
    el("div", { text: "or paste an API key (teil.ing/settings)", attrs: { style: "color:var(--secondary);font-size:11px;margin:12px 0 8px" } }),
  );

  const input = el("input", { attrs: { type: "password", placeholder: "Paste your API key" } });
  pane.appendChild(input);

  const error = el("div", { class: "error-text", attrs: { style: "margin-top:6px" } });
  pane.appendChild(error);

  const btn = el("button", { class: "bordered", text: "Continue", attrs: { style: "margin-top:8px" } }) as HTMLButtonElement;
  btn.onclick = async () => {
    const key = input.value.trim();
    if (!key) return;
    btn.disabled = true;
    btn.textContent = "Validating…";
    error.textContent = "";
    try {
      await ipc.saveApiKey(key);
      state.view = "main";
      render();
      refreshAll();
    } catch (err) {
      error.textContent = String(err).replace(/^Error:\s*/, "");
      btn.disabled = false;
      btn.textContent = "Continue";
    }
  };
  pane.appendChild(btn);
  root.appendChild(pane);
}

// ---- Main view -----------------------------------------------------------

function renderMain() {
  const c = el("div");

  if (state.uploadError) {
    const banner = el("div", { class: "banner" });
    banner.appendChild(el("div", { class: "line", html: `${icons.warn}<span>${escapeHtml(state.uploadError)}</span>` }));
    const retry = el("button", { class: "bordered", text: "Retry Upload" });
    retry.onclick = () => ipc.retryUpload();
    banner.appendChild(retry);
    c.appendChild(banner);
    c.appendChild(el("div", { class: "divider" }));
  }

  c.appendChild(captureSection());
  c.appendChild(el("div", { class: "divider" }));
  c.appendChild(historySection());
  c.appendChild(el("div", { class: "divider" }));
  if (state.quota) {
    c.appendChild(quotaSection(state.quota));
    c.appendChild(el("div", { class: "divider" }));
  }
  c.appendChild(footer());

  root.appendChild(c);
}

function captureSection() {
  const s = el("div", { class: "section" });
  s.appendChild(el("div", { class: "section-label", text: "Capture" }));
  const modes: [string, string, string, () => void][] = [
    ["Region", icons.region, "region", () => ipc.beginRegionCapture()],
    ["Fullscreen", icons.fullscreen, "fullscreen", () => ipc.captureFullscreen()],
    ["Window", icons.window, "window", openWindowPicker],
  ];
  for (const [label, icon, mode, action] of modes) {
    const row = el("div", { class: "row capture-row" });
    row.appendChild(el("div", { class: "label grow", html: `${wrapIcon(icon)}<span>${label}</span>` }));
    const accel = state.shortcuts[mode];
    if (accel) row.appendChild(el("kbd", { class: "kbd", text: formatShortcut(accel) }));
    row.onclick = () => {
      ipc.hidePopover();
      action();
    };
    s.appendChild(row);
  }
  return s;
}

function openWindowPicker() {
  // Rust opens a fullscreen overlay window that renders the picker (overlay.ts, mode=window).
  ipc.beginWindowCapture();
}

function historySection() {
  const s = el("div", { class: "section" });
  const header = el("div", { class: "history-header" });
  header.appendChild(el("span", { text: "Images" }));
  if (state.loadingRemote) header.appendChild(el("span", { class: "spinner" }));
  header.appendChild(el("span", { class: "grow" }));

  const refresh = el("button", { class: "icon-btn", html: icons.refresh });
  refresh.onclick = () => refreshAll();
  header.appendChild(refresh);
  s.appendChild(header);

  if (state.remoteError) {
    s.appendChild(el("div", { text: state.remoteError, attrs: { style: "font-size:11px;color:var(--secondary);padding:0 12px 4px" } }));
  }

  if (state.images.length === 0 && !state.loadingRemote) {
    s.appendChild(el("div", { class: "empty", html: `<div class="big">${wrapIcon(icons.clock)}</div><div>No uploads yet</div>` }));
    return s;
  }

  const list = el("div", { class: "history-list" });
  for (const img of state.images) list.appendChild(historyRow(img));
  s.appendChild(list);
  return s;
}

function historyRow(img: ImageResponse) {
  const shareUrl = `https://teil.ing/i/${img.slug}`;
  const row = el("div", { class: "history-row" });

  const thumb = el("img", { class: "thumb" }) as HTMLImageElement;
  if (img.thumbnailUrl) thumb.src = img.thumbnailUrl;
  row.appendChild(thumb);

  // Inline delete confirmation replaces the row's meta + actions (Swift-less
  // equivalent of a destructive confirm sheet, kept inside the menu).
  if (state.confirmingDelete === img.id) {
    const meta = el("div", { class: "meta" });
    meta.appendChild(el("div", { class: "time", text: "Delete this image?" }));
    meta.appendChild(el("div", { class: "views", text: "This cannot be undone." }));
    row.appendChild(meta);

    const del = el("button", {
      class: "badge badge-danger",
      text: state.deleting === img.id ? "Deleting…" : "Delete",
    }) as HTMLButtonElement;
    del.disabled = state.deleting !== null;
    del.onclick = async () => {
      state.deleting = img.id;
      render();
      try {
        await ipc.deleteImage(img.id);
        state.images = state.images.filter((i) => i.id !== img.id);
        refreshAll(); // re-sync list + quota
      } catch (err) {
        state.remoteError = String(err).replace(/^Error:\s*/, "");
      } finally {
        state.deleting = null;
        state.confirmingDelete = null;
        render();
      }
    };
    row.appendChild(del);

    const cancel = el("button", { class: "badge", text: "Cancel" }) as HTMLButtonElement;
    cancel.disabled = state.deleting !== null;
    cancel.onclick = () => {
      state.confirmingDelete = null;
      render();
    };
    row.appendChild(cancel);

    return row;
  }

  const meta = el("div", { class: "meta" });
  const badges = [img.isPrivate ? wrapIcon(icons.lock) : "", img.hasPassword ? wrapIcon(icons.key) : ""].join("");
  meta.appendChild(el("div", { class: "time", html: `<span>${relativeTime(img.createdAt)}</span>${badges}` }));
  meta.appendChild(el("div", { class: "views", text: `${img.viewCount} ${img.viewCount === 1 ? "view" : "views"}` }));
  row.appendChild(meta);

  // Edit (web) + Copy + Open — same trio as HistoryRowView.
  const edit = el("button", { class: "icon-btn", html: icons.edit, attrs: { title: "Edit on teil.ing" } });
  edit.onclick = () => ipc.openExternal(editUrl(shareUrl));
  row.appendChild(edit);

  const copy = el("button", { class: "icon-btn", html: icons.copy, attrs: { title: "Copy URL" } });
  copy.onclick = async () => {
    const { writeText } = await import("@tauri-apps/plugin-clipboard-manager");
    await writeText(shareUrl);
    copy.innerHTML = icons.check;
    copy.style.color = "var(--success)";
    setTimeout(() => {
      copy.innerHTML = icons.copy;
      copy.style.color = "";
    }, 1500);
  };
  row.appendChild(copy);

  const open = el("button", { class: "icon-btn", html: icons.open, attrs: { title: "Open in browser" } });
  open.onclick = () => ipc.openExternal(shareUrl);
  row.appendChild(open);

  const del = el("button", { class: "icon-btn del", html: icons.trash, attrs: { title: "Delete image" } });
  del.onclick = () => {
    state.confirmingDelete = img.id;
    render();
  };
  row.appendChild(del);

  return row;
}

function quotaSection(q: QuotaResponse) {
  const s = el("div", { class: "quota" });
  if (q.storageQuota && q.storageQuota > 0) {
    const ratio = q.storageUsed / q.storageQuota;
    const color = ratio > 0.9 ? "var(--danger)" : ratio > 0.7 ? "#ff9500" : "var(--accent)";
    const bar = el("div", { class: "bar" });
    bar.appendChild(el("span", { attrs: { style: `width:${Math.min(100, ratio * 100)}%;background:${color}` } }));
    s.appendChild(bar);
    const line = el("div", { class: "line" });
    line.appendChild(el("span", { text: `${formatBytes(q.storageUsed)} / ${formatBytes(q.storageQuota)}` }));
    line.appendChild(el("span", { class: "chip", text: cap(q.tier) }));
    s.appendChild(line);
  } else {
    const line = el("div", { class: "line" });
    line.appendChild(el("span", { text: "Storage: Unlimited" }));
    line.appendChild(el("span", { class: "chip", text: cap(q.tier) }));
    s.appendChild(line);
  }
  return s;
}

function footer() {
  const f = el("div", { class: "footer" });
  const gear = el("button", { class: "icon-btn", html: icons.gear });
  gear.onclick = () => ipc.openPreferences();
  f.appendChild(gear);
  if (state.updateVersion) {
    const pill = el("button", { class: "update-pill", html: `${icons.download}<span>v${state.updateVersion}</span>` });
    pill.title = `Install v${state.updateVersion} and restart`;
    pill.onclick = async () => {
      pill.textContent = "Installing…";
      try {
        await ipc.installUpdate(); // relaunches on success
      } catch {
        pill.textContent = "Update failed";
      }
    };
    f.appendChild(pill);
  }
  f.appendChild(el("span", { class: "grow" }));
  const quit = el("button", { class: "text-btn", text: "Quit" });
  quit.onclick = () => ipc.quitApp();
  f.appendChild(quit);
  return f;
}

// ---- Utils ---------------------------------------------------------------

const wrapIcon = (svg: string) => `<span class="icon-btn" style="pointer-events:none">${svg}</span>`;
const cap = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);
const escapeHtml = (s: string) => s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]!));

boot();
