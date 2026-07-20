#!/usr/bin/env bash
# Generate THIRD-PARTY-LICENSES.txt (repo root): full license texts for every
# bundled open-source component, as required for redistribution (MIT/BSD/Apache
# etc. all mandate reproducing their text + copyright notices).
#
#   Rust crates  → cargo-about (needs: cargo install cargo-about --features cli)
#   npm packages → the @tauri-apps/* LICENSE files in node_modules
#
# Run from anywhere; paths are resolved relative to the repo root.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/THIRD-PARTY-LICENSES.txt"
TMP="$(mktemp)"
trap 'rm -f "$TMP"' EXIT

echo "==> Rust crates (cargo-about)..."
( cd "$ROOT/src-tauri" && cargo about generate about.hbs --all-features ) > "$TMP"

# --- npm production dependencies -------------------------------------------
# All current runtime JS deps are @tauri-apps/* (dual "MIT OR Apache-2.0"). Some
# ship only a LICENSE.spdx metadata file (not license text), so we reproduce the
# canonical MIT + Apache-2.0 texts (bundled with @tauri-apps/api) once for the
# whole family. Extend if a non-@tauri-apps runtime dep is ever added.
echo "==> npm packages..."
{
  echo ""
  echo "================================================================================"
  echo "NPM PACKAGES"
  echo "================================================================================"
  echo ""
  for pkg in $(node -e "console.log(Object.keys(require('$ROOT/package.json').dependencies||{}).join(' '))"); do
    dir="$ROOT/node_modules/$pkg"
    [ -d "$dir" ] || { echo "  ! $pkg not installed, skipping" >&2; continue; }
    ver="$(node -e "console.log(require('$dir/package.json').version)")"
    lic="$(node -e "console.log(require('$dir/package.json').license||'')")"
    echo "  - $pkg $ver${lic:+ ($lic)}"
  done
  echo ""
  echo "All packages above are © The Tauri Programme in the Commons Conservancy /"
  echo "Tauri Apps Contributors, dual-licensed under MIT OR Apache-2.0. Full texts:"
  api="$ROOT/node_modules/@tauri-apps/api"
  for f in "$api"/LICENSE_MIT "$api"/LICENSE_APACHE-2.0; do
    [ -f "$f" ] || continue
    echo ""
    echo "--------------------------------------------------------------------------------"
    cat "$f"
  done
} >> "$TMP"

mv "$TMP" "$OUT"
trap - EXIT
lines=$(wc -l < "$OUT" | tr -d ' ')
echo "==> Wrote $OUT ($lines lines)"
