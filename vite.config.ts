import process from "node:process";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// Set by `tauri dev` when the app is run on another device.
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],

  // Rust errors from `tauri dev` stay on screen.
  clearScreen: false,
  // Not Vite's usual `dist`: that folder holds the built app. Tauri reads the pages from here
  // (`frontendDist` in src-tauri/tauri.conf.json).
  build: { outDir: "build" },
  server: {
    // Tauri looks for the dev server on this port and nowhere else.
    port: 1420,
    strictPort: true,
    host: host || "127.0.0.1",
    hmr: host ? { protocol: "ws", host, port: 1421 } : undefined,
    // Rebuilding the Rust side is Tauri's job, not Vite's.
    watch: { ignored: ["**/src-tauri/**"] },
  },
});
