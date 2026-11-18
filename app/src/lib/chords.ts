// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

// Chord names for the chord lane and panel. Pure functions, no state: the
// store passes the song's chords (concert pitch, from the backend) and its
// tuning state, and gets names and the panel's tuning line back.

import type { ChordSpan, ChordsData } from "./types";

const SHARPS = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
const FLATS = ["C", "Db", "D", "Eb", "E", "F", "Gb", "G", "Ab", "A", "Bb", "B"];
/** Spelling without a key: the names guitarists use most. */
const COMMON = ["C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B"];

/** Major keys spelled with flats (F, Bb, Eb, Ab, Db, Gb); minor keys use their relative major. */
const FLAT_MAJOR = new Set([5, 10, 3, 8, 1, 6]);

const pc = (n: number) => ((n % 12) + 12) % 12;

export type Spelling = "sharps" | "flats" | "common";

/** How to spell notes in a key `shift` semitones from the detected one. */
export function spellingFor(key: ChordsData["key"], shift: number): Spelling {
  if (!key) return "common";
  const major = pc(key.tonic + shift + (key.minor ? 3 : 0));
  if (major === 0) return "common";
  return FLAT_MAJOR.has(major) ? "flats" : "sharps";
}

export function noteOf(pitchClass: number, spelling: Spelling): string {
  const names = spelling === "flats" ? FLATS : spelling === "sharps" ? SHARPS : COMMON;
  return names[pc(pitchClass)];
}

/**
 * "E5", "D/F#", "C#m7"; null for no chord. `shift` transposes from concert
 * pitch; `detailed` uses the detailed quality ("A6") when there is one.
 */
export function chordName(s: ChordSpan, shift: number, spelling: Spelling, detailed = false): string | null {
  if (s.root == null) return null;
  let n = noteOf(s.root + shift, spelling) + ((detailed && s.detail) || s.quality || "");
  if (s.bass != null) n += "/" + noteOf(s.bass + shift, spelling);
  return n;
}

/** Shorter forms of a name, longest first, for narrow blocks: "D/F#" → "D", "C#m7" → "C#m". */
export function shortNames(name: string): string[] {
  const out = [name];
  const noSlash = name.split("/")[0];
  if (noSlash !== name) out.push(noSlash);
  const m = /^([A-G][#b]?)(m(?!aj))?/.exec(noSlash);
  if (m && m[0] !== noSlash) out.push(m[0]);
  return out;
}

/**
 * Which names the chords get. `main` is the shape you would fret on your
 * instrument in My tuning to play what you hear; `heard` is what you hear
 * (after any pitch shift). Both are semitones from concert pitch. They
 * differ only when My tuning isn't Standard.
 */
export interface ChordFrame {
  main: number;
  heard: number;
  /** "Song is in" was set by the user. */
  confirmed: boolean;
}

export function chordFrame(o: {
  /** "Song is in" (Eb standard = −1). Only for the tuning line. */
  songSemitones: number;
  confirmed: boolean;
  /** The song's measured offset from A440 in cents (0 when not measured). */
  offsetCents: number;
  /** Playback shift in cents: Pitch plus the tuning's rate correction. */
  shiftCents: number;
  /** My tuning in semitones from Standard at A440 (Eb standard = −1). */
  mySemitones?: number;
}): ChordFrame {
  // The chords are named at the song's own reference; what plays is that
  // plus the shift, rounded to the nearest A440 semitone. A shape X on an
  // instrument tuned m semitones from Standard sounds X + m.
  const heard = Math.round((o.offsetCents + o.shiftCents) / 100) || 0;
  return { main: heard - (o.mySemitones ?? 0) || 0, heard, confirmed: o.confirmed };
}

export interface ChordView {
  start: number;
  end: number;
  /** null: no chord (N.C.). */
  main: string | null;
  /** What you hear, when it differs from `main`. */
  second: string | null;
  push: boolean;
}

/** The chord views; `detailed` names them in detail (A6, Aadd9) where the detector found it. */
export function chordViews(data: ChordsData | null, f: ChordFrame, detailed = false): ChordView[] {
  if (!data) return [];
  const spMain = spellingFor(data.key, f.main);
  const spHeard = spellingFor(data.key, f.heard);
  return data.segments.map((s) => {
    const main = chordName(s, f.main, spMain, detailed);
    const heard = chordName(s, f.heard, spHeard, detailed);
    return { start: s.start_s, end: s.end_s, main, second: heard !== main ? heard : null, push: !!s.push };
  });
}

/** A passing chord or a figure, named like the chords. */
export interface PassingView {
  start: number;
  end: number;
  main: string;
  second: string | null;
  /** A figure (the chord moving to an extension of itself, A to A6), not a passing chord. */
  figure: boolean;
}

/** Passing chords and figures together, in time order. */
export function passingViews(data: ChordsData | null, f: ChordFrame): PassingView[] {
  if (!data?.passing?.length && !data?.figures?.length) return [];
  const spMain = spellingFor(data.key, f.main);
  const spHeard = spellingFor(data.key, f.heard);
  const view = (figure: boolean) => (p: NonNullable<ChordsData["passing"]>[number]): PassingView => {
    const span = { start_s: p.start_s, end_s: p.end_s, root: p.root, quality: p.quality };
    const main = chordName(span, f.main, spMain)!;
    const heard = chordName(span, f.heard, spHeard)!;
    return { start: p.start_s, end: p.end_s, main, second: heard !== main ? heard : null, figure };
  };
  return [...(data.passing ?? []).map(view(false)), ...(data.figures ?? []).map(view(true))].sort((a, b) => a.start - b.start);
}

/** Index of the chord at time `t` (binary search), −1 when none. */
export function chordIndexAt(views: ChordView[], t: number): number {
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

/** "Standard", "Eb", "D", "C#" (players say C# standard), "F". */
function tuneName(n: number): string {
  if (n === 0) return "Standard";
  if (n === -3) return "C#";
  return (n < 0 ? FLATS : SHARPS)[pc(4 + n)];
}

function steps(n: number): string {
  const a = Math.abs(n);
  return a === 1 ? "a half step" : a === 2 ? "a whole step" : `${a} semitones`;
}

/** The tuning line at the end of the CHORDS header. */
export interface TuningLine {
  /** A sample pair (shape, heard) shown before the text, or null. */
  legend: { main: string; heard: string } | null;
  text: string;
  /** The shapes won't match the player's instrument (amber). */
  warn: boolean;
  /** Offer "Set song to <guess>" with these semitones. */
  useGuess: { semitones: number; label: string } | null;
  /** Offer "Shift song to my tuning" and "I'm tuned …". */
  fixes: { tunedLabel: string } | null;
}

export function tuningLine(o: {
  frame: ChordFrame;
  songSemitones: number;
  /** My tuning in semitones from Standard (Eb standard = −1). */
  mySemitones: number;
  /** Playback shift in cents (Pitch plus the tuning's rate correction). */
  shiftCents: number;
  /** The best guess for "Song is in" (first candidate), when there is one. */
  guess: { semitones: number; label: string } | null;
  /**
   * Semitones the song would still have to move to match My tuning (after
   * the current shift); 0 = the song's usual shapes are what you fret.
   * Positive: the player's instrument sits higher than what plays.
   */
  mismatch: number;
  /** A sample chord (the first one) for the legend. */
  sample: ChordView | null;
}): TuningLine {
  const { frame: f, songSemitones: s } = o;
  const parts: string[] = [];
  let useGuess: TuningLine["useGuess"] = null;
  let fixes: TuningLine["fixes"] = null;
  const dir = (n: number) => `${n < 0 ? "down" : "up"} ${steps(n)}`;
  // What is known about the song's tuning: a guess, or "Song is in" (still
  // only what the user believes), and how to play its usual shapes.
  const shifted = Math.round(o.shiftCents / 100) || 0;
  if (!f.confirmed) {
    if (o.guess && o.guess.semitones !== 0) {
      parts.push(`Song may be in an alternate tuning (probably ${o.guess.label}).`);
      useGuess = o.guess;
    }
  } else if (o.mismatch !== 0 && shifted === 0) {
    // Once the song is shifted, the shift is the news (and the fixes stay).
    parts.push(`Song may be in ${tuneName(s)}. To play its usual shapes, tune ${dir(-o.mismatch)} or shift the song.`);
  }
  if (f.confirmed && o.mismatch !== 0) fixes = { tunedLabel: o.mismatch > 0 ? "I'm tuned down" : "I'm tuned up" };
  if (shifted !== 0) parts.push(`Song was shifted ${dir(shifted)}.`);
  let legend: TuningLine["legend"] = null;
  if (f.main !== f.heard) {
    legend = o.sample?.main && o.sample.second ? { main: o.sample.main, heard: o.sample.second } : null;
    parts.push(
      `Chords shifted to match your tuning shift (${dir(o.mySemitones)}). Smaller names show how they sound.`,
    );
  } else if (shifted !== 0) {
    parts.push("Chords match what you hear.");
  } else if (useGuess) {
    parts.push("Names will adjust if you set your tuning.");
  }
  return { legend, text: parts.join(" "), warn: false, useGuess, fixes };
}

/**
 * The stretch of song the chords panel shows: the loop (with a little
 * margin) when `fit` and a loop is on, otherwise `bars` bars around the
 * playhead, which sits a quarter of the way in.
 */
export function detailWindow(o: {
  position: number;
  duration: number;
  loop: { a_sec: number | null; b_sec: number | null; enabled: boolean };
  fit: boolean;
  bars: number;
  downbeats: number[];
}): { start: number; end: number; fitting: boolean } {
  const { a_sec: a, b_sec: b } = o.loop;
  if (o.fit && o.loop.enabled && a != null && b != null && b > a) {
    const pad = Math.min(0.5, (b - a) * 0.03);
    return { start: a - pad, end: b + pad, fitting: true };
  }
  const span = Math.min(o.duration, o.bars * barLength(o.downbeats));
  const start = Math.max(0, Math.min(o.duration - span, o.position - span * 0.25));
  return { start, end: start + span, fitting: false };
}

/**
 * Typical bar length of the song in seconds (median of all bars); 2 s
 * without a grid. Song-wide, so a few misdetected bars (short ones in an
 * intro) don't change the panel's width while it follows the playhead.
 */
export function barLength(downbeats: number[]): number {
  if (downbeats.length < 2) return 2;
  const d: number[] = [];
  for (let k = 1; k < downbeats.length; k++) d.push(downbeats[k] - downbeats[k - 1]);
  d.sort((x, y) => x - y);
  return d[d.length >> 1] || 2;
}

/**
 * Fill of a chord block: a subdued hue per root (in the frame the names
 * use), so the same chord has the same color in the waveform's lane and
 * the panel, and sections read as patterns. `panel` is a touch deeper, so
 * two lines of text stay readable on it.
 */
export function rootFill(root: number | null, shift: number, dark: boolean, panel = false): string {
  if (root == null) return "transparent";
  const hue = pc(root + shift) * 30;
  if (dark) return `hsl(${hue} 28% ${panel ? 24 : 30}%)`;
  return `hsl(${hue} 40% ${panel ? 88 : 82}%)`;
}

/** The current chord: the same hue as {@link rootFill}, more saturated and lifted, with a stronger edge. */
export function rootHighlight(root: number | null, shift: number, dark: boolean): { fill: string; edge: string } {
  if (root == null) return { fill: "transparent", edge: "transparent" };
  const hue = pc(root + shift) * 30;
  if (dark) return { fill: `hsl(${hue} 42% 38%)`, edge: `hsl(${hue} 55% 62%)` };
  return { fill: `hsl(${hue} 58% 72%)`, edge: `hsl(${hue} 50% 42%)` };
}

/** True for a dark color ("#rrggbb"). */
export function isDark(color: string): boolean {
  const m = /^#?([0-9a-f]{6})$/i.exec(color.trim());
  if (!m) return true;
  const n = parseInt(m[1], 16);
  return 0.299 * (n >> 16) + 0.587 * ((n >> 8) & 255) + 0.114 * (n & 255) < 128;
}
