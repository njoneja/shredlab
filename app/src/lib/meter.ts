// Meter = beats per bar on top of a detected beat grid. Overrides are kept in
// the frontend; this module turns raw beats + downbeats + overrides into the
// effective downbeats sent to the backend (set_meter_overrides).
import type { BeatGrid, MeterOverride } from "./types";

/** Index of the beat closest to time t. */
export function nearestBeatIndex(beats: number[], t: number): number {
  if (!beats.length) return -1;
  let lo = 0;
  let hi = beats.length - 1;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (beats[mid] < t) lo = mid + 1;
    else hi = mid;
  }
  if (lo > 0 && Math.abs(beats[lo - 1] - t) <= Math.abs(beats[lo] - t)) return lo - 1;
  return lo;
}

/** Largest i with arr[i] <= t, or -1. */
export function floorIndex(arr: number[], t: number): number {
  let lo = 0;
  let hi = arr.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (arr[mid] <= t) lo = mid + 1;
    else hi = mid;
  }
  return lo - 1;
}

/** Normalise: sort by beat index, last override at an index wins. */
export function normaliseOverrides(list: MeterOverride[]): MeterOverride[] {
  const byIdx = new Map<number, MeterOverride>();
  for (const o of list) byIdx.set(o.beat_index, o);
  return [...byIdx.values()].sort((a, b) => a.beat_index - b.beat_index);
}

/**
 * Effective downbeat beat-indices.
 * - Before the first override: detected downbeats.
 * - `beats_per_bar` at beat k: k is a downbeat, then every N beats until the
 *   next override.
 * - `beat_one` at beat k: k is a downbeat. If the previous segment was a
 *   fixed `beats_per_bar` one, continue every N beats from k; otherwise the
 *   detected downbeats are shifted so that the nearest one lands on k.
 */
export function effectiveDownbeatIndices(grid: BeatGrid, overrides: MeterOverride[]): number[] {
  const { beats } = grid;
  const n = beats.length;
  const raw = grid.downbeats.map((t) => nearestBeatIndex(beats, t)).filter((i) => i >= 0);
  const rawSorted = [...new Set(raw)].sort((a, b) => a - b);
  const ovs = normaliseOverrides(overrides).filter((o) => o.beat_index >= 0 && o.beat_index < n);
  const out: number[] = [];
  const firstEnd = ovs.length ? ovs[0].beat_index : n;
  for (const i of rawSorted) if (i < firstEnd) out.push(i);
  let fixed: number | null = null;
  for (let j = 0; j < ovs.length; j++) {
    const o = ovs[j];
    const k = o.beat_index;
    const end = j + 1 < ovs.length ? ovs[j + 1].beat_index : n;
    if (o.kind === "beats_per_bar") fixed = Math.max(1, Math.round(o.beats_per_bar));
    if (fixed != null) {
      for (let i = k; i < end; i += fixed) out.push(i);
    } else {
      let nearest = rawSorted[0] ?? k;
      for (const d of rawSorted) if (Math.abs(d - k) < Math.abs(nearest - k)) nearest = d;
      const shift = k - nearest;
      out.push(k);
      for (const d of rawSorted) {
        const s = d + shift;
        if (s > k && s < end) out.push(s);
      }
    }
  }
  return [...new Set(out)].sort((a, b) => a - b);
}

export function effectiveDownbeats(grid: BeatGrid | null, overrides: MeterOverride[]): number[] {
  if (!grid) return [];
  return effectiveDownbeatIndices(grid, overrides).map((i) => grid.beats[i]);
}

/** Most common number of beats per bar for the effective grid. */
export function commonBeatsPerBar(grid: BeatGrid | null, downbeats: number[]): number {
  if (!grid || downbeats.length < 2) return 4;
  const idx = downbeats.map((t) => nearestBeatIndex(grid.beats, t));
  const counts = new Map<number, number>();
  for (let i = 1; i < idx.length; i++) {
    const d = idx[i] - idx[i - 1];
    if (d > 0) counts.set(d, (counts.get(d) ?? 0) + 1);
  }
  let best = 4;
  let bestCount = -1;
  for (const [d, c] of counts) if (c > bestCount || (c === bestCount && d < best)) [best, bestCount] = [d, c];
  return best;
}

/** 1-based bar number at time t; 0 = pickup before the first downbeat. */
export function barAt(downbeats: number[], t: number): number {
  return floorIndex(downbeats, t) + 1;
}

/** Bar number for a loop end (bar containing the instant just before t). */
export function barEndingAt(downbeats: number[], t: number): number {
  return Math.max(0, floorIndex(downbeats, t - 1e-3) + 1);
}

const FRACTIONS: Record<string, string> = {
  "1/2": "½", "1/3": "⅓", "2/3": "⅔", "1/4": "¼", "3/4": "¾", "1/5": "⅕", "2/5": "⅖",
  "3/5": "⅗", "4/5": "⅘", "1/6": "⅙", "5/6": "⅚", "1/8": "⅛", "3/8": "⅜", "5/8": "⅝", "7/8": "⅞",
};

function gcd(a: number, b: number): number {
  return b ? gcd(b, a % b) : a;
}

/** "1¾ bars (7 beats)", "1 bar (4 beats)". */
export function formatCountIn(beats: number, beatsPerBar: number): string {
  const n = Math.max(1, beatsPerBar);
  const whole = Math.floor(beats / n);
  const rem = beats % n;
  let bars = whole ? String(whole) : "";
  if (rem) {
    const g = gcd(rem, n);
    const key = `${rem / g}/${n / g}`;
    bars += FRACTIONS[key] ?? (whole ? ` ${key}` : key);
  }
  if (!bars) bars = "0";
  const isOne = beats === n;
  return `${bars} ${isOne ? "bar" : "bars"} (${beats} ${beats === 1 ? "beat" : "beats"})`;
}

export function formatTime(sec: number, tenths = false): string {
  if (!Number.isFinite(sec) || sec < 0) sec = 0;
  if (tenths) {
    const t = Math.round(sec * 10) / 10;
    const m = Math.floor(t / 60);
    return `${m}:${(t - m * 60).toFixed(1).padStart(4, "0")}`;
  }
  const m = Math.floor(sec / 60);
  const s = sec - m * 60;
  return `${m}:${String(Math.floor(s)).padStart(2, "0")}`;
}
