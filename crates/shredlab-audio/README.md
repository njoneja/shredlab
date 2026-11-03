# shredlab-audio

Plays a song's stems (and the original mix) through a mixer on the default
output device, and renders the same mix offline for export to WAV, FLAC or
MP3. Playback and export run the same pipeline, so an exported file matches
what was heard, including speed, pitch, Refine and the click track.

## Boundaries

Owns the real-time engine, the mixer (including the backing track built
from the original and the per-stem Refine chains), the Signalsmith Stretch
wrapper, click and count-in scheduling, both limiters, the offline renderer
and the encoders.

It does not decode files (the caller passes `Track`s: planar stereo, each
channel f32 or 16-bit integers times a step, all at the song rate, 44.1 kHz
in practice), implement the Refine DSP
(`RefineChain` and `RefineParams` come from
[shredlab-dsp](../shredlab-dsp/src/refine/mod.rs)), detect beats or the
music start (they arrive in `ClickConfig`), map the mixer's on/solo/volume
switches to gains, or re-derive stems for separation strength; the app does
the last two and calls `set_mix` / `load_with_original`. No Tauri
dependency; the only path it touches is the one passed to `encode`.

Dependencies: `shredlab-dsp`, cpal, rtrb, rubato, hound, flacenc,
mp3lame-encoder, `cc` at build time; `shredlab-io` for tests and the example
only. Used by `app/src-tauri` (`app/src-tauri/src/audio.rs` and the backend)
and by `shredlab-cli` (version only). Vendored and linked components and
the LAME (LGPL) obligations are in [THIRD_PARTY.md](THIRD_PARTY.md).

## Layout

Start with `src/lib.rs` (`Track`, `Channel`, `ClickConfig`, `Position`,
`AudioError`, re-exports), then `Engine` in `src/engine.rs` for playback or `render` in
`src/render.rs` for export. Both drive a `Pipeline`.

- `src/engine.rs`: device stream, producer thread, ring buffer, device-rate resampling, master volume, device watcher, end and loop-wrap callbacks.
- `src/pipeline.rs`: transport (play, pause, seek, loop, end), count-in state machine, stretcher voice, crossfades, click mixing, per-frame `Meta`.
- `src/mix.rs`: `Mixer`, `MixMode`, `choose_mode`: gained sum, original-based backing track, Refine chains, gain ramps, track-set crossfades.
- `src/stretch.rs`: safe `Stretcher` over the C ABI in `cpp/stretch_wrapper.cpp`; `build.rs` compiles it against the headers in `third_party/`.
- `src/click.rs`: click sounds and pure scheduling functions (`clicks_in_range`, `plan_count_in`, `count_in_interval`, `effective_start`).
- `src/limiter.rs`: playback safety limiter.
- `src/render.rs`, `src/encode.rs`: export (`render`, `RenderRequest`, `encode`, `ExportFormat`, the whole-file `limiter_gain`).
- `tests/refine.rs`: Refine through the mixer, playback pipeline and export.
- `examples/play.rs`: plays a folder of stem WAVs, or renders it to a file.

## How it works

### Threads

The cpal callback only pops frames (left, right, `Meta`) from an `rtrb`
ring, zero-fills and counts an underrun when it runs short, and publishes
the last frame's position and flags through atomics. A producer thread
handles control commands from a channel and renders the pipeline in
256-frame blocks until the ring holds about 25 ms (or twice the largest
callback plus a block), applying the master volume (smoothed) and a rubato
sinc resampler when the device rate differs from the song rate. After 5 s
paused it pauses the stream and blocks on the channel; shorter pauses keep
the stream running so Bluetooth outputs stay awake. An events thread runs
the end and loop-wrap callbacks (they may call back into the `Engine`), and
a watcher polls the default device every 2 s so the producer can reopen it.

Each frame's `Meta` carries song time, playing/end/wrap flags, count-in
clicks left and a 16-bit command generation. `play`, `pause` and `seek`
wait (up to 500 ms) for the producer's reply, and `Engine::position`
reports that commanded state until frames of the new generation reach the
output. End and wrap flags travel with their frame, so callbacks fire when
the event is heard.

### Pipeline

Per block, at the song rate: Refine and mix → stretch → fades and
crossfade tail → safety limiter (playback only) → clicks. The engine then
applies the master volume and resamples.

- Speed is clamped to 0.5–1.5 and pitch to ±12.5 semitones. Speed 1.0 at
  pitch 0 bypasses the stretcher (bit-transparent); switching in or out of
  bypass is crossfaded.
- `Voice::start` aligns the stretcher: reset, a pre-roll (one block plus
  one interval) ending at the start position plus the input latency, then
  the output latency rendered and discarded, so the first output frame is
  the start position.
- Seeks, loop wraps, pause and stretch on/off mix a fading tail of the old
  voice (about 10 ms) under the new one. Playback from silence gets a
  64-sample fade-in (real-time only).
- Loops under 50 ms are ignored; `play` outside `[a, b)` jumps to `a`, and
  at the end restarts from 0.

### Mixer

With `s_i` a whole stem, `r_i` the output of its Refine chain and `w` the
weight of the original (1 in `MixMode::Original` with an original loaded,
else 0):

```text
out = w·original + Σ (gain_i·r_i − w·s_i)
```

An unrefined chain passes its stem through, so this is
`original + Σ (gain_i − 1)·s_i`: all stems at unity give the original bit
for bit, an off stem is subtracted, and whatever the model missed (reverb,
transients) stays in the backing track. `choose_mode` picks Original when
nothing is soloed and at least half the stems are on ("on" ignores solo and
volume); the app uses it for playback and export alike.

Gain changes ramp over 512 samples, a mode change ramps `w` over 30 ms (a
linear crossfade between the formulas), and `load` crossfades to a new
track set over 2048 samples, keeping the position. That is how the app
moves from the original to fresh stems, or to stems re-derived for a new
separation strength, without a gap.

Every stem's `RefineChain` (stereo-window STFT mask, then EQ) has a constant
latency of `PAN_LATENCY` (4096 samples), even when it does nothing. Instead
of delaying every other path, the mixer reads each stem that far ahead of
the output position. When a mix call doesn't continue where the last one
ended (seek, loop wrap, play start, stretcher pre-roll, new track set), the
chains are reset, primed with the look-ahead and run over the 8192 samples
before that point, so filters and overlap-add are settled. The chain is
block-size independent, so playback and export produce the same samples.
Which part is heard (stem without the window, or the window alone) is set
by the caller through `PanParams::invert`. `set_analysis` picks the one
track whose stereo field is published for `Engine::take_field`.

### Click track and count-in

Clicks are short synthesized sines (accent, beat, softer subdivision)
triggered at song times, so they follow the speed. A beat is accented when
a downbeat lies within 1 ms of it.

`plan_count_in` counts into the first beat at or after the start; the last
click sounds one interval before it. The interval (`count_in_interval`) is
the longest of the next 8 beat intervals that at least 3 of them agree with
within 10%, divided by the speed, which ignores beats the tracker split in
half, a lone missed beat, and an intro at a different pulse. A pickup plays
under the last clicks; after a long intro the audio starts at once and the
clicks come in over it. `effective_start` applies `music_start`: with a
count-in, a start at the song start (or before the first beat) jumps to the
music start unless that is past the loop or range end. With
`count_in_each_repeat`, playback stops at the loop end and counts in again.

### Limiters and export

The playback limiter (`src/limiter.rs`) is linked stereo with a 64-sample
look-ahead and a tanh soft knee from −1 dBFS toward a −0.1 dBFS ceiling; it
is bit-transparent below the threshold apart from its delay. Export uses
`limiter_gain` instead: a gain envelope over the whole file (look-ahead,
80 ms release, −0.1 dBFS ceiling), or `None` when nothing exceeds it.

`render` builds a non-real-time `Pipeline` (no start fade, no limiter,
click track only if requested) with the request's gains, mode, Refine,
speed and pitch, seeks to the range start, plays once and stops at the
range end. The result is `round(range ÷ speed)` frames plus the count-in,
with 8 ms fades only at cut points inside the song. With a click track or
count-in, `render_split` keeps the clicks in a separate buffer; the music
goes through `limiter_gain` and the clicks are added after it, as in
playback. `encode` limits the final sum again and writes WAV 16-bit (TPDF
dither from a fixed seed), WAV 24-bit, FLAC 24-bit or MP3 CBR (bitrate
snapped to a standard rate), removing the partial file on error.

## Invariants and gotchas

- The cpal callback takes no locks and does not allocate; anything new it
  needs goes through `Frame`/`Meta` or an atomic. The producer may allocate
  when settings change, but steady-state rendering must not, and it never
  blocks on the API side (the stereo field is written with `try_lock`).
  Underruns show up in `Engine::stats`.
- `Track::quantized` holds a stem as 16-bit integers (half the memory of
  f32), divided by `max(peak, 1)` and rounded with exactly the arithmetic of
  the FLAC stem cache writer in `shredlab-library`, so a stem reloaded from a
  FLAC cache gives the same integers as the fresh stem (tested in the app's
  backend). Change the two together. The mixer reads every track through
  `Channel::read` / `get`; the bit-exact properties below hold for the
  values a track yields (`int × step` for a quantized one).
- All tracks of a set, including the original, must share the sample rate
  and be sample-aligned; the length is the shortest. The original must come
  from the same decode as the separation input.
- Gains are matched by name and missing names get 0. In Original mode a
  stem at gain 0 is subtracted, so a stem left out of the gain list also
  disappears from the backing track.
- Bit-exact properties are tested and relied on: an unstretched unity
  render equals the input, all stems at unity in Original mode equal the
  original, identity Refine equals the plain mix, and the device path at
  the song rate skips the resampler. The fast paths in the mixer loop
  exist for this.
- Playback and export share `Pipeline`, `Mixer`, `choose_mode` and the
  click code; don't special-case one side. Export needs `realtime = false`
  and no pipeline limiter (`render_split` asserts it in debug builds).
- `render` resolves `music_start` once (`render_range`), then clears it in
  the pipeline's click settings; leaving it set moves the start twice.
- Refine stems are read ahead by the chain latency, never delayed. The
  warm-up length must stay a multiple of the mask hop so a start at 0
  matches `refine_offline` exactly.
- With the playback limiter on, clicks are scheduled `Limiter::latency()`
  samples late to stay in time with the delayed music.
- The stretcher has a fixed seed (renders are deterministic), and
  `build.rs` always compiles it at `-O3`; unoptimized it is about 10 times
  slower. A Signalsmith update can change its latencies;
  `stretched_output_is_aligned_with_reported_position` checks alignment.
- The master volume is playback only; exports ignore it.
- flacenc writes the last block's size as the minimum block size, which
  symphonia rejects; `fix_streaminfo_min_block` patches it (the same fix
  is in `shredlab-library`).
- The events thread is not joined on drop, because a callback may own the
  `Engine`.

## Testing

```sh
cargo test -p shredlab-audio                 # unit tests + tests/refine.rs
cargo test -p shredlab-audio --test refine   # Refine integration tests only
```

No audio device, models or fixtures are needed, and nothing is ignored:
engine tests drive the producer side without a device, encoder tests
round-trip through a temp directory with `shredlab-io`. To listen:

```sh
cargo run --release -p shredlab-audio --example play -- <stems-dir> --speed 0.8 --pitch -2 --loop 10,20 --click --count-in 4
cargo run --release -p shredlab-audio --example play -- <stems-dir> --render out.flac
```

`SHREDLAB_AUDIO_DEVICE_RATE=48000` requests that device rate (if
supported) to exercise the resampler.

## Common changes

- Adding an export format: an `ExportFormat` variant and a writer in
  `src/encode.rs` reading through `Samples` (limiter gain applied,
  clamped), a round-trip test, then the mapping in the app's export
  command. Note a new encoder dependency in [THIRD_PARTY.md](THIRD_PARTY.md).
- Adding an engine control: a `Cmd` variant, an `Engine` method that sends
  it, a match arm in the producer loop, and the behavior on `Pipeline` (and
  `Mixer` if it affects the mix). If export should honor it, add a field to
  `RenderRequest` and apply it in `render`.
- Changing the click sounds: `ClickBank::new` and `synth` in `src/click.rs`.
- Dropping MP3 export: see the LGPL note in [THIRD_PARTY.md](THIRD_PARTY.md).
