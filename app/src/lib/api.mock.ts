// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

// Browser mock of the backend so `pnpm dev` works without Tauri. Fakes a
// song, separation progress, a beat grid, the tuning measurement, playback
// position and export. Audio: a soft WebAudio click when the click track /
// count-in is on, and the reference tones; otherwise silence. The live
// tuner gets fake input devices and a synthetic stream of readings. Notes
// are made up on the mock grid, in step with the fake chords, one stem at a
// time like the app's notes job.
import type {
  AppSettingsInfo,
  AppSettingsUpdate,
  Backend,
  BeatGrid,
  ClickState,
  EventMap,
  EventName,
  InputDevice,
  LibraryEntry,
  LibraryListing,
  LoopState,
  ModelInfo,
  Quality,
  SeparatedWith,
  SeparationStage,
  SongInfo,
  RefineParams,
  SongSettings,
  StemMixState,
  StemFormat,
  StemName,
  StrengthSetting,
  Theme,
  ToneSound,
  TuningData,
  ChordsData,
  ChordSpan,
  NoteSpan,
  NotesFast,
  StemNotes,
} from "./types";
import { THEMES } from "./types";
import { isIdentity, mockPresets } from "./refine";
import { formatHeard, parseHeard, type HeardSpan } from "./notes";

type Handler = (payload: unknown) => void;
const handlers = new Map<string, Set<Handler>>();
function emit<E extends EventName>(event: E, payload: EventMap[E]) {
  handlers.get(event)?.forEach((h) => h(payload));
}

const LS_LIB = "shredlab.mock.library";
const LS_SETTINGS = "shredlab.mock.settings.";
// Cached stem sets, most recent last: "<song_id>|<model_id>|<quality>".
const LS_CACHED = "shredlab.mock.cached_sets";
const LS_APP = "shredlab.mock.app_settings";
const LS_THEME = "shredlab.mock.theme";
const LS_REFINE = "shredlab.mock.refine.";
const LS_STRENGTH = "shredlab.mock.strength.";
const LS_NOTES_FAST = "shredlab.mock.notes_fast.";
const LS_NOTES_QUIET = "shredlab.mock.notes_quiet.";
const LS_TUNING = "shredlab.mock.tuning.";
// Song ids removed from the recent list, and demo songs whose data was deleted.
const LS_HIDDEN = "shredlab.mock.hidden";
const LS_GONE = "shredlab.mock.deleted";

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
const LS_MUSIC_START = "shredlab.mock.countInToMusicStart";
let toMusicStart: boolean = lsGet<boolean>(LS_MUSIC_START, true);
const LS_CLICK_VOLUME = "shredlab.mock.clickVolume";
let clickVolume: number = lsGet<number>(LS_CLICK_VOLUME, 0.8);
const LS_MASTER_VOLUME = "shredlab.mock.masterVolume";
let masterVolume: number = lsGet<number>(LS_MASTER_VOLUME, 1);
const LS_STEM_FORMAT = "shredlab.mock.stemFormat";
let stemFormat: StemFormat = lsGet<StemFormat>(LS_STEM_FORMAT, "mp3");
const LS_TUNER_INPUT = "shredlab.mock.tunerInput";
let tunerInput = lsGet<{ device_id: string | null; channel: number }>(LS_TUNER_INPUT, { device_id: null, channel: 0 });

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
    click_volume: clickVolume,
    master_volume: masterVolume,
    stem_format: stemFormat,
    tuner_device_id: tunerInput.device_id,
    tuner_channel: tunerInput.channel,
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
let click: ClickState = { enabled: false, count_in_enabled: false, count_in_beats: 4, subdivision: 1, beats_per_bar: 4, volume: 0.8, count_in_each_repeat: false };
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
let tuningTimer: number | null = null;
let rateCents = 0;

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
    emit("strength-error", { song_id, job: strengthJob, message: "Canceled", canceled: true });
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
      // The notes follow the loaded stems' strength.
      runNotes();
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

/**
 * A fake tuning measurement. The demo songs each show one state of the
 * Tuning button: Houses Of The Holy drifts, Mr. Crowley is 18 cents flat,
 * Green-Tinted Sixties Mind is in tune, Decadence Dance is in Eb standard
 * and 4 cents flat (the guess says likely Eb standard), The Ocean can't be
 * measured. Green-Tinted Sixties Mind's guess is possibly drop D or D
 * standard. Other files are 18 cents flat. From the console,
 * `__mockTuning({ offset_cents: 7 })` replaces the open song's measurement.
 */
function fakeTuning(path: string, duration: number): TuningData {
  const title = describe(path).title;
  const n = Math.max(2, Math.floor(duration / 10));
  const seg = (f: (x: number) => number): TuningData["segments"] =>
    Array.from({ length: n }, (_, i) => ({
      start_s: i * 10,
      offset_cents: Math.round(f(i / (n - 1)) * 10) / 10,
      weight: 1,
    }));
  const make = (offset: number, extra: Partial<TuningData> = {}): TuningData => ({
    version: 3,
    offset_cents: offset,
    reference_hz: 440 * Math.pow(2, offset / 1200),
    confidence: 0.98,
    peaks: 4200,
    drifts: false,
    segments: seg((x) => offset + 0.6 * Math.sin(x * 23)),
    ...extra,
  });
  switch (title) {
    case "Houses Of The Holy": {
      const segments = seg((x) => -22 + 13 * x + Math.sin(x * 17) * 0.8);
      const offset = segments.reduce((n, s) => n + s.offset_cents, 0) / segments.length;
      return make(Math.round(offset * 10) / 10, { drifts: true, segments, confidence: 0.93 });
    }
    case "Green-Tinted Sixties Mind":
      return make(1.2, { guess: { lowest_midi: 26, below_standard: -2, likely: false } });
    case "Decadence Dance":
      return make(-4.3, { guess: { lowest_midi: 27, below_standard: -1, likely: true } });
    case "The Ocean":
      return make(9, { confidence: 0.18, peaks: 140 });
    default:
      return make(-18);
  }
}

/** Measures the open song after a moment (as the app does after separation or on open). */
function measureTuning(ms: number) {
  if (tuningTimer != null) clearTimeout(tuningTimer);
  const id = song?.song_id;
  tuningTimer = window.setTimeout(() => {
    tuningTimer = null;
    if (!song || song.song_id !== id) return;
    const t = fakeTuning(song.path, song.duration_sec);
    song.tuning = t;
    lsSet(LS_TUNING + song.song_id, t);
    emit("tuning-ready", { song_id: song.song_id, tuning: t });
    // Chords follow the tuning (they need its offset), as in the app.
    window.setTimeout(() => {
      if (!song || song.song_id !== id) return;
      song.chords = songChords();
      if (song.chords) emit("chords-ready", { song_id: song.song_id, chords: song.chords });
      runNotes();
    }, 900);
  }, ms);
}

(globalThis as { __mockTuning?: unknown }).__mockTuning = (change: Partial<TuningData>) => {
  if (!song) return;
  const base = song.tuning ?? fakeTuning(song.path, song.duration_sec);
  const t = { ...base, ...change };
  if (change.offset_cents != null && change.reference_hz == null) t.reference_hz = 440 * Math.pow(2, change.offset_cents / 1200);
  song.tuning = t;
  lsSet(LS_TUNING + song.song_id, t);
  emit("tuning-ready", { song_id: song.song_id, tuning: t });
};

// Live tuner: fake input devices and a synthetic reading stream. The stream
// plucks the strings of a 6-string tuning built on the lowest note it was
// started with (fourths and a third, like standard tuning), lowest first: each
// pluck starts up to 25 cents off and settles near its target (every third
// one stays 8 cents sharp), with small jitter, then a short silence. "Line in"
// gives digital silence, like a microphone blocked by Windows privacy settings.
const INPUT_DEVICES: InputDevice[] = [
  { id: "Focusrite USB Audio", name: "Focusrite USB Audio", channels: 2, default: false },
  { id: "Microphone (Realtek Audio)", name: "Microphone (Realtek Audio)", channels: 1, default: true },
  { id: "Line In (Realtek Audio)", name: "Line In (Realtek Audio)", channels: 2, default: false },
];
const PLUCK_S = 4.5;
const GUITAR_STEPS = [0, 5, 10, 15, 19, 24];
let tunerTimer: number | null = null;

function startTunerStream(deviceId: string, lowHz: number) {
  stopTunerStream();
  const silent = deviceId.startsWith("Line In");
  const base = Math.max(lowHz, 27);
  const t0 = performance.now() / 1000;
  let seed = 1;
  const rnd = () => ((seed = (seed * 16807) % 2147483647) / 2147483647) * 2 - 1;
  tunerTimer = window.setInterval(() => {
    if (silent) {
      emit("tuner-reading", { hz: null, clarity: 0, level_db: -120 });
      return;
    }
    const t = performance.now() / 1000 - t0;
    const n = Math.floor(t / PLUCK_S);
    const u = t - n * PLUCK_S;
    const i = n % GUITAR_STEPS.length;
    if (u < 0.4 || u > PLUCK_S - 0.3) {
      emit("tuner-reading", { hz: null, clarity: 0.3, level_db: -72 + 2 * rnd() });
      return;
    }
    const start = 25 * Math.sin(n * 2.3 + 1);
    const settle = n % 3 === 2 ? 8 : 0.4 * Math.sin(n);
    const cents = settle + (start - settle) * Math.exp(-(u - 0.4) / 0.9) + 0.7 * rnd();
    const hz = base * Math.pow(2, (GUITAR_STEPS[i] * 100 + cents) / 1200);
    emit("tuner-reading", { hz, clarity: 0.97, level_db: -14 - 6 * (u - 0.4) + rnd() });
  }, 33);
}

function stopTunerStream(reason?: "playback" | "song_closed") {
  if (tunerTimer == null) return;
  clearInterval(tunerTimer);
  tunerTimer = null;
  if (reason) emit("tuner-stopped", { reason });
}

// Reference tones: a decaying string retriggered every 2.5 s, or a filtered sawtooth drone.
let tone: { stop: () => void } | null = null;
function startTone(hz: number, sound: ToneSound) {
  tone?.stop();
  tone = null;
  try {
    audio ??= new AudioContext();
    const ctx = audio;
    const out = ctx.createGain();
    out.gain.value = 0.25 * masterVolume; // about -12 dBFS
    out.connect(ctx.destination);
    const nodes: OscillatorNode[] = [];
    let timer: number | null = null;
    if (sound === "drone") {
      const o = ctx.createOscillator();
      o.type = "sawtooth";
      o.frequency.value = hz;
      const lp = ctx.createBiquadFilter();
      lp.type = "lowpass";
      lp.frequency.value = Math.min(18000, 4 * hz);
      lp.Q.value = 0.5;
      const g = ctx.createGain();
      g.gain.setValueAtTime(0, ctx.currentTime);
      g.gain.linearRampToValueAtTime(0.6, ctx.currentTime + 0.05);
      o.connect(lp).connect(g).connect(out);
      o.start();
      nodes.push(o);
    } else {
      const pluck = () => {
        const t = ctx.currentTime;
        for (const [mult, level] of [
          [1, 0.7],
          [2, 0.3],
          [3, 0.12],
        ]) {
          const o = ctx.createOscillator();
          o.type = "triangle";
          o.frequency.value = hz * mult;
          const g = ctx.createGain();
          g.gain.setValueAtTime(level, t);
          g.gain.exponentialRampToValueAtTime(0.0001, t + 4 / mult);
          o.connect(g).connect(out);
          o.start(t);
          o.stop(t + 4 / mult);
          nodes.push(o);
        }
      };
      pluck();
      timer = window.setInterval(pluck, 2500);
    }
    tone = {
      stop() {
        if (timer != null) clearInterval(timer);
        out.gain.setTargetAtTime(0, ctx.currentTime, 0.02);
        setTimeout(() => {
          for (const n of nodes) {
            try {
              n.stop();
            } catch {
              /* already stopped */
            }
          }
          out.disconnect();
        }, 150);
      },
    };
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

/**
 * Made-up chords on the mock beat grid: a riff with pushes on the "and" of 3
 * and 4, a verse over an E pedal (slash chords), a chorus and a bridge, in E
 * shapes moved by `shift` semitones (a song in Eb gets Eb at concert pitch).
 */
function fakeChords(g: BeatGrid, duration: number, shift: number): ChordsData {
  const E = 4, D = 2, A = 9, G = 7, B = 11, Cs = 1, Fs = 6, C = 0;
  type Ev = { bar: number; beat: number; root: number; quality: string; bass?: number; detail?: string };
  const ev: Ev[] = [];
  const add = (bar: number, beat: number, root: number, quality: string, bass?: number, detail?: string) =>
    ev.push({ bar, beat, root, quality, bass, detail });
  const passing: { bar: number; beat: number; len: number; root: number; quality: string }[] = [];
  const riff = (b: number) => {
    add(b, 0, E, "5");
    add(b, 2.5, D, "5");
    add(b, 3.5, A, "5");
    add(b + 1, 2, E, "5");
  };
  const verse = (b0: number) => {
    for (let b = b0; b < b0 + 16; b += 2) {
      add(b, 0, E, "5");
      // A quick G on the "and" of 3, inside the E5 bar (passing).
      passing.push({ bar: b, beat: 2.5, len: 0.5, root: G, quality: "" });
      add(b + 1, 0, D, "", E);
      add(b + 1, 2.5, A, "", E);
    }
  };
  const chorus = (b0: number) => {
    for (let c = b0; c < b0 + 16; c += 4) {
      add(c, 0, A, "5");
      add(c + 1, 3.5, G, "5");
      add(c + 2, 2, D, "", Fs, "add9");
      add(c + 3, 0, E, "5");
    }
  };
  const bridge = (b0: number) => {
    add(b0, 0, Cs, "m7");
    add(b0 + 1, 0, Fs, "m7");
    add(b0 + 2, 0, B, "sus4", undefined, "7sus4");
    add(b0 + 2, 2, B, "7");
    add(b0 + 3, 0, C, "maj7");
    add(b0 + 3, 2, D, "", undefined, "6");
  };
  const bars = g.downbeats.length;
  for (let b = 0; b < 8; b += 2) riff(b);
  for (let b = 8; b + 40 <= bars; b += 40) {
    verse(b);
    chorus(b + 16);
    bridge(b + 32);
    for (let r = b + 36; r < b + 40; r += 2) riff(r);
  }
  const beatIdx = new Map(g.beats.map((t, i) => [t, i]));
  const timeAt = (bar: number, beat: number): number | null => {
    if (bar >= g.downbeats.length) return null;
    const i0 = beatIdx.get(g.downbeats[bar]);
    if (i0 == null) return null;
    const i = i0 + Math.floor(beat);
    if (i + 1 >= g.beats.length) return null;
    return g.beats[i] + (beat % 1) * (g.beats[i + 1] - g.beats[i]);
  };
  const tr = (n: number) => (((n + shift) % 12) + 12) % 12;
  const segs: ChordSpan[] = [{ start_s: 0, end_s: 0 }];
  for (const e of ev) {
    const t = timeAt(e.bar, e.beat);
    if (t == null) continue;
    const last = segs[segs.length - 1];
    const same = last.root === tr(e.root) && (last.quality ?? "") === e.quality && (last.bass ?? null) === (e.bass == null ? null : tr(e.bass));
    if (same || t <= last.start_s) continue;
    last.end_s = t;
    segs.push({
      start_s: t,
      end_s: t,
      root: tr(e.root),
      quality: e.quality,
      bass: e.bass == null ? null : tr(e.bass),
      detail: e.detail ?? null,
      push: e.beat % 1 !== 0,
    });
  }
  const lastBeat = g.beats[g.beats.length - 1] ?? duration;
  segs[segs.length - 1].end_s = lastBeat;
  if (lastBeat < duration) segs.push({ start_s: lastBeat, end_s: duration });
  const pass = passing.flatMap((p) => {
    const a = timeAt(p.bar, p.beat);
    const b = timeAt(p.bar, p.beat + p.len);
    return a == null || b == null ? [] : [{ start_s: a, end_s: b, root: tr(p.root), quality: p.quality }];
  });
  // Figures: the chorus's A5 bar moves to A6 on beats 3–4 (A to A6 and back).
  const figs = segs
    .filter((s) => s.root === tr(A) && s.quality === "5" && s.end_s - s.start_s > 2)
    .map((s) => ({ start_s: s.start_s + (s.end_s - s.start_s) / 2, end_s: s.start_s + (3 * (s.end_s - s.start_s)) / 4, root: tr(A), quality: "6" }));
  return { version: 8, stems: "htdemucs_6s_s1", key: { tonic: tr(E), minor: false }, segments: segs, passing: pass, figures: figs };
}

// ---- notes ----

const NOTE_STEMS_MOCK: StemName[] = ["guitar", "bass", "vocals", "piano", "other"];

/** Time of `beat` (fractional) in `bar` on the grid, or null past the end. */
function gridTime(g: BeatGrid) {
  const beatIdx = new Map(g.beats.map((t, i) => [t, i]));
  return (bar: number, beat: number): number | null => {
    if (bar < 0 || bar >= g.downbeats.length) return null;
    const i0 = beatIdx.get(g.downbeats[bar]);
    if (i0 == null) return null;
    const i = i0 + Math.floor(beat);
    if (i + 1 >= g.beats.length) return null;
    return g.beats[i] + (beat % 1) * (g.beats[i + 1] - g.beats[i]);
  };
}

/**
 * Made-up notes for one stem, following the fake chords' song form (riff,
 * verse, chorus, bridge): the guitar plays a single-note riff and a lead
 * line in the chorus with bends, slides, hammer-ons and pull-offs, and
 * chords in the verse and bridge (chordal stretches, dropped when a stereo
 * mask isolates the lead); the bass plays root eighths; vocals sing in
 * the verse and chorus; keys only play chords; other has no notes. In E
 * shapes moved by `shift` semitones, like the chords. Other notes heard:
 * the guitar's longer picked notes ring on for a beat under the next ones,
 * and the chord tones sound in the chordal stretches (guitar and keys).
 */
function fakeNotes(g: BeatGrid, chords: ChordsData, shift: number, stem: StemName, masked: boolean): Omit<StemNotes, "source"> {
  const at = gridTime(g);
  const notes: NoteSpan[] = [];
  const chordal: [number, number][] = [];
  const others: HeardSpan[] = [];
  const bars = g.downbeats.length;
  type L = [number, number, number, number, NoteSpan["attack"]?, number?, boolean?]; // bar, beat, len, shape midi, attack, bend, release
  const play = (b0: number, lick: L[]) => {
    for (const [bar, beat, len, m, attack, bend, release] of lick) {
      const a = at(b0 + bar, beat);
      const e = at(b0 + bar, beat + len);
      if (a == null || e == null) continue;
      const end = e - 0.03;
      const n: NoteSpan = { start_s: a, end_s: end, midi: m + shift, conf: 0.8 };
      if (attack) n.attack = attack;
      if (bend) {
        const d = end - a;
        n.bend = { start_s: a + 0.25 * d, peak_s: a + 0.45 * d, semitones: bend, ...(release ? { release_s: a + 0.85 * d } : {}) };
      }
      notes.push(n);
      // A longer picked guitar note rings on under the next one.
      const ring = at(b0 + bar, beat + len + 1);
      if (stem === "guitar" && !attack && !bend && len >= 1 && ring != null) {
        others.push({ start_s: end, end_s: ring - 0.05, midi: m + shift, conf: 0.45 });
      }
    }
  };
  const stretch = (b0: number, n: number) => {
    const a = at(b0, 0);
    const e = at(b0 + n, 0) ?? g.beats[g.beats.length - 1];
    if (a != null && e != null && e > a) chordal.push([a, e]);
  };
  const riffLick: L[] = [
    [0, 0, 1, 52], [0, 1, 0.5, 52], [0, 1.5, 0.5, 55], [0, 2, 0.5, 57, "legato"], [0, 2.5, 1, 50, "slide"], [0, 3.5, 0.5, 45],
    [1, 0, 1.5, 59, undefined, 2], [1, 2, 0.5, 52], [1, 2.5, 0.5, 55], [1, 3, 0.5, 57, "legato"], [1, 3.5, 0.5, 55, "legato"],
  ];
  const chorusLick: L[] = [
    [0, 0, 1, 69], [0, 1, 0.5, 72, "legato"], [0, 1.5, 0.5, 74, "legato"], [0, 2, 2, 76, undefined, 2],
    [1, 0, 1, 74], [1, 1, 1, 72, "slide"], [1, 2, 1, 69], [1, 3, 0.5, 67], [1, 3.5, 0.5, 64, "legato"],
    [2, 0, 2, 67, undefined, 1, true], [2, 2, 1, 69, "slide"], [2, 3, 1, 71],
    [3, 0, 1.5, 64], [3, 1.5, 0.5, 67, "legato"], [3, 2, 2, 69, undefined, 3],
  ];
  const vocalLick: L[] = [[0, 0, 1.5, 68], [0, 2, 2, 66, "slide"], [1, 0, 3, 64], [1, 3, 1, 66]];
  const blocks: number[] = [];
  for (let b = 8; b + 40 <= bars; b += 40) blocks.push(b);
  if (stem === "guitar") {
    for (let b = 0; b < 8; b += 2) play(b, riffLick);
    for (const b of blocks) {
      if (!masked) stretch(b, 16);
      for (let c = b + 16; c < b + 32; c += 4) play(c, chorusLick);
      if (!masked) stretch(b + 32, 4);
      for (let r = b + 36; r < b + 40; r += 2) play(r, riffLick);
    }
  } else if (stem === "vocals") {
    for (const b of blocks) for (let v = b; v < b + 32; v += 2) play(v, vocalLick);
  } else if (stem === "piano") {
    for (const b of blocks) stretch(b, 36);
  } else if (stem === "bass") {
    const low = 28 + shift; // E1 in the song's tuning
    let k = 0;
    for (const s of chords.segments) {
      if (s.root == null) continue;
      const root = s.bass ?? s.root;
      const m = low + ((((root - (4 + shift)) % 12) + 12) % 12);
      for (const t of g.beats) {
        if (t < s.start_s || t >= s.end_s) continue;
        const next = g.beats.find((x) => x > t) ?? t + 0.5;
        const mid = (t + next) / 2;
        const fifth = k % 8 === 7;
        notes.push({ start_s: t, end_s: mid - 0.02, midi: m, conf: 0.85 });
        notes.push({ start_s: mid, end_s: Math.min(next, s.end_s) - 0.02, midi: fifth ? m + 7 : m, conf: 0.8, ...(fifth ? { attack: "slide" as const } : {}) });
        k++;
      }
    }
  }
  // Chord tones in the chordal stretches: root, third and fifth from E2 up.
  for (const [a, e] of chordal) {
    for (const c of chords.segments) {
      if (c.root == null || c.end_s <= a || c.start_s >= e) continue;
      const root = 40 + ((c.root - 4 + 12) % 12);
      const q = c.quality ?? "";
      const third = q.startsWith("m") && !q.startsWith("maj") ? 3 : 4;
      for (const [k, m] of [root, root + third, root + 7, root + 12].entries()) {
        others.push({ start_s: Math.max(a, c.start_s) + 0.01 * k, end_s: Math.min(e, c.end_s) - 0.05, midi: m, conf: 0.6 - 0.05 * k });
      }
    }
  }
  notes.sort((a, b) => a.start_s - b.start_s);
  // Nothing inside a chordal stretch, nothing overlapping.
  const kept = notes.filter((n) => !chordal.some(([a, e]) => n.start_s < e && n.end_s > a));
  for (let i = 1; i < kept.length; i++) kept[i - 1].end_s = Math.min(kept[i - 1].end_s, kept[i].start_s - 0.01);
  const heard = others.filter((h) => h.end_s - h.start_s >= 0.07).sort((a, b) => a.start_s - b.start_s);
  return { stem, notes: kept.filter((n) => n.end_s > n.start_s), chordal, ...(heard.length ? { others: formatHeard(heard) } : {}) };
}

type NotesDetail = { fast: NotesFast; quiet: boolean };

/** The note settings that apply to `stem` (the bass ignores them, as in the app). */
function detailFor(stem: StemName): NotesDetail {
  return stem === "bass" || !song ? { fast: "normal", quiet: false } : { fast: song.notes_fast, quiet: song.notes_quiet };
}

/**
 * The note settings made visible: Fast runs Fewer drops the short notes;
 * More adds a short grace note a whole step under each longer unbent note,
 * which becomes a hammer-on from it. Quiet notes Include adds a short soft
 * note at the previous pitch in each gap of half a second or more.
 */
function withDetail(n: Omit<StemNotes, "source">, detail: NotesDetail): Omit<StemNotes, "source"> {
  let notes: NoteSpan[] = n.notes;
  if (detail.fast === "fewer") notes = notes.filter((x) => x.end_s - x.start_s >= 0.3);
  if (detail.fast === "more") {
    const split: NoteSpan[] = [];
    for (const x of notes) {
      if (x.end_s - x.start_s < 0.4 || x.bend) {
        split.push(x);
        continue;
      }
      const mid = x.start_s + 0.1;
      split.push({ start_s: x.start_s, end_s: mid, midi: x.midi - 2, conf: 0.4 });
      split.push({ ...x, start_s: mid, attack: "legato" });
    }
    notes = split;
  }
  if (detail.quiet) {
    const soft: NoteSpan[] = [];
    notes.forEach((x, i) => {
      soft.push(x);
      const next = notes[i + 1];
      if (next && next.start_s - x.end_s >= 0.5) soft.push({ start_s: x.end_s + 0.1, end_s: x.end_s + 0.25, midi: x.midi, conf: 0.3 });
    });
    notes = soft;
  }
  return notes === n.notes ? n : { ...n, notes };
}

/** The source key of a stem's notes now (as the app builds it). */
function notesSource(stem: StemName, d: NotesDetail = detailFor(stem)): string | null {
  if (!song?.separated_with) return null;
  const sw = song.separated_with;
  const st = song.strength;
  let key = `${sw.model_id}_s${sw.quality === "high" ? 2 : 1}|st${st.level}${st.level > 0 && st.smooth ? "s" : ""}`;
  const p = notesPans[stem];
  if (p?.enabled) key += `|pan${(p.center + 0).toFixed(2)}:${(p.width + 0).toFixed(2)}:${(p.softness + 0).toFixed(2)}`;
  if (d.fast !== "normal") key += `|f${d.fast}`;
  if (d.quiet) key += "|q";
  return key;
}

/** Stereo masks the notes follow (settled a second after the last change), by stem. */
let notesPans: Partial<Record<StemName, RefineParams["pan"]>> = {};
let notesPanTimer: number | null = null;
let notesTimer: number | null = null;
/** Sources computed per song: `${song_id}|${stem}|${source}`, so going back is instant. */
const notesSeen = new Set<string>();
let notesRunning: StemName | null = null;
let notesStepMs = 900;

function notesFor(stem: StemName): StemNotes | null {
  const src = notesSource(stem);
  if (!song || !grid || !song.chords || src == null) return null;
  const g = song.tuning?.guess;
  const shift = g?.likely ? g.below_standard : 0;
  const n = fakeNotes(grid, song.chords, shift, stem, !!notesPans[stem]?.enabled);
  return { ...withDetail(n, detailFor(stem)), source: src };
}

function notesPending(): StemName[] {
  if (!song?.stems_cached) return [];
  return NOTE_STEMS_MOCK.filter((s) => !notesSeen.has(`${song!.song_id}|${s}|${notesSource(s)}`));
}

function emitNotesStatus() {
  if (song) emit("notes-status", { song_id: song.song_id, pending: notesPending(), running: notesRunning });
}

/** Runs the pending stems one at a time, like the app's notes job (needs the chords' grid and tuning). */
function runNotes() {
  if (notesTimer != null || !song || !song.chords) {
    emitNotesStatus();
    return;
  }
  const next = notesPending()[0];
  if (!next) {
    notesRunning = null;
    emitNotesStatus();
    return;
  }
  notesRunning = next;
  emitNotesStatus();
  const id = song.song_id;
  notesTimer = window.setTimeout(() => {
    notesTimer = null;
    notesRunning = null;
    if (!song || song.song_id !== id) return;
    const n = notesFor(next);
    if (n) {
      notesSeen.add(`${id}|${next}|${n.source}`);
      emit("notes-ready", { song_id: id, stem: next, source: n.source, notes: n });
    }
    runNotes();
  }, notesStepMs);
}

function stopNotes() {
  if (notesTimer != null) clearTimeout(notesTimer);
  if (notesPanTimer != null) clearTimeout(notesPanTimer);
  notesTimer = null;
  notesPanTimer = null;
  notesRunning = null;
}

/** The notes follow a changed stereo mask a second after the last change. */
function followNotesPans() {
  if (notesPanTimer != null) clearTimeout(notesPanTimer);
  notesPanTimer = window.setTimeout(() => {
    notesPanTimer = null;
    if (!song) return;
    const before = notesPans;
    notesPans = Object.fromEntries(Object.entries(song.refine).filter(([, p]) => p?.pan.enabled).map(([k, p]) => [k, p!.pan]));
    for (const s of NOTE_STEMS_MOCK) {
      if (JSON.stringify(before[s]) === JSON.stringify(notesPans[s])) continue;
      const n = notesFor(s);
      if (n && notesSeen.has(`${song.song_id}|${s}|${n.source}`)) emit("notes-ready", { song_id: song.song_id, stem: s, source: n.source, notes: n });
    }
    runNotes();
  }, 1000);
}

/** Loop results already found (`${song_id}|${stem}|${source}`): they come back at once, like the app's cache. */
const loopNotesSeen = new Set<string>();
let loopNotesSeq = 0;

/** The notes of `stem` in `start`..`end` with the loop's own settings, clipped to the loop (the app's `detect_loop_notes`). */
async function detectLoopNotes(start: number, end: number, fast: NotesFast, quiet: boolean, stem: StemName): Promise<StemNotes> {
  if (!song) throw new Error("No song open");
  if (stem === "bass" || !NOTE_STEMS_MOCK.includes(stem)) throw new Error(`No loop notes for the ${stem} stem`);
  if (!song.stems_cached || !grid || !song.chords) throw new Error("The song's notes are not ready yet");
  const s = Math.max(0, start);
  const e = Math.min(song.duration_sec, end);
  if (!(e - s >= 0.05)) throw new Error("The loop is too short or outside the song");
  let src = notesSource(stem, { fast, quiet });
  if (src == null) throw new Error("The song's notes are not ready yet");
  src += `|loop${Math.round(s * 1000)}-${Math.round(e * 1000)}`;
  const id = song.song_id;
  const key = `${id}|${stem}|${src}`;
  const seq = ++loopNotesSeq;
  if (!loopNotesSeen.has(key)) {
    await new Promise((r) => window.setTimeout(r, 700));
    if (seq !== loopNotesSeq) throw new Error("Superseded by a newer loop request");
  }
  if (!song || song.song_id !== id || !grid || !song.chords) throw new Error("No song open");
  loopNotesSeen.add(key);
  const g = song.tuning?.guess;
  const shift = g?.likely ? g.below_standard : 0;
  const n = withDetail(fakeNotes(grid, song.chords, shift, stem, !!notesPans[stem]?.enabled), { fast, quiet });
  const clip = (a: number, b: number): [number, number] | null => (Math.min(b, e) > Math.max(a, s) ? [Math.max(a, s), Math.min(b, e)] : null);
  const notes: NoteSpan[] = [];
  for (const x of n.notes) {
    const c = clip(x.start_s, x.end_s);
    if (!c) continue;
    const y: NoteSpan = { ...x, start_s: c[0], end_s: c[1] };
    if (y.bend && (y.bend.start_s < c[0] || y.bend.start_s >= c[1])) delete y.bend;
    notes.push(y);
  }
  const chordal = (n.chordal ?? []).map(([a, b]) => clip(a, b)).filter((c): c is [number, number] => c != null);
  const heard = parseHeard(n.others)
    .map((h) => ({ h, c: clip(h.start_s, h.end_s) }))
    .filter((x) => x.c != null)
    .map(({ h, c }) => ({ ...h, start_s: c![0], end_s: c![1] }));
  return { stem, source: src, notes, ...(chordal.length ? { chordal } : {}), ...(heard.length ? { others: formatHeard(heard) } : {}) };
}

/** Debug: find every stem's notes again, slowly (shows the panel's progress). */
(globalThis as { __mockNotesAgain?: unknown }).__mockNotesAgain = (stepMs = 4000) => {
  if (!song) return;
  for (const k of [...notesSeen]) if (k.startsWith(song.song_id + "|")) notesSeen.delete(k);
  stopNotes();
  notesStepMs = stepMs;
  runNotes();
};

/** Mock chords for the open song: in Eb when its tuning guess says Eb. */
function songChords(): ChordsData | null {
  if (!song || !song.tuning || !grid) return null;
  const g = song.tuning.guess;
  return fakeChords(grid, song.duration_sec, g?.likely ? g.below_standard : 0);
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
      position += dt * (speed / 100) * Math.pow(2, rateCents / 1200);
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
  for (const k of [LS_HIDDEN, LS_GONE]) lsSet(k, lsGet<string[]>(k, []).filter((id) => id !== s.song_id));
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

// Every library entry, hidden ones included.
function allEntries(): LibraryEntry[] {
  const lib = lsGet<LibraryEntry[]>(LS_LIB, []);
  const gone = lsGet<string[]>(LS_GONE, []);
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
  return [...lib, ...seeds.filter((d) => !gone.includes(d.song_id) && !lib.some((e) => e.song_id === d.song_id))];
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
      if (old) emit("separation-error", { song_id: old.song_id, message: "Canceled", canceled: true });
    }
    if (beatsTimer != null) clearTimeout(beatsTimer);
    stopNotes();
    notesStepMs = 900;
    const d = describe(path);
    const song_id = songIdFor(path);
    const loaded = cachedSetFor(song_id);
    const cached = loaded != null;
    position = 0;
    playing = false;
    countInLeft = 0;
    rateCents = 0; // like the app: the rate correction resets on open
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
      notes_fast: lsGet<NotesFast>(LS_NOTES_FAST + song_id, "normal"),
      notes_quiet: lsGet<boolean>(LS_NOTES_QUIET + song_id, false),
      tuning: cached ? lsGet<TuningData | null>(LS_TUNING + song_id, null) : null,
      chords: null,
      notes: [],
      notes_pending: [],
      notes_running: null,
      bpmVal: d.bpm,
    };
    song.chords = cached ? songChords() : null;
    notesPans = Object.fromEntries(Object.entries(song.refine).filter(([, p]) => p?.pan.enabled).map(([k, p]) => [k, p!.pan]));
    if (song.chords) {
      // Saved notes come with the song (computed earlier, as the app caches them).
      for (const s of NOTE_STEMS_MOCK) {
        const n = notesFor(s);
        if (!n) continue;
        notesSeen.add(`${song_id}|${s}|${n.source}`);
        song.notes.push(n);
      }
    }
    song.notes_pending = notesPending();
    if (tuningTimer != null) clearTimeout(tuningTimer);
    // An older algorithm version is measured again, as in the app.
    if (cached && (song.tuning?.version ?? 0) < 3) measureTuning(2500);
    analysisStem = null;
    updateFieldTimer();
    // A saved strength is re-applied on open, like the app does.
    if (cached && song.strength.level > 0) {
      const { song_id: id, strength } = song;
      song.strength_job = runStrength(id, strength);
    }
    if (!cached) {
      // Beats are analyzed separately from stems; pretend it takes a moment.
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
    stopTunerStream("song_closed");
    stopNotes();
    stopSeparation();
    if (tuningTimer != null) clearTimeout(tuningTimer);
    analysisStem = null;
    updateFieldTimer();
    song = null;
    playing = false;
  },
  async getLibrary() {
    const hidden = lsGet<string[]>(LS_HIDDEN, []);
    return allEntries().filter((e) => !hidden.includes(e.song_id));
  },
  async hideRecent(songId) {
    lsSet(LS_HIDDEN, [...lsGet<string[]>(LS_HIDDEN, []).filter((id) => id !== songId), songId]);
  },
  async listLibrary(): Promise<LibraryListing> {
    await delay(400); // the app walks the song folders
    const hidden = lsGet<string[]>(LS_HIDDEN, []);
    const cached = lsGet<string[]>(LS_CACHED, []);
    const songs = allEntries().map((e) => {
      // 16-bit stereo at 44.1 kHz: the original copy plus 6 stems per cached set.
      const sets = cached.filter((k) => k.startsWith(e.song_id + "|")).length;
      const wav = Math.round(e.duration_sec * 44100 * 4);
      return {
        song_id: e.song_id,
        path: e.path,
        title: e.title,
        artist: e.artist,
        size_bytes: 2048 + (sets ? wav * (1 + 6 * sets) : 0),
        last_opened_ms: e.last_opened_ms,
        // Pretend the last demo song's file was moved.
        source_exists: e.path !== DEMO_SONGS[DEMO_SONGS.length - 1].path,
        hidden: hidden.includes(e.song_id),
        open: e.song_id === song?.song_id,
      };
    });
    songs.sort((a, b) => b.size_bytes - a.size_bytes);
    return {
      dir: "C:\\Users\\you\\AppData\\Roaming\\com.shredlab.app\\library",
      total_bytes: songs.reduce((n, s) => n + s.size_bytes, 0),
      songs,
    };
  },
  async songSize(songId) {
    const s = (await mockBackend.listLibrary()).songs.find((x) => x.song_id === songId);
    if (!s) throw new Error(`Unknown song ${songId}`);
    return s.size_bytes;
  },
  async deleteSong(songId) {
    await delay(150);
    if (song?.song_id === songId) throw new Error("This song is open. Close it before deleting its stems.");
    const freed = (await mockBackend.listLibrary()).songs.find((s) => s.song_id === songId)?.size_bytes ?? 0;
    lsSet(LS_LIB, lsGet<LibraryEntry[]>(LS_LIB, []).filter((e) => e.song_id !== songId));
    lsSet(LS_CACHED, lsGet<string[]>(LS_CACHED, []).filter((k) => !k.startsWith(songId + "|")));
    lsSet(LS_HIDDEN, lsGet<string[]>(LS_HIDDEN, []).filter((id) => id !== songId));
    lsSet(LS_GONE, [...lsGet<string[]>(LS_GONE, []), songId]);
    for (const k of [LS_SETTINGS, LS_REFINE, LS_STRENGTH, LS_NOTES_FAST, LS_NOTES_QUIET, LS_TUNING]) {
      try {
        localStorage.removeItem(k + songId);
      } catch {
        /* ignore */
      }
    }
    return freed;
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
          // New stems: the tuning is measured again on them (then the chords and notes).
          song.chords = null;
          song.notes = [];
          for (const k of [...notesSeen]) if (k.startsWith(id + "|")) notesSeen.delete(k);
          measureTuning(1500);
        }
        emit("separation-done", { song_id: id, separated_with: sw, original_available: true });
        emitNotesStatus();
      }
    }, 100);
    return sw;
  },
  async cancelSeparation() {
    if (sepTimer == null || !song) return;
    stopSeparation();
    emit("separation-error", { song_id: song.song_id, message: "Canceled", canceled: true });
  },
  async play() {
    if (!song) return;
    stopTunerStream("playback");
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
    emit("export-error", { message: "Canceled", canceled: true });
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
  async setStemFormat(format) {
    stemFormat = format;
    lsSet(LS_STEM_FORMAT, format);
    return appSettings();
  },
  async setClickVolume(volume) {
    clickVolume = Math.min(1, Math.max(0, volume));
    click = { ...click, volume: clickVolume };
    lsSet(LS_CLICK_VOLUME, clickVolume);
    return appSettings();
  },
  async setMasterVolume(volume, save) {
    masterVolume = Math.min(1, Math.max(0, volume));
    if (save) lsSet(LS_MASTER_VOLUME, masterVolume);
    return appSettings();
  },
  async getRefinePresets() {
    return mockPresets();
  },
  async setRefine(stem, params) {
    if (!song) throw new Error("No song open");
    const key = LS_REFINE + song.song_id;
    const all = lsGet<Partial<Record<StemName, RefineParams>>>(key, {});
    if ("mode" in params.pan) throw new Error("PanParams has no mode any more");
    if (isIdentity(params)) delete all[stem];
    else all[stem] = params;
    lsSet(key, all);
    song.refine = all;
    followNotesPans();
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
  async setNotesOptions(fast, quiet) {
    if (!song) throw new Error("No song open");
    if (fast !== "fewer" && fast !== "normal" && fast !== "more") throw new Error(`unknown fast runs setting: ${fast}`);
    if (typeof quiet !== "boolean") throw new Error(`bad quiet notes setting: ${quiet}`);
    if (song.notes_fast === fast && song.notes_quiet === quiet) return;
    song.notes_fast = fast;
    song.notes_quiet = quiet;
    lsSet(LS_NOTES_FAST + song.song_id, fast);
    lsSet(LS_NOTES_QUIET + song.song_id, quiet);
    // Saved notes for the new settings come at once; the others are queued.
    for (const s of NOTE_STEMS_MOCK) {
      if (s === "bass") continue;
      const n = notesFor(s);
      if (n && notesSeen.has(`${song.song_id}|${s}|${n.source}`)) emit("notes-ready", { song_id: song.song_id, stem: s, source: n.source, notes: n });
    }
    runNotes();
  },
  detectLoopNotes,
  async setRateCorrection(cents) {
    if (!song) return;
    rateCents = Math.max(-200, Math.min(200, Math.round(cents)));
  },
  async playTone(hz, sound) {
    if (!(hz > 0) || !Number.isFinite(hz)) throw new Error("Invalid tone frequency");
    if (sound !== "pluck" && sound !== "drone") throw new Error(`unknown tone sound: ${sound}`);
    startTone(hz, sound);
  },
  async stopTone() {
    tone?.stop();
    tone = null;
  },
  async listInputDevices() {
    return INPUT_DEVICES.map((d) => ({ ...d }));
  },
  async startTuner(deviceId, channel, lowHz) {
    if (!(lowHz > 0) || !Number.isFinite(lowHz)) throw new Error(`Invalid lowest note ${lowHz} Hz`);
    playing = false;
    countInLeft = 0;
    emitPos();
    tone?.stop();
    tone = null;
    const found = deviceId != null ? INPUT_DEVICES.find((d) => d.id === deviceId) : undefined;
    const fell_back = deviceId != null && !found;
    const dev = found ?? INPUT_DEVICES.find((d) => d.default)!;
    const ch = !fell_back && channel < dev.channels ? channel : 0;
    if (!fell_back) {
      tunerInput = { device_id: deviceId, channel: ch };
      lsSet(LS_TUNER_INPUT, tunerInput);
    }
    startTunerStream(dev.id, Math.min(2000, Math.max(20, lowHz)));
    return { device_id: dev.id, device_name: dev.name, channels: dev.channels, channel: ch, sample_rate: 48000, fell_back };
  },
  async stopTuner() {
    stopTunerStream();
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
