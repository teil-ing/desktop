// Tiny DOM + formatting helpers. No framework — the popover is small enough that
// direct DOM building keeps the port readable and dependency-free.

/** Create an element with class, html, and attributes in one call. */
export function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  opts: { class?: string; html?: string; text?: string; attrs?: Record<string, string> } = {},
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (opts.class) node.className = opts.class;
  if (opts.html !== undefined) node.innerHTML = opts.html;
  if (opts.text !== undefined) node.textContent = opts.text;
  if (opts.attrs) for (const [k, v] of Object.entries(opts.attrs)) node.setAttribute(k, v);
  return node;
}

/**
 * Relative time matching HistoryRowView.relativeTimestamp — clamps fresh/future
 * timestamps to "Just now" so a just-uploaded shot never reads as a future time.
 */
export function relativeTime(iso: string): string {
  const then = new Date(iso).getTime();
  if (Number.isNaN(then)) return "";
  const secondsAgo = (Date.now() - then) / 1000;
  if (secondsAgo < 5) return "Just now";
  const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
  const units: [Intl.RelativeTimeFormatUnit, number][] = [
    ["year", 31536000],
    ["month", 2592000],
    ["day", 86400],
    ["hour", 3600],
    ["minute", 60],
    ["second", 1],
  ];
  for (const [unit, secs] of units) {
    if (secondsAgo >= secs || unit === "second") {
      return rtf.format(-Math.floor(secondsAgo / secs), unit);
    }
  }
  return "Just now";
}

/** Byte formatter matching QuotaBarView.formatBytes. */
export function formatBytes(bytes: number): string {
  const mb = bytes / 1_048_576;
  if (mb >= 1) return `${mb.toFixed(1)} MB`;
  return `${Math.round(bytes / 1024)} KB`;
}

/** teil.ing share URL → its web edit page (append /edit). */
export const editUrl = (shareUrl: string) => `${shareUrl}/edit`;

/**
 * Render a Tauri accelerator ("CmdOrCtrl+Shift+X") the way the platform shows shortcuts:
 * ⌃⌥⇧⌘X on macOS, "Ctrl+Shift+X" on Windows.
 */
export function formatShortcut(accel: string): string {
  const isMac = /mac/i.test(navigator.userAgent) || /mac/i.test((navigator as { platform?: string }).platform ?? "");
  let cmd = false;
  let ctrl = false;
  let alt = false;
  let shift = false;
  let key = "";
  for (const raw of accel.split("+")) {
    const t = raw.trim().toLowerCase();
    if (["cmdorctrl", "cmd", "command", "super", "meta"].includes(t)) cmd = true;
    else if (["ctrl", "control"].includes(t)) ctrl = true;
    else if (["alt", "option"].includes(t)) alt = true;
    else if (t === "shift") shift = true;
    else key = raw.trim().toUpperCase();
  }
  if (isMac) {
    // Canonical macOS order: Control, Option, Shift, Command, then the key — no separators.
    return (ctrl ? "⌃" : "") + (alt ? "⌥" : "") + (shift ? "⇧" : "") + (cmd ? "⌘" : "") + key;
  }
  const parts: string[] = [];
  if (cmd || ctrl) parts.push("Ctrl"); // CmdOrCtrl → Ctrl on Windows
  if (alt) parts.push("Alt");
  if (shift) parts.push("Shift");
  parts.push(key);
  return parts.join("+");
}
