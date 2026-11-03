# tools/reference/

NumPy reference implementation of the app's separation pipeline: the two
adapters' DSP and chunking, the `synth-v1` self-test input, and the
`model.json` / `selftest.json` helpers. The Rust port is validated against
it.

## Boundaries

- Importable package (`from tools.reference import ...`) with the repo root
  on `sys.path`. Inference needs only numpy, scipy and onnxruntime; no
  torch. Works in `.venv` (CPU and DirectML) and in `.venv-models` (CPU).
- Used by `tools/setup_models.py`, `tools/registry.py`,
  `tools/add_personal_model.py`, the fixture generators and
  `tools/validate_roformer_chunking.py`. Never imported by the app.
- Rust counterparts: `crates/shredlab-dsp/src/hybrid.rs` (`spec` /
  `ispec`), `crates/shredlab-dsp/src/stft.rs` (RoFormer STFT),
  `crates/shredlab-dsp/src/chunk.rs` (song normalization, both chunking
  schemes), `crates/shredlab-separation/src/adapters.rs` (graph I/O) and
  `crates/shredlab-separation/src/selftest.rs` (synth-v1, pass rule). Their
  doc comments point back here.
- The spikes (`spikes/onnx_demucs_own.py`, `spikes/onnx_demucs.py`,
  `spikes/onnx_roformer.py`) are where this code was first written;
  this package is the maintained copy and is numerically identical to them.
- Graph I/O, chunking rules and the self-test format are specified in
  [docs/MODEL_MANIFEST.md](../../docs/MODEL_MANIFEST.md); exact STFT padding
  rules in [tests/fixtures/README.md](../../tests/fixtures/README.md).

## Layout

Start with `demucs_hybrid.py` (the bundled model's adapter).

- `demucs_hybrid.py`: `demucs-hybrid` adapter. HTDemucs `_spec` / `_ispec`
  equivalents, complex-as-channels packing, song normalization,
  `apply_model` chunking, deterministic shifts, and the single-segment
  self-test path. Also a CLI that separates an audio file to WAVs.
- `roformer_separate.py`: `roformer-mask` adapter. `torch.stft` /
  `torch.istft` equivalents (center, reflect, periodic Hann), mask multiply,
  and the chunking of audio-separator 0.48 `MDXCSeparator.demix`.
- `synth.py`: the `synth-v1` generator.
- `manifest.py`: builds `model.json`, computes and writes `selftest.json`,
  implements the self-test pass rule, verifies a model folder, hardlinks
  ONNX files into folders.
- `sessions.py`: onnxruntime session from a manifest's `session` block, for
  `cpu` or `dml`.
- `data/`: reference outputs of the known-good bundled exports, used by
  `tools/setup_models.py` step 6.

## How it works

### `demucs-hybrid`

The graph takes the normalized waveform segment `mix` (1, 2, S) and its
complex-as-channels spectrogram `mag` (1, 4, n_fft/2, F); it returns `wave`
(1, N, 2, S) and `spec` (1, N, 4, n_fft/2, F). The segment output is
`wave + ispec(uncac(spec))`. S and n_fft are read from the graph's input
shapes; hop is n_fft / 4, which `ispec` asserts.

For a song (2, L): normalize by mean and std of the mono mix (float32 numpy
semantics, population std, floored at float32 eps); chunk with
stride = int(0.75 * S) at offsets 0, stride, ... < L; center each chunk of
n = min(S, L - offset) samples in a full S window filled with neighboring
audio (zeros past the ends); run, crop back to n, multiply by triangular
weights[:n], overlap-add; divide by the summed weights; de-normalize.

Shifts (`separate(..., shifts=N)` with N > 1): normalize once over the real
audio, then for each offset in `shift_offsets(N)` pad the normalized song
with `max_shift - offset` zeros in front and `offset` behind, run the
chunking, crop, average, de-normalize. `max_shift` is half a second; offsets
are `round(k * max_shift / N)` with halves rounded up, so N = 2 at 44.1 kHz
gives [0, 11025]. Upstream Demucs draws random offsets; the deterministic
ones make output reproducible and match `shredlab_separation::shift_offsets`.

### `roformer-mask`

The graph takes `stft` (1, 2, n_fft/2+1, F, 2) and returns `mask`
(1, N, (n_fft/2+1)*2, F, 2), frequency and channel interleaved with the
channel fastest. Chunk length = hop * (F - 1), read from the graph. Chunk
starts are 0, step, 2*step, ... while start + chunk < L, then one tail start
at max(L - chunk, 0) unless it equals the previous start; step =
chunk // num_overlap. Each chunk is weighted by a symmetric Hamming window;
the result is divided by max(summed window, 1e-10). No song normalization.
Inputs shorter than one chunk are zero-padded (the graph has a static frame
count), so they differ slightly from audio-separator, which runs them at
their true length.

### Self-test helpers (`manifest.py`)

`write_selftest` runs synth-v1 of exactly `segment_samples` through the
adapter's single-segment path (no chunking, no weights) on the given EP and
stores 64 channel-0 probes and the rms per stem, plus `input_rms`,
`floor_rel` = 0.1, `tolerance_db` = 30 and `rms_tolerance` = 0.05.
`check_selftest` applies the floored probe-SNR and rms rule documented in
MODEL_MANIFEST.md. `verify_model_dir` raises on a sha256 mismatch, then runs
the self-test; its `override` argument is merged over the manifest's session
options (`{"disable_graph_fusion": False}` reproduces the DirectML bug).

### Sessions (`sessions.py`)

CPU: `intra_op_threads` from the manifest, or when 0, logical cores / 2
capped at 16 (ONNX Runtime's default was several times slower on a CPU with
performance and efficiency cores). DirectML: `enable_mem_pattern`,
`sequential` and `disable_graph_fusion` (as the session config entry
`ep.dml.disable_graph_fusion`) from the manifest.

### Reference data (`data/`)

- `htdemucs_6s_selftest.json`: the `selftest.json` of the known-good
  `htdemucs_6s` folder, copied verbatim. `setup_models.py` compares a fresh
  build's CPU output against it with the normal pass rule.
- `beat_this_final0_check.json`: Beat This! beat and downbeat logits on the
  first 1500 log-mel frames of mono synth-v1 at 22.05 kHz (100 probes and
  the rms per output), plus the sha256 of the ONNX file it came from.
  Written by `python tools/setup_models.py _worker make-beat-reference
  --onnx <file>`.

## Invariants and gotchas

- synth-v1 is part of the model format: every `selftest.json` (bundled and
  personal), the Rust generator, the fixtures and `data/` depend on it, and
  the Rust manifest parser accepts only the input name `synth-v1`. Don't
  change it; define a new generator name instead. All of its arithmetic is
  float64 in the documented order, then cast to float32.
- The DSP must stay numerically compatible with torch: `checks` in
  `tests/fixtures/index.json` records about 1e-7 relative error for both
  STFTs and both iSTFTs. A change here moves the committed fixtures, the
  model folders' `selftest.json`, and `data/`.
- The Demucs roundtrip `ispec(spec(x))` is not exact near the edges, by
  design: frames 0-1, the tail frames and the Nyquist bin are dropped, as in
  HTDemucs.
- Triangular weights, chunk offsets and RoFormer chunk starts are compared
  exactly (not within a tolerance) by `crates/shredlab-dsp/tests/fixtures.rs`.
- `make_session` builds sessions only for `cpu` and `dml`; there is no
  CoreML or CUDA path in Python.
- `tools/registry.py` and `tools/add_personal_model.py` own the session
  blocks they write into manifests (`DEMUCS_SESSION`, `ROFORMER_SESSION`);
  this package only reads them.

## Testing / running

There are no Python unit tests; the package is exercised through its users
and checked against torch by `tools/make_fixtures.py` (see `checks` in its
output) and against PyTorch inference by
`tools/validate_roformer_chunking.py` and `tools/add_personal_model.py`.

```
python -m tools.reference.demucs_hybrid song.mp3 out/demucs [--shifts 2] [--model-dir DIR] [--ep cpu|dml]
python tools/registry.py verify models/registry/htdemucs_6s --ep cpu
```

From Python, with the repo root on `sys.path`:

```python
from tools.reference import demucs_hybrid, manifest
from tools.reference.sessions import make_session
man = manifest.load_manifest("models/registry/htdemucs_6s")
sess = make_session("models/registry/htdemucs_6s/" + man["file"], "cpu", man["session"])
stems = demucs_hybrid.separate(sess, mix)   # mix: (2, L) float32 at 44.1 kHz -> (6, 2, L)
```

## Common changes

- Changing the STFT or chunking: change it here and in the Rust crate
  together, run `python tools/make_fixtures.py` and
  `cargo test -p shredlab-dsp --test fixtures`, regenerate the goldens
  (`tools/make_goldens.py`) and run the ignored golden tests, rebuild the
  model folders, and update `data/` (see [tools/README.md](../README.md)).
- Adding an adapter: add a module with `separate` and
  `separate_single_segment`, dispatch to it in
  `manifest.run_selftest_input`, document its graph I/O and chunking in
  MODEL_MANIFEST.md, and add the matching adapter in `shredlab-separation`.
