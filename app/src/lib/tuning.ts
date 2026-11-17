// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

// Instrument tunings and the song-to-instrument match. Pure functions, no
// state: the store keeps the song's tuning and calls these. Cents are always
// relative to Standard at A440 unless a name says otherwise.

export type InstrumentId = "guitar6" | "guitar7" | "guitar8" | "bass4" | "bass5";
export type PresetId = "standard" | "dropD" | "dadgad" | "dropA" | "custom";

export interface Preset {
  id: PresetId;
  label: string;
  /** MIDI notes, lowest string first. */
  strings: number[];
}

export interface Instrument {
  id: InstrumentId;
  label: string;
  presets: Preset[];
}

/** The player's tuning for a song (saved per song). */
export interface MyTuning {
  instrument: InstrumentId;
  preset: PresetId;
  /** MIDI notes, lowest first; used when `preset` is "custom". */
  custom: number[] | null;
  /** Whole semitones, −12..12. */
  shift: number;
  /** A4 in Hz, 400..480. */
  refHz: number;
}

export const INSTRUMENTS: Instrument[] = [
  {
    id: "guitar6",
    label: "Guitar, 6 strings",
    presets: [
      { id: "standard", label: "Standard", strings: [40, 45, 50, 55, 59, 64] },
      { id: "dropD", label: "Drop D", strings: [38, 45, 50, 55, 59, 64] },
      { id: "dadgad", label: "DADGAD", strings: [38, 45, 50, 55, 57, 62] },
    ],
  },
  {
    id: "guitar7",
    label: "Guitar, 7 strings",
    presets: [
      { id: "standard", label: "Standard", strings: [35, 40, 45, 50, 55, 59, 64] },
      { id: "dropA", label: "Drop A", strings: [33, 40, 45, 50, 55, 59, 64] },
    ],
  },
  {
    id: "guitar8",
    label: "Guitar, 8 strings",
    presets: [{ id: "standard", label: "Standard", strings: [30, 35, 40, 45, 50, 55, 59, 64] }],
  },
  {
    id: "bass4",
    label: "Bass, 4 strings",
    presets: [
      { id: "standard", label: "Standard", strings: [28, 33, 38, 43] },
      { id: "dropD", label: "Drop D", strings: [26, 33, 38, 43] },
    ],
  },
  {
    id: "bass5",
    label: "Bass, 5 strings",
    presets: [{ id: "standard", label: "Standard", strings: [23, 28, 33, 38, 43] }],
  },
];

export const DEFAULT_TUNING: MyTuning = {
  instrument: "guitar6",
  preset: "standard",
  custom: null,
  shift: 0,
  refHz: 440,
};

export const SHIFT_MAX = 12;
export const REF_MIN = 400;
export const REF_MAX = 480;
/** Within this many cents of A440 a song counts as in tune. */
export const IN_TUNE_CENTS = 3;
/** Below this confidence (agreement between 5 s blocks), or with fewer peaks, the song couldn't be measured. */
export const MIN_CONFIDENCE = 0.7;
export const MIN_PEAKS = 200;
/** The playback-rate method is offered up to this shift. */
export const RATE_MAX_CENTS = 200;
/** Pitch's own limits (st and ¢). */
const PITCH_SEMI_MAX = 12;

export function instrument(id: InstrumentId): Instrument {
  return INSTRUMENTS.find((i) => i.id === id) ?? INSTRUMENTS[0];
}

/** A tuning with every field in range and a preset that exists for its instrument. */
export function sanitize(t: Partial<MyTuning> | null | undefined): MyTuning {
  const inst = instrument((t?.instrument ?? DEFAULT_TUNING.instrument) as InstrumentId);
  let preset = (t?.preset ?? "standard") as PresetId;
  let custom = Array.isArray(t?.custom) ? t.custom.filter((n) => Number.isFinite(n)).map(Math.round) : null;
  if (preset === "custom") {
    if (!custom || custom.length === 0) {
      preset = "standard";
      custom = null;
    }
  } else if (!inst.presets.some((p) => p.id === preset)) {
    preset = "standard";
  }
  const shift = Number.isFinite(t?.shift) ? Math.round(t!.shift!) : 0;
  const refHz = Number.isFinite(t?.refHz) ? Math.round(t!.refHz! * 10) / 10 : 440;
  return {
    instrument: inst.id,
    preset,
    custom,
    shift: Math.max(-SHIFT_MAX, Math.min(SHIFT_MAX, shift)),
    refHz: Math.max(REF_MIN, Math.min(REF_MAX, refHz)),
  };
}

/** The tuning's strings as MIDI notes before the shift, lowest first. */
export function baseStrings(t: MyTuning): number[] {
  if (t.preset === "custom" && t.custom?.length) return t.custom;
  const inst = instrument(t.instrument);
  return (inst.presets.find((p) => p.id === t.preset) ?? inst.presets[0]).strings;
}

/** Cents of an A4 reference relative to 440 Hz. */
export function refCents(refHz: number): number {
  return 1200 * Math.log2(refHz / 440);
}

/** The tuning's total offset from Standard at A440, in cents. */
export function totalCents(t: MyTuning): number {
  return 100 * t.shift + refCents(t.refHz);
}

/** Target frequency of a (shifted) MIDI note in this tuning's reference. */
export function noteHz(midi: number, refHz: number): number {
  return refHz * Math.pow(2, (midi - 69) / 12);
}

const SHARPS = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
const FLATS = ["C", "Db", "D", "Eb", "E", "F", "Gb", "G", "Ab", "A", "Bb", "B"];

/** Note name without octave; flats when `flats`. */
export function noteName(midi: number, flats = false): string {
  const pc = ((Math.round(midi) % 12) + 12) % 12;
  return (flats ? FLATS : SHARPS)[pc];
}

/** Scientific pitch name, for example "Eb2". */
export function noteLabel(midi: number, flats = false): string {
  return `${noteName(midi, flats)}${Math.floor(Math.round(midi) / 12) - 1}`;
}

export interface TuningString {
  /** 1 = the highest string. */
  number: number;
  midi: number;
  name: string;
  label: string;
  hz: number;
}

/** The strings to tune to, highest number (lowest string) first. */
export function strings(t: MyTuning): TuningString[] {
  const base = baseStrings(t);
  const flats = t.shift < 0;
  return base.map((m, i) => {
    const midi = m + t.shift;
    return {
      number: base.length - i,
      midi,
      name: noteName(midi, flats),
      label: noteLabel(midi, flats),
      hz: noteHz(midi, t.refHz),
    };
  });
}

/** "440", "435.5": A4 without a trailing ".0". */
export function formatHz(hz: number): string {
  const r = Math.round(hz * 10) / 10;
  return Number.isInteger(r) ? String(r) : r.toFixed(1);
}

/** Signed whole cents: "+18¢", "−7¢", "0¢". */
export function formatCents(c: number): string {
  const r = Math.round(c);
  if (r === 0) return "0¢";
  return `${r > 0 ? "+" : "−"}${Math.abs(r)}¢`;
}

/** Name without the reference: "Standard", "Eb standard", "Drop C", "DADGAD −1". */
export function tuningBaseName(t: MyTuning): string {
  const flats = t.shift < 0;
  const base = baseStrings(t);
  if (t.preset === "standard") {
    // Named after the guitar's E (Eb standard, D standard), for every instrument.
    return t.shift === 0 ? "Standard" : `${noteName(64 + t.shift, flats)} standard`;
  }
  if (t.preset === "dropD" || t.preset === "dropA") return `Drop ${noteName(base[0] + t.shift, flats)}`;
  const label = t.preset === "dadgad" ? "DADGAD" : "Custom";
  return t.shift === 0 ? label : `${label} ${t.shift > 0 ? "+" : "−"}${Math.abs(t.shift)}`;
}

/** Display name: "Standard", "Eb standard, A430", "Drop C", "DADGAD −1". */
export function tuningName(t: MyTuning): string {
  const name = tuningBaseName(t);
  return t.refHz === 440 ? name : `${name}, A${formatHz(t.refHz)}`;
}

/** Name with the reference always stated: "Standard at A440", "Eb standard at A430". */
export function tuningLongName(t: MyTuning): string {
  return `${tuningBaseName(t)} at A${formatHz(t.refHz)}`;
}

/** "140 cents below Standard at A440", "at Standard, A440" for no offset. */
export function offsetText(t: MyTuning): string {
  const c = Math.round(totalCents(t));
  if (c === 0) return "Standard at A440";
  return `${Math.abs(c)} ${Math.abs(c) === 1 ? "cent" : "cents"} ${c < 0 ? "below" : "above"} Standard at A440`;
}

/** A tuning as saved in the song's settings (snake_case, like the rest of the blob). */
export interface SavedTuning {
  instrument: InstrumentId;
  preset: PresetId;
  custom: number[] | null;
  shift: number;
  ref_hz: number;
}

export function toSaved(t: MyTuning): SavedTuning {
  return { instrument: t.instrument, preset: t.preset, custom: t.custom ? [...t.custom] : null, shift: t.shift, ref_hz: t.refHz };
}

/** Reads a saved tuning; anything missing or out of range gets its default. */
export function fromSaved(s: Partial<SavedTuning> | null | undefined): MyTuning {
  if (!s || typeof s !== "object") return { ...DEFAULT_TUNING };
  return sanitize({ instrument: s.instrument, preset: s.preset, custom: s.custom, shift: s.shift, refHz: s.ref_hz });
}

/** Frequency a reference tone plays for a string (1 = highest), an octave up if asked. */
export function toneHz(t: MyTuning, stringNumber: number, octaveUp: boolean): number | null {
  const s = strings(t).find((x) => x.number === stringNumber);
  return s ? s.hz * (octaveUp ? 2 : 1) : null;
}

/** MIDI notes the custom-string editor offers: B0 (23) to E5 (76). */
export const CUSTOM_NOTE_MIN = 23;
export const CUSTOM_NOTE_MAX = 76;

/** Segments in the running median over segment offsets, as the backend's drift test uses. */
export const DRIFT_SMOOTHING = 5;

/** Running median over `width` values (odd); windows are cut short at the ends. */
export function medianSmooth(v: number[], width = DRIFT_SMOOTHING): number[] {
  const h = Math.floor(width / 2);
  return v.map((_, i) => {
    const w = v.slice(Math.max(0, i - h), i + h + 1).sort((a, b) => a - b);
    const m = w.length >> 1;
    return w.length % 2 ? w[m] : (w[m - 1] + w[m]) / 2;
  });
}

export interface TuningSegment {
  start_s: number;
  offset_cents: number;
  weight: number;
}

/**
 * How a drifting song moves: smoothed first and last offsets when the drift
 * mostly goes one way ("from −22¢ to −9¢"), else the smoothed range
 * ("between −22¢ and −9¢"). Null without segments.
 */
export function driftSummary(segments: TuningSegment[]): { kind: "from" | "between"; a: number; b: number } | null {
  if (!segments.length) return null;
  const m = medianSmooth(segments.map((s) => s.offset_cents));
  const lo = Math.min(...m);
  const hi = Math.max(...m);
  const first = m[0];
  const last = m[m.length - 1];
  if (hi - lo < 0.5 || Math.abs(last - first) >= 0.6 * (hi - lo)) return { kind: "from", a: Math.round(first), b: Math.round(last) };
  return { kind: "between", a: Math.round(lo), b: Math.round(hi) };
}

/** Name for the song's own semitone setting ("Song is in"). */
export function songTuningName(semitones: number): string {
  if (semitones === 0) return "Standard";
  const n = `${noteName(64 + semitones, semitones < 0)} standard`;
  return `${n} (${semitones > 0 ? "+" : "−"}${Math.abs(semitones)})`;
}

/** The shift that makes the song match `t`, in whole cents. */
export function matchCents(t: MyTuning, songSemitones: number, offsetCents: number): number {
  return Math.round(totalCents(t) - (100 * songSemitones + offsetCents));
}

/** Splits a shift into Pitch semitones and cents (−50..50); null if out of Pitch's range. */
export function splitPitch(cents: number): { semitones: number; cents: number } | null {
  const c = Math.round(cents);
  // Nearest semitone, exact halves toward zero (150 → 1 st +50¢), symmetric in sign.
  const semitones = Math.sign(c) * Math.floor((Math.abs(c) + 49) / 100);
  const rest = c - 100 * semitones;
  if (Math.abs(semitones) > PITCH_SEMI_MAX) return null;
  return { semitones: semitones === 0 ? 0 : semitones, cents: rest === 0 ? 0 : rest };
}

/** "0 st +18¢", "−1 st −22¢". */
export function formatPitch(p: { semitones: number; cents: number }): string {
  const st = p.semitones === 0 ? "0" : `${p.semitones > 0 ? "+" : "−"}${Math.abs(p.semitones)}`;
  return `${st} st ${formatCents(p.cents)}`;
}

/** Tempo change of the playback-rate method, in percent (+1.0 for +18¢). */
export function rateTempoPercent(cents: number): number {
  return (Math.pow(2, cents / 1200) - 1) * 100;
}

/** The A4 a measured offset implies, rounded to 0.1 Hz. */
export function songRefHz(offsetCents: number): number {
  return Math.round(440 * Math.pow(2, offsetCents / 1200) * 10) / 10;
}

export type SongTuningState = "measuring" | "in-tune" | "off" | "unmeasured";

export interface MeasuredTuning {
  offset_cents: number;
  reference_hz: number;
  confidence: number;
  peaks: number;
  drifts: boolean;
}

/** What the Song card and the Tuning button show. */
export function songState(m: MeasuredTuning | null, measuring: boolean): SongTuningState {
  if (!m) return measuring ? "measuring" : "unmeasured";
  if (m.confidence < MIN_CONFIDENCE || m.peaks < MIN_PEAKS) return "unmeasured";
  if (Math.abs(m.offset_cents) <= IN_TUNE_CENTS && !m.drifts) return "in-tune";
  return "off";
}
