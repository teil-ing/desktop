# Icons (required before first build)

Tauri needs real icon files here (referenced in `tauri.conf.json` → `bundle.icon`, and used
as the fallback tray icon). Generate them from a single source PNG (1024×1024 recommended):

```
cd crossplatform
npm run tauri icon path/to/teil-ing-1024.png
```

That produces `32x32.png`, `128x128.png`, `128x128@2x.png`, `icon.icns`, `icon.ico`, and the
Windows Store logos into this folder.

For a crisp **menu-bar** icon on macOS, also add a monochrome template `tray.png`
(a black-on-transparent glyph, ~22×22@2x). The app currently falls back to the app icon
with `icon_as_template(true)`; a dedicated template PNG looks sharper. This mirrors the
macOS app's `rectangle.dashed` template symbol.
