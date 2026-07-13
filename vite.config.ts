import { defineConfig } from "vite";
import { resolve } from "node:path";

// Two entry points: the tray popover (index.html) and the capture overlay (overlay.html).
// Tauri opens each in its own window. Dev server on a fixed port so Tauri can attach.
export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  build: {
    target: "es2021",
    rollupOptions: {
      input: {
        main: resolve(__dirname, "index.html"),
        overlay: resolve(__dirname, "overlay.html"),
        preferences: resolve(__dirname, "preferences.html"),
      },
    },
  },
});
