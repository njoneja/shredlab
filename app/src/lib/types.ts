// Shared types for the UI <-> backend contract. See ./API.md for the
// authoritative description; field names are the exact JSON names.

export const STEMS = ["vocals", "guitar", "bass", "drums", "piano", "other"] as const;
export type StemName = (typeof STEMS)[number];

/** UI themes (from the brand guide); the first is the default. */
export const THEMES = ["stage", "rosewood", "valve", "manuscript"] as const;
export type Theme = (typeof THEMES)[number];

/** "standard" = 1 pass, "high" = 2 passes (Demucs shifts=2). */
export type Quality = "standard" | "high";

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
  /** Stems and the sample-aligned original are loaded (backing track from the original, P1). */
  original_available: boolean;
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
  model_from_env: boolean; // STEMX_MODEL overrides the choice
  quality: Quality; // quality the next separation uses
  quality_choice: Quality | null; // saved choice; null = default for the device
  default_quality: Quality;
  device_label: string; // predicted device of model_id
  theme: Theme; // UI theme ("stage" by default)
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
  /** The stems are the soloed ones: mixed as the plain sum (never from the original). */
  solo: boolean;
  speed_percent: number;
  semitones: number;
  cents: number;
  /** Mix in the click track (current click settings). Optional; missing = false. */
  click?: boolean;
  /** Start with the count-in (current count-in settings). Optional; missing = false. */
  count_in?: boolean;
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
  getAppSettings(): Promise<AppSettingsInfo>;
  setAppSettings(settings: AppSettingsUpdate): Promise<AppSettingsInfo>;
  setTheme(theme: Theme): Promise<AppSettingsInfo>;
  /** Paths dropped onto the window (Tauri drag-drop; HTML5 drop in the mock). */
  onFileDrop(cb: (paths: string[]) => void): Promise<Unlisten>;
  listen<E extends EventName>(event: E, cb: (payload: EventMap[E]) => void): Promise<Unlisten>;
}
