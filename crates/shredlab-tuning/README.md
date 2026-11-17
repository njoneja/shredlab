# shredlab-tuning

Tuning analysis: how far a song is from A440, measured from its separated
stems. Pure DSP: mono `f32` samples in, a `TuningEstimate` out.

## Boundaries

Owns the offline song estimator (`estimate_song_tuning`). It does not
decode or downmix stems, pick which stems exist, run as a job, store the
result in `song.json`, or turn the offset into a Pitch setting; those
belong to the app and `shredlab-library`. It takes no paths and opens no
audio device. A live pitch detector for a microphone input is planned as
a second module next to the song estimator and will reuse the spectrum
and cents helpers.

Dependencies: realfft. Results are plain structs without serde; callers
map them to their own storage types.

## Layout

- `src/lib.rs`: public types (`StemKind`, `StemAudio`, `TuningEstimate`,
  `Segment`), the algorithm `VERSION` and the reliability thresholds.
- `src/song.rs`: the song estimator: per-stem frame loop, stem weights,
  segments and the drift rule.
- `src/spectrum.rs`: Hann-windowed spectra, peak picking with interpolation,
  and the non-octave harmonic filter.
- `src/cents.rs`: cents conversion, folding and the weighted circular mean.
- `src/tests.rs`: estimator tests on synthesized stems.

## How it works

Each stem (mono, song rate) is analyzed on its own thread:

1. STFT with a Hann window of 8192 samples and a hop of 2048 at 44.1 kHz.
   Other rates scale both to keep the same duration and resolution.
2. Frames whose RMS is more than 40 dB below the stem's loudest frame are
   skipped. A stem whose loudest frame is below −90 dBFS is silent.
3. Peaks: local maxima within 40 dB of the frame's strongest bin, between
   30 Hz (bass) or 70 Hz (other stems) and 2 kHz. Each peak is refined by
   quadratic interpolation on the magnitude raised to the power 0.2308,
   which is nearly unbiased for a Hann window (interpolating on the log
   magnitude is off by up to 0.016 bins, several cents for bass notes).
4. Peaks within ±20 cents of a non-octave multiple (3, 5, 6, 7, 9, ...) of
   a stronger lower peak are dropped: those partials are off equal
   temperament, and string stiffness sharpens them further. Octaves (2, 4,
   8, ...) are kept. Multiples above 34 are not tested, because their ±20
   cent windows nearly cover the whole semitone.
5. Each remaining peak adds a unit vector at angle `2π·cents/100` (cents
   from A440) with weight `sqrt(magnitude)`, so a few loud notes don't
   dominate.

Stems are combined by summing their vectors scaled by stem weight: bass
1.0, guitar 1.0, keys 0.8, other 0.6. Vocals (0.5) are analyzed only when
every other stem yields no peaks. The offset is the angle of the sum
(−50..+50 cents).

The confidence is how well 5-second blocks agree, not how tightly the
peaks cluster. In real recordings the individual peaks scatter widely
(vibrato, bends, bleed between stems, inharmonic partials), so the
resultant length of all peaks sits around 0.2 to 0.4 even when every
block lands within a few cents of the same offset. Each block with at
least 20 peaks and a tenth of the heaviest block's weight adds a unit
vector at its own mean offset, weighted by its peak weight; the confidence
is the resultant length of those. Unpitched audio gives block means at
random. Fewer than four usable blocks (a clip under 20 seconds) gives 0.

The same sum per 10-second segment gives the drift curve. Segments with
fewer than 40 peaks, less than a quarter of the heaviest segment's weight
(fades, breaks), or more than 12 cents from the song offset (outliers) are
left out. Segment offsets are unwrapped around the song offset so the
curve stays continuous near ±50. The song drifts when the 5-segment moving
median of the segment offsets spreads by more than 8 cents; a narrower
median lets the normal section-to-section swing of a few cents count as
drift. The reported offset is still the whole-song mean.

`TuningEstimate::is_reliable` is false when the confidence is below 0.7 or
fewer than 200 peaks were used; the app shows "can't measure" then. On
real songs the confidence is typically 0.95 or more, and white noise
scores about 0.4 to 0.5. The estimator returns `None` only when there were
no peaks at all.

## Invariants and gotchas

- `VERSION` is stored with each result and older results are recomputed.
  Bump it with any change that can move the numbers (window, thresholds,
  weights, filters).
- Offsets are circular: −50 and +50 cents are the same tuning. Average
  them with `Circular`, never arithmetically.
- The peak interpolation exponent is tied to the Hann window. A different
  window needs a different exponent (or the bias check in the spectrum
  tests fails).
- Drums must not be passed: their peaks are not pitched and only blur
  the estimate.
- The UI repeats `MIN_CONFIDENCE` and the drift smoothing width; keep them
  in step.

## Testing

```sh
cargo test -p shredlab-tuning
cargo test -p shredlab-tuning --release -- --nocapture five_minute
cargo run --release -p shredlab-tuning --example measure_library -- <library root>
```

The `measure_library` example runs the estimate on the stems of every song
in a ShredLab library (the `library` folder in the app's data folder) and
prints the offset, confidence, drift and segment offsets per song. Use it
to check a change on real recordings before bumping `VERSION`. With
`SHIFT_CENTS=-40,-25,10` set, it also resamples every song's stems by
those amounts and prints the error of the measured change for each.

The tests synthesize their audio (plucked notes with several partials and
mild inharmonicity, chords, a weak-fundamental guitar voice) and need no
fixtures. They cover offsets from −45 to +45 cents and near the wrap
(within 1 cent), a linear drift, a held low bass note, noise, silence,
bass only, the vocals fallback, a 48 kHz input, and that noise alone and
clips shorter than 20 seconds are not reported as measured. The five-minute test
prints the runtime; its bound is loose so it doesn't depend on the
machine.

## Common changes

- Tuning a threshold or weight: change the constant in `src/song.rs` or
  `src/spectrum.rs`, run the tests, and bump `VERSION`.
- Adding a stem kind: add it to `StemKind` with a weight and lowest
  frequency; decide whether it is a primary stem or a fallback like vocals
  in `estimate_song_tuning`.
