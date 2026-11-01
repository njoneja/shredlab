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
  SeparationStage,
  SongInfo,
  SongSettings,
  StemMixState,
  StemName,
  Subdivision,
  Theme,
} from "./types";
import { STEMS } from "./types";
import { backingFromOriginal, clampGain } from "./mix";
import { applyTheme, currentTheme, isTheme } from "./theme";
import { barAt, commonBeatsPerBar, effectiveDownbeats, nearestBeatIndex, normaliseOverrides } from "./meter";

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
  click = $state<ClickState>({ enabled: false, count_in_enabled: false, count_in_beats: 4, subdivision: 1, beats_per_bar: 4, volume: 0.8 });
  countInSet = false; // count_in_beats came from saved settings

  // export
  exportOpen = $state(false);
  exportFormat = $state<ExportFormat>("wav");
  exporting = $state(false);
  exportFraction = $state(0);
  exportDonePath = $state<string | null>(null);
  exportError = $state<string | null>(null);

  downbeats = $derived(effectiveDownbeats(this.grid, this.overrides));
  beatsPerBar = $derived(commonBeatsPerBar(this.grid, this.downbeats));
  anySolo = $derived(STEMS.some((s) => this.mix[s].solo));
  hasLoop = $derived(this.loop.a_sec != null && this.loop.b_sec != null);
  /**
   * Stems the export mixes: exactly what is audible. With a solo, the soloed
   * stems (always the plain sum); otherwise the switched-on ones (on at −∞
   * still counts as on for P1).
   */
  exportStems = $derived(STEMS.filter((s) => (this.anySolo ? this.mix[s].solo : this.mix[s].on)));
  /** Click and count-in go into the export when they are on ("what you hear"). */
  exportExtras = $derived(
    [this.click.enabled && "click", this.click.count_in_enabled && "count-in"].filter(Boolean).join(", "),
  );
  exportSummary = $derived.by(() => {
    const on = this.exportStems;
    const extras = this.exportExtras ? ` + ${this.exportExtras}` : "";
    if (this.anySolo) return `Exports: ${on.join(", ")} (soloed)${extras}`;
    const off = STEMS.filter((s) => !on.includes(s));
    if (!on.length) return "Nothing to export: all stems are off";
    if (!off.length) return `Exports: all stems${extras}`;
    return `Exports: ${on.join(", ")} (${off.join(", ")} off)${extras}`;
  });
  /** Number of stems the loaded song has (from its model; the mixer rows otherwise). */
  songStemCount = $derived.by(() => {
    const id = this.song?.separated_with?.model_id;
    const m = id ? this.appSettings?.models.find((x) => x.id === id) : undefined;
    return m?.stems.length || STEMS.length;
  });
  exportFromOriginal = $derived(
    !this.anySolo && backingFromOriginal(!!this.song?.original_available, this.exportStems.length, this.songStemCount),
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

  private saveTimer: number | null = null;
  private started = false;

  audible(stem: StemName): boolean {
    return this.anySolo ? this.mix[stem].solo : this.mix[stem].on;
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
      this.sepError = p.cancelled ? "Separation cancelled." : p.message;
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
      this.exportError = p.cancelled ? null : p.message;
    });
    await api.onFileDrop((paths) => this.openPath(paths[0]));
    await Promise.all([this.refreshLibrary(), this.loadAppSettings()]);
  }

  async loadAppSettings() {
    try {
      this.appSettings = await api.getAppSettings();
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
      this.flushSave();
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
    this.click.subdivision = 1;
    this.countInSet = false;
    if (!s || s.version !== 1) return;
    this.loop = { ...s.loop };
    this.overrides = normaliseOverrides(s.overrides ?? []);
    this.click.count_in_enabled = !!s.count_in_enabled;
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
      subdivision: this.click.subdivision,
      overrides: $state.snapshot(this.overrides),
      mix: STEMS.map((s) => ({ stem: s, on: this.mix[s].on, volume: this.mix[s].volume })),
    };
  }

  private scheduleSave() {
    if (!this.song) return;
    if (this.saveTimer != null) clearTimeout(this.saveTimer);
    this.saveTimer = window.setTimeout(() => this.flushSave(), 600);
  }

  private flushSave() {
    if (this.saveTimer == null || !this.song) return;
    clearTimeout(this.saveTimer);
    this.saveTimer = null;
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
    this.scheduleSave();
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
    if (save) this.scheduleSave();
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
    if (save) this.scheduleSave();
  }

  async toggleClick() {
    this.click.enabled = !this.click.enabled;
    await this.pushClick(false);
  }

  async toggleCountIn() {
    this.click.count_in_enabled = !this.click.count_in_enabled;
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
    this.overrides = normaliseOverrides(list);
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

  // ---- export ----
  openExport() {
    this.exportOpen = true;
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
        stems: this.exportStems.map((s) => ({ stem: s, volume: this.mix[s].volume })),
        solo: this.anySolo,
        speed_percent: this.speed,
        semitones: this.semitones,
        cents: this.cents,
        click: this.click.enabled,
        count_in: this.click.count_in_enabled,
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
