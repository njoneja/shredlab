// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

// Note names and layout helpers for the notes panel. Pure functions, no
// state: the store passes a stem's notes (concert pitch, from the backend),
// the chord frame and the key, and gets views, names, the piano roll's
// pitch range and labels back. Names follow the same frame as the chords
// (lib/chords.ts): the shape you fret in My tuning, and what is heard when
// that differs.

import { noteOf, spellingFor, type ChordFrame } from "./chords.ts";
import { quickOf } from "./refine.ts";
import type { ChordsData, NotesFast, NoteSpan, PanParams, StemName, StemNotes } from "./types";

/** Stems with notes, in the stem picker's order. */
export const NOTE_STEMS: StemName[] = ["guitar", "bass", "vocals", "piano", "other"];

export const NOTE_STEM_LABEL: Record<StemName, string> = {
  vocals: "Vocals",
  guitar: "Guitar",
  bass: "Bass",
  drums: "Drums",
  piano: "Keys",
  other: "Other",
};

/** "E4": pitch class and octave (MIDI 60 = C4). */
export function octaveName(midi: number, spelling: ReturnType<typeof spellingFor>): string {
  return noteOf(midi, spelling) + (Math.floor(midi / 12) - 1);
}

export interface BendView {
  start: number;
  peak: number;
  release: number | null;
  semitones: number;
  /** "½", "full", "1½" (see {@link bendLabel}). */
  label: string;
}

export interface NoteView {
  start: number;
  end: number;
  /** Concert pitch (MIDI), before any bend. */
  midi: number;
  /** Shape name in My tuning (what you hear on a Standard instrument). */
  main: string;
  /** What you hear, when it differs from `main`. */
  second: string | null;
  /** "E4" in the frame of `main`. */
  octave: string;
  attack: "picked" | "legato" | "slide";
  bend: BendView | null;
  /** The note before, when this one joins it (slide or legato) without a gap. */
  joinsPrev: boolean;
}

/** Bend size as tab writes it: "¼", "½", "full", "1½", "2"… (rounded to quarter steps below a half, half steps above). */
export function bendLabel(semitones: number): string {
  const a = Math.abs(semitones);
  if (a < 0.75) return "¼";
  const steps = Math.round(a) / 2;
  if (steps === 1) return "full";
  const whole = Math.floor(steps);
  const half = steps - whole >= 0.5;
  if (whole === 0) return "½";
  return `${whole}${half ? "½" : ""}`;
}

/** "a half step", "a whole step", "1½ steps" for tooltips. */
export function bendWords(semitones: number): string {
  const l = bendLabel(semitones);
  if (l === "¼") return "a quarter step";
  if (l === "½") return "a half step";
  if (l === "full") return "a whole step";
  return `${l} steps`;
}

/** Notes closer than this count as joined (a slide or legato from the note before). */
const JOIN_GAP = 0.15;

/** The views of one stem's notes, named in the chord frame. */
export function noteViews(data: StemNotes | null | undefined, f: ChordFrame, key: ChordsData["key"] | null | undefined): NoteView[] {
  if (!data) return [];
  const spMain = spellingFor(key ?? null, f.main);
  const spHeard = spellingFor(key ?? null, f.heard);
  return data.notes.map((n: NoteSpan, i) => {
    const main = noteOf(n.midi + f.main, spMain);
    const heard = noteOf(n.midi + f.heard, spHeard);
    const prev = data.notes[i - 1];
    const attack = n.attack === "legato" ? "legato" : n.attack === "slide" ? "slide" : "picked";
    return {
      start: n.start_s,
      end: n.end_s,
      midi: n.midi,
      main,
      second: heard !== main ? heard : null,
      octave: octaveName(n.midi + f.main, spMain),
      attack,
      bend: n.bend
        ? {
            start: n.bend.start_s,
            peak: n.bend.peak_s,
            release: n.bend.release_s ?? null,
            semitones: n.bend.semitones,
            label: bendLabel(n.bend.semitones),
          }
        : null,
      joinsPrev: attack !== "picked" && !!prev && n.start_s - prev.end_s <= JOIN_GAP,
    };
  });
}

/** A note the model heard outside the lead line (`StemNotes.others`). */
export interface HeardSpan {
  start_s: number;
  end_s: number;
  midi: number;
  conf: number;
}

/**
 * `StemNotes.others` as notes: per note `start end midi conf` separated by
 * spaces, notes separated by commas. Malformed notes are skipped.
 */
export function parseHeard(text: string | null | undefined): HeardSpan[] {
  if (!text) return [];
  const out: HeardSpan[] = [];
  for (const part of text.split(",")) {
    const f = part.trim().split(/\s+/).map(Number);
    if (f.length !== 4 || f.some((x) => !Number.isFinite(x)) || !(f[1] > f[0])) continue;
    out.push({ start_s: f[0], end_s: f[1], midi: Math.round(f[2]), conf: f[3] });
  }
  return out;
}

/** The text form of {@link parseHeard} (times to 3 decimals, confidence to 2), as the backend stores it. */
export function formatHeard(spans: HeardSpan[]): string {
  const n = (x: number, d: number) => String(Number(x.toFixed(d)));
  return spans.map((h) => `${n(h.start_s, 3)} ${n(h.end_s, 3)} ${h.midi} ${n(h.conf, 2)}`).join(",");
}

/** A note heard outside the lead line, named like a {@link NoteView}. */
export interface HeardView {
  start: number;
  end: number;
  /** Concert pitch (MIDI). */
  midi: number;
  main: string;
  second: string | null;
  octave: string;
}

/** The views of one stem's other notes heard, sorted by start (they may overlap). */
export function heardViews(data: StemNotes | null | undefined, f: ChordFrame, key: ChordsData["key"] | null | undefined): HeardView[] {
  if (!data?.others) return [];
  const spMain = spellingFor(key ?? null, f.main);
  const spHeard = spellingFor(key ?? null, f.heard);
  return parseHeard(data.others).map((h) => {
    const main = noteOf(h.midi + f.main, spMain);
    const heard = noteOf(h.midi + f.heard, spHeard);
    return {
      start: h.start_s,
      end: h.end_s,
      midi: h.midi,
      main,
      second: heard !== main ? heard : null,
      octave: octaveName(h.midi + f.main, spMain),
    };
  });
}

/** Name of `midi` (concert pitch) in the frame of `main`, e.g. the target of a bend. */
export function nameIn(midi: number, f: ChordFrame, key: ChordsData["key"] | null | undefined): string {
  return noteOf(midi + f.main, spellingFor(key ?? null, f.main));
}

/** Index of the note at time `t` (binary search), −1 between notes. */
export function noteIndexAt(views: { start: number; end: number }[], t: number): number {
  let lo = 0;
  let hi = views.length - 1;
  while (lo <= hi) {
    const m = (lo + hi) >> 1;
    if (t < views[m].start) hi = m - 1;
    else if (t >= views[m].end) lo = m + 1;
    else return m;
  }
  return -1;
}

/** Index of the first note ending after `t` (for drawing from a window's start). */
export function firstNoteFrom(views: { start: number; end: number }[], t: number): number {
  let lo = 0;
  let hi = views.length;
  while (lo < hi) {
    const m = (lo + hi) >> 1;
    if (views[m].end <= t) lo = m + 1;
    else hi = m;
  }
  return lo;
}

/** Other notes heard widen the roll only within this many semitones of the lead notes (or of the fallback range). */
export const HEARD_REACH = 12;

/**
 * The piano roll's pitch range (MIDI, inclusive) for the visible notes: a
 * semitone below the lowest, two above the highest (bend targets
 * included), at least `minSpan` semitones, grown evenly around the middle.
 * Visible other notes heard (`others`) count when they lie within
 * {@link HEARD_REACH} of the lead notes' range (of `fallback` when no lead
 * note is visible), so a few stray ones don't stretch it. `fallback` when
 * nothing is visible.
 */
export function rollRange(
  notes: { midi: number; bend?: { semitones: number } | null }[],
  fallback: { lo: number; hi: number },
  minSpan = 8,
  others: { midi: number }[] = [],
): { lo: number; hi: number } {
  let lo = Infinity;
  let hi = -Infinity;
  for (const n of notes) {
    const b = n.bend?.semitones ?? 0;
    lo = Math.min(lo, n.midi + Math.min(0, b));
    hi = Math.max(hi, n.midi + Math.max(0, b));
  }
  const near = notes.length ? { lo, hi } : fallback;
  for (const o of others) {
    if (o.midi < near.lo - HEARD_REACH || o.midi > near.hi + HEARD_REACH) continue;
    lo = Math.min(lo, o.midi);
    hi = Math.max(hi, o.midi);
  }
  if (lo > hi) return fallback;
  lo -= 1;
  hi += 2;
  const short = minSpan - (hi - lo);
  if (short > 0) {
    lo -= Math.floor(short / 2);
    hi += Math.ceil(short / 2);
  }
  return { lo, hi };
}

/** Pitch range shown when no notes are in view, per stem (MIDI). */
export function defaultRange(stem: StemName): { lo: number; hi: number } {
  if (stem === "bass") return { lo: 28, hi: 52 };
  if (stem === "guitar") return { lo: 52, hi: 76 };
  return { lo: 55, hi: 79 };
}

/** The loop between two notes (in either order): from the earlier start to the later end. */
export function loopBetween(a: { start: number; end: number }, b: { start: number; end: number }): { a: number; b: number } {
  return { a: Math.min(a.start, b.start), b: Math.max(a.end, b.end) };
}

/** "left", "center", "right" or "custom" for a stem whose Refine stereo mask is on; null when off. */
export function refinedSide(p: PanParams | undefined): string | null {
  if (!p?.enabled) return null;
  return quickOf(p) ?? "custom";
}

/**
 * Stem the panel shows: the remembered one when it has notes or is being
 * analyzed, else guitar if it has notes (or is pending), else bass, else
 * the first stem with any. `available` are the pitched stems of the loaded
 * set (with notes or pending).
 */
export function pickNotesStem(
  remembered: StemName | null | undefined,
  available: StemName[],
  hasNotes: (s: StemName) => boolean,
  pending: (s: StemName) => boolean,
): StemName | null {
  if (remembered && available.includes(remembered)) return remembered;
  if (available.includes("guitar") && (hasNotes("guitar") || pending("guitar"))) return "guitar";
  if (available.includes("bass")) return "bass";
  return available.find(hasNotes) ?? available[0] ?? null;
}

/** A stretch of the song in seconds. */
export interface Span {
  start: number;
  end: number;
}

/** The notes panel's two note settings (Fast runs, Quiet notes). */
export interface NoteSettings {
  fast: NotesFast;
  quiet: boolean;
}

/** A loop's own note settings, for the loop they were set on. */
export type LoopNoteSettings = NoteSettings & Span;

/** The same stretch (to the millisecond, as the backend keys loop results). */
export function sameSpan(a: Span | null | undefined, b: Span | null | undefined): boolean {
  if (!a || !b) return false;
  return Math.round(a.start * 1000) === Math.round(b.start * 1000) && Math.round(a.end * 1000) === Math.round(b.end * 1000);
}

/**
 * What the panel's note settings apply to and show: with a loop on, the loop
 * (its own settings when they were set on this loop, else the song's); with
 * none, the song. `own` is true when the loop's settings differ from the
 * song's, so its notes have to be found for it.
 */
export function panelNoteSettings(
  song: NoteSettings,
  loop: Span | null,
  own: LoopNoteSettings | null,
): NoteSettings & { scope: "loop" | "song"; own: boolean } {
  if (!loop) return { ...song, scope: "song", own: false };
  const s = own && sameSpan(own, loop) ? own : song;
  const differs = s.fast !== song.fast || s.quiet !== song.quiet;
  return { fast: s.fast, quiet: s.quiet, scope: "loop", own: differs };
}

/** The parts of `a`..`b` outside `range` (none, one or two). */
function outside(a: number, b: number, range: Span): [number, number][] {
  const out: [number, number][] = [];
  if (a < range.start) out.push([a, Math.min(b, range.start)]);
  if (b > range.end) out.push([Math.max(a, range.end), b]);
  return out.filter(([x, y]) => y > x);
}

/** `a`..`b` cut to `range`, or null when they don't overlap. */
function inside(a: number, b: number, range: Span): [number, number] | null {
  const x = Math.max(a, range.start);
  const y = Math.min(b, range.end);
  return y > x ? [x, y] : null;
}

/** A note moved to `start`..`end`; its bend stays only if it starts there (its rise cut at the end). */
function cutNote(n: NoteSpan, start: number, end: number): NoteSpan {
  if (start === n.start_s && end === n.end_s) return n;
  const out: NoteSpan = { ...n, start_s: start, end_s: end };
  const b = n.bend;
  if (b) {
    if (b.start_s < start || b.start_s >= end) delete out.bend;
    else out.bend = { ...b, peak_s: Math.min(b.peak_s, end), release_s: b.release_s != null && b.release_s < end ? b.release_s : null };
  }
  return out;
}

/**
 * What the notes panel shows for one stem while a loop has its own note
 * settings: the song's notes, chordal stretches and other notes heard
 * outside `range` (cut at its edges), and the loop's inside it (`loop`, as
 * `detect_loop_notes` returns it, already clipped). With `loop` null (its
 * notes are still being found) the loop stays empty. Without a range, the
 * song's notes as they are.
 */
export function withLoopNotes(song: StemNotes | null | undefined, loop: StemNotes | null, range: Span | null): StemNotes | undefined {
  if (!range) return song ?? undefined;
  if (!song && !loop) return undefined;
  const notes: NoteSpan[] = [];
  for (const n of song?.notes ?? []) for (const [a, b] of outside(n.start_s, n.end_s, range)) notes.push(cutNote(n, a, b));
  for (const n of loop?.notes ?? []) {
    const c = inside(n.start_s, n.end_s, range);
    if (c) notes.push(cutNote(n, c[0], c[1]));
  }
  notes.sort((a, b) => a.start_s - b.start_s);
  const chordal: [number, number][] = [];
  for (const [s, e] of song?.chordal ?? []) chordal.push(...outside(s, e, range));
  for (const [s, e] of loop?.chordal ?? []) {
    const c = inside(s, e, range);
    if (c) chordal.push(c);
  }
  chordal.sort((a, b) => a[0] - b[0]);
  const heard: HeardSpan[] = [];
  for (const h of parseHeard(song?.others)) for (const [a, b] of outside(h.start_s, h.end_s, range)) heard.push({ ...h, start_s: a, end_s: b });
  for (const h of parseHeard(loop?.others)) {
    const c = inside(h.start_s, h.end_s, range);
    if (c) heard.push({ ...h, start_s: c[0], end_s: c[1] });
  }
  heard.sort((a, b) => a.start_s - b.start_s);
  return {
    stem: (song ?? loop)!.stem,
    source: song?.source ?? loop!.source,
    notes,
    ...(chordal.length ? { chordal } : {}),
    ...(heard.length ? { others: formatHeard(heard) } : {}),
  };
}
