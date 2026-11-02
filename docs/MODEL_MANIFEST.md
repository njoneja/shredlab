# Model manifest (`model.json`) — schema v1

Every separation model the app can load is a folder containing the ONNX file,
a `model.json` manifest, and a self-test file:

```
<models dir>/<id>/
  model.json
  <onnx file>
  selftest.json
```

Bundled models live in the app's resource dir; personal models in the user
models folder (see [ARCHITECTURE.md](ARCHITECTURE.md#models)). Both use this
format.

## `model.json`

```jsonc
{
  "schema": 1,
  "id": "htdemucs_6s",                 // unique, [a-z0-9_-]
  "name": "Demucs v4 (6 stems)",       // shown in the UI
  "adapter": "demucs-hybrid",          // "demucs-hybrid" | "roformer-mask"
  "file": "htdemucs_6s_own.onnx",
  "sha256": "<hex of the onnx file>",
  "sample_rate": 44100,
  "channels": 2,
  "stems": ["drums", "bass", "other", "vocals", "guitar", "piano"], // output order
  "segment_samples": 343980,           // fixed model input length (roformer-mask: chunk = hop * (frames - 1))
  "overlap": 0.25,                     // demucs-hybrid: fraction; roformer-mask: num_overlap (integer)
  "stft": { "n_fft": 4096, "hop": 1024 },
  "session": {                          // per execution provider; all optional
    "dml": { "disable_graph_fusion": true, "enable_mem_pattern": false, "sequential": true },
    "cpu": { "intra_op_threads": 0 }    // 0 = app decides (default: min(physical cores, 16))
  },
  "self_test": "selftest.json",
  "license": {
    "spdx": "MIT",                      // or "unknown"
    "source": "https://github.com/facebookresearch/demucs",
    "notes": ""
  },
  "personal": false                     // true for user-supplied models
}
```

## Adapters

### `demucs-hybrid` (reference: `spikes/onnx_demucs_own.py`, `spikes/onnx_demucs.py`)
- ONNX inputs: `mix` (1, 2, S) float32 and `mag` (1, 4, n_fft/2, F) float32,
  where S = `segment_samples` and F = frames (336 for htdemucs_6s).
- ONNX outputs: `wave` (1, N, 2, S), `spec` (1, N, 4, n_fft/2, F).
- Song-level normalization: subtract mean, divide by std of the mono mix;
  undo on output.
- Chunking: demucs `apply_model` semantics: 25% overlap, triangular weights,
  short last chunk centered in a full segment filled with neighboring audio.
- STFT glue: `tools/reference/demucs_hybrid.py` (`spec`/`ispec`); exact
  padding rules in `tests/fixtures/README.md`. Assumes hop = n_fft / 4.

### `roformer-mask` (reference: `spikes/onnx_roformer.py`, `tools/reference/roformer_separate.py`)
- ONNX input: `stft` (1, 2, n_fft/2+1, F, 2) float32 (real/imag last).
- ONNX output: `mask` (1, N, (n_fft/2+1)*2, F, 2): complex masks; frequency
  axis is (freq, stereo) interleaved, stereo fastest.
- STFT: torch.stft(center=True, reflect pad, periodic Hann, not normalized).
- F = `segment_samples / hop + 1` (1151 for BS-RoFormer-SW, chunk 588800).
- Chunking: as in `tools/reference/roformer_separate.py`, which replicates
  audio-separator 0.48 `MDXCSeparator.demix`: step = chunk // overlap;
  starts 0, step, … while start + chunk < L, then one end-aligned tail chunk
  at max(L − chunk, 0); symmetric Hamming window over each chunk;
  out = Σ y·w / max(Σ w, 1e-10). `overlap` in the manifest = number of
  overlaps (e.g. 2 or 4). Songs shorter than one chunk are zero-padded to the
  chunk (static graph). No song normalization.

## `selftest.json`

Used to detect a GPU backend silently producing wrong output (e.g. DirectML
graph fusion on htdemucs_6s: probe SNR −36 to −102 dB, rms off by 10³–10⁷).
The input is **synthesized deterministically** (no audio file), so Rust and
Python produce the same input (float64 math in the order written, cast to
float32; libm `sin` may differ in the last ulp, irrelevant at these
tolerances):

```
for channel c in {0,1}, sample i in [0, S):
  t = i / sample_rate
  x[c][i] = 0.30*sin(2π·110·t) + 0.20*sin(2π·(440 + 3c)·t)
          + 0.15*sin(2π·1250·t)·(0.5 + 0.5*sin(2π·2·t))
          + 0.05*sin(2π·3·(1+c)·t + 7.0*sin(2π·0.5·t))
```
Length = exactly one model segment (`segment_samples`), run as a **single
segment, no chunking**:
- `demucs-hybrid`: song normalization over the segment (mean/std of the mono
  mix), one graph run + `ispec`, de-normalize. No triangular weights.
- `roformer-mask`: one chunk (STFT → graph → mask multiply → iSTFT). No
  window, no normalization.

The reference values are computed with the Python reference on the CPU EP
(`tools/reference/manifest.py`).

```jsonc
{
  "schema": 1,
  "input": "synth-v1",
  "length": 343980,
  "stems": {
    "drums": { "rms": 0.0123, "probe": [/* 64 floats */] },   // rms over both channels, all samples
    ...
  },
  "probe_offsets": [/* 64 sample indices, channel 0 */],     // floor((k + 0.5) * length / 64)
  "input_rms": 0.2447,     // rms of the synth input (both channels)
  "floor_rel": 0.1,        // level floor relative to input_rms
  "tolerance_db": 30.0,
  "rms_tolerance": 0.05
}
```

Pass rule, per stem (all stems must pass), with P = number of probes,
`ref` = expected probe, `got` = output channel 0 at `probe_offsets`,
`floor = floor_rel * input_rms`:

```
snr_db  = 10·log10( max(Σ ref², P·floor²) / Σ (got − ref)² )
rms_err = |rms(got stem) − rms| / max(rms, floor)
pass    = snr_db >= tolerance_db  and  rms_err < rms_tolerance
```

The floor matters: on synth-v1 several stems are near-silent (rms ~1e-4),
and their output is numerically noisy relative to their own level
(BS-RoFormer-SW ONNX vs PyTorch on CPU: 17–24 dB unfloored, > 90 dB floored).
Without the floor a healthy backend could fail.
