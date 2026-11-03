// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

// App state (Svelte 5 runes). All backend calls go through `api`.
import { api } from "./api";
import type {
  AppSettingsInfo,
  AppSettingsUpdate,
  BeatGrid,
  ClickState,
  ExportFormat,
  LibraryEntry,
  LoopState,
  MeterOverride,
  Quality,
  RefineParams,
  RefinePreset,
  SeparationStage,
  SongInfo,
  SongSettings,
  StemMixState,
  StemFormat,
  StemName,
  StrengthSetting,
  Subdivision,
  Theme,
} from "./types";
import { STEMS } from "./types";
import { backingFromOriginal, clampGain } from "./mix";
import { applyTheme, currentTheme, isTheme } from "./theme";
import { defaultRefine, exceptionSide, hasException, isIdentity, mockPresets, normalize, STRENGTH_NAMES } from "./refine";
import { barAt, commonBeatsPerBar, effectiveDownbeats, formatTime, nearestBeatIndex, normalizeOverrides } from "./meter";

export interface StemUi {
  on: boolean;
  volume: number;
  solo: boolean;
}

export const STAGE_TEXT: Record<SeparationStage, string> = {
  loading_model: "Loading model…",
  decoding: "Decoding audio…",
  separating: "Separating stems…",
  writing: "Saving stems…",
  beats: "Detecting beats…",
};

export const QUALITY_TEXT: Record<Quality, string> = { standard: "Standard", high: "High" };

export const SPEED_MIN = 50;
export const SPEED_MAX = 150;
export const SEMI_MAX = 12;
export const CENTS_MAX = 50;
const MIN_LOOP = 0.05;

/** What the export covers: the whole song, playhead → end, or the A–B loop once. */
export type ExportRange = "song" | "playhead" | "loop";
/** Sections of the help dialog that other panels can link to. */
export type HelpSection = "practice" | "play-along" | "click" | "keys";

function defaultRefineAll(): Record<StemName, RefineParams> {
  return Object.fromEntries(STEMS.map((s) => [s, defaultRefine()])) as Record<StemName, RefineParams>;
}

function defaultMix(): Record<StemName, StemUi> {
  return Object.fromEntries(STEMS.map((s) => [s, { on: true, volume: 1, solo: false }])) as Record<StemName, StemUi>;
}

const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

class AppState {
  song = $state<SongInfo | null>(null);
  library = $state<LibraryEntry[]>([]);
  opening = $state(false);
  error = $state<string | null>(null);
  toast = $state<string | null>(null);

  // separation
  separating = $state(false);
  sepStage = $state<SeparationStage>("loading_model");
  sepFraction = $state(0);
  sepModel = $state("");
  sepDevice = $state("");
  sepModelId = $state("");
  sepQuality = $state<Quality | null>(null);
  sepError = $state<string | null>(null);
  stemsReady = $state(false);

  // app settings (model, quality); persisted by the backend
  appSettings = $state<AppSettingsInfo | null>(null);
  /** Master playback volume, linear 0..1 (app setting; not in exports). */
  masterVolume = $state(1);
  savingSettings = $state(false);
  /** UI theme; applied to <html> before first paint from a localStorage mirror. */
  theme = $state<Theme>(currentTheme());
  /** Set when the user changed a setting; the prompt shows while the loaded stems don't match. */
  askReseparate = $state(false);

  // analysis
  peaks = $state<number[] | null>(null);
  grid = $state.raw<BeatGrid | null>(null);
  overrides = $state<MeterOverride[]>([]);

  // playback
  position = $state(0);
  playing = $state(false);
  countInLeft = $state(0);

  mix = $state<Record<StemName, StemUi>>(defaultMix());
  speed = $state(100);
  semitones = $state(0);
  cents = $state(0);
  loop = $state<LoopState>({ a_sec: null, b_sec: null, enabled: false });
  click = $state<ClickState>({
    enabled: false,
    count_in_enabled: false,
    count_in_beats: 4,
    subdivision: 1,
    beats_per_bar: 4,
    volume: 0.8,
    count_in_each_repeat: false,
  });
  countInSet = false; // count_in_beats came from saved settings

  // help dialog (ShortcutsHelp.svelte): open, and the section to scroll to
  helpOpen = $state(false);
  helpSection = $state<HelpSection | null>(null);
  // About dialog (AboutDialog.svelte)
  aboutOpen = $state(false);
  // Manage storage dialog (StorageDialog.svelte)
  storageOpen = $state(false);
  // "Remove from recent?" confirmation (RemoveRecentDialog.svelte): the song, and a callback
  // the opener uses to put focus back (removed = the song left the list).
  removeAsk = $state<{ entry: LibraryEntry; done?: (removed: boolean) => void } | null>(null);

  // export
  exportOpen = $state(false);
  exportFormat = $state<ExportFormat>("wav");
  exporting = $state(false);
  exportFraction = $state(0);
  exportDonePath = $state<string | null>(null);
  exportError = $state<string | null>(null);
  /** The user's range pick in the dialog (null = the default, re-chosen each time it opens). */
  exportRangePick = $state<ExportRange | null>(null);

  // refine
  refine = $state<Record<StemName, RefineParams>>(defaultRefineAll());
  /** Stem whose Refine panel is open (one at a time). */
  refineOpen = $state<StemName | null>(null);
  refinePresets = $state.raw<RefinePreset[]>(mockPresets());
  /** Latest stereo field of the open panel's stem (3 × 41 linear powers). */
  field = $state.raw<number[] | null>(null);

  // strength
  strength = $state<StrengthSetting>({ level: 0, smooth: false });
  strengthBusy = $state(false);
  strengthFraction = $state(0);
  strengthError = $state<string | null>(null);
  /** Newest job the UI waits for, and the newest job that finished (any song). */
  private strengthWait = 0;
  private strengthDoneJob = 0;

  downbeats = $derived(effectiveDownbeats(this.grid, this.overrides));
  beatsPerBar = $derived(commonBeatsPerBar(this.grid, this.downbeats));
  anySolo = $derived(STEMS.some((s) => this.mix[s].solo));
  hasLoop = $derived(this.loop.a_sec != null && this.loop.b_sec != null);
  /** "Count in to where the music starts" (app setting, on by default). */
  countInToMusicStart = $derived(this.appSettings?.count_in_to_music_start ?? true);
  /** Where the music starts after a quiet intro, if the song has one. */
  musicStart = $derived(this.grid?.music_start ?? null);
  /** "From the playhead" makes sense only before the end. */
  canExportFromPlayhead = $derived(!!this.song && this.position < this.song.duration_sec - 0.1);
  /** Effective range: the pick if still possible, else Loop A–B when a loop is set, else Whole song. */
  exportRange = $derived.by((): ExportRange => {
    const p = this.exportRangePick;
    if (p === "loop" && this.hasLoop) return p;
    if (p === "playhead" && this.canExportFromPlayhead) return p;
    if (p === "song") return p;
    return this.hasLoop ? "loop" : "song";
  });
  /** The chosen range in song seconds, before the music-start rule. */
  exportSpan = $derived.by(() => {
    const dur = this.song?.duration_sec ?? 0;
    if (this.exportRange === "loop" && this.loop.a_sec != null && this.loop.b_sec != null) return { start: this.loop.a_sec, end: this.loop.b_sec };
    if (this.exportRange === "playhead") return { start: this.position, end: dur };
    return { start: 0, end: dur };
  });
  /**
   * Where the export really starts when "Count in to where the music starts"
   * applies (count-in on, range starting at the song start, music start
   * inside the range); null otherwise. Same rule as the backend.
   */
  exportMusicStart = $derived.by(() => {
    const ms = this.musicStart;
    const { start, end } = this.exportSpan;
    if (!this.click.count_in_enabled || !this.countInToMusicStart || ms == null) return null;
    const first = this.grid?.beats[0];
    const atStart = start <= 0.005 || (first != null && start < first - 0.005);
    return atStart && ms > start && ms < end ? ms : null;
  });
  /**
   * Stems the export mixes: exactly what is audible. With a solo, the soloed
   * stems (always the plain sum); otherwise the switched-on ones (on at −∞
   * still counts as on for the backing track) plus off ones with a stereo exception (they
   * play the excepted part).
   */
  exportStems = $derived(STEMS.filter((s) => this.audible(s)));
  /** Click and count-in go into the export when they are on ("what you hear"). */
  exportExtras = $derived(
    [this.click.enabled && "click", this.click.count_in_enabled && "count-in"].filter(Boolean).join(", "),
  );
  /** Short range note for the summary ("" for the whole song). */
  exportRangeNote = $derived.by(() => {
    const { start, end } = this.exportSpan;
    if (this.exportRange === "loop") return ` · loop ${formatTime(start)}–${formatTime(end)}`;
    if (this.exportRange === "playhead" && start > 0.05) return ` · from ${formatTime(start)}`;
    return "";
  });
  /** Refined stems (a dot on their row; mentioned in the export summary). */
  refinedStems = $derived(STEMS.filter((s) => !isIdentity(this.refine[s])));
  /** " · guitar refined" when refined stems are heard in the export. */
  exportRefineNote = $derived.by(() => {
    // A stem that plays nothing is simply left out (its refine does nothing).
    const r = this.refinedStems.filter((s) => this.exportStems.includes(s));
    return r.length ? ` · ${r.join(", ")} refined` : "";
  });
  exportSummary = $derived.by(() => {
    const on = this.exportStems;
    const extras = (this.exportExtras ? ` + ${this.exportExtras}` : "") + this.exportRangeNote + this.exportRefineNote;
    // An off row with an exception plays only that part: "guitar (left only)".
    const name = (s: StemName) =>
      !this.mix[s].on && hasException(this.refine[s]) ? `${s} (${exceptionSide(this.refine[s].pan)} only)` : s;
    if (this.anySolo) return `Exports: ${on.map(name).join(", ")} (soloed)${extras}`;
    const off = STEMS.filter((s) => !on.includes(s));
    if (!on.length) return "Nothing to export: all stems are off";
    if (!off.length && on.every((s) => this.mix[s].on)) return `Exports: all stems${extras}`;
    return `Exports: ${on.map(name).join(", ")}${off.length ? ` (${off.join(", ")} off)` : ""}${extras}`;
  });
  /** Number of stems the loaded song has (from its model; the mixer rows otherwise). */
  songStemCount = $derived.by(() => {
    const id = this.song?.separated_with?.model_id;
    const m = id ? this.appSettings?.models.find((x) => x.id === id) : undefined;
    return m?.stems.length || STEMS.length;
  });
  exportFromOriginal = $derived(
    // Like playback: only switched-on rows count (an off row with an exception doesn't).
    !this.anySolo &&
      backingFromOriginal(!!this.song?.original_available, STEMS.filter((s) => this.mix[s].on).length, this.songStemCount),
  );
  /** Every stem on and nothing soloed ("All on" would change nothing). */
  allOnAlready = $derived(STEMS.every((s) => this.mix[s].on && !this.mix[s].solo));
  /** Loaded stems were made with a different model/quality than the current settings. */
  settingsMismatch = $derived.by(() => {
    const sw = this.song?.separated_with;
    const st = this.appSettings;
    if (!sw || !st || st.model_id == null) return false;
    return sw.model_id !== st.model_id || sw.quality !== st.quality;
  });
  showReseparate = $derived(this.askReseparate && this.settingsMismatch && !this.separating && this.stemsReady);

  private started = false;

  /**
   * The row plays something: with a solo, the soloed rows; otherwise rows
   * that are on, and off rows with a stereo exception (they play that part).
   */
  audible(stem: StemName): boolean {
    const m = this.mix[stem];
    return this.anySolo ? m.solo : m.on || hasException(this.refine[stem]);
  }

  /** Off, but playing the excepted part (drawn half dimmed, switch half lit). */
  partlyOn(stem: StemName): boolean {
    return this.audible(stem) && !this.mix[stem].on && !this.mix[stem].solo;
  }

  async init() {
    if (this.started) return;
    this.started = true;
    await api.listen("playback-position", (p) => {
      this.position = p.position_sec;
      this.playing = p.playing;
      this.countInLeft = p.count_in_beats_left;
    });
    await api.listen("separation-progress", (p) => {
      if (p.song_id !== this.song?.song_id) return;
      this.separating = true;
      this.sepStage = p.stage;
      this.sepFraction = p.fraction;
      this.sepModel = p.model_name;
      this.sepDevice = p.device_label;
    });
    await api.listen("separation-done", (p) => {
      if (p.song_id !== this.song?.song_id) return;
      this.separating = false;
      this.stemsReady = true;
      this.song = { ...this.song, stems_cached: true, separated_with: p.separated_with, original_available: p.original_available };
      this.showToast("Stems ready");
      this.refreshLibrary();
    });
    await api.listen("separation-error", (p) => {
      if (p.song_id !== this.song?.song_id) return;
      this.separating = false;
      this.sepError = p.canceled ? "Separation canceled." : p.message;
    });
    await api.listen("beats-ready", (p) => {
      if (p.song_id === this.song?.song_id) this.loadBeats();
    });
    await api.listen("export-progress", (p) => (this.exportFraction = p.fraction));
    await api.listen("export-done", (p) => {
      this.exporting = false;
      this.exportDonePath = p.path;
    });
    await api.listen("export-error", (p) => {
      this.exporting = false;
      this.exportError = p.canceled ? null : p.message;
    });
    await api.listen("refine-field", (p) => {
      if (p.stem === this.refineOpen) this.field = p.bins;
    });
    await api.listen("strength-progress", (p) => {
      if (p.song_id !== this.song?.song_id || p.job < this.strengthWait) return;
      this.strengthBusy = true;
      this.strengthFraction = p.fraction;
    });
    const strengthEnd = (job: number, songId: string, err: string | null) => {
      this.strengthDoneJob = Math.max(this.strengthDoneJob, job);
      if (songId !== this.song?.song_id || job < this.strengthWait) return;
      this.strengthBusy = false;
      this.strengthError = err;
    };
    await api.listen("strength-done", (p) => strengthEnd(p.job, p.song_id, null));
    await api.listen("strength-error", (p) => strengthEnd(p.job, p.song_id, p.canceled ? null : p.message));
    api
      .getRefinePresets()
      .then((p) => (this.refinePresets = p))
      .catch((e) => console.warn("get_refine_presets failed", e));
    await api.onFileDrop((paths) => this.openPath(paths[0]));
    await Promise.all([this.refreshLibrary(), this.loadAppSettings()]);
  }

  async loadAppSettings() {
    try {
      this.appSettings = await api.getAppSettings();
      this.click.volume = this.appSettings.click_volume;
      this.masterVolume = this.appSettings.master_volume;
      // The backend setting wins over the mirror (e.g. a cleared WebView cache).
      const t = this.appSettings.theme;
      if (isTheme(t) && t !== this.theme) {
        this.theme = t;
        applyTheme(t);
      }
    } catch (e) {
      console.warn("get_app_settings failed", e);
    }
  }

  /** Applies a theme instantly and saves it with the app settings. */
  async setTheme(t: Theme) {
    if (t === this.theme) return;
    this.theme = t;
    applyTheme(t);
    try {
      this.appSettings = await api.setTheme(t);
    } catch (e) {
      this.report(e);
    }
  }

  /** Saves "Count in to where the music starts"; playback uses it at once. */
  async setCountInToMusicStart(on: boolean) {
    if (this.appSettings) this.appSettings = { ...this.appSettings, count_in_to_music_start: on };
    try {
      this.appSettings = await api.setCountInToMusicStart(on);
    } catch (e) {
      this.report(e);
      await this.loadAppSettings();
    }
  }

  /** Saves the format of newly written stems (existing stems keep theirs). */
  async setStemFormat(format: StemFormat) {
    if (this.appSettings) this.appSettings = { ...this.appSettings, stem_format: format };
    try {
      this.appSettings = await api.setStemFormat(format);
    } catch (e) {
      this.report(e);
      await this.loadAppSettings();
    }
  }

  /** Save a changed setting; fields left out keep the current saved choice. */
  async saveAppSettings(change: Partial<AppSettingsUpdate>) {
    const cur = this.appSettings;
    const update: AppSettingsUpdate = {
      model_id: cur?.model_choice ?? null,
      quality: cur?.quality_choice ?? null,
      ...change,
    };
    this.savingSettings = true;
    try {
      this.appSettings = await api.setAppSettings(update);
      this.askReseparate = true;
    } catch (e) {
      this.report(e);
    } finally {
      this.savingSettings = false;
    }
  }

  async reseparate() {
    this.askReseparate = false;
    await this.startSeparation();
  }

  keepCurrentStems() {
    this.askReseparate = false;
  }

  async refreshLibrary() {
    try {
      this.library = await api.getLibrary();
    } catch (e) {
      console.warn("get_library failed", e);
    }
  }

  /** Removes a song from the recent list only; its stems stay and opening it brings it back. */
  async hideRecent(songId: string) {
    this.library = this.library.filter((e) => e.song_id !== songId);
    try {
      await api.hideRecent(songId);
    } catch (e) {
      this.report(e);
    }
    this.refreshLibrary();
  }

  showToast(msg: string) {
    this.toast = msg;
    const m = msg;
    setTimeout(() => {
      if (this.toast === m) this.toast = null;
    }, 2500);
  }

  private report(e: unknown) {
    this.error = e instanceof Error ? e.message : String(e);
  }

  async pickAndOpen() {
    try {
      const path = await api.pickAudioFile();
      if (path) await this.openPath(path);
    } catch (e) {
      this.report(e);
    }
  }

  async openPath(path: string) {
    if (this.opening) return;
    this.opening = true;
    this.error = null;
    try {
      const info = await api.openSong(path);
      this.song = info;
      this.stemsReady = info.stems_cached;
      this.separating = false;
      this.sepError = null;
      this.sepFraction = 0;
      this.askReseparate = false;
      this.position = 0;
      this.playing = false;
      this.countInLeft = 0;
      this.peaks = null;
      this.grid = null;
      this.applySettings(info.settings);
      this.refineOpen = null;
      this.field = null;
      this.refine = defaultRefineAll();
      for (const [stem, p] of Object.entries(info.refine ?? {})) {
        if (stem in this.refine) this.refine[stem as StemName] = normalize(p);
      }
      this.strength = { ...(info.strength ?? { level: 0, smooth: false }) };
      this.strengthError = null;
      this.strengthWait = info.strength_job ?? 0;
      this.strengthBusy = this.strengthWait > this.strengthDoneJob;
      this.strengthFraction = 0;
      // Push the full UI state so the backend matches what the UI shows.
      await Promise.all([
        api.setMix(this.mixPayload()),
        api.setSpeed(this.speed),
        api.setPitch(this.semitones, this.cents),
        api.setLoop($state.snapshot(this.loop)),
        this.pushClick(false),
      ]);
      api
        .getWaveform(1200)
        .then((w) => {
          if (this.song?.song_id === info.song_id) this.peaks = w.peaks;
        })
        .catch((e) => console.warn("get_waveform failed", e));
      if (info.beats_ready) await this.loadBeats();
      if (!info.stems_cached) await this.startSeparation();
      this.refreshLibrary();
    } catch (e) {
      this.report(e);
    } finally {
      this.opening = false;
    }
  }

  private applySettings(s: SongSettings | null) {
    this.mix = defaultMix();
    this.overrides = [];
    this.loop = { a_sec: null, b_sec: null, enabled: false };
    this.click.count_in_enabled = false;
    this.click.count_in_each_repeat = false;
    this.click.subdivision = 1;
    this.countInSet = false;
    if (!s || s.version !== 1) return;
    this.loop = { ...s.loop };
    this.overrides = normalizeOverrides(s.overrides ?? []);
    this.click.count_in_enabled = !!s.count_in_enabled;
    this.click.count_in_each_repeat = !!s.count_in_each_repeat;
    this.click.subdivision = s.subdivision ?? 1;
    if (s.count_in_beats != null) {
      this.click.count_in_beats = s.count_in_beats;
      this.countInSet = true;
    }
    for (const m of s.mix ?? []) {
      // Volumes are linear gains (old settings: 0..1, now up to +6 dB).
      if (this.mix[m.stem]) this.mix[m.stem] = { on: m.on, volume: clampGain(Number(m.volume)), solo: false };
    }
  }

  async loadBeats() {
    const id = this.song?.song_id;
    try {
      const g = await api.getBeats();
      if (!g || this.song?.song_id !== id) return;
      this.grid = g;
      if (this.song && g.bpm != null) this.song = { ...this.song, beats_ready: true, bpm: g.bpm };
      if (!this.countInSet) this.click.count_in_beats = this.beatsPerBar;
      this.clampCountIn();
      await this.sendMeter();
      await this.pushClick(false);
    } catch (e) {
      console.warn("get_beats failed", e);
    }
  }

  // ---- settings persistence ----
  private settingsSnapshot(): SongSettings {
    return {
      version: 1,
      loop: $state.snapshot(this.loop),
      count_in_enabled: this.click.count_in_enabled,
      count_in_beats: this.countInSet || this.grid ? this.click.count_in_beats : null,
      count_in_each_repeat: this.click.count_in_each_repeat,
      subdivision: this.click.subdivision,
      overrides: $state.snapshot(this.overrides),
      mix: STEMS.map((s) => ({ stem: s, on: this.mix[s].on, volume: this.mix[s].volume })),
    };
  }

  /** Sent on every change: the backend debounces the write and flushes it on song switch and quit. */
  private saveSettings() {
    if (!this.song) return;
    api.saveSongSettings(this.song.song_id, this.settingsSnapshot()).catch((e) => console.warn("save_song_settings failed", e));
  }

  // ---- separation ----
  async startSeparation() {
    this.sepError = null;
    try {
      const s = await api.startSeparation();
      this.separating = true;
      this.sepStage = "loading_model";
      this.sepFraction = 0;
      this.sepModel = s.model_name;
      this.sepDevice = s.device_label;
      this.sepModelId = s.model_id;
      this.sepQuality = s.quality;
    } catch (e) {
      this.sepError = e instanceof Error ? e.message : String(e);
    }
  }

  async cancelSeparation() {
    await api.cancelSeparation().catch((e) => this.report(e));
  }

  // ---- transport ----
  async togglePlay() {
    if (!this.song) return;
    if (this.playing) await api.pause();
    else await api.play();
  }

  async seek(sec: number) {
    if (!this.song) return;
    const t = clamp(sec, 0, this.song.duration_sec);
    this.position = t;
    await api.seek(t);
  }

  async seekBy(delta: number) {
    await this.seek(this.position + delta);
  }

  async backToStart() {
    const target = this.loop.enabled && this.loop.a_sec != null ? this.loop.a_sec : 0;
    await this.seek(target);
  }

  // ---- loop ----
  async setLoop(a: number | null, b: number | null, enabled?: boolean) {
    if (a != null && b != null && b < a) [a, b] = [b, a];
    if (a != null && b != null && b - a < MIN_LOOP) b = a + MIN_LOOP;
    this.loop = { a_sec: a, b_sec: b, enabled: enabled ?? this.loop.enabled };
    if (this.loop.a_sec == null || this.loop.b_sec == null) this.loop.enabled = false;
    await api.setLoop($state.snapshot(this.loop));
    this.saveSettings();
  }

  async toggleLoop() {
    if (!this.song) return;
    if (!this.hasLoop) {
      // No region yet: loop the current bar-ish 8 seconds from the playhead.
      const a = this.position;
      const b = Math.min(this.song.duration_sec, a + 8);
      await this.setLoop(a, b, true);
      return;
    }
    await this.setLoop(this.loop.a_sec, this.loop.b_sec, !this.loop.enabled);
  }

  async setAAtPlayhead() {
    if (!this.song) return;
    const a = this.position;
    let b = this.loop.b_sec;
    if (b == null || b <= a) b = Math.min(this.song.duration_sec, a + 8);
    await this.setLoop(a, b, true);
  }

  async setBAtPlayhead() {
    if (!this.song) return;
    const b = this.position;
    let a = this.loop.a_sec;
    if (a == null || a >= b) a = Math.max(0, b - 8);
    await this.setLoop(a, b, true);
  }

  // ---- mixer ----
  private mixPayload(): StemMixState[] {
    return STEMS.map((s) => ({ stem: s, ...$state.snapshot(this.mix[s]) }));
  }

  private async pushMix(save = true) {
    await api.setMix(this.mixPayload());
    if (save) this.saveSettings();
  }

  async setStemOn(stem: StemName, on: boolean) {
    this.mix[stem].on = on;
    await this.pushMix();
  }

  async toggleStem(stem: StemName) {
    await this.setStemOn(stem, !this.mix[stem].on);
  }

  async setVolume(stem: StemName, v: number) {
    this.mix[stem].volume = clampGain(v);
    await this.pushMix();
  }

  /** "All on": every stem on and solo cleared; volumes unchanged. */
  async allOn() {
    for (const s of STEMS) {
      this.mix[s].on = true;
      this.mix[s].solo = false;
    }
    await this.pushMix();
  }

  /** "All but this" (play along): this stem off, all others on, solo cleared. */
  async allButThis(stem: StemName) {
    for (const s of STEMS) {
      this.mix[s].on = s !== stem;
      this.mix[s].solo = false;
    }
    await this.pushMix();
  }

  async toggleSolo(stem: StemName) {
    const next = !this.mix[stem].solo;
    for (const s of STEMS) this.mix[s].solo = false;
    this.mix[stem].solo = next;
    await this.pushMix(false);
  }

  // ---- refine ----
  refined(stem: StemName): boolean {
    return !isIdentity(this.refine[stem]);
  }

  /** Opens the stem's Refine panel (closing any other), or closes it. */
  async toggleRefine(stem: StemName) {
    const next = this.refineOpen === stem ? null : stem;
    this.refineOpen = next;
    this.field = null;
    await api.setRefineAnalysis(next).catch((e) => console.warn("set_refine_analysis failed", e));
  }

  async closeRefine() {
    if (this.refineOpen) await this.toggleRefine(this.refineOpen);
  }

  /** Live change from the panel (heard at once; the backend saves it). */
  async setRefine(stem: StemName, p: RefineParams) {
    this.refine[stem] = p;
    try {
      await api.setRefine(stem, $state.snapshot(this.refine[stem]));
    } catch (e) {
      this.report(e);
    }
  }

  async resetRefine(stem: StemName) {
    await this.setRefine(stem, defaultRefine());
  }

  /** A preset sets the EQ and keeps the stereo selection. */
  async applyPreset(stem: StemName, id: string) {
    const pr = this.refinePresets.find((p) => p.id === id);
    if (!pr) return;
    const cur = $state.snapshot(this.refine[stem]);
    await this.setRefine(stem, { ...cur, eq: normalize({ eq: structuredClone(pr.eq) }).eq });
  }

  // ---- strength ----
  /** Strength needs cached stems and the original (it re-derives from it). */
  strengthAvailable = $derived(this.stemsReady && !!this.song?.original_available && !this.separating);
  strengthLabel = $derived(STRENGTH_NAMES[this.strength.level] ?? "Natural");

  async setStrength(change: Partial<StrengthSetting>) {
    if (!this.song) return;
    const next = { ...this.strength, ...change };
    next.level = clamp(Math.round(next.level), 0, STRENGTH_NAMES.length - 1);
    if (next.level === this.strength.level && next.smooth === this.strength.smooth) return;
    this.strength = next;
    this.strengthError = null;
    try {
      const { job } = await api.setStrength(next);
      if (job > 0) {
        this.strengthWait = job;
        this.strengthBusy = job > this.strengthDoneJob;
        this.strengthFraction = 0;
      } else {
        this.strengthBusy = false;
      }
    } catch (e) {
      this.strengthError = e instanceof Error ? e.message : String(e);
    }
  }

  // ---- speed / pitch ----
  async setSpeed(p: number) {
    this.speed = clamp(Math.round(p / 5) * 5, SPEED_MIN, SPEED_MAX);
    await api.setSpeed(this.speed);
  }

  async setPitch(semi: number, cents: number) {
    this.semitones = clamp(Math.round(semi), -SEMI_MAX, SEMI_MAX);
    this.cents = clamp(Math.round(cents), -CENTS_MAX, CENTS_MAX);
    await api.setPitch(this.semitones, this.cents);
  }

  // ---- click / count-in / meter ----
  private async pushClick(save = true) {
    this.click.beats_per_bar = this.beatsPerBar;
    await api.setClick($state.snapshot(this.click));
    if (save) this.saveSettings();
  }

  async toggleClick() {
    this.click.enabled = !this.click.enabled;
    await this.pushClick(false);
  }

  /** Master volume (0..1): heard at once; `save` (on release) stores it. */
  async setMasterVolume(volume: number, save = false) {
    this.masterVolume = clamp(volume, 0, 1);
    try {
      const s = await api.setMasterVolume(this.masterVolume, save);
      if (save) this.appSettings = s;
    } catch (e) {
      this.report(e);
    }
  }

  /** Click and count-in volume (0..1): heard at once while dragging; `save`
   *  (on release) stores it with the app settings. */
  async setClickVolume(volume: number, save = false) {
    this.click.volume = clamp(volume, 0, 1);
    try {
      if (save) this.appSettings = await api.setClickVolume(this.click.volume);
      else await this.pushClick(false);
    } catch (e) {
      this.report(e);
    }
  }

  async toggleCountIn() {
    this.click.count_in_enabled = !this.click.count_in_enabled;
    await this.pushClick();
  }

  /** With a loop: count in again before each repeat (saved with the song). */
  async toggleCountInEachRepeat() {
    this.click.count_in_each_repeat = !this.click.count_in_each_repeat;
    await this.pushClick();
  }

  get countInMin() {
    return this.beatsPerBar;
  }
  get countInMax() {
    return this.beatsPerBar * 4;
  }

  private clampCountIn() {
    this.click.count_in_beats = clamp(this.click.count_in_beats, this.countInMin, this.countInMax);
  }

  async stepCountIn(delta: number) {
    this.click.count_in_beats = clamp(this.click.count_in_beats + delta, this.countInMin, this.countInMax);
    this.countInSet = true;
    await this.pushClick();
  }

  async setSubdivision(s: Subdivision) {
    this.click.subdivision = s;
    await this.pushClick();
  }

  private async sendMeter() {
    await api.setMeterOverrides({ overrides: $state.snapshot(this.overrides), downbeats: [...this.downbeats] });
  }

  private async setOverrides(list: MeterOverride[]) {
    this.overrides = normalizeOverrides(list);
    this.clampCountIn();
    await this.sendMeter();
    await this.pushClick();
  }

  /** "This is beat 1": the beat nearest the playhead becomes a downbeat. */
  async beatOneHere() {
    if (!this.grid) return;
    const k = nearestBeatIndex(this.grid.beats, this.position);
    if (k < 0) return;
    await this.setOverrides([...this.overrides, { kind: "beat_one", beat_index: k }]);
    this.showToast(`Bar ${barAt(this.downbeats, this.grid.beats[k])} now starts here`);
  }

  /** Beats per bar from bar `bar` (1-based, effective grid) onward. */
  async beatsPerBarFrom(bar: number, n: number) {
    if (!this.grid || !this.downbeats.length) return;
    const b = clamp(Math.round(bar), 1, this.downbeats.length);
    const k = nearestBeatIndex(this.grid.beats, this.downbeats[b - 1]);
    await this.setOverrides([...this.overrides, { kind: "beats_per_bar", beat_index: k, beats_per_bar: clamp(Math.round(n), 1, 16) }]);
  }

  async removeOverride(i: number) {
    await this.setOverrides(this.overrides.filter((_, j) => j !== i));
  }

  async clearOverrides() {
    await this.setOverrides([]);
  }

  // ---- help ----
  openHelp(section: HelpSection | null = null) {
    this.helpSection = section;
    this.helpOpen = true;
  }

  closeHelp() {
    this.helpOpen = false;
    this.helpSection = null;
  }

  // ---- about ----
  openAbout() {
    this.aboutOpen = true;
  }

  closeAbout() {
    this.aboutOpen = false;
  }

  // ---- remove from recent (with an option to delete the stems) ----
  askRemoveRecent(entry: LibraryEntry, done?: (removed: boolean) => void) {
    this.removeAsk = { entry, done };
  }

  closeRemoveRecent(removed: boolean) {
    const done = this.removeAsk?.done;
    this.removeAsk = null;
    done?.(removed);
  }

  /** Deletes everything stored for a song (not the open one) and refreshes the recent list. */
  async deleteSongData(songId: string): Promise<number | null> {
    try {
      return await api.deleteSong(songId);
    } catch (e) {
      this.report(e);
      return null;
    } finally {
      await this.refreshLibrary();
    }
  }

  // ---- storage ----
  openStorage() {
    this.storageOpen = true;
  }

  closeStorage() {
    this.storageOpen = false;
    this.refreshLibrary();
  }

  // ---- export ----
  openExport() {
    this.exportOpen = true;
    this.exportRangePick = null;
    this.exportDonePath = null;
    this.exportError = null;
    this.exportFraction = 0;
  }

  closeExport() {
    if (this.exporting) return;
    this.exportOpen = false;
  }

  async runExport() {
    if (!this.song) return;
    this.exportError = null;
    this.exportDonePath = null;
    try {
      const base = `${this.song.title} (mix)`;
      const path = await api.pickExportPath(base, this.exportFormat);
      if (!path) return;
      this.exporting = true;
      this.exportFraction = 0;
      await api.exportMix({
        path,
        format: this.exportFormat,
        stems: this.exportStems.map((s) => ({ stem: s, volume: this.mix[s].volume, on: this.mix[s].on })),
        solo: this.anySolo,
        speed_percent: this.speed,
        semitones: this.semitones,
        cents: this.cents,
        click: this.click.enabled,
        count_in: this.click.count_in_enabled,
        start_sec: this.exportRange === "song" ? null : this.exportSpan.start,
        end_sec: this.exportRange === "loop" ? this.exportSpan.end : null,
      });
    } catch (e) {
      this.exporting = false;
      this.exportError = e instanceof Error ? e.message : String(e);
    }
  }

  async cancelExport() {
    await api.cancelExport().catch(() => {});
  }
}

export const app = new AppState();
