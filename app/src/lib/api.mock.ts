// Browser mock of the backend so `pnpm dev` works without Tauri. Fakes a
// song, separation progress, a beat grid, playback position and export.
// Audio: a soft WebAudio click when the click track / count-in is on;
// otherwise silence.
import type {
  Backend,
  BeatGrid,
  ClickState,
  EventMap,
  EventName,
  LibraryEntry,
  LoopState,
  SeparationStage,
  SongInfo,
  SongSettings,
  StemMixState,
} from "./types";

type Handler = (payload: unknown) => void;
const handlers = new Map<string, Set<Handler>>();
function emit<E extends EventName>(event: E, payload: EventMap[E]) {
  handlers.get(event)?.forEach((h) => h(payload));
}

const LS_LIB = "stemx.mock.library";
const LS_SETTINGS = "stemx.mock.settings.";
const LS_CACHED = "stemx.mock.cached";

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
  return { path, title, artist: null, duration_sec: 180 + (title.length % 7) * 20, bpm: 100 + (title.length % 5) * 8 };
}

const MODEL = { model_name: "Demucs 6-stem", device_label: "GPU" };

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

function makeGrid(duration: number, bpm: number): BeatGrid {
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
  return { beats, downbeats, bpm };
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
    const cached = lsGet<string[]>(LS_CACHED, []).includes(song_id);
    position = 0;
    playing = false;
    countInLeft = 0;
    loop = { a_sec: null, b_sec: null, enabled: false };
    grid = makeGrid(d.duration_sec, d.bpm);
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
      separated_with: cached ? MODEL : null,
      beats_ready: cached,
      bpm: cached ? d.bpm : null,
      settings: lsGet<SongSettings | null>(LS_SETTINGS + song_id, null),
      bpmVal: d.bpm,
    };
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
    song = null;
    playing = false;
  },
  async getLibrary() {
    const lib = lsGet<LibraryEntry[]>(LS_LIB, []);
    if (lib.length) return lib;
    // Seed a couple of entries so the empty state shows recents.
    return DEMO_SONGS.slice(1, 3).map((s, i) => ({
      song_id: songIdFor(s.path),
      path: s.path,
      title: s.title,
      artist: s.artist,
      duration_sec: s.duration_sec,
      stems_cached: false,
      last_opened_ms: Date.now() - (i + 1) * 86400000,
    }));
  },
  async startSeparation() {
    if (!song) throw new Error("No song open");
    if (sepTimer != null) throw new Error("Separation already running");
    const id = song.song_id;
    const total = 9000; // ms
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
      emit("separation-progress", { song_id: id, stage, fraction: f, ...MODEL });
      if (f >= 1) {
        stopSeparation();
        const cached = lsGet<string[]>(LS_CACHED, []);
        if (!cached.includes(id)) lsSet(LS_CACHED, [...cached, id]);
        if (song?.song_id === id) {
          song.stems_cached = true;
          song.separated_with = MODEL;
          addToLibrary(song);
        }
        emit("separation-done", { song_id: id, separated_with: MODEL });
      }
    }, 100);
    return MODEL;
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
