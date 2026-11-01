// Theme handling (from the brand guide). The theme is saved with the app
// settings in the backend; localStorage keeps a mirror that
// public/theme-init.js applies before first paint (no flash).
import { THEMES, type Theme } from "./types";

const MIRROR_KEY = "shredlab.theme";

export const THEME_INFO: Record<Theme, { name: string; bg: string; accent: string }> = {
  stage: { name: "Stage", bg: "#0B0C10", accent: "#FF3D7F" },
  rosewood: { name: "Rosewood", bg: "#1A0F10", accent: "#E0875A" },
  valve: { name: "Valve", bg: "#16120E", accent: "#F0A83A" },
  manuscript: { name: "Manuscript", bg: "#F4EFE4", accent: "#C2412D" },
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
