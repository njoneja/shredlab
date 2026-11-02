// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

// The single seam between UI and backend. Inside Tauri this talks to the Rust
// side (see API.md); in a plain browser it uses an in-memory mock so the UI
// is fully clickable with `pnpm dev`.
import { mockBackend } from "./api.mock";
import { tauriBackend } from "./api.tauri";
import type { Backend } from "./types";

export * from "./types";

export const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export const api: Backend = inTauri ? tauriBackend : mockBackend;
