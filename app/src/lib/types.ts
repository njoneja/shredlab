// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

// Shared types for the UI <-> backend contract. See ./API.md for the
// authoritative description; field names are the exact JSON names.
import type { SavedTuning } from "./tuning";

export const STEMS = ["vocals", "guitar", "bass", "drums", "piano", "other"] as const;
export type StemName = (typeof STEMS)[number];

/** UI themes: Stage, Rosewood, Valve (dark) and Manuscript (light); the first is the default. */
export const THEMES = ["stage", "rosewood", "valve", "manuscript"] as const;
export type Theme = (typeof THEMES)[number];

/** "standard" = 1 pass, "high" = 2 passes (Demucs shifts=2). */
export type Quality = "standard" | "high";
/** How newly written stems are stored in the library. */
export type StemFormat = "mp3" | "flac";

export interface SeparatedWith {
  model_name: string; // e.g. "Demucs 6-stem"
  device_label: string; // e.g. "GPU" or "CPU"
  model_id: string; // e.g. "htdemucs_6s"
  quality: Quality;
}

export interface SongInfo {
  song_id: string;
  path: string;
  title: string;
  artist: string | null;
  duration_sec: number;
  sample_rate: number;
  channels: number;
  stems_cached: boolean;
  separated_with: SeparatedWith | null;
  beats_ready: boolean;
  bpm: number | null;
  settings: SongSettings | null;
  /** Stems and the sample-aligned original are loaded (backing track from the original). */
  original_available: boolean;
  /** Refine settings of the refined stems (others are unrefined). */
  refine: Partial<Record<StemName, RefineParams>>;
  /** Saved separation strength of the song. */
  strength: StrengthSetting;
  /** Strength job the backend started while opening (0 = none); its done/error event has this id. */
  strength_job: number;
  /** The song's Fast runs setting (stems on the note model; the bass ignores it). */
  notes_fast: NotesFast;
  /** Whether the song's softer notes take part in the lead line (stems on the note model; the bass ignores it). */
  notes_quiet: boolean;
  /** Measured tuning; null until the tuning job has run on the stems. */
  tuning: TuningData | null;
  /** Detected chords of the loaded stems; null until the chords job has run. */
  chords: ChordsData | null;
  /** Detected notes of the pitched stems for their current source; a stem is missing until its notes job has run. */
  notes: StemNotes[];
  /** Pitched stems whose notes for the current source are not in yet. */
  notes_pending: StemName[];
  /** The stem the notes job works on now. */
  notes_running: StemName | null;
}

// ---- Notes ----

/** How the notes job splits quick runs into notes: More splits fast runs but can chop held notes, Fewer keeps only clearly attacked notes. */
export type NotesFast = "fewer" | "normal" | "more";

/** A bend inside a note: when the pitch leaves the note, reaches the target and comes back. */
export interface BendSpan {
  start_s: number;
  peak_s: number;
  /** Missing: held to the end of the note. */
  release_s?: number | null;
  /** Size in semitones, positive up (1 = half step, 2 = whole step). */
  semitones: number;
}

/** One detected note, at concert pitch (MIDI number, before any bend). */
export interface NoteSpan {
  start_s: number;
  end_s: number;
  midi: number;
  /** Median deviation from `midi` in cents; missing = 0. */
  cents?: number;
  /** Confidence 0..1. */
  conf?: number;
  /** Missing = picked; "legato" = hammer-on (up) or pull-off (down); "slide" = slid into from the previous note. */
  attack?: "legato" | "slide" | null;
  bend?: BendSpan | null;
  /** Stored, not shown yet. */
  vibrato?: boolean;
}

/** The notes of one stem as computed from one source (stem set, strength, Refine stereo mask). */
export interface StemNotes {
  stem: StemName;
  source: string;
  /** Sorted by start, non-overlapping. */
  notes: NoteSpan[];
  /** Stretches [start, end] where the stem plays several notes at once (no notes inside); missing = none. */
  chordal?: [number, number][];
  /**
   * Other notes the model heard (not in the lead line), at their own length,
   * sorted by start, possibly overlapping: one string, per note
   * `start end midi conf` separated by spaces, notes separated by commas
   * (`parseHeard` in lib/notes.ts). Missing = none (and always for the bass).
   */
  others?: string;
}

/** `song.json` `notes`: entries per (stem, source), a few sources per stem. */
export interface NotesData {
  version: number;
  entries: StemNotes[];
}

export interface NotesReady {
  song_id: string;
  stem: StemName;
  source: string;
  notes: StemNotes;
}

export interface NotesStatus {
  song_id: string;
  pending: StemName[];
  running: StemName | null;
}

// ---- Chords ----

/** One stretch of the song with a single chord, at concert pitch (C = 0). */
export interface ChordSpan {
  start_s: number;
  end_s: number;
  /** Root pitch class; absent for no chord. */
  root?: number | null;
  /** Suffix after the root: "", "m", "5", "7", "maj7", "m7", "sus2", "sus4", "dim". */
  quality?: string;
  /** Bass pitch class of a slash chord (D/F#). */
  bass?: number | null;
  /** Suffix of the detailed name when it differs ("6", "m6", "add9", "9", "7sus4"). */
  detail?: string | null;
  /** Starts off the beat. */
  push?: boolean;
}

/** A short chord passing by inside a segment (a quick Bm in a D bar). */
export interface PassingSpan {
  start_s: number;
  end_s: number;
  root: number;
  quality?: string;
}

/** The song's detected chords (`song.json`). */
export interface ChordsData {
  /** Algorithm version; an older one is detected again on open. */
  version: number;
  /** Stem set they come from (`<model id>_s<shifts>`). */
  stems: string;
  /** Key estimate of the whole song. */
  key?: { tonic: number; minor: boolean } | null;
  /** Contiguous, in time order, covering the whole song. */
  segments: ChordSpan[];
  /** Passing chords in time order; missing = none. */
  passing?: PassingSpan[];
  /** Figures: the chord moving to an extension of itself (A to A6 and back), in time order; missing = none. */
  figures?: PassingSpan[];
}

export interface ChordsReady {
  song_id: string;
  chords: ChordsData;
}

// ---- Tuning ----

/** The song's measured tuning (`song.json`). */
export interface TuningData {
  /** Algorithm version; an older one is measured again on open. */
  version: number;
  /** Cents from the nearest A440 semitone, −50..50. */
  offset_cents: number;
  /** The A4 the offset implies, in Hz. */
  reference_hz: number;
  /** 0..1; the UI treats low values as "couldn't measure". */
  confidence: number;
  /** Spectral peaks the estimate used. */
  peaks: number;
  /** The tuning moves over the song. */
  drifts: boolean;
  /** Per-segment offsets (for the drift curve). */
  segments: { start_s: number; offset_cents: number; weight: number }[];
  /** Guess of the song's tuning from the lowest bass notes; only with a reliable offset. */
  guess?: { lowest_midi: number; below_standard: number; likely: boolean } | null;
}

export interface TuningReady {
  song_id: string;
  tuning: TuningData;
}

/** Reference tone sound: a plucked string or a steady drone. */
export type ToneSound = "pluck" | "drone";

// ---- Refine panel ----

/**
 * Stereo exception: a raised-cosine window over pan −1 (left) … +1 (right).
 * The window does the opposite of the row's on switch: on → the stem without
 * the window, off → only the window (heard at the row's volume).
 */
export interface PanParams {
  enabled: boolean;
  center: number; // −1 … +1
  width: number; // full width between the −6 dB points, 0 … 2
  softness: number; // edge width in pan units (0.2 default)
}

/** Low/high end of the EQ: a 24 dB/oct cut or a shelf (±12 dB). */
export type EndMode = "cut" | "shelf";
export interface EndParams {
  enabled: boolean;
  freq: number; // Hz (cut: −3 dB point; shelf: half-gain point)
  mode: EndMode;
  gain_db: number; // shelf only
}

export interface BandParams {
  enabled: boolean;
  freq: number;
  gain_db: number;
  q: number;
}

export interface EqParams {
  low_cut: EndParams;
  high_cut: EndParams;
  bands: [BandParams, BandParams, BandParams];
}

export interface RefineParams {
  pan: PanParams;
  eq: EqParams;
}

export interface RefinePreset {
  id: string;
  name: string;
  eq: EqParams;
}

/** `refine-field`: 3 bands × 41 pan bins (band-major, hard left first), linear power. */
export interface RefineFieldEvent {
  stem: StemName;
  bins: number[];
}

/** Separation strength: level 0 = Natural (cached stems) … 4 = Tight. */
export interface StrengthSetting {
  level: number;
  smooth: boolean;
}

export interface StrengthProgress {
  song_id: string;
  job: number;
  fraction: number;
}
export interface StrengthDone {
  song_id: string;
  job: number;
  strength: StrengthSetting;
}
export interface StrengthError {
  song_id: string;
  job: number;
  message: string;
  canceled: boolean;
}

export interface ModelInfo {
  id: string; // registry id, e.g. "htdemucs_6s"
  name: string;
  personal: boolean;
  license: string; // SPDX id, e.g. "MIT", or "unknown"
  stems: string[];
}

export interface AppSettingsInfo {
  models: ModelInfo[];
  model_id: string | null; // model the next separation uses (null: none installed)
  model_choice: string | null; // saved choice; null = default model
  model_from_env: boolean; // SHREDLAB_MODEL overrides the choice
  quality: Quality; // quality the next separation uses
  quality_choice: Quality | null; // saved choice; null = default for the device
  default_quality: Quality;
  device_label: string; // predicted device of model_id
  theme: Theme; // UI theme ("stage" by default)
  count_in_to_music_start: boolean; // "Count in to where the music starts" (on by default)
  click_volume: number; // click and count-in volume, linear 0..1 (0.8 by default)
  master_volume: number; // master playback volume, linear 0..1 (1 by default; not in exports)
  tuner_device_id: string | null; // live tuner input device (null = the system default)
  tuner_channel: number; // live tuner input channel, 0-based
  stem_format: StemFormat; // format of newly written stems ("mp3" by default)
}

/** null = use the default. */
export interface AppSettingsUpdate {
  model_id: string | null;
  quality: Quality | null;
}

export interface LibraryEntry {
  song_id: string;
  path: string;
  title: string;
  artist: string | null;
  duration_sec: number;
  stems_cached: boolean;
  last_opened_ms: number;
}

/** One song in the storage view (`list_library`). */
export interface StoredSongInfo {
  song_id: string;
  path: string;
  title: string;
  artist: string | null;
  size_bytes: number;
  last_opened_ms: number;
  source_exists: boolean;
  /** Removed from the recent list. */
  hidden: boolean;
  /** Currently open (cannot be deleted). */
  open: boolean;
}

export interface LibraryListing {
  dir: string;
  total_bytes: number;
  songs: StoredSongInfo[];
}

export type SeparationStage = "loading_model" | "decoding" | "separating" | "writing" | "beats";

export type SeparationStarted = SeparatedWith;

export interface SeparationProgress {
  song_id: string;
  stage: SeparationStage;
  fraction: number; // overall 0..1
  model_name: string;
  device_label: string;
}

export interface SeparationDone {
  song_id: string;
  separated_with: SeparatedWith;
  original_available: boolean;
}

export interface SeparationError {
  song_id: string;
  message: string;
  canceled: boolean;
}

export interface PlaybackPosition {
  position_sec: number;
  playing: boolean;
  count_in_beats_left: number;
}

export interface StemMixState {
  stem: StemName;
  on: boolean;
  volume: number; // linear gain 0..2 (the UI caps at +6 dB = 1.995)
  solo: boolean;
}

export interface LoopState {
  a_sec: number | null;
  b_sec: number | null;
  enabled: boolean;
}

export type Subdivision = 1 | 2 | 3;

export interface ClickState {
  enabled: boolean;
  count_in_enabled: boolean;
  count_in_beats: number;
  subdivision: Subdivision;
  beats_per_bar: number; // most common beats per bar (for count-in accents)
  volume: number; // linear gain 0..1
  count_in_each_repeat: boolean; // with a loop and the count-in on: count in again before each repeat
}

export type MeterOverride =
  | { kind: "beat_one"; beat_index: number }
  | { kind: "beats_per_bar"; beat_index: number; beats_per_bar: number };

export interface MeterOverridesPayload {
  overrides: MeterOverride[];
  downbeats: number[]; // effective downbeat times (subset of beats)
}

export interface Waveform {
  duration_sec: number;
  peaks: number[];
}

export interface BeatGrid {
  beats: number[];
  downbeats: number[];
  bpm: number | null;
  /** Where the music starts after a quiet intro (seconds), if the song has one. */
  music_start: number | null;
}

export type ExportFormat = "wav" | "flac" | "mp3";

export interface ExportRequest {
  path: string;
  format: ExportFormat;
  /**
   * The stems that play, each with its volume and on switch (`on` decides
   * which part a stem with a stereo exception plays; missing = true).
   */
  stems: { stem: StemName; volume: number; on?: boolean }[];
  /** The stems are the soloed ones: mixed as the plain sum (never from the original). */
  solo: boolean;
  speed_percent: number;
  semitones: number;
  cents: number;
  /** Mix in the click track (current click settings). Optional; missing = false. */
  click?: boolean;
  /** Start with the count-in (current count-in settings). Optional; missing = false. */
  count_in?: boolean;
  /** Range start (song seconds). Optional; missing = the song start. */
  start_sec?: number | null;
  /** Range end (song seconds). Optional; missing = the song end. */
  end_sec?: number | null;
  /** Playback-rate correction in cents (−200..200), as in playback. Optional; missing = 0. */
  rate_cents?: number;
}

export interface ExportProgress {
  fraction: number;
}
export interface ExportDone {
  path: string;
}
export interface ExportError {
  message: string;
  canceled: boolean;
}

/** Opaque to the backend: stored and returned verbatim. */
export interface SongSettings {
  version: 1;
  loop: LoopState;
  count_in_enabled: boolean;
  count_in_beats: number | null;
  count_in_each_repeat?: boolean; // missing = false
  subdivision: Subdivision;
  overrides: MeterOverride[];
  mix: { stem: StemName; on: boolean; volume: number }[];
  /** The song's tuning settings; missing = defaults (My tuning from the last one set). */
  tuning?: SongTuningSettings;
  /** Stem the notes panel shows; missing = the default (guitar if it has notes, else bass). */
  notes_stem?: StemName;
}

/** Per-song tuning settings, inside `SongSettings`. */
export interface SongTuningSettings {
  /** The player's tuning for this song (see lib/tuning.ts `SavedTuning`). */
  my_tuning: SavedTuning;
  /** Semitones the song sits below or above its written pitch ("Song is in"). */
  song_semitones: number;
  /** The active match, reapplied on open (recomputed from the current measurement). */
  match: { method: MatchMethod; cents: number } | null;
  /** The Guess button was pressed; it stops hinting. Missing = false. */
  guess_answered?: boolean;
  /**
   * "Song is in" was set by the user (chord names follow it only then).
   * Missing: true when `song_semitones` is not 0.
   */
  song_semitones_set?: boolean;
}

/** How a match shifts the song: through Pitch, or by changing the playback rate. */
export type MatchMethod = "pitch" | "rate";

/** An audio input device for the live tuner. */
export interface InputDevice {
  id: string;
  name: string;
  channels: number;
  default: boolean;
}

/** What `startTuner` opened. */
export interface TunerStarted {
  device_id: string;
  device_name: string;
  channels: number;
  /** 0-based. */
  channel: number;
  sample_rate: number;
  /** The requested device is gone; the default device is used (the saved choice is kept). */
  fell_back: boolean;
}

/** `tuner-reading`: about 30 per second while the tuner listens. */
export interface TunerReading {
  hz: number | null;
  clarity: number;
  /** Highest RMS level since the previous reading, dBFS (−120 = digital silence). */
  level_db: number;
}

export interface EventMap {
  "separation-progress": SeparationProgress;
  "separation-done": SeparationDone;
  "separation-error": SeparationError;
  "beats-ready": { song_id: string };
  "playback-position": PlaybackPosition;
  "export-progress": ExportProgress;
  "export-done": ExportDone;
  "export-error": ExportError;
  "refine-field": RefineFieldEvent;
  "strength-progress": StrengthProgress;
  "strength-done": StrengthDone;
  "strength-error": StrengthError;
  "tuning-ready": TuningReady;
  "chords-ready": ChordsReady;
  "notes-ready": NotesReady;
  "notes-status": NotesStatus;
  "tuner-reading": TunerReading;
  "tuner-error": { message: string };
  "tuner-stopped": { reason: "playback" | "song_closed" };
}
export type EventName = keyof EventMap;
export type Unlisten = () => void;

export interface Backend {
  readonly isMock: boolean;
  pickAudioFile(): Promise<string | null>;
  pickExportPath(defaultName: string, format: ExportFormat): Promise<string | null>;
  openSong(path: string): Promise<SongInfo>;
  closeSong(): Promise<void>;
  getLibrary(): Promise<LibraryEntry[]>;
  hideRecent(songId: string): Promise<void>;
  listLibrary(): Promise<LibraryListing>;
  /** Bytes stored for one song (what deleteSong would free). */
  songSize(songId: string): Promise<number>;
  /** Returns the bytes freed. */
  deleteSong(songId: string): Promise<number>;
  startSeparation(): Promise<SeparationStarted>;
  cancelSeparation(): Promise<void>;
  play(): Promise<void>;
  pause(): Promise<void>;
  seek(positionSec: number): Promise<void>;
  setMix(stems: StemMixState[]): Promise<void>;
  setSpeed(percent: number): Promise<void>;
  setPitch(semitones: number, cents: number): Promise<void>;
  setLoop(loop: LoopState): Promise<void>;
  setClick(click: ClickState): Promise<void>;
  setMeterOverrides(payload: MeterOverridesPayload): Promise<void>;
  getWaveform(nPoints: number): Promise<Waveform>;
  getBeats(): Promise<BeatGrid | null>;
  exportMix(req: ExportRequest): Promise<void>;
  cancelExport(): Promise<void>;
  saveSongSettings(songId: string, settings: SongSettings): Promise<void>;
  getAppSettings(): Promise<AppSettingsInfo>;
  setAppSettings(settings: AppSettingsUpdate): Promise<AppSettingsInfo>;
  setTheme(theme: Theme): Promise<AppSettingsInfo>;
  setCountInToMusicStart(on: boolean): Promise<AppSettingsInfo>;
  setStemFormat(format: StemFormat): Promise<AppSettingsInfo>;
  setClickVolume(volume: number): Promise<AppSettingsInfo>;
  setMasterVolume(volume: number, save: boolean): Promise<AppSettingsInfo>;
  getRefinePresets(): Promise<RefinePreset[]>;
  /** Live: heard at once; saved with the song shortly after the last change. */
  setRefine(stem: StemName, params: RefineParams): Promise<void>;
  /** Stereo-field analysis (`refine-field` events) for this stem, or off. */
  setRefineAnalysis(stem: StemName | null): Promise<void>;
  /** Saves the song's strength and re-derives the stems in the background; job 0 = nothing to do. */
  setStrength(strength: StrengthSetting): Promise<{ job: number }>;
  /** Saves the song's Fast runs and Quiet notes settings together; saved notes for them come at once (`notes-ready`), the rest are queued (`notes-status`). */
  setNotesOptions(fast: NotesFast, quiet: boolean): Promise<void>;
  /**
   * The notes of one stem (not the bass) in the loop `start`..`end` of the open song, found with the loop's
   * own settings; clipped to the loop, cached by the backend. Nothing is saved with the song. A newer call
   * can make an older one that has not started fail.
   */
  detectLoopNotes(start: number, end: number, fast: NotesFast, quiet: boolean, stem: StemName): Promise<StemNotes>;
  /** Playback-rate correction in cents (−200..200, 0 = off); reset to 0 by open_song. */
  setRateCorrection(cents: number): Promise<void>;
  /** Plays a reference tone (replaces any playing one); runs whether or not the song plays. */
  playTone(hz: number, sound: ToneSound): Promise<void>;
  stopTone(): Promise<void>;
  listInputDevices(): Promise<InputDevice[]>;
  /** Listens on `channel` (0-based) of `deviceId` (null = the default); pauses the song and stops the tone. */
  startTuner(deviceId: string | null, channel: number, lowHz: number): Promise<TunerStarted>;
  stopTuner(): Promise<void>;
  /** Paths dropped onto the window (Tauri drag-drop; HTML5 drop in the mock). */
  onFileDrop(cb: (paths: string[]) => void): Promise<Unlisten>;
  listen<E extends EventName>(event: E, cb: (payload: EventMap[E]) => void): Promise<Unlisten>;
}
