# Stem Extractor

Stem Extractor is a desktop practice and transcription tool for musicians.
It separates a song into instruments (vocals, drums, bass, guitar, piano,
other) on your own machine, so you can play along with an instrument
removed or listen to one by itself, with speed change without pitch change,
transposition, A–B loops and a click track with count-in. It is built with Tauri 2 and a Rust core, runs separation models with
ONNX Runtime (DirectML / CoreML / CPU), and targets Windows and macOS.

Status: works end to end on Windows (separate, beats, mixer, speed/pitch,
loops, click, export); macOS has not been built or run yet. See
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for how it fits together and
[docs/DEVELOPING.md](docs/DEVELOPING.md) for practical development notes.

## Repository layout

| Path | What |
|---|---|
| `app/` | Tauri app: Svelte 5 + TypeScript + Vite frontend (`app/src`), Rust shell (`app/src-tauri`) |
| `crates/stemx-dsp` | STFT/iSTFT, chunking, overlap-add |
| `crates/stemx-io` | Decode, encode, resample |
| `crates/stemx-separation` | ONNX Runtime sessions, model registry, adapters |
| `crates/stemx-audio` | Real-time audio engine, offline render, WAV/FLAC/MP3 export |
| `crates/stemx-beats` | Beat/downbeat detection (Beat This! ONNX) |
| `crates/stemx-library` | Song folders, JSON manifest, stem cache |
| `crates/stemx-cli` | `stemx`, headless CLI |
| `tools/` | Python tooling: model setup/export, model folders, fixtures ([tools/README.md](tools/README.md)) |
| `spikes/`, `scripts/` | Python reference implementations (Rust ports are validated against them; [spikes/README.md](spikes/README.md)) |
| `docs/` | Architecture, development notes, model manifest format |

## Building from scratch

Windows 11 x64 is the tested platform. macOS (Apple Silicon) should work but
is **untested**: the CoreML code path compiles but has never run on a Mac
(see [macOS notes](#macos-notes)).

### 1. Prerequisites

- **Rust** stable via [rustup](https://rustup.rs), with `rustfmt` and `clippy`
  (`rustup component add rustfmt clippy`).
- **Node.js 24** ([nodejs.org](https://nodejs.org)) and **pnpm 12**
  ([pnpm.io/installation](https://pnpm.io/installation)).
- A C/C++ compiler (Tauri, the vendored Signalsmith Stretch and LAME are
  compiled from source):
  - Windows: [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)
    with the **Desktop development with C++** workload, and WebView2 (preinstalled
    on Windows 11).
  - macOS: Xcode Command Line Tools (`xcode-select --install`).
  - Details: [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/).
- **uv** ([docs.astral.sh/uv](https://docs.astral.sh/uv/getting-started/installation/))
  or **Python 3.12** ([python.org](https://www.python.org/downloads/)), only
  for the one-time model setup.

### 2. Frontend dependencies

```sh
pnpm --dir app install
```

### 3. Models

The ONNX models are not in git (`models/` is ignored). They are built from
the official MIT-licensed checkpoints (Demucs v4 `htdemucs_6s`, Beat This!
`final0`). Every app build needs them, because the Tauri config bundles
them. One command does it all: creates a CPU-only Python venv
(`.venv-models`), downloads the checkpoints (~140 MB, sha256-checked),
exports the ONNX files and verifies them against reference outputs.

```powershell
# Windows (PowerShell)
.\scripts\setup-models.ps1
```

```sh
# macOS
./scripts/setup-models.sh
```

Or run `python tools/setup_models.py` with any Python 3.9+ directly. It takes
a few minutes (mostly downloading ~250 MB of PyTorch and ~140 MB of
checkpoints; the exports themselves take seconds to a minute) and about
1.7 GB of disk: ~1.4 GB for `.venv-models` (delete it afterwards if you
like) and ~320 MB in `models/`. Running it again skips everything that is
already there; `--force` rebuilds (old files are kept as `*.bak-*`),
`--out DIR` writes somewhere else. See [tools/README.md](tools/README.md).

### 4. Build and run

```sh
pnpm --dir app tauri dev                   # run the app (debug, hot reload)
pnpm --dir app tauri build --no-bundle     # release exe: target/release/shredlab(.exe)
pnpm --dir app bundle                      # installers: target/release/bundle/ (NSIS + MSI / .app + .dmg)
```

On Windows the release exe needs `DirectML.dll` next to it (the build puts
it in `target/release`; `pnpm bundle` ships it in the installers).

### 5. Tests and checks

```sh
pnpm --dir app build                       # once, see note below
pnpm --dir app check                       # type-check the frontend
cargo test --workspace                     # Rust unit tests
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

Note: the Tauri crate embeds the built frontend, so `cargo build`/`clippy`/
`test` on the whole workspace need `app/dist` to exist. Run
`pnpm --dir app build` once first (any `tauri dev`/`tauri build` also
creates it). They also need the models from step 3.

Headless end-to-end check (separates a song, detects beats and plays a few
seconds of audio): `STEMX_AUTOTEST=<song> STEMX_AUTOTEST_EXIT=1 pnpm --dir app
tauri dev` prints `AUTOTEST PASS`. Other environment overrides are listed in
[docs/DEVELOPING.md](docs/DEVELOPING.md).

### macOS notes

Not built or run yet; expect some fixing on the first try:

- There is no `DirectML.dll`; GPU inference goes through CoreML (with a
  self-test and CPU fallback). The Windows-only DLL is added to the bundle by
  `app/src-tauri/tauri.bundle.windows.conf.json`, which `pnpm bundle` uses
  only on Windows.
- Model setup needs macOS 14+ on Apple Silicon (onnxruntime 1.24 wheels).
- The app is unsigned and not notarized; Gatekeeper will complain.

## Developer notes

### Separation CLI (`stemx`)

```sh
cargo build --release -p stemx-cli
target/release/stemx models                                   # list models (+ license, personal flag)
target/release/stemx selftest [--model ID] [--device auto|cpu|gpu]
target/release/stemx separate song.mp3 --model htdemucs_6s --out out/song [--device auto|cpu|gpu]
target/release/stemx decode song.mp3 song.wav                 # decoder check (44.1 kHz float WAV)
```

Models are looked up in `--models-dir DIR` (repeatable), else in
`$STEMX_MODELS_DIR` (`;`-separated on Windows, `:` elsewhere), else in
`models/registry` and `models/personal` of the repo. `--device auto` uses
the GPU (DirectML on Windows, CoreML on macOS, CUDA with the `cuda` cargo
feature) only if its self-test passes, else the CPU. Self-test results are
cached in `target/stemx-cache/` (`--cache-dir`).

Slow golden tests against the Python reference (need
`tests/fixtures/local/` and the model folders):

```sh
cargo test --release -p stemx-separation --test golden -- --ignored --nocapture --test-threads 1
```

### ONNX Runtime binaries

`stemx-separation` uses the `ort` crate with `download-binaries`: at build
time it downloads pyke's prebuilt ONNX Runtime (1.31, with DirectML on
Windows) into the user cache (`%LOCALAPPDATA%\ort.pyke.io`) and links it
**statically**. The only runtime DLL is `DirectML.dll`, which `copy-dylibs`
copies next to the built binaries (`target/debug`, `target/release`); ship
it next to the app executable. Set `ORT_LIB_LOCATION` to link a local ORT
build instead. The `load-dynamic` feature of `stemx-separation` loads
`onnxruntime.dll` at runtime instead (`ORT_DYLIB_PATH`, or next to the
executable), e.g. to test against Microsoft's official builds.

## License

[MIT](LICENSE). Bundled model weights carry their own licenses (see
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)).
Third-party notices: [app/THIRD_PARTY.md](app/THIRD_PARTY.md) (fonts) and
[crates/stemx-audio/THIRD_PARTY.md](crates/stemx-audio/THIRD_PARTY.md)
(audio).
