# scripts/

Entry points for setting up the bundled models, plus two early Python
experiments with audio-separator that predate the ONNX pipeline.

## Boundaries

- `setup-models.ps1` and `setup-models.sh` are part of building ShredLab:
  the README's build instructions use them, and they are thin wrappers that
  find a Python and run `tools/setup_models.py` with all arguments passed
  through. They need nothing installed beyond uv or a system Python; the
  script creates its own `.venv-models`. CI calls `tools/setup_models.py`
  directly.
- `separate.py` and `pan_split.py` are one-off tools from the start of the
  project. Nothing in the build, the app or the tests uses them.
  `separate.py` needs audio-separator, so it runs only in `.venv`
  (`requirements.txt`); `pan_split.py` needs only numpy, scipy and
  soundfile.
- `pan_split.py` is the origin of the pan estimate the Refine stereo-position
  mask uses (`crates/shredlab-dsp/src/refine/pan.rs` cites it), but the Rust
  mask is its own streaming design and is not validated against this script.

## Layout

- `setup-models.ps1`: Windows wrapper. Prefers `uv run --python 3.12`, then
  `py -3`, then `python`.
- `setup-models.sh`: macOS/Linux wrapper. Prefers `uv run --python 3.12`,
  then `python3.12`, `python3`, `python`.
- `separate.py`: separates songs with several audio-separator models
  (htdemucs_6s, htdemucs_ft, BS-RoFormer-SW; optionally a drum-kit
  splitter) and writes stems, "everything but X" practice mixes and a
  left/center/right split of the guitar stem.
- `pan_split.py`: splits a stereo stem into left/center/right parts (or one
  custom pan range) with soft per-bin masks that sum to 1.

## How it works

`pan_split.py` estimates each STFT bin's pan as `(|R| - |L|) / (|L| + |R|)`
(-1 hard left, +1 hard right) with n_fft 4096, then assigns bins to regions
with smoothstep edges. The left/center/right masks sum to exactly 1, so the
parts add back up to the input. It works best on an already isolated stem.

`separate.py` uses audio-separator's `Separator` with `models/` as the model
file directory and DirectML unless `--cpu`. Output goes to
`out/<song>/<model>/` (local).

## Invariants and gotchas

- The wrappers must keep passing every argument through unchanged; the
  top-level README documents `--out` and `--force` for them.
- `separate.py` imports `pan_split` as a sibling module, so the two files
  have to stay in the same directory.

## Testing / running

```
.\scripts\setup-models.ps1 [--out DIR] [--force]        # Windows
./scripts/setup-models.sh [--out DIR] [--force]         # macOS / Linux
.venv\Scripts\python.exe scripts/separate.py input/ [--models htdemucs_6s] [--drumsep] [--cpu]
.venv\Scripts\python.exe scripts/pan_split.py guitar.wav [--range -1 -0.4] [--out DIR]
```

A successful model setup ends with "overall: PASS" in step 6 and prints the
model paths. Details: [tools/README.md](../tools/README.md).
