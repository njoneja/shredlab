# app/src

The ShredLab UI: Svelte 5 (runes) and TypeScript, built with Vite into
`app/dist` (generated), which the Tauri shell in
[app/src-tauri](../src-tauri) embeds. In a plain browser it runs against an
in-memory mock of the backend.

## Boundaries

- Owns what the user sees and the UI state: the mixer rows (on, solo,
  volume in dB), the loop, click and count-in settings, meter overrides and
  the effective downbeats computed from them, which stems an export
  includes, the theme on `<html>`, and the per-song settings blob
  (`SongSettings`), which the backend stores verbatim without reading it.
- Talks to the backend only through `api` from `lib/api.ts`, which is
  `lib/api.tauri.ts` (Tauri `invoke`/`listen`) inside Tauri and
  `lib/api.mock.ts` elsewhere. Components never import `@tauri-apps/api`.
  The contract is [lib/API.md](lib/API.md); the TypeScript side of it is
  `lib/types.ts` (`Backend`, `EventMap` and all payload types).
- Does no audio work. Mixing rules that the UI must predict (audibility,
  the "backing track built from the original" note, the Refine curves) are
  mirrored in `lib/mix.ts`, `lib/refine.ts` and `AppState.audible`; the
  backend is authoritative.
- Runtime dependency: `@tauri-apps/api` only. Fonts are bundled
  (`assets/fonts/`, licenses in [app/THIRD_PARTY.md](../THIRD_PARTY.md)).

## Layout

Entry: `../index.html` runs `../public/theme-init.js` (theme before first
paint), then `main.ts` mounts `App.svelte` and loads `app.css`. All state and
backend calls are in `lib/store.svelte.ts`; read it next.

- `App.svelte`: page layout, global keyboard shortcuts, mounting of dialogs and the toast.
- `app.css`: fonts, theme tokens per `data-theme`, shared button and control styles.
- `globals.d.ts`: `__SHREDLAB_COMMIT__`, the short commit hash that `../vite.config.ts` defines at build time; About shows it after the version.
- `lib/store.svelte.ts`: `AppState` (the `app` singleton): every `$state`, the event listeners, and one method per user action.
- `lib/api.ts`, `lib/api.tauri.ts`, `lib/api.mock.ts`, `lib/types.ts`, `lib/API.md`: the backend seam (above).
- `lib/mix.ts`: dB and linear gain, the volume slider law, the backing-track-from-original rule.
- `lib/meter.ts`: effective downbeats from the grid and overrides, bar numbers, time formatting.
- `lib/refine.ts`: Refine defaults, quick stereo presets, the stereo window and EQ response (same formulas as `shredlab-dsp`), strength names.
- `lib/theme.ts`, `lib/recent.ts`: theme application and the localStorage mirror; recent-list and size formatting.

Components, all in `lib/`:

- Frame: `Header.svelte` (with `OpenMenu.svelte` and `SettingsPopover.svelte`: model, quality, stem format, theme), `EmptyState.svelte` (start screen and recent songs), `SeparationBanner.svelte`.
- Song view: `Waveform.svelte` (canvas: peaks, bars, loop handles, playhead), `Mixer.svelte` (stem rows; opens `RefinePanel.svelte`), `Transport.svelte` (play, master volume, click and count-in, speed and pitch; opens `MeterPanel.svelte`).
- Dialogs: `ExportDialog.svelte`, `ShortcutsHelp.svelte`, `AboutDialog.svelte`, `StorageDialog.svelte`, `RemoveRecentDialog.svelte`. Each has an open flag in `AppState`.
- Small parts: `Icon.svelte` (inline SVG paths), `Wordmark.svelte`.

## How it works

**State.** `AppState` is a class with `$state` fields and `$derived`
values (effective downbeats, audible stems, export summary, whether the
loaded stems match the settings). Components read `app.*` directly and call
its methods; the methods update local state first, then call `api`. Only
`StorageDialog.svelte` and `RemoveRecentDialog.svelte` call `api` directly.
Large values that are replaced whole (`grid`, `field`, `refinePresets`) are
`$state.raw`. Bookkeeping that must not trigger renders (`saveTimer`,
`countInSet`, strength job ids) is plain fields.

**Startup** (`AppState.init`, from `onMount` in `App.svelte`): listen to
every event, fetch Refine presets, register the file drop handler, then load
the library and app settings. `openPath` calls `open_song`, resets the UI
from `SongInfo` (saved settings, Refine, strength), pushes the whole
transport state (`set_mix`, `set_speed`, `set_pitch`, `set_loop`,
`set_click`), asks for 1200 waveform points, loads beats if ready and starts
a separation if no stems are cached. The backend keeps no player state the
UI can query, so this push is what makes the two agree.

**Events.** Each listener drops payloads whose `song_id` isn't the open
song. Strength events also carry a job id: the UI waits for the newest job
it knows (`strengthWait`) and remembers the newest finished one
(`strengthDoneJob`), because `strength-done` can arrive before the command
that started the job returns.

**Persistence.** Per-song settings (`version: 1`: mix on and volume, loop,
count-in, subdivision, meter overrides) go to `save_song_settings` on
every change (`saveSettings`); the backend debounces the write and flushes
it on song switch, close and quit, so nothing is pending in the UI. Solo,
speed and pitch are not saved. An unknown `version` loads defaults. Refine
and strength are saved by the backend, not in this blob. App settings live
in the backend; the store keeps the last `AppSettingsInfo` it returned.

**Live controls.** Sliders send on every input and save on release
(`set_master_volume(v, save)`, click volume via `set_click` while dragging
and `set_click_volume` on release). `RefinePanel.svelte` coalesces drags to
one `set_refine` per animation frame and sends the last pending change when
it closes. The playhead follows `playback-position` events; the UI never
runs its own clock.

**Theme.** `public/theme-init.js` sets `data-theme` from the localStorage
key `shredlab.theme` before first paint (no flash). The backend's saved
theme wins once `get_app_settings` returns; `applyTheme` updates both.
Canvas components read colors from CSS variables, so their draw effects
depend on `app.theme`.

**Mock.** `lib/api.ts` picks the mock when `window.__TAURI_INTERNALS__` is
missing. It fakes demo songs (Open cycles through them; drops get fake
paths), separation progress, cached stem sets per model and quality, a beat
grid with a music start, playback position, the count-in, export, strength
jobs and a synthetic stereo field, and plays a WebAudio click; otherwise it
is silent. It persists to localStorage keys starting with `shredlab.mock.`
(clear them to reset). A "Mock backend" badge shows in the corner.

**Build.** `vite build` writes `app/dist`; the target is `chrome105` on
Windows (WebView2) and `safari15` elsewhere (WebKit). The dev server is
fixed at port 1420 (`strictPort`), matching `devUrl` in
`../src-tauri/tauri.conf.json`. Files in `../public` are served at the root,
including `THIRD_PARTY_LICENSES.html` (generated by
`tools/third_party_licenses/generate.mjs`), which the About dialog shows in
an iframe.

## Invariants and gotchas

- Changing the contract means changing, together: `lib/API.md`,
  `lib/types.ts`, `lib/api.tauri.ts`, `lib/api.mock.ts`, and on the Rust side
  `app/src-tauri/src/commands.rs` and `types.rs`. The `Backend` interface
  makes TypeScript catch a missing mock or Tauri method; nothing checks the
  names or JSON shapes against Rust. A typo in an `invoke` name or argument
  key fails only at run time.
- Argument keys are camelCase (`{ positionSec }`) and map to snake_case Rust
  parameters; payload fields are snake_case JSON. `set_loop` takes
  `loopState`, because `loop` is a keyword in Rust.
- Pass `$state.snapshot(...)` (or a fresh plain object) to `api`, never a
  reactive proxy; the store does this for the mix, loop, click, overrides and
  Refine params.
- After an `await`, check that `this.song?.song_id` is still the song you
  started with before writing results (`loadBeats` and the waveform do).
- Long work must go through events, never a long-awaited command: the
  backend returns from `start_separation`, `export_mix` and `set_strength`
  at once.
- The UI keeps solo exclusive, sends all six stems in every `set_mix`, and
  sends linear gains (0 to about 2.0); dB exists only in the UI.
- Keyboard shortcuts in `App.svelte` must match the list in
  `lib/ShortcutsHelp.svelte` and [docs/HELP.md](../../docs/HELP.md). They use
  `e.code` (layout-independent digits) and are ignored while typing in a
  field or while any dialog is open; a new dialog must be added to that guard
  in both `onKeydown` and `onKeyup`. `Mixer.svelte` keeps arrow keys on a
  focused volume slider from reaching the global seek and speed shortcuts.
- The CSP (in `tauri.conf.json`) allows only `'self'` scripts and fonts:
  no inline `<script>`, no remote assets, and fonts must stay files (the
  Vite config never inlines `.woff2`). Inline styles are allowed.
- Theme names are listed in `lib/types.ts` (`THEMES`), `lib/theme.ts`
  (`THEME_NAME`), `../public/theme-init.js`, `../index.html` (page
  background), `app.css` (tokens) and `app/src-tauri/src/settings.rs`.
- Edges of clickable or editable controls (buttons, segmented controls,
  inputs, selects, switches) use `--control-border`; panels, dialogs and
  dividers use the fainter `--border`. Every theme defines both.
- Drag and drop comes from Tauri's webview event (`onFileDrop`), not the
  DOM; keep `dragDropEnabled` at its default in `tauri.conf.json`.
- Adding an npm dependency that ships in the UI changes the third-party
  notices; CI runs `node tools/third_party_licenses/generate.mjs --check`.

## Testing

There are no JavaScript unit tests; correctness of the contract is covered by
the backend tests and the autotest (see [app/src-tauri](../src-tauri/README.md)).

```sh
pnpm --dir app check    # svelte-check: TypeScript and Svelte diagnostics
pnpm --dir app build    # production build into app/dist
pnpm --dir app dev      # UI alone with the mock: http://localhost:1420
pnpm dev                # (repo root) the real app with hot reload
```

`pnpm check` at the repo root runs the frontend check and build along with
the Rust checks. The mock needs only Node and the installed packages
(`pnpm --dir app install`); the real app also needs Rust and the models.

## Common changes

- **Calling a new command:** after the backend side (see
  [app/src-tauri](../src-tauri/README.md)), add the method to `Backend` in
  `lib/types.ts`, implement it in `lib/api.tauri.ts` (`invoke` with camelCase
  keys) and `lib/api.mock.ts` (plausible fake behavior), document it in
  `lib/API.md`, and call it from an `AppState` method.
- **Handling a new event:** add it to `EventMap` in `lib/types.ts`, listen in
  `AppState.init()` with the `song_id` check, and emit it from the mock.
- **A per-song UI setting:** add the field to `SongSettings` (optional, for
  older saves), write it in `settingsSnapshot`, restore it with a default in
  `applySettings`, and call `saveSettings()` where it changes.
- **An app setting:** a field in `AppSettingsInfo`, a setter command, a
  store method that applies it optimistically and reloads app settings on
  error (like `setStemFormat`), and a control in `SettingsPopover.svelte`.
- **A dialog:** an open flag and open/close methods in `AppState`, an
  `{#if}` mount in `App.svelte`, and the flag in the keyboard guard.
- **A theme:** every place listed under the theme gotcha above.
