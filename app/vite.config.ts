// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

import { execFileSync } from "node:child_process";
import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// The commit the build came from, shown in About (build.rs stamps the same
// into the startup log line). Empty outside a git checkout.
let commit = "";
try {
  commit = execFileSync("git", ["rev-parse", "--short", "HEAD"], { encoding: "utf8" }).trim();
} catch {
  // Not a git checkout (e.g. a source archive).
}

// Set by `tauri dev` when targeting a remote device (mobile); unused on desktop.
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/ and https://v2.tauri.app/start/frontend/vite/
export default defineConfig({
  plugins: [svelte()],
  define: { __SHREDLAB_COMMIT__: JSON.stringify(commit) },
  // Don't hide Rust errors printed by `tauri dev`.
  clearScreen: false,
  server: {
    // Must match `build.devUrl` in src-tauri/tauri.conf.json.
    port: 1420,
    strictPort: true,
    host: host || "localhost",
    hmr: host ? { protocol: "ws", host, port: 1421 } : undefined,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  build: {
    // Tauri uses Chromium (WebView2) on Windows and WebKit on macOS.
    target: process.env.TAURI_ENV_PLATFORM === "windows" ? "chrome105" : "safari15",
    minify: !process.env.TAURI_ENV_DEBUG,
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
    // Never inline fonts as data: URIs; the Tauri CSP (font-src via
    // default-src 'self') would block them.
    assetsInlineLimit: (file: string) => (file.endsWith(".woff2") ? false : undefined),
  },
});
