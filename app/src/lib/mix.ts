// Pure helpers for the mixer: dB <-> linear gain, the volume slider law, and
// the "backing track built from the original" rule (API.md, P1).

/** Top of the volume range. */
export const DB_MAX = 6;
/** Lowest dB on the slider; the position below it is "off" (−∞, gain 0). */
export const DB_MIN = -40;
/** One slider step (arrow keys, drag resolution). */
export const DB_STEP = 0.5;
/** Linear gain at +6 dB (≈ 1.995). Stored/sent volumes never exceed this. */
export const GAIN_MAX = 10 ** (DB_MAX / 20);

/**
 * Slider positions are integers 0..SLIDER_MAX:
 * 0 = off (−∞), 1 = −40 dB, then +0.5 dB per step up to +6 dB.
 */
export const SLIDER_MAX = 1 + Math.round((DB_MAX - DB_MIN) / DB_STEP);
/** Slider position of 0 dB (unity, "as separated"). */
export const SLIDER_UNITY = 1 + Math.round(-DB_MIN / DB_STEP);

export const dbToGain = (db: number) => (db === -Infinity ? 0 : 10 ** (db / 20));
export const gainToDb = (g: number) => (g > 0 ? 20 * Math.log10(g) : -Infinity);

export function clampGain(g: number): number {
  if (!Number.isFinite(g) || g <= 0) return 0;
  return Math.min(GAIN_MAX, g);
}

export function sliderToGain(pos: number): number {
  const p = Math.round(pos);
  if (p <= 0) return 0;
  if (p >= SLIDER_MAX) return GAIN_MAX;
  return dbToGain(DB_MIN + (p - 1) * DB_STEP);
}

/** Nearest slider position for a linear gain (gains below −40 dB but > 0 sit at the −40 dB step). */
export function gainToSlider(g: number): number {
  if (!(g > 0)) return 0;
  const db = gainToDb(g);
  const p = 1 + Math.round((db - DB_MIN) / DB_STEP);
  return Math.max(1, Math.min(SLIDER_MAX, p));
}

/** "+2.5 dB", "0 dB", "−12 dB", "−∞". */
export function formatDb(g: number): string {
  if (!(g > 0)) return "−∞";
  const db = Math.round(gainToDb(g) * 10) / 10;
  if (db === 0) return "0 dB";
  const abs = Math.abs(db);
  const num = Number.isInteger(abs) ? abs.toFixed(0) : abs.toFixed(1);
  return `${db > 0 ? "+" : "−"}${num} dB`;
}

/**
 * Whether playback/export of `onCount` stems (of a song with `stemCount`
 * stems) is built from the original: `original_available && 2·on >= n`.
 * Export ignores solo, so this is exactly the export rule.
 */
export function backingFromOriginal(originalAvailable: boolean, onCount: number, stemCount: number): boolean {
  return originalAvailable && onCount > 0 && 2 * onCount >= stemCount;
}
