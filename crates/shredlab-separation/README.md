# shredlab-separation

Runs the separation models: finds model folders, loads an ONNX Runtime
session on the best device that produces correct output, and turns a song
into stems through the adapter the model declares. It also holds the ORT
session for the Beat This! beat model, so ONNX Runtime is linked in one
place.

## Boundaries

- Owns: the process-wide ORT environment, execution providers and session
  options, `model.json` / `selftest.json` parsing and validation, the model
  registry, the two adapters (`demucs-hybrid`, `roformer-mask`), the device
  self-test with its cache, sha256 verification of ONNX files, `Separator`
  (load with fallback, separate, time-shifted passes) and `BeatModel`.
- Does not: decode or resample audio (callers pass planar f32 at the
  model's rate, see `shredlab-io`), write stems anywhere (`shredlab-library`),
  or do beat pre/post-processing (`shredlab-beats`, which stays ORT-free
  and receives `BeatModel::run` as a closure). The STFT, chunking and
  overlap-add math lives in `shredlab-dsp`; this crate wires it to the
  graph. All paths (model dirs, cache dir) are passed in by the caller; no
  Tauri dependency.
- Depends on: `shredlab-dsp`, `ort` (pinned to an exact release
  candidate), `sha2`, `serde`/`serde_json`, `num_cpus`, `thiserror`.
  Cargo features: `cuda` (CUDA provider) and `load-dynamic` (load an ORT
  shared library at run time instead of pyke's static build). How ORT is
  downloaded and linked is in the
  [top-level README](../../README.md#onnx-runtime-binaries).
- Used by: `app/src-tauri` and `shredlab-cli`.

## Layout

Start with `src/lib.rs` (usage example and re-exports), then
`src/separator.rs`.

- `src/separator.rs`: `Separator::load` (device choice, self-test,
  fallback), `separate` / `separate_shifts`, `shift_offsets`,
  `run_selftest` (diagnostics on one provider).
- `src/adapters.rs`: the `Adapter` trait and `DemucsHybrid` /
  `RoformerMask`, each split into public stages (STFT input, graph run,
  post-graph DSP) so the golden tests can check them one by one.
- `src/runtime.rs`: `Ep`, `DevicePref`, `SessionOverrides`,
  `build_session`, default CPU threads, the CoreML compile cache folder.
- `src/manifest.rs`: `Manifest`, `EpOptions`, `SelfTestSpec` and their
  validation.
- `src/registry.rs`: `ModelRegistry::search_dirs` / `scan`, `ModelEntry`.
- `src/selftest.rs`: the `synth-v1` input generator, the pass rule
  (`check`), the JSON `Cache` and `sha256_file`.
- `src/beat_model.rs`: `BeatModel`, the session for the beat model.
- `build.rs`: macOS only, links clang's runtime for ORT's CoreML code in
  LTO builds.
- `tests/golden.rs`: ignored end-to-end tests against the Python
  reference.

## How it works

Model folders and both file formats are specified in
[MODEL_MANIFEST.md](../../docs/MODEL_MANIFEST.md); this section covers
what the code does with them.

Registry. `search_dirs` lists the bundled dir, the personal dir, then each
entry of `SHREDLAB_MODELS_DIR` (split like `PATH`). `scan` reads
`<dir>/<id>/model.json` in sorted order, skipping missing dirs and
dot-folders. A folder with an invalid manifest or a missing ONNX file goes
into `problems` instead of failing the scan; for duplicate ids the first
one wins and the later one is reported. A model is personal if its
manifest says so or it was found in the personal dir.

Loading. `Separator::load` first checks the ONNX file's sha256 against the
manifest (cached by path, size and mtime), then tries candidates in order:
the compiled-in GPU providers (CUDA if the `cuda` feature is on, DirectML
on Windows, CoreML on macOS) unless the device is `Cpu`, then CPU. A GPU
is used only if its session builds and the self-test passes; a cached
failure skips it without building a session. CPU is always accepted, even
if its own self-test fails (status `Failed`). `rejected()` records why each
GPU was skipped, for the UI and logs.

Sessions. `build_session` applies the manifest's per-provider options
plus debug overrides. DirectML always gets memory patterns off and
sequential execution regardless of the manifest, and graph fusion off when
the manifest asks (`disable_graph_fusion`). CPU uses `intra_op_threads`
from the manifest, or min(physical cores, 16); ORT's own default was about
5x slower on a hybrid P/E-core CPU. GPU providers are registered with
`error_on_failure`, so ORT never silently falls back to CPU inside a
session. CoreML uses the MLProgram format and caches its compiled model
under `<cache dir>/coreml/<model id>/<sha256 prefix>`, deleting older
compiles of the same model.

Adapters. Each adapter maps a flat `(channels, segment)` window to
`(stems, channels, segment)` and runs the chunking from `shredlab-dsp`
around it.

- `demucs-hybrid`: song normalization (mean and std of the mono mix over
  the whole song), then `apply_demucs`. Per window: `HybridStft` of each
  channel into the CaC `mag` input, graph run with inputs `mix` and `mag`,
  outputs `wave` and `spec`; the result is `wave + ispec(spec)`.
- `roformer-mask`: no normalization; `roformer_chunk_starts` with
  `step = segment / overlap` and a symmetric Hamming window. Per window:
  `Stft` per channel, graph input `(1, 2, bins, frames, 2)`, complex mask
  output with the frequency axis interleaved `(bin, channel)`, complex
  multiply, iSTFT. It uses the graph's first input and first output, whatever
  their names.

At construction each adapter checks the graph's static input dimensions
against what the manifest implies, so a mismatched `model.json` fails at
load rather than mid-song.

Quality (shifts). `separate_shifts` averages `shifts` time-shifted
passes, as Demucs `apply_model(shifts=N)` does, but with deterministic
offsets `round(k * max_shift / shifts)`, `max_shift = sample_rate / 2`
(0 and 11025 samples for two passes at 44.1 kHz), so output is
reproducible. The song is normalized once over the real audio, each pass
sees it zero-padded by `max_shift - offset` in front, and results are
cropped, summed, divided and denormalized. `shifts <= 1` is the plain
pass. Progress is reported across all passes; `cancel` is checked before
every chunk.

Self-test. `run_selftest_on` generates `synth-v1` for exactly one segment,
runs `Adapter::run_single_segment` (normalization over that segment for
`demucs-hybrid`, no chunking, no weights) and applies the pass rule from
MODEL_MANIFEST.md. Results go to `shredlab-cache.json` in the caller's
cache dir, keyed by `st1|sha256|provider|ORT version|overrides`; without a
cache dir nothing is cached.

`BeatModel::load` tries GPU providers then CPU, with no self-test (there
is no reference for it); `run` feeds `spect` `[1, frames, mels]` and
returns the `beat` and `downbeat` logits.

## Invariants and gotchas

- GPU backends can be wrong without failing. With graph fusion on,
  DirectML produces garbage for the Demucs export with no error (probe SNR
  far below zero). Keep `disable_graph_fusion` in that manifest and never
  skip the self-test for a GPU in normal runs. The hidden CLI flag
  `--debug-dml-fusion` forces fusion on to reproduce this.
- CoreML must use MLProgram: the default NeuralNetwork format runs on the
  Neural Engine in fp16, which silently ruins `htdemucs_6s` (about 6 dB
  SNR). MLProgram keeps fp32 and matches the CPU to about 96 dB.
- The self-test cache key starts with `st1`. Bump it when the pass rule or
  the self-test input changes, or stale results will be trusted. The
  overrides fingerprint covers only the DirectML fusion override; CPU
  thread count does not affect correctness and is not in the key.
- A GPU that fails during separation with a runtime error triggers a
  reload on CPU and a restart of the song: progress jumps back to 0. Other
  errors (including `Canceled`) are returned as is.
- The self-test input is computed in f64 in the order written and cast to
  f32, so Rust and Python generate the same samples. Do not reorder the
  terms in `synth_v1`.
- `segment_samples` is a fixed graph dimension. For `roformer-mask` songs
  shorter than one chunk are zero-padded to the chunk; for
  `demucs-hybrid` short chunks are filled with neighboring audio.
- `overlap` means a fraction in [0, 1) for `demucs-hybrid` and an integer
  number of overlaps for `roformer-mask`; validation enforces both.
- Only stereo models (`channels = 2`) and manifest `schema` 1 are
  accepted; `file` must be a plain file name inside the model folder.
- `ort` is pinned to an exact version, and `shredlab-beats` pins the same
  one for its dev tests. Change both together, then rerun the self-tests
  and golden tests (a new ORT build also invalidates cached self-test
  results, since the ORT version is in the key).
- `Separator` and `BeatModel` run their sessions through `&mut self`;
  they are `Send`, so a caller moves them to a worker thread rather than
  sharing them.

## Testing

```
cargo test -p shredlab-separation
```

Unit tests need no models: manifest parsing and validation, registry
scanning, the synth generator, the pass rule, and shift alignment,
progress and cancellation through fake adapters.

The golden tests in `tests/golden.rs` are ignored by default. They need
the local fixtures in `tests/fixtures/local/` (gitignored, see
[tests/fixtures/README.md](../../tests/fixtures/README.md)) and the model
folders in the gitignored `models/registry/` and `models/personal/` (built
by `tools/setup_models.py`; see [tools/README.md](../../tools/README.md)):

```
cargo test --release -p shredlab-separation --test golden -- --ignored --nocapture --test-threads 1
```

Each test checks the stages separately on CPU (STFT input above 100 dB
SNR, graph output, post-graph DSP), then full separation on CPU and on the
GPU if one passes the self-test. Loud stems (at least -40 dB relative to
the mix) must reach 60 dB SNR; quieter stems are only reported, since
their SNR mostly measures ORT build differences. `roformer_golden` needs
the personal `bs-roformer-sw` model; `demucs_shifts_golden` needs
`demucs_shifts2_stems.bin` from `tools/make_goldens.py --shifts-only`.
Tests whose files are missing print a message and pass.

On real hardware, `shredlab-cli selftest` runs the self-test of every
model on every available provider, and `shredlab-cli separate` runs a
full separation (`--device`, `--shifts`, `--no-selftest`).

## Common changes

- Adding a model of an existing family: no code. Build the folder with
  `model.json`, the ONNX file and `selftest.json` (the self-test reference
  comes from the Python reference on the CPU provider, see
  [tools/README.md](../../tools/README.md); `tools/add_personal_model.py`
  does all of this for a personal checkpoint), drop it in a model dir and
  check it with `shredlab-cli selftest --model <id>`.
- Adding a new family: write the Python reference in `tools/reference/`
  first. Then add the DSP pieces to `shredlab-dsp` with fixtures, a
  variant to `AdapterKind` (with its serde name) and its validation in
  `src/manifest.rs`, an adapter struct implementing `Adapter` in
  `src/adapters.rs` (including `run_single_segment` for the self-test) and
  its arm in `adapters::create`. Document the graph I/O and chunking in
  MODEL_MANIFEST.md and add a golden test.
- Adding an execution provider: an `Ep` variant with its manifest `key`,
  its place in `gpu_candidates`, registration in `register_gpu` behind the
  matching cfg or cargo feature, and the `ort` feature in `Cargo.toml`.
- Changing the pass rule: change it in `src/selftest.rs` and in
  `tools/reference/manifest.py` together, update MODEL_MANIFEST.md and
  bump the `st1` cache prefix.
