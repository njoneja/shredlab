# shredlab-beats

The model-independent half of beat and downbeat tracking with Beat This!
(CPJKU, MIT): audio to log-mel chunks for the network, network logits to
beat times, a bar decoder for the downbeats, plus tempo and meter helpers. Inference is a
closure, so the crate has no ONNX Runtime dependency.

## Boundaries

Owns preprocessing, chunking and stitching, peak picking, downbeat
decoding and the `BeatGrid` helpers. It does not load or run the model (the app backs the
closure with `BeatModel` from `crates/shredlab-separation/src/beat_model.rs`),
pick a device, store the grid (`BeatData` in `shredlab-library`), apply the
user's "this beat is one" and beats-per-bar overrides (app and UI), or
schedule clicks (`shredlab-audio`). It takes no paths.

Dependencies: realfft, rubato, thiserror. `ort`, symphonia and serde_json
are dev-dependencies for the examples only (`ort` pinned to the same
version as `shredlab-separation`). Used by `app/src-tauri` (the beat job and
`app/src-tauri/src/music_start.rs` tests).

## Layout

Everything is in `src/lib.rs`; `track` (or `track_scored`, which also
returns the per-beat downbeat scores) is the entry point: preprocess,
infer, `postprocess_bars`. The file is split into preprocessing, chunking,
postprocessing, downbeat decoding and UI helper sections.

- `tests/reference.rs`: log-mel, resampling and peak picking against the Python reference.
- `examples/beat_e2e.rs`: the full pipeline with ONNX Runtime on CPU, compared with PyTorch outputs.
- `examples/library_meter.rs`: upstream downbeats vs the bar decoder on every song of a ShredLab library.

## How it works

The ONNX graph contains only the network: a log-mel chunk `(1, 1500, 128)`
in, beat and downbeat logits `(1, 1500)` out. Everything around it is
ported from the Python reference `spikes/onnx_beat_this.py`.

1. `preprocess`: average the channels (in f64, as upstream), resample to
   22,050 Hz, then `log_mel`: `torch.stft` semantics (centered, reflect
   padding, normalized, periodic Hann, 1024-point FFT, hop 441 = 50 frames
   per second), magnitude, 128 Slaney mel bands from 30 Hz to 11 kHz
   without normalization, `log1p(1000·x)`. A song of `n` samples at
   22,050 Hz gives `n / 441 + 1` frames.
2. `split` / `infer` / `aggregate`: 1500-frame chunks with a 6-frame border
   (upstream `split_piece` with `avoid_short_end`: the last chunk is
   end-aligned, zero padding where a chunk runs past the song). `infer`
   calls the closure once per chunk and checks that both outputs have 1500
   values. `aggregate` stitches with `keep_first` (earlier chunks win in
   the overlap); uncovered frames get −1000.
3. Beats: a frame is a peak if it is the maximum of its 7-frame
   neighborhood and above 0; peaks at most one frame apart merge into their
   mean; frames become seconds (upstream "minimal" postprocessor).
4. Downbeats (`postprocess_bars`): each beat gets a downbeat score, the
   largest downbeat logit within two frames of it (`downbeat_scores`).
   `decode_downbeats` runs Viterbi over the beats with state (home meter
   2..=7, length of the current bar, position in it). A beat costs
   `softplus(-s)` as a downbeat and `softplus(s)` otherwise (`s` clamped to
   ±8). After the last beat of a bar the next bar starts in the home meter
   for free; a one-off bar of another length (1 to 7 beats) costs 6, or 2
   when it lasts as long as a home bar in seconds (the beat tracker added
   or missed a beat), interpolated up to 6 at a 15% timing error, plus 8
   more for a one-beat bar; a lasting change of home meter costs 8. Bars
   also pay a small per-length cost (4: 0, 3: 0.5, 2 and 6: 1, 5 and 7:
   1.5). The first beat may sit anywhere in a bar (pickup) and the last bar
   may be incomplete. The costs are `DecoderCosts::default()`.
   `postprocess` keeps upstream's minimal downbeats (peak picking on the
   downbeat logits, snapped to the nearest beat) for the reference tests
   and comparisons.
5. `BeatGrid` helpers: `tempo_bpm` (60 over the median beat interval),
   `beats_per_bar` (beats in each complete bar between downbeats, pickup
   excluded) and `most_common_beats_per_bar` (ties go to the smaller value).

Resampling uses rubato's FFT resampler instead of soxr. The FFT length is
chosen so the anti-alias roll-off near 11 kHz is close to soxr HQ, which
the reference uses, because the top mel bands are sensitive to it. The
output length matches soxr (`round(n · 22050 / rate)`) and the resampler
delay is removed.

## Invariants and gotchas

- The decoder never moves or adds beats: downbeats are a subset of the
  beats, and `decode_bars` decodes for a given beat list, so a caller can
  keep an earlier list (the app's meter overrides are beat indices).
- `DOWNBEAT_DECODER_VERSION` must be bumped whenever the decoder's output
  changes (costs included); the app stores it with the grid and decodes
  older grids again, from the stored scores when it has them.
- Scores are raw logits per beat, so they stay valid for any decoder that
  works on the same beats.

- The numerics follow torchaudio and NumPy on purpose: NumPy-style reflect
  padding (repeating for long pads), `linspace` with an exact last element,
  the Slaney mel formula. They look simplifiable; don't simplify them
  without rerunning the reference tests.
- The resampler's output FFT length must be even, otherwise rubato's
  integer delay is off by half a sample. Changing the FFT length also
  changes the top mel bands.
- Remaining differences from the Python pipeline come from rubato vs soxr
  and from symphonia vs libsndfile MP3 decoding. The reference test asks
  for a log-mel max difference below 1e-3 on identical input, but only
  above 35 dB spectrogram SNR for the full preprocessing.
- `CHUNK`, `N_MELS` and `INPUT_SHAPE` are the model's contract. They must
  match the exported graph (`models/onnx/beat_this_final0.onnx`, local, built
  by `tools/setup_models.py`).
- Keep `ort` out of `[dependencies]`; the closure keeps this crate free of
  ONNX Runtime and testable without a model.

## Testing

```sh
cargo test -p shredlab-beats
```

The unit tests always run; the decoder tests use synthetic scores (steady
4/4 with spurious and missed peaks, a lasting 3/4 to 4/4 change, a clear
one-off 2-beat bar, 3/4, 2 and 6 beats per bar, pickups, an extra beat
inside a bar). `tests/reference.rs` needs fixtures in
`tests/fixtures/local/beat_this/` (local, not committed); without them each
test prints `SKIP` and passes. Generate them with the dev Python
environment:

```sh
.venv/Scripts/python.exe tools/beat_this_fixtures.py
```

End-to-end check with the real model (local, see above) on CPU, compared
with the PyTorch reference when fixtures exist:

```sh
cargo run --release -p shredlab-beats --example beat_e2e -- [--model <onnx>] [--threads 8] <audio>...
```

Upstream vs decoded downbeats on a library (the logits are cached per
song with `--cache`, so decoder changes rerun in seconds):

```sh
cargo run --release -p shredlab-beats --example library_meter -- [--library <dir>] [--cache <dir>] [--song <title>] [--dump <t0> <t1>]
```

## Common changes

- Tuning the downbeat decoder: change `DecoderCosts::default()`, bump
  `DOWNBEAT_DECODER_VERSION`, and compare with `library_meter` on real
  songs before and after.

- Changing beat peak picking or the tempo and meter helpers: edit
  `peaks`/`dedup`, `postprocess` or the `BeatGrid` methods. `postprocess_matches_upstream` (with local
  fixtures) requires output identical to upstream's minimal postprocessor,
  so a deliberate change to `postprocess` means changing that test as well.
- Supporting a beat model with a different input: change the constants at
  the top of `src/lib.rs` and the preprocessing to match its training
  pipeline, and regenerate the reference fixtures from its Python code.
