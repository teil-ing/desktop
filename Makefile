# teil.ing crossplatform (Tauri) Makefile — macOS builds
#
# Usage:
#   make dev            — run with hot reload (vite dev server + debug build)
#   make app            — unsigned (ad-hoc) .app for local testing
#   make app-signed     — Developer ID-signed .app
#   make dmg            — unsigned DMG
#   make dmg-signed     — Developer ID-signed DMG
#   make dmg-release    — signed + notarized + stapled DMG (requires ASC_* env)
#   make bump V=x.y.z   — set version in tauri.conf.json + Cargo.toml (+ lockfile)
#   make release V=x.y.z — bump → commit → tag → push; CI signs, notarizes and
#                          publishes the GitHub release (incl. auto-update manifest)
#   make clean          — remove build artifacts
#
# Signing (same conventions as the Swift app's build-dmg.sh):
#   CODE_SIGN_IDENTITY  — default "Developer ID Application: Tillmann Hubner (5A7M476YY2)"
#   ASC_KEY_ID          — App Store Connect API Key ID        (dmg-release)
#   ASC_ISSUER_ID       — App Store Connect Issuer ID         (dmg-release)
#   ASC_KEY_PATH        — Path to the .p8 API key file        (dmg-release)
#
# Tauri does the heavy lifting: APPLE_SIGNING_IDENTITY makes the bundler codesign
# the .app (hardened runtime on by default), and the APPLE_API_* variables make it
# notarize + staple the DMG via notarytool.

SHELL := /bin/bash
CODE_SIGN_IDENTITY ?= Developer ID Application: Tillmann Hubner (5A7M476YY2)
BUNDLE_DIR := src-tauri/target/release/bundle
VERSION    := $(shell sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' src-tauri/tauri.conf.json | head -1)

.PHONY: dev deps app app-signed dmg dmg-signed dmg-release bump release clean licenses

# Regenerate THIRD-PARTY-LICENSES.txt from the current dependency tree.
# Requires: cargo install cargo-about --features cli
# Run after changing Rust/npm dependencies, then commit the result.
licenses:
	./scripts/gen-licenses.sh

deps:
	@test -d node_modules || npm install

dev: deps
	npm run tauri dev

# Unsigned (ad-hoc) — for local testing only; Gatekeeper will refuse it elsewhere.
app: deps
	npm run tauri build -- --bundles app
	@echo "→ $(BUNDLE_DIR)/macos/teil.ing.app"

app-signed: deps
	APPLE_SIGNING_IDENTITY="$(CODE_SIGN_IDENTITY)" npm run tauri build -- --bundles app
	@codesign -dvv "$(BUNDLE_DIR)/macos/teil.ing.app" 2>&1 | grep '^Authority=' | head -1
	@echo "→ $(BUNDLE_DIR)/macos/teil.ing.app"

dmg: deps
	npm run tauri build -- --bundles dmg
	@ls "$(BUNDLE_DIR)/dmg/"*.dmg

dmg-signed: deps
	APPLE_SIGNING_IDENTITY="$(CODE_SIGN_IDENTITY)" npm run tauri build -- --bundles dmg
	@codesign -dvv "$(BUNDLE_DIR)/macos/teil.ing.app" 2>&1 | grep '^Authority=' | head -1
	@ls "$(BUNDLE_DIR)/dmg/"*.dmg

# Signed + notarized + stapled. Notarization credentials use the same ASC_* names
# as the Swift app's build-dmg.sh and are mapped to Tauri's APPLE_API_* here.
dmg-release: deps
	@test -n "$$ASC_KEY_ID"    || { echo "ASC_KEY_ID required (App Store Connect key id)"; exit 1; }
	@test -n "$$ASC_ISSUER_ID" || { echo "ASC_ISSUER_ID required (App Store Connect issuer id)"; exit 1; }
	@test -n "$$ASC_KEY_PATH"  || { echo "ASC_KEY_PATH required (path to .p8 key)"; exit 1; }
	APPLE_SIGNING_IDENTITY="$(CODE_SIGN_IDENTITY)" \
	APPLE_API_KEY="$$ASC_KEY_ID" \
	APPLE_API_ISSUER="$$ASC_ISSUER_ID" \
	APPLE_API_KEY_PATH="$$ASC_KEY_PATH" \
	npm run tauri build -- --bundles dmg
	@xcrun stapler validate "$(BUNDLE_DIR)/dmg/"*.dmg
	@ls "$(BUNDLE_DIR)/dmg/"*.dmg

# Set the app version everywhere Tauri reads it.
# Usage: make bump V=0.2.0
bump:
	@test -n "$(V)" || { echo "Usage: make bump V=x.y.z"; exit 1; }
	sed -i '' 's/"version": *"[^"]*"/"version": "$(V)"/' src-tauri/tauri.conf.json
	sed -i '' 's/^version = ".*"/version = "$(V)"/' src-tauri/Cargo.toml
	cd src-tauri && cargo update -q --package teil-ing-crossplatform
	@echo "Bumped to v$(V)"

# Full release pipeline: bump → commit → tag → push. CI (release.yml) builds,
# signs, notarizes, uploads the DMG + updater artifacts + latest.json to a
# draft release and publishes it as latest once everything is attached.
# Usage: make release V=0.2.0
release:
	@test -n "$(V)" || { echo "Usage: make release V=x.y.z"; exit 1; }
	@echo "==> Bumping to v$(V)..."
	$(MAKE) bump V=$(V)
	@echo "==> Committing..."
	git add src-tauri/tauri.conf.json src-tauri/Cargo.toml src-tauri/Cargo.lock
	git commit -m "Bump version to v$(V)"
	@echo "==> Tagging v$(V)..."
	git tag -a "v$(V)" -m "v$(V)"
	@echo "==> Pushing..."
	git push && git push --tags
	@echo "==> v$(V) tagged — CI publishes the release when the build finishes"

clean:
	rm -rf dist src-tauri/target src-tauri/swift/TeilCapture/.build
