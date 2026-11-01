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

## License

[MIT](LICENSE). Bundled model weights carry their own licenses (see
[docs/SPEC.md](docs/SPEC.md)).
