# teil.ing desktop

Menu-bar / tray client for [teil.ing](https://teil.ing) — capture screenshots and share them instantly.

## Features

- **Region, window, and fullscreen capture** with global shortcuts (⌘⇧X / ⌘⇧C / ⌘⇧S, customizable)
- Native capture on macOS: crosshair selection overlay, hover-to-pick window capture, multi-monitor support
- Uploads straight to teil.ing — share link on your clipboard, optionally opened in the browser
- Sign in with your browser (no API key juggling), key stored in the OS keychain
- Upload history and quota at a glance from the tray popover
- Automatic updates

## Requirements

- macOS 14+ or Windows 10+

## Development

```
npm install
make dev          # run with hot reload
make app          # build a local .app
```

## Release

```
make release V=x.y.z
```

Tags the version and pushes — CI builds, signs, notarizes, and publishes the release.
