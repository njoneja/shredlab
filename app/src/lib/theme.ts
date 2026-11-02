// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

// Theme handling (from the brand guide). The theme is saved with the app
// settings in the backend; localStorage keeps a mirror that
// public/theme-init.js applies before first paint (no flash).
import { THEMES, type Theme } from "./types";

const MIRROR_KEY = "shredlab.theme";

export const THEME_NAME: Record<Theme, string> = {
  stage: "Stage",
  rosewood: "Rosewood",
  valve: "Valve",
  manuscript: "Manuscript",
};

export function isTheme(t: unknown): t is Theme {
  return typeof t === "string" && (THEMES as readonly string[]).includes(t);
}

/** The theme on <html> right now (set by theme-init.js). */
export function currentTheme(): Theme {
  const t = typeof document !== "undefined" ? document.documentElement.dataset.theme : undefined;
  return isTheme(t) ? t : THEMES[0];
}

/** Applies a theme to <html> and updates the localStorage mirror. */
export function applyTheme(t: Theme) {
  document.documentElement.dataset.theme = t;
  try {
    localStorage.setItem(MIRROR_KEY, t);
  } catch {
    /* storage unavailable: the backend setting still applies after load */
  }
}
