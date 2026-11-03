# shredlab-io

Audio file input and the simplest output: decode any supported file to
planar f32 stereo, resample it, and write WAV. Every other crate gets its
audio through here, so decoding behaves the same in the app, the CLI and
tests.

## Boundaries

- Owns: decoding (symphonia: MP3, FLAC, WAV, OGG Vorbis, M4A AAC/ALAC),
  resampling (rubato) and WAV writing (hound, 32-bit float or 16-bit int).
- Does not: encode FLAC or MP3 (export encoders live in `shredlab-audio`,
  stem cache encoding in `shredlab-library`), stream, or play audio. It
  takes paths from the caller and has no Tauri dependency.
- Depends on: `symphonia`, `rubato`, `hound`, `thiserror`. No workspace
  crates. The enabled symphonia codecs and containers are set in the
  workspace `Cargo.toml`.
- Used by: `app/src-tauri`, `shredlab-library` (reading cached stems and
  the cached original), `shredlab-audio` (tests and examples) and
  `shredlab-cli`.

## Layout

Everything is in `src/lib.rs`:

- `Audio`: `sample_rate` plus one `Vec<f32>` per channel; `as_slices()`
  gives the `&[&[f32]]` the DSP and separation code take.
- `decode_file`: file to `Audio` at its native rate, always two channels.
- `resample`: to a target rate, length-exact and delay-compensated.
- `load_for_separation`: `decode_file` then `resample` to
  `MODEL_SAMPLE_RATE` (44.1 kHz).
- `write_wav` with `WavFormat::F32` or `WavFormat::I16`.
- `IoError`: one error type with the path in the message.

## How it works

`decode_file` probes the format (the file extension is passed as a hint),
picks the first track with a real codec and decodes packets until end of
stream. Gapless mode is on, so encoder delay and padding declared in the
file (for example a LAME tag) are trimmed. Mono is duplicated to both
channels; more than two channels keep the first two.

`resample` uses rubato's `FftFixedIn` (chunk 1024, 2 sub-chunks), flushes
the tail, then drops the resampler's output delay and cuts to exactly
`round(len * rate / input_rate)` frames. Same rate, or empty input, is a
copy.

`write_wav` interleaves planar input. The 16-bit path clamps to [-1, 1],
scales by 32767 and rounds (no dither).

## Invariants and gotchas

- Gapless decoding is load-bearing: `shredlab-library` stores stems as MP3
  with a LAME tag and relies on symphonia trimming the encoder delay so
  the stems decode sample-aligned with the original. Do not turn
  `enable_gapless` off.
- Corrupt packets (`DecodeError`) are skipped silently, so a damaged file
  decodes shorter rather than failing. Packets whose channel count differs
  from the first decoded packet are dropped.
- `resample` output length is exact and aligned (delay compensated), which
  callers rely on to line up stems with the original. Keep both
  properties if you change the resampler.
- The 16-bit WAV path clips anything outside [-1, 1]; use `F32` for
  intermediate files.

## Testing

```
cargo test -p shredlab-io
```

The unit tests in `src/lib.rs` cover the float WAV round trip, 16-bit mono
duplication, 48 kHz to 44.1 kHz against an ideal sine, and the same-rate
no-op. They need no fixtures. FLAC, MP3 and gapless round trips through
this decoder are tested in `crates/shredlab-audio/src/encode.rs` and
`crates/shredlab-library/src/codec.rs`. `shredlab-cli decode <input>
<output.wav>` decodes any file to a 44.1 kHz float WAV for a quick manual
check.

## Common changes

- Supporting another format: enable the symphonia feature in the
  workspace `Cargo.toml`, then add the extension to `AUDIO_EXTS` in
  `app/src-tauri/src/commands.rs` (the open dialog filter) and to the text
  in `app/src/lib/EmptyState.svelte`. If the format needs its own handling
  of delay or padding, add a decode test with a known length.
