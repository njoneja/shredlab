# tools/

Dev-machine tooling: building and verifying the bundled models, converting
personal models, the Python reference implementations, golden-fixture
generators, and the repo checks (third-party notices, SPDX headers, docs).
Nothing here ships in the app.

## Boundaries

- The Rust crates never import or call anything here. The connection is
  through files: model folders under `models/` (consumed by
  `shredlab-separation`, the CLI and the Tauri bundle), fixtures under
  [tests/fixtures/](../tests/fixtures/README.md) (consumed by Rust tests),
  and `app/public/THIRD_PARTY_LICENSES.html` (shown in the app).
- [reference/](reference/README.md) is the ground truth for the Rust DSP
  and adapters. The model export code for the bundled models lives in
  [spikes/](../spikes/README.md); `export/` holds only the personal-model
  exporter.
- Python environments (details in
  [docs/DEVELOPING.md](../docs/DEVELOPING.md#python-environments)):
  - `setup_models.py` runs with any Python 3.9+ (stdlib only) and creates
    and uses `.venv-models` (`requirements-models.txt`, CPU only).
  - Everything else uses `.venv` (`requirements.txt`: audio-separator,
    DirectML ONNX Runtime, torch). `add_personal_model.py` and
    `validate_roformer_chunking.py` need audio-separator's model code, so
    they only run there; DirectML checks (`--ep dml`, `--check-dml`) need
    it too.
  - `make_icon.py` needs fonttools, which is in neither requirements file.
  - `third_party_licenses/generate.mjs` and `check_docs.mjs` need Node;
    `check_spdx.sh` needs sh and git.
- Run Python tools from the repo root. Tools that decode audio (`--audio`
  options, `make_goldens.py`, `validate_roformer_chunking.py`) call
  `ffmpeg`, which must be on PATH.

## Layout

Start with `setup_models.py` (how the bundled models come to exist), then
[reference/](reference/README.md).

- `setup_models.py`: downloads the bundled models' checkpoints, exports them
  to ONNX, builds the model folder, verifies. Wrapped by
  `scripts/setup-models.ps1` and `scripts/setup-models.sh`.
- `registry.py`: builds the `htdemucs_6s` model folder (`build-htdemucs`)
  and verifies any model folder (`verify`).
- `add_personal_model.py`: converts a BS-RoFormer checkpoint into a
  personal model folder.
- `personal_models.example.json`: format of the short-name list read by
  `add_personal_model.py`. The real list, `personal_models.local.json`, is
  local and gitignored.
- `export/roformer.py`: BS-RoFormer PyTorch-to-ONNX export of the network
  core (torch; never used at inference time).
- `reference/`: NumPy reference of both adapters, `model.json` /
  `selftest.json` helpers, the `synth-v1` generator, and the reference data
  `setup_models.py` verifies against.
- `fixture_io.py`: writer for the raw float32 `.bin` + `index.json` fixture
  format.
- `make_fixtures.py`: small, model-free fixtures committed in
  `tests/fixtures/`.
- `make_goldens.py`: full-model goldens in `tests/fixtures/local/` (local).
- `beat_this_fixtures.py`: Beat This! pre/post-processing fixtures in
  `tests/fixtures/local/beat_this/` (local).
- `validate_roformer_chunking.py`: full-song check of the `roformer-mask`
  reference against audio-separator's PyTorch inference.
- `third_party_licenses/`: `generate.mjs` builds
  `app/public/THIRD_PARTY_LICENSES.html`; `texts/` holds vendored license
  and notice files of non-crate components; `spdx/` holds standard license
  texts used for crates that ship none.
- `check_spdx.sh`: fails if a tracked source file lacks an
  `SPDX-License-Identifier` line in its first five lines.
- `check_docs.mjs`: fails on a missing directory README, a broken relative
  link, a backticked repo path that does not exist (unless gitignored), or a
  file (docs or code) that refers to unpublished internal files.
- `make_icon.py`: builds `app/src-tauri/icons/app-icon.svg` from the Noto
  Music clef glyphs, converted to paths.

## How it works

### Bundled model setup (`setup_models.py`)

```
python tools/setup_models.py [--out DIR] [--venv DIR] [--force]
```

The top level is stdlib only, so the system Python can run it; the heavy
work runs in subprocesses (`setup_models.py _worker ...`) inside the model
venv. Six steps, each skipped when its output already exists and checks out:

1. Venv (default `.venv-models`, Python 3.12) from
   `requirements-models.txt`, then `demucs==4.0.1` with `--no-deps` (its
   declared dependencies are training-only). Uses uv if on PATH, else
   `python3.12 -m venv` + pip. It refuses to touch `.venv`. A marker file in
   the venv stores a hash of the requirements, the `--no-deps` list and the
   Python version; a change to any of them reinstalls.
2. Checkpoints, sha256-checked: `5c90dfd2-34c22ccb.th` (Demucs v4
   htdemucs_6s, Meta, MIT, from dl.fbaipublicfiles.com) and
   `beat_this/beat_this-final0.ckpt` (Beat This! final0, CPJKU, MIT, from
   cloud.cp.jku.at).
3. `onnx/htdemucs_6s_own.onnx` via `spikes/onnx_demucs_own.py` `export()`.
4. `onnx/beat_this_final0.onnx` via `spikes/onnx_beat_this.py` `export()`.
5. `registry/htdemucs_6s/` via `registry.py build-htdemucs`.
6. CPU verification: the folder's own self-test (sha256 plus
   `selftest.json`), then the same input against
   `reference/data/htdemucs_6s_selftest.json`, and Beat This! logits on a
   synth-v1 log-mel chunk against `reference/data/beat_this_final0_check.json`
   (100 probes per output, pass at SNR >= 30 dB and rms error < 5%). Step 6
   always runs; it is what catches a broken export on another platform or
   torch build.

All paths are under `--out` (default: the repo's `models/`, which the Tauri
bundle config and debug builds read). An export counts as done when its
sha256 is in `KNOWN_GOOD` (the hashes the app was developed against, torch
2.4.1 on Windows) or matches the record in `<out>/.setup_models.json` for
the same checkpoint hash and `EXPORT_VERSION`. Exports go to a temporary
file first. Nothing is deleted: a file that has to be replaced is renamed to
`<name>.bak-<timestamp>`. `models/personal/` and the personal-model list
are never touched. With a non-default `--out`, the script prints the
`SHREDLAB_MODELS_DIR` and `SHREDLAB_BEAT_MODEL` values that point the app at
the result.

Model code comes from pinned PyPI packages (`demucs`, `beat-this`). The
spikes prefer those and fall back to audio-separator's vendored Demucs or a
local copy of Beat This! in `spikes/third_party/` when run from `.venv`.
Upstream `demucs.states` imports `omegaconf` and `dora` (training only);
the spike replaces them with inert stubs when they are missing.

### Model folders (`registry.py`)

`build-htdemucs` hardlinks (or copies, across volumes)
`models/onnx/htdemucs_6s_own.onnx` into `<dest>/htdemucs_6s/`, writes
`model.json` (id, stems, segment, STFT, per-EP session options, MIT license
block, `personal: false`) and computes `selftest.json` on the CPU EP. The
Demucs session options include `disable_graph_fusion` for DirectML, which
this export needs. `verify` checks the sha256 and runs the self-test on the
chosen EP; `--dml-fusion` turns graph fusion back on to show that the
self-test catches the resulting garbage. Format and pass rule:
[docs/MODEL_MANIFEST.md](../docs/MODEL_MANIFEST.md).

### Personal models (`add_personal_model.py`)

```
python tools/add_personal_model.py <short-name | path/to/model.ckpt> [--config model.yaml]
    [--dest DIR] [--id ID] [--name "Display name"] [--audio song.mp3] [--check-dml]
```

- Source: a short name from `personal_models.local.json`, or a checkpoint
  path (the MSST-style `.yaml` config is looked up next to it unless
  `--config`; license becomes "unknown"). List entries give `checkpoint` /
  `config` paths relative to the repo root and/or `checkpoint_url` /
  `config_url` (downloaded to `models/downloads/`, checked against optional
  `checkpoint_sha256` / `config_sha256`), plus optional `id`, `name` and a
  `license` block.
- Steps: print the license (with a do-not-redistribute warning when
  unknown), load the PyTorch model, export the core with
  `export/roformer.py` (minutes, several GB of RAM), check accuracy, write
  `model.json` (`personal: true`, adapter `roformer-mask`, `overlap` =
  `num_overlap` from the yaml) and `selftest.json`, re-verify on the CPU,
  then move the folder into `<dest>/<id>/`.
- Accuracy check: one chunk of synth-v1 (and an excerpt of `--audio`, one
  third into the song) through ONNX on the CPU plus the NumPy DSP from
  `reference/roformer_separate.py`, against the PyTorch model. Pass: at
  least 40 dB per stem, with the reference energy floored at -20 dB
  relative to the input, because near-silent stems are numerically noisy.
- Default `--dest` is `models/personal/` in the repo, which debug builds
  scan. The installed app reads personal models from `<app data dir>/models`.
  Processes started from a packaged (MSIX) app can see a virtualized
  `%APPDATA%`, so pass an explicit `--dest` when targeting the app from
  such a shell.
- Supported: stereo BS-RoFormer without linear-attention layers or
  `skip_connection`. Mel-Band RoFormer is detected and rejected; it needs
  its own core wrapper in `export/roformer.py`.

### RoFormer export (`export/roformer.py`)

A parameterized copy of the export code in `spikes/onnx_roformer.py` (the
spike stays as it was). The graph maps `stft (1, 2, n_fft/2+1, F, 2)` to
`mask (1, N, (n_fft/2+1)*2, F, 2)` with a static F = chunk / hop + 1. The
model is rewritten for the TorchScript exporter in torch 2.4 (explicit GLU,
explicit attention with constant rotary angles, static shapes, positive
axes, no einops pack/unpack); the math is unchanged and the accuracy check
above confirms it. STFT parameters, chunk size, stems and `num_overlap` come
from the yaml.

### Fixture generators

`make_fixtures.py` writes the committed, model-free STFT/iSTFT, song
normalization and chunking vectors, and records numpy-vs-torch cross-checks
in `index.json` (it needs torch for those). `make_goldens.py` runs both
adapters' references on a real song on the CPU EP and writes inputs, graph
inputs/outputs, intermediate DSP and full separations.
`beat_this_fixtures.py` writes Beat This! preprocessing vectors for three
synthetic signals plus, for every `out/beat_this/final0_*.npz` written by
`spikes/onnx_beat_this.py ref`, the torch logits, beats and spectrogram.
Every file's shape and meaning:
[tests/fixtures/README.md](../tests/fixtures/README.md).

### Third-party notices (`third_party_licenses/generate.mjs`)

Collects the normal (non-dev, non-build) dependencies of the `shredlab`
package from `cargo metadata --locked` for Windows x64 and both macOS
targets, merged, so one file covers every platform. Each crate's texts come
from its own LICENSE / COPYING / NOTICE files in the cargo registry; a crate
without any gets `spdx/<ID>.txt` with `{{COPYRIGHT}}` replaced by its
authors. Components that are not crates (ONNX Runtime, DirectML, LAME,
Signalsmith, the two bundled models, fonts, `@tauri-apps/api`, Svelte) are
listed in `otherComponents()`, with texts from `texts/`, the vendored source
trees, `app/node_modules` or the LAME sources inside the `mp3lame-sys`
crate. Identical texts are shown once. The output is deterministic and
committed. `pnpm build-installers` (`app/scripts/bundle.mjs`) regenerates it
first; CI runs `--check` on Windows.

## Invariants and gotchas

- Personal models are never bundled or downloaded by the public build: the
  Tauri resources list only `models/registry/htdemucs_6s/` and
  `models/onnx/beat_this_final0.onnx`, and the notices generator lists only
  the bundled models. Never commit `personal_models.local.json`, a converted
  personal model, or URLs for models with unclear licenses.
- A model's `selftest.json` is computed by the reference on the CPU EP. Any
  change to the reference DSP, the synth-v1 generator or the export changes
  self-test outputs: rebuild the model folders, and expect step 6 of
  `setup_models.py` to fail until `reference/data/` is updated.
- `reference/data/htdemucs_6s_selftest.json` is a byte-for-byte copy of the
  known-good folder's `selftest.json`; no script writes it.
  `reference/data/beat_this_final0_check.json` is written by
  `setup_models.py _worker make-beat-reference --onnx <file>` (run with the
  model venv's Python) and records the source ONNX sha256.
- Changing the export code in `spikes/onnx_demucs_own.py` or
  `spikes/onnx_beat_this.py`: bump `EXPORT_VERSION` in `setup_models.py`, or
  existing exports keep counting as done. A fresh export on another machine
  may differ in bytes from `KNOWN_GOOD`; it is accepted when it passes
  step 6.
- CI caches `models/` keyed on `requirements-models.txt`,
  `setup_models.py`, `registry.py`, everything under `reference/`, and the
  three spikes the export uses. Touching any of them rebuilds the models in
  CI.
- DirectML: the Demucs export needs graph fusion off (with it on, DirectML
  returns wrong output without an error); the RoFormer manifests written by
  `add_personal_model.py` leave fusion on. A DirectML device crash poisons
  the process, so run each DirectML configuration in a fresh process.
- `add_personal_model.py` replaces an existing `<dest>/<id>/` without
  asking. A failed run leaves its work in `<dest>/.<id>.partial`.
- `generate.mjs` hard-codes some version strings in notes (ONNX Runtime,
  DirectML, Signalsmith Stretch); update them, and the vendored texts, when
  those dependencies change. It fails if a crate has no license file and no
  matching `spdx/` text, if `mp3lame-sys` leaves the dependency graph, or if
  `app/node_modules` is missing.
- `check_spdx.sh` skips `third_party/` directories and unpublished internal
  files; it checks `.rs .ts .js .mjs .svelte .py .sh .css .c .cc .cpp .h .hpp`
  files only.

## Testing / running

```
python tools/setup_models.py                      # or scripts/setup-models.ps1 / .sh
python tools/registry.py build-htdemucs           # -> models/registry/htdemucs_6s/
python tools/registry.py verify models/registry/htdemucs_6s --ep cpu
python tools/registry.py verify models/registry/htdemucs_6s --ep dml
python tools/registry.py verify models/registry/htdemucs_6s --ep dml --dml-fusion   # must FAIL
python tools/add_personal_model.py <short-name> --audio song.mp3 --check-dml
python tools/validate_roformer_chunking.py song.mp3 [--model-dir DIR] [--ep cpu] [--seconds N]
python tools/make_fixtures.py                     # -> tests/fixtures/
python tools/make_goldens.py --audio song.mp3     # -> tests/fixtures/local/
python tools/make_goldens.py --shifts-only        # only demucs_shifts2_stems
python tools/beat_this_fixtures.py                # -> tests/fixtures/local/beat_this/
node tools/third_party_licenses/generate.mjs [--check]
sh tools/check_spdx.sh
node tools/check_docs.mjs
python tools/make_icon.py <NotoMusic-Regular.ttf> app/src-tauri/icons/app-icon.svg
```

Python commands use `.venv\Scripts\python.exe` except `setup_models.py`
(any Python). `validate_roformer_chunking.py` loads the audio-separator
checkpoint (default `BS-Roformer-SW.ckpt`) from `models/`, defaults to
DirectML and caches the PyTorch result in `out/validation/`. After
`make_icon.py`, run `pnpm --dir app tauri icon src-tauri/icons/app-icon.svg`
to regenerate the icon set.

A model folder can also be checked from Rust:
`cargo run -p shredlab-cli -- selftest --model <id>`.

## Common changes

- Regenerating the bundled models from scratch: `python tools/setup_models.py --force`
  (old files are kept as `*.bak-*`).
- Changing the Demucs or Beat This! export: edit the spike, bump
  `EXPORT_VERSION`, run `setup_models.py`. If step 6 fails because the new
  export is intentionally different, verify it by listening and with the
  goldens, then copy the new `models/registry/htdemucs_6s/selftest.json` to
  `reference/data/htdemucs_6s_selftest.json` and/or rerun
  `make-beat-reference`, and add the new hashes to `KNOWN_GOOD`.
- Adding a personal model: add an entry to `personal_models.local.json`
  (copy the example), run `add_personal_model.py <name> --audio <song>`,
  then pick it in the app (debug builds see `models/personal/`) or set
  `SHREDLAB_MODEL=<id>`.
- Adding a bundled model of a known family: write its folder with
  `reference.manifest.build_manifest` / `write_selftest` (as
  `registry.py build-htdemucs` does), add it to the Tauri resources, add its
  checkpoint and export to `setup_models.py`, and add its license to
  `otherComponents()` in `generate.mjs` with the text in `texts/`. A new
  family also needs an adapter in the reference and in
  `shredlab-separation`.
- After a dependency change (Cargo.lock or `app/node_modules`): run
  `node tools/third_party_licenses/generate.mjs` and commit
  `app/public/THIRD_PARTY_LICENSES.html`. For a crate with a license that
  has no file and no `spdx/` text, add `spdx/<ID>.txt` (with `{{COPYRIGHT}}`
  where the copyright line goes).
