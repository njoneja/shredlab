// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

// Refine panel helpers (P5): defaults, quick stereo presets, the stereo
// window and the EQ curve. Mirrors crates/shredlab-dsp/src/refine (pan.rs,
// eq.rs); the curve uses the same RBJ formulas, so it is what you hear.
import type { BandParams, EndParams, EqParams, PanParams, RefineParams, RefinePreset } from "./types";

export const SR = 44100;
export const F_MIN = 20;
export const F_MAX = 20000;
/** Shelf gain range (±dB) and band gain range shown in the UI. */
export const SHELF_MAX_DB = 12;
export const BAND_MAX_DB = 15;
export const STRENGTH_NAMES = ["Natural", "Light", "Medium", "Strong", "Tight"] as const;

export const defaultPan = (): PanParams => ({ enabled: false, center: 0, width: 0.66, softness: 0.2 });
const end = (freq: number): EndParams => ({ enabled: false, freq, mode: "cut", gain_db: 0 });
const band = (freq: number): BandParams => ({ enabled: false, freq, gain_db: 0, q: 1 });
export const flatEq = (): EqParams => ({ low_cut: end(80), high_cut: end(12000), bands: [band(200), band(1000), band(5000)] });
export const defaultRefine = (): RefineParams => ({ pan: defaultPan(), eq: flatEq() });

/** Quick stereo selections: the left / centre / right third of the field. */
export const PAN_QUICK = {
  left: { center: -1, width: 1.34 },
  center: { center: 0, width: 0.66 },
  right: { center: 1, width: 1.34 },
} as const;
export type PanQuick = keyof typeof PAN_QUICK;

export function quickOf(p: PanParams): PanQuick | null {
  if (!p.enabled) return null;
  for (const [k, v] of Object.entries(PAN_QUICK)) {
    if (Math.abs(p.center - v.center) < 1e-3 && Math.abs(p.width - v.width) < 1e-3) return k as PanQuick;
  }
  return null;
}

const bandGain = (b: BandParams) => (b.enabled && Number.isFinite(b.gain_db) ? b.gain_db : 0);
export const shelfGain = (e: EndParams) =>
  e.enabled && e.mode === "shelf" && Number.isFinite(e.gain_db) ? Math.max(-SHELF_MAX_DB, Math.min(SHELF_MAX_DB, e.gain_db)) : 0;
export const cutOn = (e: EndParams) => e.enabled && e.mode === "cut";

export function eqIsFlat(eq: EqParams): boolean {
  return !cutOn(eq.low_cut) && !cutOn(eq.high_cut) && shelfGain(eq.low_cut) === 0 && shelfGain(eq.high_cut) === 0 && eq.bands.every((b) => bandGain(b) === 0);
}

export function isIdentity(p: RefineParams | undefined): boolean {
  return !p || (!p.pan.enabled && eqIsFlat(p.eq));
}

/** Drops fields PanParams no longer has (the old Keep/Remove `mode`). */
function panOnly(p: PanParams): PanParams {
  return { enabled: !!p.enabled, center: p.center, width: p.width, softness: p.softness };
}

/** True when the stem has a stereo exception (the window flips with the switch). */
export const hasException = (p: RefineParams | undefined) => !!p?.pan.enabled;

/** Which part of the field the exception covers, by its centre. */
export type Side = "left" | "centre" | "right";
export function exceptionSide(p: PanParams): Side {
  return p.center <= -0.2 ? "left" : p.center >= 0.2 ? "right" : "centre";
}

/** "Guitar on" / "Guitar off, except left" (switch tooltip and accessible name). */
export function switchText(label: string, on: boolean, p: RefineParams | undefined): string {
  const base = `${label} ${on ? "on" : "off"}`;
  return p && hasException(p) ? `${base}, except ${exceptionSide(p.pan)}` : base;
}

/** Fills in fields missing from older saved params (shelf fields etc.). */
export function normalise(p: Partial<RefineParams> | undefined): RefineParams {
  const d = defaultRefine();
  if (!p) return d;
  const e = (x: Partial<EndParams> | undefined, f: EndParams): EndParams => ({ ...f, ...x });
  const b = (x: Partial<BandParams> | undefined, f: BandParams): BandParams => ({ ...f, ...x });
  const eq = p.eq ?? d.eq;
  return {
    pan: panOnly({ ...d.pan, ...p.pan }),
    eq: {
      low_cut: e(eq.low_cut, d.eq.low_cut),
      high_cut: e(eq.high_cut, d.eq.high_cut),
      bands: [b(eq.bands?.[0], d.eq.bands[0]), b(eq.bands?.[1], d.eq.bands[1]), b(eq.bands?.[2], d.eq.bands[2])],
    },
  };
}

/** Raised-cosine window gain (pan.rs `window_gain`). */
export function windowGain(pan: number, center: number, width: number, softness: number): number {
  const d = Math.abs(pan - center);
  const t = Math.max(0, Math.min(1, (d - (width * 0.5 - softness * 0.5)) / softness));
  return 0.5 + 0.5 * Math.cos(Math.PI * t);
}
/**
 * How much of the stem at `pan` is heard (0…1) for a row whose switch is
 * `on`: no exception → all or nothing; with one, the window does the
 * opposite of the switch.
 */
export function heardGain(p: PanParams, on: boolean, pan: number): number {
  if (!p.enabled) return on ? 1 : 0;
  const g = windowGain(pan, p.center, p.width, Math.max(0.02, p.softness));
  return on ? 1 - g : g;
}

// ---- EQ response (RBJ cookbook, as eq.rs `response_db`) ----

type Biquad = [number, number, number, number, number, number]; // b0 b1 b2 a0 a1 a2
const BUTTER4_Q = [0.541196100146197, 1.3065629648763765];
const clampF = (f: number) => Math.max(10, Math.min(0.45 * SR, Number.isFinite(f) ? f : 1000));

function trig(f: number) {
  const w0 = (2 * Math.PI * clampF(f)) / SR;
  return { s: Math.sin(w0), c: Math.cos(w0) };
}
function hp(f: number, q: number): Biquad {
  const { s, c } = trig(f);
  const al = s / (2 * q);
  return [(1 + c) / 2, -(1 + c), (1 + c) / 2, 1 + al, -2 * c, 1 - al];
}
function lp(f: number, q: number): Biquad {
  const { s, c } = trig(f);
  const al = s / (2 * q);
  return [(1 - c) / 2, 1 - c, (1 - c) / 2, 1 + al, -2 * c, 1 - al];
}
function peak(f: number, q: number, g: number): Biquad {
  const { s, c } = trig(f);
  const al = s / (2 * Math.max(0.1, Math.min(18, q)));
  const a = 10 ** (g / 40);
  return [1 + al * a, -2 * c, 1 - al * a, 1 + al / a, -2 * c, 1 - al / a];
}
function shelf(f: number, g: number, high: boolean): Biquad {
  const { s, c } = trig(f);
  const a = 10 ** (g / 40);
  const k = 2 * Math.sqrt(a) * ((s / 2) * Math.SQRT2);
  return high
    ? [a * (a + 1 + (a - 1) * c + k), -2 * a * (a - 1 + (a + 1) * c), a * (a + 1 + (a - 1) * c - k), a + 1 - (a - 1) * c + k, 2 * (a - 1 - (a + 1) * c), a + 1 - (a - 1) * c - k]
    : [a * (a + 1 - (a - 1) * c + k), 2 * a * (a - 1 - (a + 1) * c), a * (a + 1 - (a - 1) * c - k), a + 1 + (a - 1) * c + k, -2 * (a - 1 + (a + 1) * c), a + 1 + (a - 1) * c - k];
}
function mag(b: Biquad, f: number): number {
  const w = (2 * Math.PI * f) / SR;
  const c1 = Math.cos(w), s1 = -Math.sin(w), c2 = Math.cos(2 * w), s2 = -Math.sin(2 * w);
  const nr = b[0] + b[1] * c1 + b[2] * c2, ni = b[1] * s1 + b[2] * s2;
  const dr = b[3] + b[4] * c1 + b[5] * c2, di = b[4] * s1 + b[5] * s2;
  return Math.sqrt((nr * nr + ni * ni) / (dr * dr + di * di));
}

export function eqSections(eq: EqParams): Biquad[] {
  const out: Biquad[] = [];
  if (cutOn(eq.low_cut)) for (const q of BUTTER4_Q) out.push(hp(eq.low_cut.freq, q));
  if (cutOn(eq.high_cut)) for (const q of BUTTER4_Q) out.push(lp(eq.high_cut.freq, q));
  const ls = shelfGain(eq.low_cut), hs = shelfGain(eq.high_cut);
  if (ls) out.push(shelf(eq.low_cut.freq, ls, false));
  if (hs) out.push(shelf(eq.high_cut.freq, hs, true));
  for (const b of eq.bands) if (bandGain(b)) out.push(peak(b.freq, b.q, bandGain(b)));
  return out;
}

/** Response in dB at each frequency. */
export function responseDb(eq: EqParams, freqs: number[]): number[] {
  const secs = eqSections(eq);
  return freqs.map((f) => 20 * Math.log10(Math.max(1e-12, secs.reduce((m, b) => m * mag(b, f), 1))));
}

export function logFreqs(n: number, lo = F_MIN, hi = F_MAX): number[] {
  const a = Math.log(lo), b = Math.log(hi);
  return Array.from({ length: n }, (_, i) => Math.exp(a + ((b - a) * i) / (n - 1)));
}

export const freqToX = (f: number) => (Math.log(f) - Math.log(F_MIN)) / (Math.log(F_MAX) - Math.log(F_MIN));
export const xToFreq = (x: number) => Math.exp(Math.log(F_MIN) + x * (Math.log(F_MAX) - Math.log(F_MIN)));

export function formatHz(f: number): string {
  return f >= 1000 ? `${(f / 1000).toFixed(f >= 10000 ? 0 : 1)} kHz` : `${Math.round(f)} Hz`;
}

/** Fallback presets (browser mock); the app gets them from the backend (shredlab-dsp presets.rs). */
export function mockPresets(): RefinePreset[] {
  const off = (f: number): EndParams => ({ enabled: false, freq: f, mode: "cut", gain_db: 0 });
  const cut = (f: number): EndParams => ({ enabled: true, freq: f, mode: "cut", gain_db: 0 });
  const B = (freq: number, gain_db: number, q: number): BandParams => ({ enabled: true, freq, gain_db, q });
  const O: BandParams = { enabled: false, freq: 1000, gain_db: 0, q: 1 };
  return [
    { id: "bass-notes", name: "Bass: hear the notes", eq: { low_cut: cut(30), high_cut: off(12000), bands: [B(80, -3, 0.7), B(700, 5, 0.9), B(1600, 4, 1)] } },
    { id: "bass-tame-attack", name: "Bass: tame slap/pick noise", eq: { low_cut: off(80), high_cut: cut(5000), bands: [O, O, B(2500, -6, 0.9)] } },
    { id: "guitar-focus", name: "Guitar: focus", eq: { low_cut: cut(90), high_cut: cut(9000), bands: [B(250, -3, 1), B(2500, 3, 1), O] } },
    { id: "guitar-no-bass", name: "Guitar: remove bass bleed (low cut)", eq: { low_cut: cut(120), high_cut: off(12000), bands: [O, O, O] } },
    { id: "vocals-clarity", name: "Vocals: clarity", eq: { low_cut: cut(100), high_cut: off(12000), bands: [B(300, -2, 1), B(3000, 3, 1), B(10000, 2, 0.7)] } },
    { id: "backing-room-for-guitar", name: "Backing track: make room for guitar", eq: { low_cut: off(80), high_cut: off(12000), bands: [B(400, -2, 1), B(2000, -4, 0.8), O] } },
    { id: "flat", name: "Flat", eq: { low_cut: off(80), high_cut: off(12000), bands: [O, O, O] } },
  ];
}

/** Preset whose EQ equals `eq`, if any (for the menu's current value). */
export function presetOf(eq: EqParams, presets: RefinePreset[]): string {
  const same = (a: EqParams, b: EqParams) => JSON.stringify(normalise({ eq: a }).eq) === JSON.stringify(normalise({ eq: b }).eq);
  if (eqIsFlat(eq)) return "flat";
  return presets.find((p) => same(p.eq, eq))?.id ?? "";
}
