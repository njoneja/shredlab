# tools/

Python tooling for the dev machine (uses the repo venv; never shipped in the
app). Run everything from the repo root with `.venv\Scripts\python.exe`.
Scripts that decode audio need ffmpeg on PATH (in shells started by some apps,
refresh PATH first; see docs/STATUS.md).

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
python tools/validate_roformer_chunking.py "song.mp3"
```

The PyTorch result is cached in `out/validation/`.

## Fixtures

```
python tools/make_fixtures.py    # small committed fixtures -> tests/fixtures/
python tools/make_goldens.py     # big local goldens -> tests/fixtures/local/ (gitignored)
```

See `tests/fixtures/README.md` for every file's shape and meaning.
