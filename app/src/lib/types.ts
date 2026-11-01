// Shared types for the UI <-> backend contract. See ./API.md for the
// authoritative description; field names are the exact JSON names.

export const STEMS = ["vocals", "guitar", "bass", "drums", "piano", "other"] as const;
export type StemName = (typeof STEMS)[number];

export interface SeparatedWith {
  model_name: string; // e.g. "Demucs 6-stem"
  device_label: string; // e.g. "GPU" or "CPU"
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

export type SeparationStage = "loading_model" | "decoding" | "separating" | "writing" | "beats";

export interface SeparationStarted {
  model_name: string;
  device_label: string;
}

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
}

export interface SeparationError {
  song_id: string;
  message: string;
  cancelled: boolean;
}

export interface PlaybackPosition {
  position_sec: number;
  playing: boolean;
  count_in_beats_left: number;
}

export interface StemMixState {
  stem: StemName;
  on: boolean;
  volume: number; // linear gain 0..1
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
}

export type ExportFormat = "wav" | "flac" | "mp3";

export interface ExportRequest {
  path: string;
  format: ExportFormat;
  stems: { stem: StemName; volume: number }[];
  speed_percent: number;
  semitones: number;
  cents: number;
}

export interface ExportProgress {
  fraction: number;
}
export interface ExportDone {
  path: string;
}
export interface ExportError {
  message: string;
  cancelled: boolean;
}

/** Opaque to the backend: stored and returned verbatim. */
export interface SongSettings {
  version: 1;
  loop: LoopState;
  count_in_enabled: boolean;
  count_in_beats: number | null;
  subdivision: Subdivision;
  overrides: MeterOverride[];
  mix: { stem: StemName; on: boolean; volume: number }[];
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
  /** Paths dropped onto the window (Tauri drag-drop; HTML5 drop in the mock). */
  onFileDrop(cb: (paths: string[]) => void): Promise<Unlisten>;
  listen<E extends EventName>(event: E, cb: (payload: EventMap[E]) => void): Promise<Unlisten>;
}
