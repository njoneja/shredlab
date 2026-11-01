# Architecture

ShredLab takes a song file, separates it into instruments (vocals,
drums, bass, guitar, piano, other) with an ONNX model running locally, and
then plays the stems back through a mixer. Musicians use it to practice with
an instrument removed, or to listen to one instrument by itself, with speed
change without pitch change, transposition, A–B loops and a click track.
Separation runs once per song and is cached; everything after that is mixing
of cached stems. The app is a Tauri 2 shell around a Rust core, with a
Svelte 5 UI.

```
Svelte UI (app/src)                 mixer, waveform, transport, export dialog
   │  Tauri commands / events
app/src-tauri                       commands → backend (state, background jobs)
   │
   ├─ stemx-separation   ONNX Runtime sessions, model registry, adapters, self-test
   │    └─ stemx-dsp     STFT/iSTFT, chunking, overlap-add
   ├─ stemx-beats        Beat This! pre/post-processing (ORT-free)
   ├─ stemx-audio        real-time engine, offline render, WAV/FLAC/MP3 export
   ├─ stemx-library      song folders, song.json, stem cache
   └─ stemx-io           decode, resample, WAV writing
```

## Repository layout

| Path | Owns |
|---|---|
| `crates/stemx-dsp` | `torch.stft`-compatible STFT/iSTFT, the Demucs hybrid spectrogram glue, chunking schedules, overlap-add, song normalization; Refine DSP (stereo-position mask, EQ with cut/shelf ends, presets, stereo-field analysis) and separation strength |
| `crates/stemx-io` | Decoding (symphonia: MP3, FLAC, WAV, OGG Vorbis, M4A AAC/ALAC), resampling (rubato), WAV writing (hound). Returns planar f32 stereo |
| `crates/stemx-separation` | ONNX Runtime (`ort`) environment and execution providers, `model.json` parsing, model registry, the two adapters, device self-test, the `Separator` entry point, and the ORT session for the beat model |
| `crates/stemx-beats` | Beat This! log-mel preprocessing, chunking and peak picking; tempo and meter helpers |
| `crates/stemx-audio` | Real-time playback engine, mixer, Signalsmith Stretch wrapper, click/count-in, safety limiter, offline render and export encoders |
| `crates/stemx-library` | One folder per song with a JSON manifest and cached stems |
| `crates/stemx-cli` | `stemx`, a headless CLI: list models, run self-tests, separate, decode |
| `app/src-tauri` | Tauri shell: commands, events, background jobs, settings, model discovery |
| `app/src` | Svelte 5 + TypeScript + Vite frontend |
| `tools/` | Python tooling: model setup and export, registry builder, reference implementations, fixture generators |
| `spikes/`, `scripts/` | Python prototypes and reference implementations the Rust code is validated against |
| `tests/fixtures/` | Golden vectors from the Python reference |

None of the `crates/` depend on Tauri. Paths (bundled resources, user data,
caches) are always passed in by the caller, so the same code runs in the
app, the CLI and tests.

## Separation pipeline

```
file ─ decode (stemx-io) ─ resample to 44.1 kHz stereo
     ─ chunking + STFT (stemx-dsp)            outside the ONNX graph
     ─ ONNX model (stemx-separation, ort)     network core only
     ─ iSTFT / mask multiply + overlap-add    outside the ONNX graph
     ─ stems (planar f32, one per model output)
```

The exported ONNX graphs contain only the network. STFT, iSTFT, chunking,
windowing and normalization run in Rust. Keeping them out of the graph is
what makes the models run on DirectML at all, and it lets the DSP be tested
bit-for-bit against the Python reference.

Each model declares an **adapter**, which defines its tensor I/O and the
glue around it:

| Adapter | Graph I/O | Glue |
|---|---|---|
| `demucs-hybrid` | waveform + magnitude spectrogram in; waveform + spectrogram out | Song normalization (mean/std of the mono mix), Demucs `apply_model` chunking: 25% overlap, triangular weights, short last chunk padded with neighbouring audio |
| `roformer-mask` | complex STFT in; complex masks out | `torch.stft(center=True)`, mask multiply, iSTFT, chunks with a symmetric Hamming window and an end-aligned tail chunk |

The exact tensor shapes and chunking rules are in
[MODEL_MANIFEST.md](MODEL_MANIFEST.md). Adding a model of a known family
needs only a new `model.json`; a new family needs a new adapter.

**Quality setting.** `Separator::separate_shifts` averages several
time-shifted passes (Demucs `shifts`). Offsets are deterministic
(0 and 0.25 s for two passes), so output is reproducible. The app offers
Standard (1 pass) and High (2 passes); the default is High on GPU and
Standard on CPU.

### Execution providers and the self-test

`Ep` covers CPU, DirectML (Windows), CoreML (macOS) and CUDA (behind the
`cuda` cargo feature). With device `auto` or `gpu`, `Separator::load` tries
each compiled-in GPU provider in order and uses it only if the session
builds **and** the model's self-test passes; otherwise it falls back to CPU.
Per-provider session options (for example `disable_graph_fusion` for
DirectML) come from the manifest.

The self-test exists because GPU backends can be wrong without failing: with
graph fusion enabled, DirectML produces garbage for the Demucs export with no
error. Each model ships a `selftest.json` with expected outputs for a
deterministic synthesized input (one segment, no chunking). The app compares
the GPU output against it by probe SNR and RMS. Results are cached per model
sha256, provider, ORT version and session overrides, so the test runs once
per machine. The ONNX file's sha256 is also checked against the manifest
(cached by size and mtime).

ONNX Runtime is linked statically from pyke's prebuilt binaries; on Windows
the only runtime DLL is `DirectML.dll`. See the README for build details.

## Beats

Beat and downbeat tracking uses Beat This! (CPJKU, MIT). `stemx-beats` holds
the model-agnostic parts: downmix to mono, resample to 22.05 kHz, log-mel
spectrogram, 1500-frame chunks, then peak picking and snapping downbeats to
beats. It has no ONNX Runtime dependency; inference is a closure, which the
app backs with `stemx_separation::BeatModel`. The app runs beats on CPU by
default because this model has no self-test. When detection is wrong, the
user can mark which beat is "one" and change beats per bar from any beat
onward; the overrides are saved with the song.

## Audio engine

`stemx-audio` plays a set of tracks (stems, plus the original mix) on the
default output device via cpal.

- A producer thread runs the pipeline: refine → mix → Signalsmith Stretch →
  crossfades → safety limiter → click → resample to the device rate. It
  stays about 30 ms ahead of the device in a lock-free ring buffer (`rtrb`).
- The cpal callback only pops frames and publishes the position through
  atomics: no locks or allocation on the real-time thread.
- Controls (gains, speed, pitch, loop, click, seek) reach the producer over
  a channel. Seeks, loop wraps, pause and mode changes are crossfaded.
- Speed and pitch use the vendored Signalsmith Stretch (MIT) through a small
  C wrapper (`cpp/stretch_wrapper.cpp`). Speed 1.0 at pitch 0 bypasses it.
- **Backing track from the original.** When at least half the stems are on
  and nothing is soloed, the mix is `original + Σ (gain_i − 1) · stem_i`,
  so everything the model missed (reverb, transients) stays in the backing
  track. Otherwise it is the plain sum of the enabled stems. `choose_mode`
  is shared by playback and export.
- **Refine.** Every stem runs through its own Refine chain (an STFT mask
  for a stereo window, then an EQ) before the mix, at the song rate, so
  before the stretcher. The window is an exception to the row's on switch:
  with the switch on the mask passes everything but the window, with it off
  only the window (the app sets the mask's run-time `invert` flag from the
  switch). With `s` the whole stem and `r` the heard part after EQ, the mix
  is `w·original + Σ (gain_i·r_i − w·s_i)` (`w` = 1 in the backing-track
  mode, 0 for the plain sum); an unrefined chain is an exact delay, so
  that is the formula above. The chain's
  latency (4096 samples) is hidden by feeding it ahead of the play
  position; chains re-prime whenever playback jumps (seek, loop, pre-roll).
- **Separation strength** re-derives the stems from the original with a
  sharper ratio mask (`stemx-dsp` `strength`) on a background thread in
  the app; the derived set replaces the cached one in memory.
- Click and count-in are generated from the beat grid, with accented
  downbeats. The count-in tempo comes from the beats after the start (the
  slowest pulse most of the next 8 intervals agree with). An optional
  "music start" in the click settings lets a count-in from the song start
  skip a quiet intro; the crate only applies it, the app detects it.

**Export** (`render` + `encode`) runs the same mix and stretch path offline
(the whole song or a range, played once, with short fades at cut points
inside the song), so the file matches what you hear, including speed
and pitch. If the click track or count-in is on, the same pipeline renders
the clicks into a separate buffer; the music goes through the export
limiter first and the clicks are added after it, as in playback, with the
count-in before the range start. Formats: WAV 16-bit (dithered) and 24-bit
(hound), FLAC 24-bit (flacenc), MP3 CBR (LAME). A look-ahead limiter on the
final sum keeps peaks below −0.1 dBFS.

## Library and stem cache

`stemx-library` stores one folder per song under `<data dir>/library`,
keyed by a hash of the file content:

```
songs/<song_id>/song.json                         manifest (schema 2)
songs/<song_id>/original.wav                      copy of the original mix
songs/<song_id>/stems/<model_id>_s<shifts>/<stem>.wav
```

Stems are cached per model and quality, so switching back is instant. They
are 16-bit WAV with a per-stem scale factor in `song.json`, so loud stems
never clip and disk use is half that of float WAV. The original is cached
too, so the backing track works after the source file moves. `song.json`
also holds the beat grid, the Refine settings and separation strength, and
per-song UI settings.

## App

`app/src-tauri` is a thin layer:

- `commands.rs`: `#[tauri::command]` wrappers (open song, start/cancel
  separation, play/pause/seek, set mix/speed/pitch/loop/click, get waveform
  and beats, export, settings).
- `backend.rs`: the state and jobs, Tauri-free. Events leave through a
  `Sink` closure. Decoding runs on a blocking worker; separation, beat
  tracking and export each run on their own background thread with
  cancellation through an `AtomicBool`; a 30 Hz thread emits the playback
  position (and the open Refine panel's stereo field). Events include
  `separation-progress`, `separation-done`, `beats-ready`,
  `export-progress`, `playback-position`, `refine-field` and
  `strength-progress`/`-done`.
- `models.rs`: finds models (bundled resources, then the user models folder,
  then `STEMX_MODELS_DIR`, then repo folders in debug builds).
- `settings.rs`: `settings.json` with the chosen model, quality, theme and
  "count in to where the music starts".
- `music_start.rs`: finds where the music starts after a quiet intro
  (per-beat loudness of the original vs the song's median, abrupt onsets
  only), so a count-in can lead straight into it.

The UI talks to the backend through one seam, `app/src/lib/api.ts`. Inside
Tauri it uses `api.tauri.ts`; in a plain browser it uses an in-memory mock,
so the UI runs with `pnpm dev`. The command and event contract is documented
in [app/src/lib/API.md](../app/src/lib/API.md).

## Models

Every separation model is a folder with the ONNX file, a `model.json`
manifest and a `selftest.json`; the format is in
[MODEL_MANIFEST.md](MODEL_MANIFEST.md).

- **Bundled:** Demucs v4 `htdemucs_6s` (MIT) and the Beat This! `final0`
  beat model (MIT), shipped as Tauri resources.
- **Personal:** models in the user models folder show up in the model
  picker marked as personal, with whatever license their manifest declares
  (often unknown). The public build never downloads or bundles them.
  `tools/add_personal_model.py` converts a checkpoint and installs it there.

The ONNX files are not in git. `tools/setup_models.py` downloads the
official checkpoints, exports them with the code in `spikes/` and
`tools/export/`, builds the registry folder and verifies the results.

Third-party notices for everything shipped (crates, ONNX Runtime, DirectML,
LAME, Signalsmith, fonts, models, JS) are generated into
`app/public/THIRD_PARTY_LICENSES.html` by `tools/third_party_licenses/generate.mjs`
and shown in the app under Settings → About ShredLab.

## Reference implementations and tests

The Python code in `spikes/` and `tools/reference/` is the reference for the
Rust port; each Rust component is checked against it:

- `tests/fixtures/` (committed, generated by `tools/make_fixtures.py`):
  model-free STFT, iSTFT and chunking vectors, used by `stemx-dsp` tests.
- `tests/fixtures/local/` (not committed, `tools/make_goldens.py`):
  full-model outputs on a real excerpt, used by the ignored golden tests in
  `stemx-separation`.
- `stemx-beats/tests/reference.rs`: log-mel and peak picking against the
  Python reference.

The debug app also has a headless end-to-end check (`STEMX_AUTOTEST`); see
the README.

## Licensing

- Code: GPL-3.0-or-later, with a section 7 permission for combining with
  Microsoft DirectML ([LICENSE-EXCEPTION](../LICENSE-EXCEPTION)). The name
  and logo are trademarks ([TRADEMARKS.md](../TRADEMARKS.md)). Some
  dependencies are Apache-2.0 only, so the code cannot move to GPL-2.0.
- Model weights carry their own licenses, declared in each `model.json` and
  shown in the model picker. Both bundled models are MIT.
- Signalsmith Stretch and Signalsmith Linear are vendored (MIT).
- MP3 export uses LAME (LGPL), compiled from source and statically linked.
  Publishing the full source satisfies the relinking requirement. Details in
  [crates/stemx-audio/THIRD_PARTY.md](../crates/stemx-audio/THIRD_PARTY.md).
