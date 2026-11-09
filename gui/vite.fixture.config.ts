import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

/** 只给桌面布局复测。不进 tauri 的 beforeBuildCommand，产物目录也和生产 dist 分开。 */
export default defineConfig({
  plugins: [react()],
  base: "./",
  build: {
    outDir: "dist-fixture",
    emptyOutDir: true,
    rollupOptions: {
      input: "fixture.html",
    },
  },
});
