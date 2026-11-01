# ShredLab: UI ↔ backend contract

This file is the contract between the Svelte UI (`app/src`) and the Tauri
backend (`app/src-tauri`). The backend implements **exactly** this. TypeScript
types live in [`types.ts`](types.ts); the Tauri bindings in
[`api.tauri.ts`](api.tauri.ts); a browser mock in [`api.mock.ts`](api.mock.ts)
shows the intended behaviour.

## Conventions

- **Commands** are `#[tauri::command]` functions, invoked with
  `invoke("<name>", args)`. Command names are snake_case.
- **Arguments:** the JS side passes camelCase keys; Tauri maps them to the
  snake_case Rust parameter names listed here (default Tauri behaviour, so do
  **not** add `rename_all`). E.g. JS `{ positionSec }` → Rust `position_sec: f64`.
- **Struct fields** inside arguments, return values and event payloads are
  **snake_case JSON** exactly as written (serde default for snake_case Rust
  fields; do not use `rename_all = "camelCase"`).
- **Errors:** a command fails by returning `Err(String)` with a human-readable
  message (shown to the user as-is). `Result<T, String>` is fine.
- **Events** are emitted app-wide (`app_handle.emit(name, payload)`), names in
  kebab-case.
- Times are **seconds as f64, in song time** (position in the original audio,
  independent of playback speed). Gains are **linear** (1.0 = unity). The UI
  shows stem volumes in dB and converts them to linear gain itself (see
  "Volume" below); the backend accepts 0..2.0 (clamped) and never sees dB.
- Async commands must not block the UI thread. Long work (separation, export,
  beat analysis) runs on a background thread and reports through events.
- Only one song is open at a time; most commands act on "the current song".
  Commands that need a song return `Err("No song open")` if none is open,
  except `play`/`pause`/`seek`/`set_*`, which are silently ignored with no song
  (so the UI need not guard them). `set_*` values persist across songs (the UI
  re-sends everything after `open_song` anyway).

Stem names (`StemName`) are exactly: `"vocals" | "guitar" | "bass" | "drums" | "piano" | "other"`.

## Types

```ts
Quality = "standard" | "high"
  // Separation quality: "standard" = 1 pass, "high" = 2 passes (Demucs
  // shifts=2: the song is also separated shifted by 0.25 s and the two
  // results are averaged; deterministic offsets 0 and 0.25 s).

SeparatedWith  { model_name: string, device_label: string, model_id: string, quality: Quality }
  // e.g. { "model_name": "Demucs 6-stem", "device_label": "GPU",
  //        "model_id": "htdemucs_6s", "quality": "high" }
  // The model and quality actually used for the loaded stems.
  // UI renders "<model_name> on <device_label>", so keep device_label short:
  // "GPU" or "CPU" (optionally "GPU (DirectML)").

SongInfo {
  song_id: string,          // stable id for this file (e.g. hash of content or canonical path)
  path: string,             // absolute path as opened
  title: string,            // from tags, else file stem
  artist: string | null,    // from tags
  duration_sec: number,
  sample_rate: number,      // of the decoded original
  channels: number,
  stems_cached: boolean,    // true = stems already on disk; playback uses stems now
  separated_with: SeparatedWith | null,   // set iff stems_cached
  beats_ready: boolean,     // true = get_beats() returns a grid now
  bpm: number | null,       // from the beat grid if beats_ready, else null
  settings: SongSettings | null,          // last value given to save_song_settings, verbatim
  original_available: boolean // stems AND the sample-aligned original are loaded,
                              // so the backing track can come from the original (P1).
                              // False before separation, or for an old cache entry
                              // without the original whose source file is gone.
  refine: { [stem in StemName]?: RefineParams },  // saved Refine settings, refined stems only
  strength: StrengthSetting,  // saved separation strength of the song
  strength_job: number        // id of a strength job open_song started (re-applying a saved
                              // strength), 0 = none; its strength-done/-error has this id
}

ModelInfo {
  id: string,               // registry id, e.g. "htdemucs_6s"
  name: string,             // display name
  personal: boolean,        // personal model (manifest flag or personal folder)
  license: string,          // SPDX id from the manifest, e.g. "MIT", or "unknown"
  stems: string[]           // stem names the model produces
}
  // UI tags personal models "personal, license unknown" (or "personal, <license>").

AppSettingsInfo {
  models: ModelInfo[],         // all installed models
  model_id: string | null,     // model the next separation uses (null: none installed)
  model_choice: string | null, // saved choice; null = default model
  model_from_env: boolean,     // STEMX_MODEL overrides the choice (dev builds)
  quality: Quality,            // quality the next separation uses
  quality_choice: Quality | null, // saved choice; null = default for the device
  default_quality: Quality,    // "high" when model_id runs on the GPU, else "standard"
  device_label: string,        // predicted device of model_id: "GPU" | "CPU"
  theme: Theme,                // UI theme; "stage" when none is saved
  count_in_to_music_start: boolean // "Count in to where the music starts"; true when none is saved
  click_volume: number // click and count-in volume, linear 0..1; 0.8 when none is saved
  master_volume: number // master playback volume, linear 0..1; 1 when none is saved
}

Theme = "stage" | "rosewood" | "valve" | "manuscript"

AppSettingsUpdate { model_id: string | null, quality: Quality | null }
  // null = use the default. Persisted in <data dir>/settings.json.

LibraryEntry {
  song_id: string, path: string, title: string, artist: string | null,
  duration_sec: number, stems_cached: boolean,
  last_opened_ms: number    // unix epoch milliseconds
}

SeparationStage = "loading_model" | "decoding" | "separating" | "writing" | "beats"

StemMixState { stem: StemName, on: boolean, volume: number /* linear 0..2, see Volume */, solo: boolean }

LoopState { a_sec: number | null, b_sec: number | null, enabled: boolean }
  // When enabled is true, a_sec and b_sec are non-null and a_sec < b_sec.

ClickState {
  enabled: boolean,          // click track during playback
  count_in_enabled: boolean, // play a count-in before playback starts
  count_in_beats: number,    // integer >= 1, number of count-in clicks
  subdivision: 1 | 2 | 3,    // 1 = beats only; 2/3 = extra soft clicks evenly between beats
  beats_per_bar: number,     // integer >= 1, most common beats per bar (used for count-in accents)
  volume: number             // 0..1 click gain
}

MeterOverride =
  | { kind: "beat_one", beat_index: number }
  | { kind: "beats_per_bar", beat_index: number, beats_per_bar: number }
  // beat_index indexes BeatGrid.beats. Serde: #[serde(tag = "kind", rename_all = "snake_case")].

Waveform { duration_sec: number, peaks: number[] }
  // peaks.length == n_points requested; peaks[i] = max |sample| over the i-th
  // equal time slice of the original mix (all channels), in 0..1.

BeatGrid { beats: number[], downbeats: number[], bpm: number | null, music_start: number | null }
  // beats ascending, seconds. downbeats ⊂ beats (same f64 values).
  // bpm: overall tempo estimate (e.g. 60 / median beat interval).
  // music_start: where the music starts after a quiet intro (studio talk,
  // silence), or null if the song has none. Detected by the backend from
  // the original mix and the beats; independent of the setting. See
  // "Count in to where the music starts".

ExportFormat = "wav" | "flac" | "mp3"

ExportRequest {
  path: string,              // absolute output path, extension already included
  format: ExportFormat,
  stems: { stem: StemName, volume: number, on: boolean }[],
                             // exactly the audible stems, with linear gains: the soloed
                             // stems if any stem is soloed, else the stems that are on
                             // (include on stems at volume 0: they count as "on" for P1)
                             // plus off stems with a stereo exception (they play the
                             // excepted part). `on` = the row's switch: it decides which
                             // part a stem with an exception plays. Optional; missing = true.
  solo: boolean,             // true = `stems` are the soloed stems: always the plain sum,
                             // never the P1 backing track. Optional; missing = false.
  speed_percent: number,     // 50..150, apply time-stretch like playback
  semitones: number,         // -12..12
  cents: number,             // -50..50
  click: boolean,            // mix in the click track (the UI sends ClickState.enabled).
                             // Optional; missing = false.
  count_in: boolean,         // start with the count-in (the UI sends
                             // ClickState.count_in_enabled). Optional; missing = false.
  start_sec: number | null,  // range start (song seconds). Optional; missing/null = song start.
  end_sec: number | null     // range end (song seconds). Optional; missing/null = song end.
}

SongSettings   // OPAQUE to the backend: store the JSON value and return it verbatim.

// ---- Refine panel (P5) ----
RefineParams { pan: PanParams, eq: EqParams }   // identity = pan off and a flat EQ
PanParams {                  // the stereo EXCEPTION: does the opposite of the row's switch
  enabled: boolean,          // false = no exception (the stem plays whole, or not at all)
  center: number,            // −1 (hard left) … +1 (hard right)
  width: number,             // full width between the −6 dB points, 0 … 2
  softness: number           // raised-cosine edge width in pan units (UI keeps 0.2)
}
  // There is no Keep/Remove mode: switch on = the stem without the window,
  // switch off = only the window. An old saved `mode` field is ignored.
  // Quick presets: Left {center −1, width 1.34}, Centre {0, 0.66}, Right {1, 1.34}.
EqParams { low_cut: EndParams, high_cut: EndParams, bands: [BandParams, BandParams, BandParams] }
EndParams {
  enabled: boolean,
  freq: number,              // Hz; cut: −3 dB point; shelf: half-gain point
  mode: "cut" | "shelf",     // cut = 24 dB/oct high-/low-pass; shelf = RBJ shelf (S = 1)
  gain_db: number            // shelf gain, ±12 dB (ignored for a cut)
}
  // mode and gain_db are optional on input (older saved params): missing = "cut", 0.
BandParams { enabled: boolean, freq: number, gain_db: number, q: number }  // peaking; ±15 dB in the UI
RefinePreset { id: string, name: string, eq: EqParams }

// ---- Separation strength (P4) ----
StrengthSetting { level: number, smooth: boolean }
  // level 0 = Natural (the cached stems), 1..4 = Light, Medium, Strong, Tight
  // (mask exponents 1.5, 2, 3, 4, re-derived from the original so the stems
  // add up exactly to it). smooth: blend from the model's stems instead
  // (gentler first steps; the stems no longer add up exactly).
```

## Commands

| Command | Rust params | Returns | Notes |
|---|---|---|---|
| `pick_audio_file` | – | `string \| null` | Native open dialog (tauri-plugin-dialog). Filter: mp3, wav, flac, m4a, aac, ogg, opus. `null` if cancelled. |
| `pick_export_path` | `default_name: String, format: ExportFormat` | `string \| null` | Native save dialog. `default_name` has no extension (e.g. `"Song (mix)"`); add `.<format>` and a filter for that format. Returns the full path or `null`. |
| `open_song` | `path: String` | `SongInfo` | Decode (or start decoding) the file and make it current. Stops playback, position = 0, loop cleared. If stems are cached, load them so playback uses stems immediately. If a separation is running for another song it is cancelled (emit `separation-error` with `cancelled: true` for that song). Adds/updates the library entry. Kick off beat analysis in the background if the grid is not cached; emit `beats-ready` when done. May return before decoding finishes only if `duration_sec` is known. |
| `close_song` | – | `()` | Stop playback, cancel separation, drop the current song. |
| `get_library` | – | `LibraryEntry[]` | Recent songs, most recently opened first, max ~20. Songs hidden with `hide_recent` are left out. |
| `hide_recent` | `song_id: String` | `()` | Removes a song from the recent list only (`hidden` in its `song.json`); stems and settings stay. `open_song` on the same file clears the flag. `Err` for an unknown song. |
| `list_library` | – | `LibraryListing` | Every stored song, hidden ones included, for the Manage storage dialog. Walks the song folders for sizes on a blocking thread (the UI shows "Calculating…" meanwhile). `LibraryListing { dir, total_bytes, songs: StoredSongInfo[] }`, largest first; `StoredSongInfo { song_id, path, title, artist, size_bytes, last_opened_ms, source_exists, hidden, open }`. |
| `song_size` | `song_id: String` | `number` (bytes) | Size of everything stored for one song (what `delete_song` would free). Walks only that song's folder on a blocking thread; same id checks as `delete_song`. Missing folder: `0`. Used by the "Remove from recent?" dialog. |
| `delete_song` | `song_id: String` | `number` (bytes freed) | Deletes the song's whole folder (stems of every model/quality, `original.wav`, `song.json` with mix/Refine/strength/beat settings). Only a direct child of `<library>/songs`, checked after canonicalizing (ids with separators, `..`, drive prefixes or a symlinked folder are refused). `Err` if the song is open (close it first); serialized with `open_song`. Missing folder: `0`. |
| `start_separation` | – | `SeparationStarted` = `SeparatedWith` | Separates the current song with the **current settings** (model + quality, see `get_app_settings`) on a background thread. Returns immediately with the model, device and quality that will be used. If that (model, quality) is already cached for the song, the stems are loaded from the cache instead (well under a second; same events: optional `separation-progress`, then `separation-done`). `Err` if already running, no song, or the song can't be separated (original file gone and no cached set). Progress via events below; with quality "high" `fraction` stays smooth across both passes. |
| `cancel_separation` | – | `()` | Cancel if running (no-op otherwise). Must emit `separation-error` with `cancelled: true`. |
| `play` | – | `()` | Start playback (see Playback rules). |
| `pause` | – | `()` | Pause; also aborts a running count-in. |
| `seek` | `position_sec: f64` | `()` | Clamp to `[0, duration]`. Works while playing or paused. |
| `set_mix` | `stems: Vec<StemMixState>` | `()` | Always all 6 stems. Apply atomically (no glitch between stems). See Audibility. |
| `set_speed` | `percent: u32` | `()` | 50..150, multiple of 5. Tempo change without pitch change. |
| `set_pitch` | `semitones: i32, cents: i32` | `()` | −12..12 and −50..50. Total shift = semitones + cents/100. Independent of speed. |
| `set_loop` | `loop_state: LoopState` | `()` | JS key is `loopState` (avoids the `loop` keyword). |
| `set_click` | `click: ClickState` | `()` | |
| `set_meter_overrides` | `payload: MeterOverridesPayload` | `()` | `{ overrides: MeterOverride[], downbeats: number[] }`. See Meter. |
| `get_waveform` | `n_points: u32` | `Waveform` | Of the original mix. UI asks for 1200. `Err` if no song. |
| `get_beats` | – | `BeatGrid \| null` | `null` until beat analysis is done for the current song. |
| `export_mix` | `req: ExportRequest` | `()` | Starts export on a background thread and returns immediately. Progress via events. `Err` if an export is already running. |
| `cancel_export` | – | `()` | Cancel if running; emit `export-error` with `cancelled: true`. Delete the partial file. |
| `save_song_settings` | `song_id: String, settings: serde_json::Value` | `()` | Persist next to the song's library entry. Returned verbatim in `SongInfo.settings` on the next `open_song` of that song. Called debounced (~600 ms) after changes. |
| `get_app_settings` | – | `AppSettingsInfo` | Models and the separation settings. |
| `set_count_in_to_music_start` | `on: bool` | `AppSettingsInfo` | Saves "Count in to where the music starts" with the app settings (`settings.json`) and applies it to playback at once. |
| `set_master_volume` | `volume: f32, save: bool` | `AppSettingsInfo` | Master playback volume (linear, clamped to 0..1), applied at once to everything played (music and click, smoothed). Playback only: exports are not affected. `save` also stores it with the app settings; the UI saves when a slider drag ends. Applied at startup from the saved value. `Err` for a non-finite value. |
| `set_click_volume` | `volume: f32` | `AppSettingsInfo` | Saves the click and count-in volume (linear, clamped to 0..1) with the app settings and applies it to playback at once. While the slider is dragged the UI sends `set_click` with the new `volume` instead (heard at once, not saved). `Err` for a non-finite value. |
| `set_theme` | `theme: String` | `AppSettingsInfo` | Saves the UI theme with the app settings (`<data dir>/settings.json`). `Err` for an unknown theme. The UI applies the theme itself and mirrors it in `localStorage` for the next start (no flash before `get_app_settings` returns). |
| `get_refine_presets` | – | `RefinePreset[]` | The Refine panel's EQ presets (stemx-dsp `presets.rs`). A preset sets the EQ and keeps the stereo selection. |
| `set_refine` | `update: { stem: StemName, params: RefineParams }` | `()` | Live: heard at once (smoothed, no clicks), also during playback; turning an exception on or off on an off row also makes it audible or silent; saved in the song's `song.json` about 600 ms after the last change (and on song switch). Identity params = unrefined. `Err` for an unknown stem or no song. |
| `set_refine_analysis` | `stem: Option<String>` | `()` | Turns the stereo-field analysis on for this stem only (the open Refine panel) or off (`null`). Data comes as `refine-field` events while playing. |
| `set_strength` | `strength: StrengthSetting` | `{ job: number }` | Saves the song's strength at once and re-derives the stems on a background thread (re-reads the cached set, about 1–2 s for a 5-min song; a newer call cancels the older job), then swaps the stems in with the usual crossfade. `job` 0 = nothing to compute (same as loaded, or not separated yet: applied after separation). |
| `set_app_settings` | `settings: AppSettingsUpdate` | `AppSettingsInfo` | Saves the choice (null = default) and returns the new effective settings. Does **not** re-separate: if a song is open and its `separated_with` (model_id, quality) differs from the new effective `model_id`/`quality`, the UI asks "Re-separate this song?" and calls `start_separation` on yes. `Err` for an unknown model id. |

## Events

| Event | Payload | When |
|---|---|---|
| `playback-position` | `{ position_sec: number, playing: boolean, count_in_beats_left: number }` | ~30 Hz while playing (including during count-in) **and** once immediately after every play, pause, seek, loop wrap, and end of song. `count_in_beats_left` = count-in clicks still to come (0 when not counting in). |
| `separation-progress` | `{ song_id, stage: SeparationStage, fraction: number, model_name, device_label }` | Throttle to ≤ 10 Hz. `fraction` is **overall** progress 0..1, monotonic across stages. |
| `separation-done` | `{ song_id, separated_with: SeparatedWith, original_available: boolean }` | Stems written and cached. Playback switches from the original to the stems seamlessly at the current position (keep playing if playing). |
| `separation-error` | `{ song_id, message: string, cancelled: boolean }` | Failure or cancel. Playback of the original continues. |
| `beats-ready` | `{ song_id }` | Beat grid for that song is available via `get_beats`. Also fine to emit after separation if beats are computed in the `beats` stage. |
| `export-progress` | `{ fraction: number }` | ≤ 10 Hz, 0..1. |
| `export-done` | `{ path: string }` | File complete. |
| `export-error` | `{ message: string, cancelled: boolean }` | Failure or cancel. |
| `refine-field` | `{ stem: StemName, bins: number[] }` | ~30 Hz while playing, for the stem set with `set_refine_analysis`. `bins`: 123 linear powers, band-major: 3 bands (< 250 Hz, 250 Hz–2.5 kHz, ≥ 2.5 kHz) × 41 pan bins from hard left (bin 0) to hard right (bin 40), centre = bin 20. Measured on the stem before refine (about 0.1 s ahead of what is heard). Nothing while paused. |
| `strength-progress` | `{ song_id, job: number, fraction: number }` | ≤ 10 Hz while re-deriving. |
| `strength-done` | `{ song_id, job: number, strength: StrengthSetting }` | The derived stems are playing. |
| `strength-error` | `{ song_id, job: number, message: string, cancelled: boolean }` | Failure, or `cancelled: true` when a newer job or a song switch replaced it. |

Events for a `song_id` that is no longer current are ignored by the UI.

## Behaviour rules

### Before / after separation
- Without stems, playback uses the **original mix** (mix settings have no
  effect). With stems, see Backing track below.
- Separation runs while the original plays; playback must not stutter.
- `open_song` loads the cached set for the current (model, quality) if
  there is one, else the most recently used cached set (so changing a
  setting never forces a re-separation by itself). `separated_with` tells
  which one is loaded.
- Cached songs open even if the source file has moved: `open_song` with
  the old path finds the library entry (stems and the original are in the
  cache).

### Backing track from the original (P1)
- With `original_available`, let *n* = number of stems, *on* = stems with
  `on: true` (ignoring volume and solo). If no stem is soloed and
  `2 * on >= n`, playback is `original + Σ (gain_i − 1) · stem_i`, where
  off stems have gain 0 (so "All but guitar" = original − guitar, and all
  stems on at 0 dB = the original, bit for bit). Otherwise (isolating, or
  any solo) it is the plain sum of the audible stems.
- Mode changes crossfade over 30 ms; gain changes ramp (~12 ms).
- Export applies the same rule to `req.stems` (on = listed stems with
  `on: true`), except that `req.solo` always gives the plain sum. The UI
  shows "Backing track built from the original" under the export summary
  when `original_available && !req.solo && 2 * on >= n`.

### Refine (P5) and strength (P4)
- **Stereo exception.** A stem's stereo window (`pan.enabled`) is an
  exception to its row's on switch: the window always does the opposite of
  the switch. Guitar on, except left = the guitar without its left part;
  guitar off, except left = only the left part, at the row's volume (so an
  off row with an exception is audible). Flipping the switch flips the
  exception, crossfaded. Volume and EQ apply to whatever part is heard;
  the EQ is not an exception. Solo plays exactly what the row plays; All
  on / All but this just flip switches.
- Every stem runs through its Refine chain (heard part, then EQ: `r`).
  Playback and export: `w·original + Σ (gain_i·r_i − w·s_i)` with `s_i` the
  whole stem and `w` = 1 in the backing-track mode, 0 for the plain sum.
  The whole stem leaves the backing and only the heard part comes back, so
  the unheard part is not in the mix. Unrefined, this is exactly the P1
  formula above (bit for bit). Examples: guitar off except left + the rest
  on = the original minus the right guitar; all on with guitar on except
  left = the original minus the left guitar; EQ on a stem is audible in
  the full mix.
- Refine sits before speed/pitch; there is no added latency (the engine
  feeds each chain ahead), and seeks, loops and exports stay aligned.
- Export includes refine and strength ("what you hear"). The UI's summary
  lists an off stem with an exception as "guitar (left only)" and adds
  " · guitar refined" for refined stems that are heard.
- Strength: Natural plays the cached stems. Other levels replace them in
  memory with stems derived from the original (no disk cache), so "original
  minus stem" equals "sum of the others". Smooth: the derived stems no
  longer add up exactly, so All but this may leave a faint trace or gap.
  Needs the original (`original_available`); re-applied by `open_song` and
  after a separation.

### Volume
- The mixer shows stem volume in dB, from −∞ (slider bottom = gain 0) to
  +6 dB; 0 dB = as separated; double-click resets to 0 dB. The UI maps dB
  to linear gain `10^(dB/20)` (+6 dB = 1.995) and sends linear gains.
- Playback has a safety limiter on the music (soft knee, look-ahead,
  transparent below −1 dBFS, output ≤ −0.1 dBFS); click and count-in are
  added after it. Export doesn't use the playback limiter. It has its own
  look-ahead peak limiter (no whole-song normalization), so boosted exports
  don't clip and match the playback level; an exported click is added after
  it (see Export).

### Audibility (playback)
- If **any** stem has `solo: true`, only stems with `solo: true` play.
- Otherwise, stems with `on: true` play, plus stems that are off but have
  a stereo exception (they play only the excepted part; see Refine).
- An audible stem plays at `volume` (linear). The UI keeps solo exclusive
  (at most one), but the backend should implement the general rule above.
- How the audible stems are combined: see Backing track (P1).

### Export
- "Export what you hear": mix exactly `req.stems` at their volumes. The UI
  has already picked the audible stems: the soloed ones when any stem is
  soloed (`req.solo: true`, plain sum), else the switched-on ones plus
  off ones with a stereo exception (P1 rule above; each with its `on`). Apply `speed_percent`, `semitones`, `cents`
  as in playback. Avoid clipping (a limiter or
  float WAV is fine; MP3 at ≥ 192 kbps).
- **Range** (`start_sec`, `end_sec`): the export covers `[start, end)`
  of the song, played once (a loop export does not repeat), the whole
  song by default. Cut points inside the song (a start after 0, an end
  before the song end) get an 8 ms fade so the file doesn't click. The
  file is `(end - start) / speed` long, plus the count-in. An empty range
  is refused (`Err`). The UI sends nothing for Whole song, `start_sec` =
  playhead for From the playhead, and both for Loop A–B.
- **Click** (`req.click`): the click track exactly as in playback, from the
  current `set_click` settings (subdivision, volume) and the effective
  downbeats (`set_meter_overrides`): same sounds and accents, on the beat
  grid, respaced for `speed_percent`. As in playback the click is added
  after the music's limiter: the music alone goes through the export peak
  limiter, then the click is added; the encoder limits that final sum
  again (−0.1 dBFS), so the file never clips.
- **Count-in** (`req.count_in`): the count-in clicks before the range
  start, with silence under them, exactly as `play` from there would play
  them (`count_in_beats`, `beats_per_bar` accents, spacing as in playback).
  The file is longer by the count-in. The click track covers only the
  range. "Count in to where the music starts" applies as in playback: a
  range starting at the song start begins at `music_start` (the skipped
  intro is not in the file), unless `music_start` is at or past the range
  end.
- Both off (or missing): the export is unchanged (bit for bit). The UI
  appends " + click", " + count-in" or " + click, count-in" to the export
  summary when they are on.

### Playback, loop and count-in
- `play` at or past the end restarts from 0.
- If the loop is enabled and the position is outside `[a_sec, b_sec)`,
  `play` starts at `a_sec`.
- While the loop is enabled, reaching `b_sec` jumps to `a_sec` seamlessly
  (no count-in on wraps).
- **Count-in:** if `count_in_enabled`, `play` first plays `count_in_beats`
  clicks, then starts audio. Spacing: see "Count-in spacing" below
  (from the beats after the start), divided by `speed_percent / 100`. The audio starts exactly one
  interval after the last click, i.e. the count-in ends exactly at the start
  point (the user sets A on the pickup note). Number the clicks by how many
  beats before the start they are (`count_in_beats` … 1); click `k` is
  accented iff `k % beats_per_bar == 0`. Example: 7 beats, 4 per bar → clicks
  7 6 5 **4** 3 2 1, then audio (the first bar is a 3-beat pickup).
  During count-in, `playing: true` and `count_in_beats_left` counts down. The count-in leads into the first beat at or after the start: its last click is one beat before that beat, and any music before it (a pickup, or a long intro) plays under the last clicks, so `count_in_beats_left` can still be counting down while `position_sec` advances. Export follows the same rule.
  Without a beat grid, use 0.5 s spacing.
  **Count-in spacing** comes from the beats being counted into: of the
  next 8 beat intervals starting at the target beat (fewer near the end
  of the grid), the longest interval that at least 3 of them agree with
  within 10 %, taking the upper median of that group; then divided by
  `speed_percent / 100`. Odd bars, half-beat tracking in a busy intro and
  a missed beat don't change it; an intro before the target never does.
- **Count in to where the music starts** (`count_in_to_music_start`,
  default on): when the count-in is on and `play` starts at the song
  start (0, or before the first beat), playback starts at `music_start`
  instead (if the song has one and, with a loop, it lies before `b_sec`).
  `playback-position` reports `music_start` right away, so the playhead
  shows the jump. A start the user placed later is never moved. Setting
  off, or no `music_start`: unchanged.
- **Click track:** if `enabled`, play a click on every beat of the grid that
  playback passes, with an accent on the effective downbeats (see Meter).
  With `subdivision` 2 or 3, add 1 or 2 softer clicks evenly spaced between
  consecutive beats. Clicks follow song time, so they stay in sync at any
  speed.

### Meter
- The UI owns meter overrides. It computes the **effective downbeats** from
  the raw grid plus overrides (`meter.ts`) and sends both in
  `set_meter_overrides`. The backend **uses `downbeats` as-is** for click
  accents and treats `overrides` as informational (the UI persists them via
  `save_song_settings`). `downbeats` values are taken from `BeatGrid.beats`,
  so they compare equal as f64; still, match with a small tolerance (1 ms).
- Before the first `set_meter_overrides`, use the grid's own downbeats.
- The UI sends `set_meter_overrides` after each `get_beats` that returns a
  grid and after every override change.

## Startup sequence (what the UI does)

1. `listen` to all events, `get_library`, `get_app_settings`.
2. User picks/drops a file → `open_song(path)`.
3. UI sends `set_mix`, `set_speed`, `set_pitch`, `set_loop`, `set_click` with
   its current state, then `get_waveform(1200)`; if `beats_ready`,
   `get_beats` + `set_meter_overrides`; if `!stems_cached`, `start_separation`.
4. On `beats-ready`: `get_beats`, `set_meter_overrides`, `set_click`.

## Drag and drop

The UI uses `getCurrentWebview().onDragDropEvent` (Tauri 2 API, enabled by
default) and calls `open_song` with the first dropped path. No backend command
is needed; keep `dragDropEnabled` at its default (true) in `tauri.conf.json`.

## Capabilities

`app/src-tauri/capabilities/default.json` needs `core:default` (events and
webview drag-drop). The file dialogs are opened from Rust inside
`pick_audio_file` / `pick_export_path` (tauri-plugin-dialog's Rust API), so the
UI needs no dialog JS permission and no `@tauri-apps/plugin-dialog` package.

## Static files

`/THIRD_PARTY_LICENSES.html` (from `app/public`, generated by
`tools/third_party_licenses/generate.mjs`) is served with the UI in dev,
the browser mock and release builds; the About dialog shows it in an iframe.
No command is involved, so the mock backend needs nothing for it.

## Not covered by this contract (yet)

- Speed/pitch are global, not saved per song.
- No `get_player_state`: the UI re-sends its full state after `open_song`.
