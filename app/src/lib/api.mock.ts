// Browser mock of the backend so `pnpm dev` works without Tauri. Fakes a
// song, separation progress, a beat grid, playback position and export.
// Audio: a soft WebAudio click when the click track / count-in is on;
// otherwise silence.
import type {
  AppSettingsInfo,
  AppSettingsUpdate,
  Backend,
  BeatGrid,
  ClickState,
  EventMap,
  EventName,
  LibraryEntry,
  LoopState,
  ModelInfo,
  Quality,
  SeparatedWith,
  SeparationStage,
  SongInfo,
  RefineParams,
  SongSettings,
  StemMixState,
  StemName,
  StrengthSetting,
  Theme,
} from "./types";
import { THEMES } from "./types";
import { isIdentity, mockPresets } from "./refine";

type Handler = (payload: unknown) => void;
const handlers = new Map<string, Set<Handler>>();
function emit<E extends EventName>(event: E, payload: EventMap[E]) {
  handlers.get(event)?.forEach((h) => h(payload));
}

const LS_LIB = "stemx.mock.library";
const LS_SETTINGS = "stemx.mock.settings.";
// Cached stem sets, most recent last: "<song_id>|<model_id>|<quality>".
const LS_CACHED = "stemx.mock.cached_sets";
const LS_APP = "stemx.mock.app_settings";
const LS_THEME = "stemx.mock.theme";
const LS_REFINE = "stemx.mock.refine.";
const LS_STRENGTH = "stemx.mock.strength.";

function lsGet<T>(key: string, fallback: T): T {
  try {
    const v = localStorage.getItem(key);
    return v ? (JSON.parse(v) as T) : fallback;
  } catch {
    return fallback;
  }
}
function lsSet(key: string, v: unknown) {
  try {
    localStorage.setItem(key, JSON.stringify(v));
  } catch {
    /* ignore */
  }
}

const DEMO_SONGS = [
  { path: "C:/Users/you/Music/Led Zeppelin/Houses Of The Holy.mp3", title: "Houses Of The Holy", artist: "Led Zeppelin", duration_sec: 243.5, bpm: 118 },
  { path: "C:/Users/you/Music/Ozzy Osbourne/Mr. Crowley.mp3", title: "Mr. Crowley", artist: "Ozzy Osbourne", duration_sec: 296.2, bpm: 128 },
  { path: "C:/Users/you/Music/Mr. Big/Green-Tinted Sixties Mind.mp3", title: "Green-Tinted Sixties Mind", artist: "Mr. Big", duration_sec: 213.8, bpm: 132 },
  { path: "C:/Users/you/Music/Extreme/Decadence Dance.mp3", title: "Decadence Dance", artist: "Extreme", duration_sec: 412.0, bpm: 140 },
  // Quiet studio talk, then the riff at 8.4 s (beats on the talk too).
  { path: "C:/Users/you/Music/Led Zeppelin/The Ocean.mp3", title: "The Ocean", artist: "Led Zeppelin", duration_sec: 271.0, bpm: 120, music_start: 8.4 },
];
let pickIndex = 0;

function songIdFor(path: string) {
  let h = 2166136261;
  for (let i = 0; i < path.length; i++) h = Math.imul(h ^ path.charCodeAt(i), 16777619);
  return (h >>> 0).toString(16).padStart(8, "0");
}

function describe(path: string) {
  const demo = DEMO_SONGS.find((s) => s.path === path);
  if (demo) return demo;
  const file = path.split(/[\\/]/).pop() ?? path;
  const title = file.replace(/\.[^.]+$/, "");
  return { path, title, artist: null, duration_sec: 180 + (title.length % 7) * 20, bpm: 100 + (title.length % 5) * 8, music_start: undefined };
}

const SIX = ["vocals", "guitar", "bass", "drums", "piano", "other"];
const MODELS: ModelInfo[] = [
  { id: "htdemucs_6s", name: "Demucs 6-stem", personal: false, license: "MIT", stems: SIX },
  { id: "bs-roformer-sw", name: "BS-RoFormer SW (6 stems, personal)", personal: true, license: "unknown", stems: SIX },
];
const DEFAULT_MODEL = MODELS[0].id;
const MOCK_DEVICE = "GPU"; // mock pretends to have a GPU, so High is the default
const DEFAULT_QUALITY: Quality = "high";

let appChoice: AppSettingsUpdate = lsGet<AppSettingsUpdate>(LS_APP, { model_id: null, quality: null });
let themeChoice: Theme = lsGet<Theme>(LS_THEME, THEMES[0]);
const LS_MUSIC_START = "stemx.mock.countInToMusicStart";
let toMusicStart: boolean = lsGet<boolean>(LS_MUSIC_START, true);

function appSettings(): AppSettingsInfo {
  const model_id = appChoice.model_id && MODELS.some((m) => m.id === appChoice.model_id) ? appChoice.model_id : DEFAULT_MODEL;
  return {
    models: MODELS,
    model_id,
    model_choice: appChoice.model_id,
    model_from_env: false,
    quality: appChoice.quality ?? DEFAULT_QUALITY,
    quality_choice: appChoice.quality,
    default_quality: DEFAULT_QUALITY,
    device_label: MOCK_DEVICE,
    theme: THEMES.includes(themeChoice) ? themeChoice : THEMES[0],
    count_in_to_music_start: toMusicStart,
  };
}

function setFor(model_id: string, quality: Quality): SeparatedWith {
  const name = MODELS.find((m) => m.id === model_id)?.name ?? model_id;
  return { model_name: name, device_label: MOCK_DEVICE, model_id, quality };
}

const setKey = (song_id: string, sw: { model_id: string; quality: Quality }) => `${song_id}|${sw.model_id}|${sw.quality}`;

/** The set open_song loads: the current settings' set if cached, else the most recent cached one. */
function cachedSetFor(song_id: string): SeparatedWith | null {
  const cur = appSettings();
  const sets = lsGet<string[]>(LS_CACHED, []).filter((k) => k.startsWith(song_id + "|"));
  if (!sets.length) return null;
  const want = setKey(song_id, { model_id: cur.model_id!, quality: cur.quality });
  const key = sets.includes(want) ? want : sets[sets.length - 1];
  const [, model_id, quality] = key.split("|");
  return setFor(model_id, quality as Quality);
}

function rememberSet(song_id: string, sw: SeparatedWith) {
  const k = setKey(song_id, sw);
  lsSet(LS_CACHED, [...lsGet<string[]>(LS_CACHED, []).filter((x) => x !== k), k]);
}

// ---- state ----
let song: (SongInfo & { bpmVal: number }) | null = null;
let grid: BeatGrid | null = null;
let position = 0;
let playing = false;
let speed = 100;
let loop: LoopState = { a_sec: null, b_sec: null, enabled: false };
let click: ClickState = { enabled: false, count_in_enabled: false, count_in_beats: 4, subdivision: 1, beats_per_bar: 4, volume: 0.8 };
let downbeatSet = new Set<number>();
let countInLeft = 0;
let countInNextAt = 0;
let countInInterval = 0;
let lastTick = 0;
let tickTimer: number | null = null;
let sepTimer: number | null = null;
let exportTimer: number | null = null;
let beatsTimer: number | null = null;
let lastBeatIdx = -1;
let analysisStem: StemName | null = null;
let fieldTimer: number | null = null;
let strengthTimer: number | null = null;
let strengthJob = 0;

/** Where each stem's energy sits in the synthetic stereo field: (pan, weight, spread). */
const FIELD_SHAPE: Record<StemName, [number, number, number][]> = {
  vocals: [[0, 1, 0.08]],
  guitar: [[-0.72, 0.8, 0.12], [0.65, 0.6, 0.12], [0, 0.35, 0.15]],
  bass: [[0, 1, 0.06]],
  drums: [[0, 0.8, 0.2], [-0.4, 0.3, 0.2], [0.45, 0.3, 0.2]],
  piano: [[0.25, 0.7, 0.3]],
  other: [[-0.5, 0.4, 0.35], [0.5, 0.4, 0.35]],
};

/** A fake `refine-field` frame: 3 bands × 41 pan bins, wobbling a little. */
function syntheticField(stem: StemName): number[] {
  const t = performance.now() / 1000;
  const bins: number[] = [];
  for (let b = 0; b < 3; b++) {
    const bandW = [0.5, 1, 0.35][b] * (stem === "bass" ? [3, 0.6, 0.1][b] : 1);
    for (let i = 0; i < 41; i++) {
      const pan = -1 + i / 20;
      let v = 0;
      for (const [c, w, sp] of FIELD_SHAPE[stem]) v += w * Math.exp(-(((pan - c) / sp) ** 2));
      const wob = 0.75 + 0.25 * Math.sin(t * 3.1 + i * 0.7 + b) * Math.sin(t * 1.3 + b * 2);
      bins.push(1e-3 * bandW * (v * wob + 0.002));
    }
  }
  return bins;
}

function updateFieldTimer() {
  if (analysisStem && fieldTimer == null) {
    fieldTimer = window.setInterval(() => {
      if (analysisStem && playing && countInLeft === 0) emit("refine-field", { stem: analysisStem, bins: syntheticField(analysisStem) });
    }, 33);
  } else if (!analysisStem && fieldTimer != null) {
    clearInterval(fieldTimer);
    fieldTimer = null;
  }
}

/** Fake re-derivation with progress (about 1.2 s). */
function runStrength(song_id: string, strength: StrengthSetting): number {
  if (strengthTimer != null) {
    clearInterval(strengthTimer);
    emit("strength-error", { song_id, job: strengthJob, message: "Cancelled", cancelled: true });
  }
  const job = ++strengthJob;
  const t0 = performance.now();
  strengthTimer = window.setInterval(() => {
    const f = Math.min(1, (performance.now() - t0) / 1200);
    emit("strength-progress", { song_id, job, fraction: f });
    if (f >= 1) {
      clearInterval(strengthTimer!);
      strengthTimer = null;
      emit("strength-done", { song_id, job, strength });
    }
  }, 100);
  return job;
}

let audio: AudioContext | null = null;
function blip(accent: boolean, soft = false) {
  try {
    audio ??= new AudioContext();
    const t = audio.currentTime;
    const o = audio.createOscillator();
    const g = audio.createGain();
    o.frequency.value = accent ? 1600 : 1000;
    const vol = click.volume * (soft ? 0.12 : 0.3);
    g.gain.setValueAtTime(vol, t);
    g.gain.exponentialRampToValueAtTime(0.0001, t + 0.05);
    o.connect(g).connect(audio.destination);
    o.start(t);
    o.stop(t + 0.06);
  } catch {
    /* no audio, fine */
  }
}

function makeGrid(duration: number, bpm: number, music_start: number | null = null): BeatGrid {
  const beats: number[] = [];
  const downbeats: number[] = [];
  const period = 60 / bpm;
  const first = 0.42;
  // A pickup of 1 beat, then 4/4, with a 2/4 bar thrown in to show meter changes.
  let beatInBar = 3;
  let barLen = 4;
  let bar = 0;
  for (let t = first, i = 0; t < duration - 0.2; i++) {
    const jitter = Math.sin(i * 1.7) * 0.004;
    const bt = Math.round((t + jitter) * 1000) / 1000;
    beats.push(bt);
    if (beatInBar === 0) {
      downbeats.push(bt);
      bar++;
      barLen = bar === 17 ? 2 : 4;
    }
    beatInBar = (beatInBar + 1) % barLen;
    t += period;
  }
  return { beats, downbeats, bpm, music_start };
}

function localBeatPeriod(at: number) {
  if (!grid || grid.beats.length < 2) return 0.5;
  const b = grid.beats;
  let i = b.findIndex((x) => x >= at);
  if (i < 1) i = 1;
  if (i >= b.length) i = b.length - 1;
  return b[i] - b[i - 1];
}

function emitPos() {
  emit("playback-position", { position_sec: position, playing, count_in_beats_left: countInLeft });
}

function tick() {
  if (!song) return;
  const now = performance.now() / 1000;
  const dt = now - lastTick;
  lastTick = now;
  if (playing) {
    if (countInLeft > 0) {
      if (now >= countInNextAt) {
        countInLeft--;
        if (countInLeft > 0) {
          blip(countInLeft % click.beats_per_bar === 0);
          countInNextAt += countInInterval;
        }
      }
    } else {
      const prev = position;
      position += dt * (speed / 100);
      if (loop.enabled && loop.a_sec != null && loop.b_sec != null && position >= loop.b_sec) {
        position = loop.a_sec + (position - loop.b_sec);
        lastBeatIdx = -1;
      }
      if (position >= song.duration_sec) {
        position = song.duration_sec;
        playing = false;
      }
      if (click.enabled && grid) clickBetween(prev, position);
    }
  }
  emitPos();
}

function clickBetween(from: number, to: number) {
  if (!grid || to < from) return;
  const b = grid.beats;
  for (let i = Math.max(0, lastBeatIdx + 1); i < b.length && b[i] <= to; i++) {
    if (b[i] >= from) blip(downbeatSet.has(b[i]));
    lastBeatIdx = i;
  }
  if (click.subdivision > 1) {
    for (let i = 0; i + 1 < b.length && b[i] <= to; i++) {
      const step = (b[i + 1] - b[i]) / click.subdivision;
      for (let k = 1; k < click.subdivision; k++) {
        const t = b[i] + k * step;
        if (t > from && t <= to) blip(false, true);
      }
    }
  }
}

function startTicking() {
  if (tickTimer == null) {
    lastTick = performance.now() / 1000;
    tickTimer = window.setInterval(tick, 33);
  }
}

function addToLibrary(s: SongInfo) {
  const lib = lsGet<LibraryEntry[]>(LS_LIB, []).filter((e) => e.song_id !== s.song_id);
  lib.unshift({
    song_id: s.song_id,
    path: s.path,
    title: s.title,
    artist: s.artist,
    duration_sec: s.duration_sec,
    stems_cached: s.stems_cached,
    last_opened_ms: Date.now(),
  });
  lsSet(LS_LIB, lib.slice(0, 20));
}

function stopSeparation() {
  if (sepTimer != null) clearInterval(sepTimer);
  sepTimer = null;
}

const delay = (ms: number) => new Promise((r) => setTimeout(r, ms));

export const mockBackend: Backend = {
  isMock: true,
  async pickAudioFile() {
    await delay(150);
    const s = DEMO_SONGS[pickIndex++ % DEMO_SONGS.length];
    return s.path;
  },
  async pickExportPath(defaultName, format) {
    await delay(100);
    return `C:/Users/you/Music/${defaultName}.${format}`;
  },
  async openSong(path) {
    await delay(200);
    if (sepTimer != null) {
      const old = song;
      stopSeparation();
      if (old) emit("separation-error", { song_id: old.song_id, message: "Cancelled", cancelled: true });
    }
    if (beatsTimer != null) clearTimeout(beatsTimer);
    const d = describe(path);
    const song_id = songIdFor(path);
    const loaded = cachedSetFor(song_id);
    const cached = loaded != null;
    position = 0;
    playing = false;
    countInLeft = 0;
    loop = { a_sec: null, b_sec: null, enabled: false };
    grid = makeGrid(d.duration_sec, d.bpm, "music_start" in d ? (d.music_start ?? null) : null);
    downbeatSet = new Set(grid.downbeats);
    song = {
      song_id,
      path,
      title: d.title,
      artist: d.artist,
      duration_sec: d.duration_sec,
      sample_rate: 44100,
      channels: 2,
      stems_cached: cached,
      separated_with: loaded,
      beats_ready: cached,
      bpm: cached ? d.bpm : null,
      settings: lsGet<SongSettings | null>(LS_SETTINGS + song_id, null),
      original_available: cached,
      refine: lsGet<Partial<Record<StemName, RefineParams>>>(LS_REFINE + song_id, {}),
      strength: lsGet<StrengthSetting>(LS_STRENGTH + song_id, { level: 0, smooth: false }),
      strength_job: 0,
      bpmVal: d.bpm,
    };
    analysisStem = null;
    updateFieldTimer();
    // A saved strength is re-applied on open, like the app does.
    if (cached && song.strength.level > 0) {
      const { song_id: id, strength } = song;
      song.strength_job = runStrength(id, strength);
    }
    if (!cached) {
      // Beats are analysed separately from stems; pretend it takes a moment.
      const id = song_id;
      beatsTimer = window.setTimeout(() => {
        if (song?.song_id === id) {
          song.beats_ready = true;
          song.bpm = song.bpmVal;
          emit("beats-ready", { song_id: id });
        }
      }, 1500);
    }
    addToLibrary(song);
    startTicking();
    const { bpmVal: _, ...info } = song;
    return info;
  },
  async closeSong() {
    stopSeparation();
    analysisStem = null;
    updateFieldTimer();
    song = null;
    playing = false;
  },
  async getLibrary() {
    const lib = lsGet<LibraryEntry[]>(LS_LIB, []);
    // Pad with the demo songs (older than anything opened) so the start screen and the
    // header's Open menu always have a few recents to show.
    const seeds = DEMO_SONGS.slice(1).map((s, i) => ({
      song_id: songIdFor(s.path),
      path: s.path,
      title: s.title,
      artist: s.artist,
      duration_sec: s.duration_sec,
      stems_cached: false,
      last_opened_ms: Date.now() - (i + 1) * 86400000,
    }));
    return [...lib, ...seeds.filter((d) => !lib.some((e) => e.song_id === d.song_id))];
  },
  async startSeparation() {
    if (!song) throw new Error("No song open");
    if (sepTimer != null) throw new Error("Separation already running");
    const id = song.song_id;
    const cur = appSettings();
    const sw = setFor(cur.model_id!, cur.quality);
    const isCached = lsGet<string[]>(LS_CACHED, []).includes(setKey(id, sw));
    // A cached (model, quality) set loads almost instantly; High takes longer.
    const total = isCached ? 300 : cur.quality === "high" ? 12000 : 7000; // ms
    const t0 = performance.now();
    const stages: [number, SeparationStage][] = [
      [0.04, "loading_model"],
      [0.1, "decoding"],
      [0.88, "separating"],
      [0.95, "writing"],
      [1.01, "beats"],
    ];
    sepTimer = window.setInterval(() => {
      const f = Math.min(1, (performance.now() - t0) / total);
      const stage = stages.find(([lim]) => f < lim)?.[1] ?? "beats";
      emit("separation-progress", { song_id: id, stage, fraction: f, model_name: sw.model_name, device_label: sw.device_label });
      if (f >= 1) {
        stopSeparation();
        rememberSet(id, sw);
        if (song?.song_id === id) {
          song.stems_cached = true;
          song.separated_with = sw;
          song.original_available = true;
          addToLibrary(song);
        }
        emit("separation-done", { song_id: id, separated_with: sw, original_available: true });
      }
    }, 100);
    return sw;
  },
  async cancelSeparation() {
    if (sepTimer == null || !song) return;
    stopSeparation();
    emit("separation-error", { song_id: song.song_id, message: "Cancelled", cancelled: true });
  },
  async play() {
    if (!song) return;
    if (position >= song.duration_sec - 0.05) position = 0;
    if (loop.enabled && loop.a_sec != null && loop.b_sec != null && (position < loop.a_sec || position >= loop.b_sec)) {
      position = loop.a_sec;
    }
    // Count in to where the music starts: a start at the song start jumps there.
    const ms = grid?.music_start;
    const limit = loop.enabled && loop.b_sec != null ? loop.b_sec : song.duration_sec;
    const atStart = position <= 0.005 || (!!grid?.beats.length && position < grid.beats[0] - 0.005);
    if (click.count_in_enabled && toMusicStart && ms != null && atStart && ms > position && ms < limit) position = ms;
    lastBeatIdx = grid ? grid.beats.findIndex((b) => b >= position) - 1 : -1;
    if (lastBeatIdx < -1) lastBeatIdx = grid ? grid.beats.length : -1;
    playing = true;
    if (click.count_in_enabled && click.count_in_beats > 0) {
      countInInterval = localBeatPeriod(position) / (speed / 100);
      // One click per beat, counting down; audio starts one interval after
      // the last click, i.e. exactly where the next beat would be.
      countInLeft = click.count_in_beats;
      blip(countInLeft % click.beats_per_bar === 0);
      countInNextAt = performance.now() / 1000 + countInInterval;
    }
    emitPos();
  },
  async pause() {
    playing = false;
    countInLeft = 0;
    emitPos();
  },
  async seek(sec) {
    if (!song) return;
    position = Math.max(0, Math.min(song.duration_sec, sec));
    lastBeatIdx = grid ? grid.beats.findIndex((b) => b >= position) - 1 : -1;
    emitPos();
  },
  async setMix(_stems: StemMixState[]) {},
  async setSpeed(p) {
    speed = p;
  },
  async setPitch() {},
  async setLoop(l) {
    loop = { ...l };
  },
  async setClick(c) {
    click = { ...c };
  },
  async setMeterOverrides(p) {
    downbeatSet = new Set(p.downbeats);
  },
  async getWaveform(n) {
    if (!song) throw new Error("No song open");
    const peaks: number[] = [];
    const dur = song.duration_sec;
    const bpm = song.bpmVal;
    for (let i = 0; i < n; i++) {
      const t = (i / n) * dur;
      const section = 0.55 + 0.35 * Math.sin(t / 23) * Math.sin(t / 7.3 + 1);
      const beatPhase = ((t * bpm) / 60) % 1;
      const transient = Math.exp(-beatPhase * 6) * 0.25;
      const noise = 0.15 * Math.abs(Math.sin(i * 12.9898) * 43758.5453 % 1);
      const fade = Math.min(1, t / 3, (dur - t) / 4);
      peaks.push(Math.max(0, Math.min(1, (section + transient + noise) * fade)));
    }
    return { duration_sec: dur, peaks };
  },
  async getBeats() {
    if (!song || !song.beats_ready) return null;
    return grid;
  },
  async exportMix(req) {
    if (exportTimer != null) throw new Error("Export already running");
    // For checking the request in the browser console.
    (globalThis as { __mockLastExport?: unknown }).__mockLastExport = req;
    const t0 = performance.now();
    exportTimer = window.setInterval(() => {
      const f = Math.min(1, (performance.now() - t0) / 3000);
      emit("export-progress", { fraction: f });
      if (f >= 1) {
        clearInterval(exportTimer!);
        exportTimer = null;
        emit("export-done", { path: req.path });
      }
    }, 100);
  },
  async cancelExport() {
    if (exportTimer == null) return;
    clearInterval(exportTimer);
    exportTimer = null;
    emit("export-error", { message: "Cancelled", cancelled: true });
  },
  async saveSongSettings(songId, settings) {
    lsSet(LS_SETTINGS + songId, settings);
  },
  async getAppSettings() {
    return appSettings();
  },
  async setAppSettings(update) {
    if (update.model_id != null && !MODELS.some((m) => m.id === update.model_id)) throw new Error(`Unknown model: ${update.model_id}`);
    appChoice = { model_id: update.model_id, quality: update.quality };
    lsSet(LS_APP, appChoice);
    return appSettings();
  },
  async setCountInToMusicStart(on) {
    toMusicStart = on;
    lsSet(LS_MUSIC_START, on);
    return appSettings();
  },
  async getRefinePresets() {
    return mockPresets();
  },
  async setRefine(stem, params) {
    if (!song) throw new Error("No song open");
    const key = LS_REFINE + song.song_id;
    const all = lsGet<Partial<Record<StemName, RefineParams>>>(key, {});
    if (isIdentity(params)) delete all[stem];
    else all[stem] = params;
    lsSet(key, all);
    song.refine = all;
  },
  async setRefineAnalysis(stem) {
    analysisStem = stem;
    updateFieldTimer();
  },
  async setStrength(strength) {
    if (!song) throw new Error("No song open");
    const s = { level: Math.max(0, Math.min(4, Math.round(strength.level))), smooth: !!strength.smooth };
    const prev = song.strength;
    song.strength = s;
    lsSet(LS_STRENGTH + song.song_id, s);
    const same = prev.level === s.level && (s.level === 0 || prev.smooth === s.smooth);
    if (!song.stems_cached || same) return { job: 0 };
    return { job: runStrength(song.song_id, s) };
  },
  async setTheme(theme) {
    if (!THEMES.includes(theme)) throw new Error(`Unknown theme: ${theme}`);
    themeChoice = theme;
    lsSet(LS_THEME, theme);
    return appSettings();
  },
  async onFileDrop(cb) {
    const over = (e: DragEvent) => e.preventDefault();
    const drop = (e: DragEvent) => {
      e.preventDefault();
      const files = Array.from(e.dataTransfer?.files ?? []);
      if (files.length) cb(files.map((f) => `D:/dropped/${f.name}`));
    };
    window.addEventListener("dragover", over);
    window.addEventListener("drop", drop);
    return () => {
      window.removeEventListener("dragover", over);
      window.removeEventListener("drop", drop);
    };
  },
  async listen(event, cb) {
    let set = handlers.get(event);
    if (!set) handlers.set(event, (set = new Set()));
    const h = cb as Handler;
    set.add(h);
    return () => set!.delete(h);
  },
};
