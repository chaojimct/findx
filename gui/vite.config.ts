import { fileViewerRenderers } from "@file-viewer/vite-plugin";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [
    react(),
    fileViewerRenderers({
      autoPresets: false,
      formats: ["pdf", "docx", "doc", "rtf", "odt", "xlsx", "xls", "pptx", "ofd"],
      copyAssets: {
        baseDir: "file-viewer",
        mode: "both",
      },
    }),
  ],

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    // 与 tauri.conf.json 的 devUrl 一致，避免 Windows 上 localhost→::1 与 Vite 监听不一致导致 tauri dev 一直等待
    host: host || "127.0.0.1",
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
