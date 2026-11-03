# shredlab-cli

`shredlab-cli`, a headless command-line tool for testing and batch use:
list models, run device self-tests, separate a file into stems, check the
decoder, print versions. It is not shipped with the app.

## Boundaries

A thin wrapper over `shredlab-separation` and `shredlab-io`; all logic lives
in those crates. `shredlab-audio` and `shredlab-library` are linked only so
`info` can print their versions. It uses clap and anyhow. Like the app, it
passes every directory in: model folders come from `--models-dir`, else
`SHREDLAB_MODELS_DIR`, else `models/registry` and `models/personal` of the
repo (found from the current directory or the executable); the self-test
cache defaults to `target/shredlab-cache` (both local, gitignored).

## Layout

Everything is in `src/main.rs`: the clap definitions (`Command`) and one
`cmd_*` function per subcommand.

| Command | Does |
|---|---|
| `info` | Crate, ONNX Runtime and GPU provider versions |
| `models` | Models found, with adapter, stems, license and personal flag |
| `selftest` | sha256 check and device self-test per model and provider; exits nonzero on any failure |
| `separate` | Decode, resample to the model rate, separate (`--shifts`, `--device`, `--no-selftest`), write `<out>/<stem>.wav` (32-bit float, or `--int16`) |
| `decode` | Decode and resample to 44.1 kHz stereo float WAV |

`--debug-dml-fusion` (hidden) forces DirectML graph fusion on, which is
known to break the Demucs export; it exists to reproduce that failure.

## Testing

```sh
cargo test -p shredlab-cli        # clap definition and argument parsing
cargo run --release -p shredlab-cli -- selftest --device auto
```

The commands other than `info` and `decode` need the models (see the
[README](../../README.md)).

## Common changes

- Adding a subcommand: a `Command` variant (reuse `ModelDirs` and
  `RuntimeArgs` via `#[command(flatten)]`), a `cmd_*` function returning
  `Result`, a match arm in `main`, and a case in `parses_separate` or a new
  parsing test.
