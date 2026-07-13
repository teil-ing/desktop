// Capture overlay — the TS counterpart of SelectionOverlayView / WindowSelectionOverlayView.
// Rust opens this as a transparent, fullscreen, always-on-top window and stashes the mode.
//
// Region mode: drag a rectangle; on mouseup we send the rect (overlay-local points) to Rust.
// Window mode: macOS-style hover-highlight — dim all but the window under the (camera) cursor.

import * as ipc from "./ipc";
import { el } from "./dom";

const root = document.getElementById("overlay-root")!;

// Virtual-desktop origin (points); set from the overlay_mode command in init().
let origin = { x: 0, y: 0 };

// Escape always cancels and closes the overlay (Rust closes on a null region).
window.addEventListener("keydown", (e) => {
  if (e.key === "Escape") ipc.finishRegionCapture(null);
});

// Ask Rust which mode this overlay is for (+ virtual origin). Reliable, unlike query/init-script.
init();

async function init() {
  let mode = "region";
  try {
    const info = await ipc.overlayMode();
    if (info.mode) mode = info.mode;
    origin = { x: info.originX, y: info.originY };
  } catch {
    // fall back to region select
  }
  if (mode === "window") {
    setupWindowSelect();
  } else {
    setupRegionSelect();
  }
}

// ---- Region drag ---------------------------------------------------------

function setupRegionSelect() {
  const dim = el("div", { class: "dim" });
  const selection = el("div", { class: "selection", attrs: { style: "display:none" } });
  const label = el("div", { class: "dimension-label", attrs: { style: "display:none" } });
  root.append(dim, selection, label);

  let startX = 0;
  let startY = 0;
  let dragging = false;

  const rectOf = () => selection.getBoundingClientRect();

  window.addEventListener("mousedown", (e) => {
    dragging = true;
    startX = e.clientX;
    startY = e.clientY;
    selection.style.display = "block";
    dim.style.display = "none"; // the selection's box-shadow provides the dimming
    update(e.clientX, e.clientY);
  });

  window.addEventListener("mousemove", (e) => {
    if (dragging) update(e.clientX, e.clientY);
  });

  window.addEventListener("mouseup", (e) => {
    if (!dragging) return;
    dragging = false;
    update(e.clientX, e.clientY);
    const r = rectOf();
    // A click with no real drag = cancel (matches macOS "no selection → no capture").
    if (r.width < 4 || r.height < 4) {
      ipc.finishRegionCapture(null);
      return;
    }
    ipc.finishRegionCapture({ x: r.left, y: r.top, width: r.width, height: r.height });
  });

  function update(x: number, y: number) {
    const left = Math.min(x, startX);
    const top = Math.min(y, startY);
    const w = Math.abs(x - startX);
    const h = Math.abs(y - startY);
    Object.assign(selection.style, { left: `${left}px`, top: `${top}px`, width: `${w}px`, height: `${h}px` });
    label.style.display = "block";
    label.textContent = `${Math.round(w)} × ${Math.round(h)}`;
    label.style.left = `${left}px`;
    label.style.top = `${Math.max(0, top - 22)}px`;
  }
}

// ---- Window select (macOS-style hover-highlight) -------------------------

// Camera cursor (white glyph, black outline so it reads on any background), hotspot centered.
const CAMERA_SVG =
  '<svg xmlns="http://www.w3.org/2000/svg" width="26" height="26" viewBox="0 0 24 24">' +
  '<path d="M4 7h3l1.5-2h7L17 7h3a1 1 0 0 1 1 1v9a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V8a1 1 0 0 1 1-1z" fill="#fff" stroke="#000" stroke-width="1.2"/>' +
  '<circle cx="12" cy="12.5" r="3.2" fill="#fff" stroke="#000" stroke-width="1.4"/></svg>';

async function setupWindowSelect() {
  document.body.style.cursor = `url("data:image/svg+xml,${encodeURIComponent(CAMERA_SVG)}") 13 13, auto`;

  // Dim shown when the cursor is over empty space (no window under it).
  const dimFull = el("div", { class: "dim-full" });
  // Highlight shown when over a window: transparent rect + a huge box-shadow that dims everything else.
  const highlight = el("div", { class: "win-highlight", attrs: { style: "display:none" } });
  const label = el("div", { class: "win-label", attrs: { style: "display:none" } });
  const debug = el("div", { class: "win-debug", text: "Loading windows…" });
  root.append(dimFull, highlight, label, debug);

  let windows: Awaited<ReturnType<typeof ipc.listWindows>> = [];
  try {
    windows = await ipc.listWindows();
  } catch (e) {
    debug.textContent = `listWindows error: ${e}`;
  }
  debug.textContent = `${windows.length} window(s) — hover one and click`;

  // Faint outline for every detected window — a live check that geometry maps correctly.
  for (const w of windows) {
    root.appendChild(
      el("div", {
        class: "win-outline",
        attrs: { style: `left:${w.x - origin.x}px;top:${w.y - origin.y}px;width:${w.width}px;height:${w.height}px` },
      }),
    );
  }

  let hoveredId: number | null = null;

  window.addEventListener("mousemove", (e) => {
    // Cursor in global points; Window::all is front-to-back, so the first hit is the topmost window.
    const gx = e.clientX + origin.x;
    const gy = e.clientY + origin.y;
    const hit = windows.find(
      (w) => gx >= w.x && gx < w.x + w.width && gy >= w.y && gy < w.y + w.height,
    );

    if (!hit) {
      hoveredId = null;
      highlight.style.display = "none";
      label.style.display = "none";
      dimFull.style.display = "block";
      return;
    }
    hoveredId = hit.id;
    dimFull.style.display = "none";
    const lx = hit.x - origin.x;
    const ly = hit.y - origin.y;
    Object.assign(highlight.style, {
      display: "block",
      left: `${lx}px`,
      top: `${ly}px`,
      width: `${hit.width}px`,
      height: `${hit.height}px`,
    });
    label.style.display = "block";
    label.textContent = hit.appName || hit.title || "Window";
    label.style.left = `${Math.max(4, lx + 6)}px`;
    label.style.top = `${Math.max(4, ly + 6)}px`;
  });

  window.addEventListener("click", () => {
    if (hoveredId != null) ipc.captureWindow(hoveredId);
  });
}
