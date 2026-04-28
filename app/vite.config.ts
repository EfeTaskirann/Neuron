import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri 2 dev server contract:
//   - Fixed port 5173 (matches src-tauri/tauri.conf.json `build.devUrl`)
//   - strictPort so Vite refuses to silently pick another port if 5173 is busy
//   - clearScreen=false so cargo / Tauri build output stays visible
//   - HMR over the same port; Tauri's WebView attaches to it.
// See https://tauri.app/v2/develop/configuration-files/ for the locked contract.
export default defineConfig(async () => ({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    host: "127.0.0.1",
  },
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  build: {
    target: "es2022",
    sourcemap: true,
  },
}));
