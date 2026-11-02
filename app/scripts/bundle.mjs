// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

// `pnpm bundle`: release build + installers.
//
// The frontend and the release binary are built before `tauri build` so that
// on Windows target/release/DirectML.dll (copied there by the `ort` crate's
// build) exists when Tauri collects the bundle resources. The DLL is added
// via tauri.bundle.windows.conf.json, on Windows only: macOS has no
// DirectML.dll and would fail on the missing resource. (Not the automatic
// tauri.windows.conf.json, because that is merged into every build, including
// `tauri dev` and `cargo test`, before the release DLL exists.)
// The DLL is staged as a real copy in target/bundle-res first: where symlinks
// are allowed (e.g. CI runners), `ort` puts a symlink to its download cache
// in target/release, and Tauri's build script would then copy the cache file
// onto itself through that symlink (os error 32).
// THIRD_PARTY_LICENSES.html is regenerated first so the notices always match
// Cargo.lock and node_modules (it ships in the UI and as a bundle resource).
// Extra arguments are passed to `tauri build`, e.g. `pnpm bundle --bundles nsis`.
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";

function run(cmd, args) {
  // One command string through the shell, so `pnpm` resolves to pnpm.cmd on Windows.
  const line = [cmd, ...args].map((a) => (/[\s"]/.test(a) ? `"${a.replace(/"/g, '\\"')}"` : a)).join(" ");
  console.log(`> ${line}`);
  const r = spawnSync(line, { stdio: "inherit", shell: true });
  if (r.status !== 0) process.exit(r.status ?? 1);
}

const extra = process.platform === "win32" ? ["--config", "src-tauri/tauri.bundle.windows.conf.json"] : [];
run("node", ["../tools/third_party_licenses/generate.mjs"]);
run("pnpm", ["exec", "vite", "build"]);
run("cargo", ["build", "--release", "-p", "shredlab"]);
if (process.platform === "win32") {
  const target = new URL("../../target/", import.meta.url);
  mkdirSync(new URL("bundle-res/", target), { recursive: true });
  // copyFileSync follows a symlink and writes a regular file.
  copyFileSync(new URL("release/DirectML.dll", target), new URL("bundle-res/DirectML.dll", target));
}
run("pnpm", ["exec", "tauri", "build", ...extra, ...process.argv.slice(2)]);
