# shredlab-dsp

Pure-Rust signal processing for separation and playback: the STFT/iSTFT
and chunking glue that runs around the ONNX networks (ported from the
Python reference so it can be checked number for number), plus the Refine
chain (stereo-position mask and EQ) and separation strength.

## Boundaries

- Owns: `torch.stft`-compatible STFT/iSTFT, the Demucs hybrid spectrogram
  (`HTDemucs._spec`/`_ispec`), Demucs `apply_model` chunking and MSST-style
  windowed overlap-add, song normalization, the per-stem Refine chain, EQ
  presets, stereo-field analysis and separation strength.
- Does not: touch files, decode audio, run models or know about stems as
  named things. Model calls are closures passed in by
  `shredlab-separation`; audio comes in as slices.
- Depends on: `realfft` (FFTs) and `serde` (Refine parameters are saved in
  `song.json`). No workspace crates, no Tauri.
- Used by: `shredlab-separation` (STFT, chunking), `shredlab-audio`
  (`RefineChain` per stem in playback and export), `shredlab-library`
  (`RefineParams` in the song manifest), `app/src-tauri` (strength, offline
  refine, presets) and `shredlab-cli` (version only).

## Layout

Start with `src/lib.rs` (module overview, windows, reflect padding), then
the module for the area you are changing.

- `src/stft.rs`: `Stft`, `torch.stft`/`istft` with `center=True`; used by
  `roformer-mask`.
- `src/hybrid.rs`: `HybridStft`, the Demucs hybrid spectrogram; used by
  `demucs-hybrid`.
- `src/chunk.rs`: `song_stats`, `triangular_weights`, `chunk_offsets`,
  `padded_window`, `apply_demucs`, `roformer_chunk_starts`,
  `apply_windowed`.
- `src/refine/mod.rs`: `RefineParams`, `RefineChain` (pan mask then EQ),
  `refine_offline`.
- `src/refine/pan.rs`: `PanMask`, the streaming STFT stereo-position mask
  and the stereo-field analysis; `pan_offline`.
- `src/refine/eq.rs`: `Eq`, low/high end (cut or shelf) plus three peaking
  bands; `response_db` for the UI curve.
- `src/refine/presets.rs`: `EQ_PRESETS`, data plus the reasoning behind
  each preset.
- `src/strength.rs`: `strength_stream` / `apply_strength`, multi-threaded
  offline re-derivation of stems.
- `examples/`: `refine_bench` (CPU cost of one chain), `refine_wav` and
  `strength_song` (listen to / measure on a WAV or a cached library song;
  they read the library and write only under the gitignored `out/`).

## How it works

Audio is planar: one slice per channel. Multi-stem outputs are flat
`(stems, channels, samples)` buffers, last axis fastest. Spectra are
`(bins, frames)` row-major.

`Stft` follows `torch.stft(center=True, pad_mode="reflect")` with a
periodic Hann window, not normalized: reflect-pad `n_fft/2` on both sides,
`1 + len / hop` frames, `n_fft/2 + 1` bins. The inverse overlap-adds the
windowed irfft, divides by the summed squared window (floor 1e-8) and drops
`n_fft/2` at the start.

`HybridStft` reproduces HTDemucs: an extra reflect pad of `3*hop/2` (plus
enough on the right to reach `ceil(len / hop) * hop`), the `center=True`
pad, a window divided by `sqrt(n_fft)` (`normalized=True`), then the
Nyquist bin is dropped and only frames `[2, 2 + le)` are kept. `ispec`
undoes it with a zero Nyquist bin, two zero frames on each side and an
envelope floor of 1e-11. The exact padding rules, and the CaC channel
layout of the graph's `mag` input, are in
[tests/fixtures/README.md](../../tests/fixtures/README.md).

Chunking (`src/chunk.rs`) has two schemes, one per adapter; the rules are
in [MODEL_MANIFEST.md](../../docs/MODEL_MANIFEST.md) and the fixture
README. `apply_demucs`: stride `int((1 - overlap) * segment)`, each chunk
centered in a full segment filled with neighboring audio (zeros past the
ends), triangular weights, divided by the summed weights.
`apply_windowed`: starts `0, step, ...` while `start + chunk < len`, then
one end-aligned tail; a symmetric Hamming window; divided by
`max(sum w, 1e-10)`. Both take the model as a fallible closure, so a
cancel or ORT error aborts the loop. `song_stats` is the mean and
population std of the mono mix, std floored at `f32::EPSILON`.

Refine. `RefineChain` is a `PanMask` followed by an `Eq`, run per stem at
the song rate. The pan mask uses an STFT of 4096/1024 (periodic Hann for
analysis and synthesis). Each bin's pan is `(|R| - |L|) / (|L| + |R|)` and
one real gain is applied to both channels, so a kept part keeps its own
stereo image. The selection is `center ± width/2` with raised-cosine edges
`softness` wide; the gain is exactly 0.5 at the edge, so a window and its
inverse sum to the input, and the Left/Center/Right presets tile the
field. The EQ uses RBJ cookbook responses (4th-order Butterworth cuts as
two sections, S = 1 shelves up to ±12 dB, peaking bands), implemented as
trapezoidal state-variable filters in f64 so gliding coefficients do not
zipper. Parameters are smoothed (pan once per hop, 30 ms; EQ every 32
samples, 20 ms). The pan mask can also accumulate a stereo-field histogram
(41 pan bins by 3 bands, split at 250 Hz and 2.5 kHz) for the Refine panel.

Separation strength re-derives every stem from the original as
`|S_i|^p / sum_j |S_j|^p · X` per bin and channel (4096/1024, f32 FFTs).
The masks sum to 1, so the derived stems sum to the original for any `p`.
`p` is clamped to 0.5..4 (`STRENGTH_MAX_P`). The `smooth` option instead
scales each stem's own spectrum by `min(1, m_i(p) / m_i(1))`, staying
close to the model's stems at low `p`, at the cost of no longer summing to
the original. Work is split into chunks of frames on a scoped thread pool
and handed to the sink in song order, so nothing song-sized is held
besides the inputs and outputs.

## Invariants and gotchas

- Numeric compatibility with the Python reference is the point of
  `Stft` and `HybridStft`. `Stft` windows in f32, transforms in f64 and
  keeps spectra in f64, as the numpy reference does: the RoFormer graph
  amplifies tiny input differences (a -138 dB STFT rounding difference
  became -67 dB in its mask output). Do not "simplify" it to f32.
  `HybridStft` is f32 throughout, with windows computed in f64 then cast,
  matching the reference.
- `reflect_pad` is numpy `mode="reflect"` (edge sample not repeated) and
  keeps reflecting when the pad is longer than the signal.
- Demucs' own spectrogram round trip is not exact near the edges (frames
  0-1 and the tail are discarded). Compare against `demucs_istft.bin`, not
  the input.
- `song_stats` averages channels in f32 (as numpy does) and accumulates in
  f64; expect about 1e-7 relative difference from the fixture values.
- `HybridStft` assumes `hop * 2 <= n_fft` (the models use hop = n_fft/4);
  `Stft` requires an even `n_fft`.
- `RefineChain` has a constant latency of `PAN_LATENCY` (4096) samples,
  enabled or not. With identity parameters the output is the input delayed
  by exactly that, bit for bit (no FFTs, EQ skipped). `shredlab-audio`
  relies on this: an unrefined chain must be an exact delay for the
  backing-track mix formula to hold. Tests assert bit equality; keep it.
- Real time: `RefineChain`, `PanMask` and `Eq` allocate only in `new`;
  `process*`, `set_params`, `reset` and `prime` never allocate or lock.
  `set_params_now` jumps without smoothing and can click mid-stream; use it
  only after `reset`, before the first sample or offline. After a seek,
  call `reset` then `prime` with the next `latency()` input samples.
- Switching the pan mask on seeds the overlap-add state with what identity
  frames would have produced, so enabling or disabling it is seamless; it
  drops back to the exact delay once only identity frames remain.
- Serialization of `RefineParams` is the `song.json` format. All structs
  use `#[serde(default)]` so missing fields load as defaults.
  `PanParams::invert` is `#[serde(skip)]`: it is a run-time flag derived
  from the mixer row's on switch, never saved. An old `mode` field is
  ignored, and old `CutParams` without `mode`/`gain_db` load as cuts.
  Preset `id`s are stable identifiers; do not rename them.
- No preset boosts by more than about 6 dB (asserted in tests), and only
  `flat` is the identity.
- Strength at `p = 1` is not the cached stems: the model's stems do not
  sum to the original, so the ratio mask redistributes that leftover and
  takes the original's phase. Callers should use the cached stems at the
  Natural end and this function only for `p > 1`.
- Strength output does not depend on the thread count (single vs multi
  thread agree to 1e-6 in tests); `cancel` is checked between chunks.

## Testing

```
cargo test -p shredlab-dsp
```

- `tests/fixtures.rs` compares STFT, iSTFT, song normalization and both
  chunking schemes with the committed vectors in
  [tests/fixtures/](../../tests/fixtures/README.md) (relative error below
  1e-5 for the STFTs, 1e-6 for chunking). It skips with a message if the
  fixtures are missing. Regenerate them with
  `.venv\Scripts\python.exe tools/make_fixtures.py`.
- `tests/refine.rs` covers mask selectivity, exact bypass, streaming vs
  offline for any block size, reset and prime, click-free parameter
  changes, the analytic EQ and shelf responses, the stereo field and
  serialization compatibility.
- `tests/strength.rs` covers the sum property, ordering, progress,
  cancellation, thread-count independence and the smooth variant.
- Manual checks: `cargo run --release -p shredlab-dsp --example
  refine_bench`; `refine_wav` and `strength_song` take a song from the
  local library (`SHREDLAB_DATA_DIR` or the app's data folder). See the
  doc comment at the top of each example for its arguments.

The Demucs and RoFormer pipelines are tested end to end, with models, in
`shredlab-separation` (see its [README](../shredlab-separation/README.md)).

## Common changes

- Changing STFT or chunking behavior: change the Python reference in
  `tools/reference/` first, regenerate the fixtures, then match it here
  until `tests/fixtures.rs` passes, and run the separation golden tests.
- Adding a Refine parameter: add the field with a default that is the
  identity, keep `#[serde(default)]`, extend `is_identity` so the exact
  bypass still holds, smooth it in the processor, and add a
  serialization test for old saved data.
- Adding an EQ preset: append to `EQ_PRESETS` in
  `src/refine/presets.rs` (Flat stays last), with a new stable `id` and a
  line in the module comment explaining it. The UI reads the list.
