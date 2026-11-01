# Stem Extractor

Stem Extractor is a desktop practice and transcription tool for musicians.
It separates a song into instruments (vocals, drums, bass, guitar, piano,
other) on your own machine, so you can play along with an instrument
removed or listen to one by itself, with speed change without pitch change,
transposition, A–B loops and a click track with count-in. It is built with Tauri 2 and a Rust core, runs separation models with
ONNX Runtime (DirectML / CoreML / CPU), and targets Windows and macOS.

Status: early development (milestone M0, skeleton). See [docs/](docs/) for
the [spec](docs/SPEC.md), [milestones](docs/MILESTONES.md) and
[current status](docs/STATUS.md).

## Repository layout

| Path | What |
|---|---|
| `app/` | Tauri app: Svelte 5 + TypeScript + Vite frontend (`app/src`), Rust shell (`app/src-tauri`) |
| `crates/stemx-dsp` | STFT/iSTFT, chunking, overlap-add |
| `crates/stemx-io` | Decode, encode, resample |
| `crates/stemx-separation` | ONNX Runtime sessions, model registry, adapters |
| `crates/stemx-audio` | Real-time audio engine |
| `crates/stemx-library` | Song folders, JSON manifest, stem cache |
| `crates/stemx-cli` | `stemx`, headless CLI |
| `spikes/`, `scripts/` | Python reference implementations (Rust ports are validated against them) |
| `docs/` | Spec, milestones, status |

## Development setup

You need:

- **Rust** (stable, via [rustup](https://rustup.rs)), with `rustfmt` and `clippy`.
- **Node.js** 24 and **pnpm** 12.
- Platform prerequisites for Tauri 2
  ([guide](https://v2.tauri.app/start/prerequisites/)): on Windows, the MSVC
  build tools and WebView2; on macOS, the Xcode command line tools.

Install the frontend dependencies once:

```sh
pnpm --dir app install
```

## Commands

```sh
pnpm --dir app tauri dev                          # run the app with hot reload
pnpm --dir app tauri build --debug --no-bundle    # build the app binary (target/debug)
pnpm --dir app check                              # type-check the frontend

cargo test --workspace                            # Rust unit tests
cargo run -p stemx-cli -- info                    # CLI: print crate versions
cargo fmt --all --check                           # formatting
cargo clippy --workspace --all-targets -- -D warnings
```

Note: the Tauri crate embeds the built frontend, so `cargo build`/`clippy`/
`test` on the whole workspace need `app/dist` to exist. Run
`pnpm --dir app build` once first (any `tauri dev`/`tauri build` also
creates it).

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
[docs/SPEC.md](docs/SPEC.md)).
