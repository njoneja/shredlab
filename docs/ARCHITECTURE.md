# Architecture

ShredLab takes a song file, separates it into instruments (vocals,
drums, bass, guitar, piano, other) with an ONNX model running locally, and
then plays the stems back through a mixer. Musicians use it to practice with
an instrument removed, or to listen to one instrument by itself, with speed
change without pitch change, transposition, A–B loops and a click track.
Separation runs once per song and is cached; everything after that is mixing
of cached stems. The stems are also analyzed for the song's tuning, chords
and notes, and the results are cached with the song. The app is a Tauri 2
shell around a Rust core, with a Svelte 5 UI.

This page is the map: what each piece does and how they connect. Each
directory has a README with the detail (layout, invariants, tests, common
changes).

```
Svelte UI (app/src)                 mixer, waveform, transport, export dialog
   │  Tauri commands / events
app/src-tauri                       commands → backend (state, background jobs)
   │
   ├─ shredlab-separation   ONNX Runtime sessions, model registry, adapters, self-test
   │    └─ shredlab-dsp     STFT/iSTFT, chunking, overlap-add, Refine DSP
   ├─ shredlab-beats        Beat This! pre/post-processing (ORT-free)
   ├─ shredlab-tuning       tuning offset, tuning guess, live pitch detector
   ├─ shredlab-chords       chord detection from the stems and beats
   ├─ shredlab-notes        notes of one stem: Basic Pitch glue (ORT-free) or pitch tracker
   ├─ shredlab-audio        real-time engine, offline render, WAV/FLAC/MP3 export
   ├─ shredlab-library      song folders, song.json, stem cache
   └─ shredlab-io           decode, resample, WAV writing
```

## Repository layout

| Path | Owns |
|---|---|
| [crates/shredlab-dsp](../crates/shredlab-dsp/README.md) | `torch.stft`-compatible STFT/iSTFT, chunking, overlap-add, normalization; Refine DSP and separation strength |
| [crates/shredlab-io](../crates/shredlab-io/README.md) | Decoding, resampling, WAV writing; returns planar f32 stereo |
| [crates/shredlab-separation](../crates/shredlab-separation/README.md) | ONNX Runtime, execution providers, model registry, adapters, self-test, `Separator`, the beat and note model sessions |
| [crates/shredlab-beats](../crates/shredlab-beats/README.md) | Beat This! preprocessing and peak picking; tempo and meter helpers |
| [crates/shredlab-tuning](../crates/shredlab-tuning/README.md) | The song's offset from A440 and a guess at its tuning, measured on the stems; the live tuner's pitch detector |
| [crates/shredlab-chords](../crates/shredlab-chords/README.md) | Chord detection on the stems and the beat grid, at concert pitch, with a key estimate |
| [crates/shredlab-notes](../crates/shredlab-notes/README.md) | The notes of one stem at concert pitch (a lead line from Basic Pitch, or the pitch tracker for the bass), with bends, slides, hammer-ons and pull-offs, and chordal stretches |
| [crates/shredlab-audio](../crates/shredlab-audio/README.md) | Real-time engine, mixer, time stretch, click, limiter, offline render and export |
| [crates/shredlab-library](../crates/shredlab-library/README.md) | One folder per song with a JSON manifest and cached stems |
| [crates/shredlab-cli](../crates/shredlab-cli/README.md) | Headless CLI: models, self-tests, separation, decoding, info |
| [app/src-tauri](../app/src-tauri/README.md) | Tauri shell: commands, events, background jobs, settings, model discovery |
| [app/src](../app/src/README.md) | Svelte 5 + TypeScript + Vite frontend |
| [tools/](../tools/README.md) | Python tooling: model setup and export, registry, personal models, fixture generators, license notices, repo checks |
| [tools/reference/](../tools/reference/README.md) | Python reference implementations the Rust code is checked against |
| [spikes/](../spikes/README.md) | Prototypes; three of them are the export code for the bundled models |
| [scripts/](../scripts/README.md) | Model setup wrappers and separation experiments |
| [tests/fixtures/](../tests/fixtures/README.md) | Golden vectors from the Python reference |

None of the `crates/` depend on Tauri. Paths (bundled resources, user data,
caches) are always passed in by the caller, so the same code runs in the
app, the CLI and tests.

## Separation

```
file ─ decode (shredlab-io) ─ resample to 44.1 kHz stereo
     ─ chunking + STFT (shredlab-dsp)            outside the ONNX graph
     ─ ONNX model (shredlab-separation, ort)     network core only
     ─ iSTFT / mask multiply + overlap-add    outside the ONNX graph
     ─ stems (planar f32, one per model output)
```

The exported ONNX graphs contain only the network. STFT, iSTFT, chunking,
windowing and normalization run in Rust. Keeping them out of the graph is
what makes the models run on DirectML at all, and it lets the DSP be tested
bit-for-bit against the Python reference.

Each model declares an adapter (`demucs-hybrid` or `roformer-mask`) that
defines its tensor I/O and the glue around it. Adding a model of a known
family needs only a new `model.json`; a new family needs a new adapter. The
manifest format is in [MODEL_MANIFEST.md](MODEL_MANIFEST.md).

GPU backends can be wrong without failing (DirectML with graph fusion
produces garbage for Demucs, with no error). So `Separator::load` uses a GPU
provider only if the session builds and the model's self-test passes against
the expected outputs in its `selftest.json`, and falls back to CPU otherwise.
Results are cached, so the test runs once per machine. The app offers
Standard (1 pass) and High (2 time-shifted passes) quality; the default is
High on GPU and Standard on CPU.

## Beats

Beat and downbeat tracking uses Beat This! (CPJKU, MIT). `shredlab-beats`
holds the model-agnostic parts and has no ONNX Runtime dependency; the app
runs inference through `shredlab_separation::BeatModel`, on CPU by default
because this model has no self-test. Beats come from the network's peaks;
downbeats from a Viterbi decoder over the beats that keeps bars a
consistent length. The per-beat downbeat scores are cached with the grid,
so a decoder change re-decodes cached songs without the model. When
detection is wrong, the user can mark which beat is "one" and change beats
per bar from any beat onward; the overrides are saved with the song.

## Playback and export

`shredlab-audio` plays the stems plus the original mix. Stems are held in
memory as 16-bit integers with a per-stem scale (half the memory of f32,
with rounding noise about 100 dB below each stem's peak); the original stays
f32. A producer thread
runs refine → mix → time stretch → crossfades → limiter → click → volume →
resample → reference tone and keeps a lock-free ring buffer filled; the
device callback only pops frames. The resample step converts to the device
rate and also applies the tuning's playback-rate correction (pitch and
tempo together); the reference tone comes after it, so it stays at its
absolute pitch. The UI's controls reach the producer over a channel.

When at least half the stems are on and nothing is soloed, the backing track is built from the original mix
minus the removed stems rather than from the sum of stems, so whatever the
model missed stays in. Refine (a per-stem stereo-window mask and EQ, DSP in
`shredlab-dsp`) runs before the mix. Separation strength re-derives sharper
stems from the original on an app background thread.

Export runs the same mix and stretch path offline, so the file matches what
you hear, including speed, pitch, the click track and the playback-rate
correction (applied by resampling the finished file). Reference tones are
never exported.

## Chords

After separation the app runs three analyses on the stems, each saved in
`song.json` with its algorithm version and rerun only when that version
changes or new stems arrive: beats (on the mix), the tuning offset, and the
chords. The chords need the other two: `shredlab-chords` names pitch classes
at the song's own reference (the measured offset removed) and segments the
song on a grid of subdivisions of the detected beats, so a chord change
pushed an eighth before the beat stays where it is played. Its result is
concert pitch only. The UI names the chords: the shape to fret in the
player's tuning (Standard unless set) to play what is heard after any
pitch shift, and what is heard when that differs (`app/src/lib/chords.ts`),
so changing Pitch or the tuning never reruns the detection.

## Notes

Once the stems, beats and tuning are in, the app also finds the single
notes of each pitched stem (guitar, bass, vocals, keys, other; not drums)
with `shredlab-notes`, one stem at a time in the background. Unlike the
chords, the notes are found on the stem as the mixer has it: after the
separation strength and after Refine's stereo mask (the part of the stereo
field the mask selects), so two guitars panned apart can be told apart.
Refine's EQ is left out. The song's two note settings in the notes panel,
saved in `song.json`, set the model path's thresholds: Fast runs (Fewer,
Normal, More: how quick runs are split into notes) and Quiet notes (Skip,
Include: whether softer notes join the lead line); the bass ignores them.
Each result is saved in `song.json` per stem and source, the source naming
the stem set, the strength, the mask and the note settings other than the
defaults, and a few
sources are kept per stem so going back to an earlier setting is
instant. A changed mask is followed a second after the last change.
While an A-B loop is on, the panel's settings apply to the loop only: the
UI asks `detect_loop_notes` for the shown stem, which detects the loop
alone (plus context on each side, from the start of a model window so the
model hears it as in the whole song) and clips the result to the loop.
The UI draws it inside the loop and the song's notes outside; nothing is
saved in `song.json`, and loop results are cached in the song folder
(`loop_notes.json`, the most recent 20). Like
the chords, the notes are concert pitch only; `app/src/lib/notes.ts` names
them in the same frame as the chords, and the notes panel shares the
chords panel's window.

The bass goes through `shredlab-notes`' own pitch tracker: it plays one
note at a time, and the tracker keeps quick repeated low notes the model
misses. The other stems go through Spotify's Basic Pitch model, which hears
several notes at once; the crate picks the lead line from its notes (one at
a time, following the melody's register, so a solo over rhythm guitar in
the same stem keeps its notes) and marks strummed stretches as chordal.
Every other note the model heard is kept too, at its own length, and the
notes panel draws those faintly behind the lead line.
Like the beat model, the network runs through
`shredlab_separation::NoteModel` (CPU) as a closure, and the crate stays
ORT-free. Without the model file every stem uses the pitch tracker.

## Live tuner

The live tuner listens to one channel of an input device, never a downmix.
`shredlab-audio`'s `InputCapture` opens the device on its own worker
thread, independent of the playback engine; the cpal callback copies the
channel into a ring buffer, and the worker drains it into a callback. In
the app (`app/src-tauri/src/tuner.rs`) that callback feeds
`shredlab-tuning`'s `LiveDetector` (McLeod pitch method, one reading per
hop of about 10 ms) and passes the latest reading, with the highest level
since the last one, to a `tuner-reading` event about 30 times a second.
The backend's position thread checks the capture for a stream error. The
UI maps a frequency to a string of the player's tuning or the nearest
note, and smooths the needle (`app/src/lib/tuning.ts`). Starting playback
stops the tuner, so the speakers don't feed the microphone.

## Library and stem cache

`shredlab-library` keeps one folder per song under `<data dir>/library`,
keyed by a hash of the file content, with `song.json` (stem sets, beat grid,
Refine and strength settings, per-song UI settings), a copy of the original
mix and the cached stems. Stems are cached per model and quality, so
switching back is instant.

## App

`app/src-tauri` is a thin layer: `commands.rs` wraps a Tauri-free
`backend.rs`, which owns the state and runs decoding, separation, beat
tracking, strength and export off the UI thread, reporting through events.
The UI talks to the backend through one seam, `app/src/lib/api.ts`: the
Tauri implementation inside the app, an in-memory mock in a plain browser.
The command and event contract is in
[app/src/lib/API.md](../app/src/lib/API.md).

## Models

Every model is a folder with the ONNX file, `model.json` and `selftest.json`.

- Bundled: Demucs v4 `htdemucs_6s` and the Beat This! `final0` beat model
  (both MIT), and Spotify's Basic Pitch note model (Apache-2.0), shipped as
  Tauri resources. The beat and note models are single ONNX files without
  a folder.
- Personal: models in the user models folder show up in the picker marked as
  personal, with whatever license their manifest declares. The public build
  never downloads or bundles them.

The ONNX files are not in git; `tools/setup_models.py` downloads the
official checkpoints and exports them, and downloads Basic Pitch's ONNX
file as published (see [tools/](../tools/README.md)).
Third-party notices for everything shipped are generated into
`app/public/THIRD_PARTY_LICENSES.html` and shown in the app.

## Testing against the reference

The Python code in `spikes/` and `tools/reference/` is the reference for the
Rust port. Model-free STFT and chunking vectors are committed in
`tests/fixtures/`; full-model goldens are generated locally and used by
ignored tests. Each directory's README says which tests use what, and
[DEVELOPING.md](DEVELOPING.md) covers the environments and the headless
end-to-end check.

## Licensing

- Code: GPL-3.0-or-later, with a section 7 permission for combining with
  Microsoft DirectML ([LICENSE-EXCEPTION](../LICENSE-EXCEPTION)). The name
  and logo are trademarks ([TRADEMARKS.md](../TRADEMARKS.md)). Some
  dependencies are Apache-2.0 only, so the code cannot move to GPL-2.0.
- Model weights carry their own licenses, declared in each `model.json` and
  shown in the model picker. The bundled Demucs and Beat This! models are
  MIT; Basic Pitch is Apache-2.0.
- Signalsmith Stretch and Signalsmith Linear are vendored (MIT); MP3 export
  uses LAME (LGPL), statically linked. Details in
  [crates/shredlab-audio/THIRD_PARTY.md](../crates/shredlab-audio/THIRD_PARTY.md).
