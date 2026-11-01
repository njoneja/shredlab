# Stem Extractor: UI ↔ backend contract

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
  independent of playback speed). Gains are **linear** (1.0 = unity).
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
SeparatedWith  { model_name: string, device_label: string }
  // e.g. { "model_name": "Demucs 6-stem", "device_label": "GPU" }
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
  settings: SongSettings | null           // last value given to save_song_settings, verbatim
}

LibraryEntry {
  song_id: string, path: string, title: string, artist: string | null,
  duration_sec: number, stems_cached: boolean,
  last_opened_ms: number    // unix epoch milliseconds
}

SeparationStage = "loading_model" | "decoding" | "separating" | "writing" | "beats"

StemMixState { stem: StemName, on: boolean, volume: number /* 0..1 */, solo: boolean }

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

BeatGrid { beats: number[], downbeats: number[], bpm: number | null }
  // beats ascending, seconds. downbeats ⊂ beats (same f64 values).
  // bpm: overall tempo estimate (e.g. 60 / median beat interval).

ExportFormat = "wav" | "flac" | "mp3"

ExportRequest {
  path: string,              // absolute output path, extension already included
  format: ExportFormat,
  stems: { stem: StemName, volume: number }[],   // exactly the stems to mix, with gains
  speed_percent: number,     // 50..150, apply time-stretch like playback
  semitones: number,         // -12..12
  cents: number              // -50..50
}

SongSettings   // OPAQUE to the backend: store the JSON value and return it verbatim.
```

## Commands

| Command | Rust params | Returns | Notes |
|---|---|---|---|
| `pick_audio_file` | – | `string \| null` | Native open dialog (tauri-plugin-dialog). Filter: mp3, wav, flac, m4a, aac, ogg, opus. `null` if cancelled. |
| `pick_export_path` | `default_name: String, format: ExportFormat` | `string \| null` | Native save dialog. `default_name` has no extension (e.g. `"Song (mix)"`); add `.<format>` and a filter for that format. Returns the full path or `null`. |
| `open_song` | `path: String` | `SongInfo` | Decode (or start decoding) the file and make it current. Stops playback, position = 0, loop cleared. If stems are cached, load them so playback uses stems immediately. If a separation is running for another song it is cancelled (emit `separation-error` with `cancelled: true` for that song). Adds/updates the library entry. Kick off beat analysis in the background if the grid is not cached; emit `beats-ready` when done. May return before decoding finishes only if `duration_sec` is known. |
| `close_song` | – | `()` | Stop playback, cancel separation, drop the current song. |
| `get_library` | – | `LibraryEntry[]` | Recent songs, most recently opened first, max ~20. |
| `start_separation` | – | `SeparationStarted` = `SeparatedWith` | Starts separating the current song on a background thread. Returns immediately with the model and device that will be used. `Err` if already running or no song. Progress via events below. |
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

## Events

| Event | Payload | When |
|---|---|---|
| `playback-position` | `{ position_sec: number, playing: boolean, count_in_beats_left: number }` | ~30 Hz while playing (including during count-in) **and** once immediately after every play, pause, seek, loop wrap, and end of song. `count_in_beats_left` = count-in clicks still to come (0 when not counting in). |
| `separation-progress` | `{ song_id, stage: SeparationStage, fraction: number, model_name, device_label }` | Throttle to ≤ 10 Hz. `fraction` is **overall** progress 0..1, monotonic across stages. |
| `separation-done` | `{ song_id, separated_with: SeparatedWith }` | Stems written and cached. Playback switches from the original to the stems seamlessly at the current position (keep playing if playing). |
| `separation-error` | `{ song_id, message: string, cancelled: boolean }` | Failure or cancel. Playback of the original continues. |
| `beats-ready` | `{ song_id }` | Beat grid for that song is available via `get_beats`. Also fine to emit after separation if beats are computed in the `beats` stage. |
| `export-progress` | `{ fraction: number }` | ≤ 10 Hz, 0..1. |
| `export-done` | `{ path: string }` | File complete. |
| `export-error` | `{ message: string, cancelled: boolean }` | Failure or cancel. |

Events for a `song_id` that is no longer current are ignored by the UI.

## Behaviour rules

### Before / after separation
- Without stems, playback uses the **original mix** (mix settings have no
  effect). With stems, playback is the sum of the audible stems.
- Separation runs while the original plays; playback must not stutter.

### Audibility (playback)
- If **any** stem has `solo: true`, only stems with `solo: true` play.
- Otherwise, stems with `on: true` play.
- An audible stem plays at `volume` (linear). The UI keeps solo exclusive
  (at most one), but the backend should implement the general rule above.

### Export
- Mix exactly `req.stems` at their volumes (the UI has already applied the
  switches and **ignores solo**). Apply `speed_percent`, `semitones`, `cents`
  as in playback. Whole song, no click, no loop. Avoid clipping (a limiter or
  float WAV is fine; MP3 at ≥ 192 kbps).

### Playback, loop and count-in
- `play` at or past the end restarts from 0.
- If the loop is enabled and the position is outside `[a_sec, b_sec)`,
  `play` starts at `a_sec`.
- While the loop is enabled, reaching `b_sec` jumps to `a_sec` seamlessly
  (no count-in on wraps).
- **Count-in:** if `count_in_enabled`, `play` first plays `count_in_beats`
  clicks, then starts audio. Spacing = the local beat interval at the start
  position (from the beat grid: the interval of the beat pair around the
  start), divided by `speed_percent / 100`. The audio starts exactly one
  interval after the last click, i.e. the count-in ends exactly at the start
  point (the user sets A on the pickup note). Number the clicks by how many
  beats before the start they are (`count_in_beats` … 1); click `k` is
  accented iff `k % beats_per_bar == 0`. Example: 7 beats, 4 per bar → clicks
  7 6 5 **4** 3 2 1, then audio (the first bar is a 3-beat pickup).
  During count-in, `playing: true` and `count_in_beats_left` counts down.
  Without a beat grid, use 0.5 s spacing.
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

1. `listen` to all events, `get_library`.
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

## Not covered by this contract (yet)

- Speed/pitch are global, not saved per song.
- No `get_player_state`: the UI re-sends its full state after `open_song`.
