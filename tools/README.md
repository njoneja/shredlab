# tools/

Python tooling for the dev machine (uses the repo venv; never shipped in the
app). Run everything from the repo root with `.venv\Scripts\python.exe`.
Scripts that decode audio need ffmpeg on PATH. Python environments are
described in [docs/DEVELOPING.md](../docs/DEVELOPING.md).

## `setup_models.py` — build the bundled models from scratch

```
python tools/setup_models.py [--out DIR] [--venv DIR] [--force]
```

Runs with any Python 3.9+ (stdlib only); wrappers: `scripts/setup-models.ps1`,
`scripts/setup-models.sh`. Steps, each skipped when its output exists with the
expected sha256:

1. CPU-only Python 3.12 venv `.venv-models` from `requirements-models.txt`
   (+ `demucs==4.0.1` with `--no-deps`), via uv if available. `.venv` is
   never touched.
2. Checkpoints (sha256-checked): `5c90dfd2-34c22ccb.th` from
   dl.fbaipublicfiles.com (Demucs, MIT) and `beat_this/beat_this-final0.ckpt`
   from cloud.cp.jku.at (Beat This!, MIT).
3. `onnx/htdemucs_6s_own.onnx` via `spikes/onnx_demucs_own.export`.
4. `onnx/beat_this_final0.onnx` via `spikes/onnx_beat_this.export`.
5. `registry/htdemucs_6s/` via `tools/registry.py build-htdemucs --onnx ...`.
6. CPU verification: the folder's self-test, plus both ONNX files against
   reference outputs of the known-good models in `reference/data/`
   (`htdemucs_6s_selftest.json`, `beat_this_final0_check.json`), so a broken
   export on another platform/torch build is caught.

Exports are accepted as "done" if their sha256 is one of the known-good
hashes or matches `<out>/.setup_models.json` (written by earlier runs).
Files are never deleted; anything replaced is renamed to `*.bak-<timestamp>`.
Model code comes from pinned PyPI packages (`demucs==4.0.1`,
`beat-this==1.1.0`); the spikes fall back to them when audio-separator /
`spikes/third_party/` are absent. Upstream `demucs.states` imports
`omegaconf` and `dora` (training only), which are stubbed when missing.

## `reference/` — reference implementations

Importable package (`from tools.reference import ...`, repo root on
`sys.path`). numpy + scipy + onnxruntime only at inference time. This is the
ground truth for the Rust port.

| Module | What |
|---|---|
| `synth.py` | `synth_v1(length, sr)`: deterministic self-test input. |
| `demucs_hybrid.py` | `separate(sess, mix)` for the `demucs-hybrid` adapter: song normalization, apply_model chunking (25% overlap, triangular weights, centered short last chunk), `spec`/`ispec`/`cac`/`uncac` glue. Numerically identical to `spikes/onnx_demucs*.py`. |
| `roformer_separate.py` | `separate(sess, mix, num_overlap)` for the `roformer-mask` adapter: torch.stft-equivalent STFT, mask multiply, iSTFT, chunking as audio-separator 0.48 (Hamming window, step = chunk // num_overlap, end-aligned tail chunk). |
| `manifest.py` | `build_manifest`, `write_selftest`, `verify_model_dir`, `check_selftest` (pass rule), sha256. |
| `sessions.py` | `make_session(onnx, "cpu"|"dml", manifest["session"])`. |

Example:

```python
import sys; sys.path.insert(0, ".")
from tools.reference import demucs_hybrid, manifest
from tools.reference.sessions import make_session
man = manifest.load_manifest("models/registry/htdemucs_6s")
sess = make_session("models/registry/htdemucs_6s/" + man["file"], "dml", man["session"])
stems = demucs_hybrid.separate(sess, mix)   # mix: (2, L) float32 at 44.1 kHz
```

## `registry.py` — model folders

```
python tools/registry.py build-htdemucs            # -> models/registry/htdemucs_6s/
python tools/registry.py verify models/registry/htdemucs_6s --ep cpu
python tools/registry.py verify models/registry/htdemucs_6s --ep dml
python tools/registry.py verify models/registry/htdemucs_6s --ep dml --dml-fusion   # must FAIL
```

`build-htdemucs` hardlinks `models/onnx/htdemucs_6s_own.onnx` (from
`spikes/onnx_demucs_own.py export`) into the folder and writes `model.json`
and `selftest.json` (self-test reference computed on the CPU EP). Run each
DirectML configuration in its own process.

## `add_personal_model.py` — personal models

```
python tools/add_personal_model.py bs-roformer-sw              # short name
python tools/add_personal_model.py path\to\model.ckpt [--config model.yaml]
    [--dest DIR] [--id ID] [--name "Display name"] [--audio song.mp3] [--check-dml]
```

- Short names come from `tools/personal_models.local.json` (gitignored;
  copy `personal_models.example.json`). Entries give a local `checkpoint` /
  `config` path (relative to the repo root) and/or `checkpoint_url` /
  `config_url` (downloaded to `models/downloads/`, optional `*_sha256`), plus
  the `license` block shown before converting.
- Steps: print license → load PyTorch model → export the network core to
  ONNX (`tools/export/roformer.py`, ~40 s, ~700 MB for SW) → accuracy check of
  ONNX + numpy DSP against PyTorch on one chunk of synth-v1 (and of `--audio`
  if given); pass ≥ 40 dB per stem with the reference energy floored at
  −20 dB re the input → write `model.json` (`personal: true`) and
  `selftest.json` → move into `<dest>/<id>/`.
- Default dest: `models/personal/` in the repo. The app's real user models
  folder is resolved by the app. Processes launched from a packaged (MSIX) desktop
  app see a virtualized `%APPDATA%`, so use an explicit `--dest` when testing
  against the app.
- Supported family: BS-RoFormer (stereo, no linear-attention layers).
  Mel-Band RoFormer needs a core wrapper (not done).
- The spike `spikes/onnx_roformer.py` is left as-is; `tools/export/roformer.py`
  is the maintained, parameterized copy of its export code.

## `validate_roformer_chunking.py`

Full-song check of `roformer_separate.separate` against audio-separator's
PyTorch `demix` with the same chunk size and overlap:

```
python tools/validate_roformer_chunking.py song.mp3
```

The PyTorch result is cached in `out/validation/`.

## Fixtures

```
python tools/make_fixtures.py    # small committed fixtures -> tests/fixtures/
python tools/make_goldens.py --audio song.mp3  # big local goldens -> tests/fixtures/local/ (gitignored)
```

See `tests/fixtures/README.md` for every file's shape and meaning.
