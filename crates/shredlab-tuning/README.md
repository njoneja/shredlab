# shredlab-tuning

Tuning analysis: how far a song is from A440, measured from its separated
stems; a guess at how the song is tuned (Eb standard, drop D) from its
lowest bass notes; and the live tuner's pitch detector. Pure DSP: mono
`f32` samples in, a `TuningEstimate`, a `TuningGuess` or a stream of
`LiveReading`s out.

## Boundaries

Owns the offline song estimator (`estimate_song_tuning`), the tuning
guess (`guess_song_tuning`) and the live pitch detector (`LiveDetector`).
It does not turn a guess into candidate tunings or words; the UI does. It does not decode or downmix stems, pick
which stems exist, run as a job, store the result in `song.json`, or turn
the offset into a Pitch setting; those belong to the app and
`shredlab-library`. For the live tuner it does not open the input device,
pick a channel, run a thread, map a frequency to a note or string, or
smooth the needle; the audio engine feeds it samples and the UI does the
rest. It takes no paths and opens no audio device. The `spectrum` module
(the Hann analyzer with interpolated peaks and the non-octave harmonic
filter) is public for `shredlab-chords`, which builds its chroma on it.

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
- `src/guess.rs`: the tuning guess: bass note shares and the rules.
- `src/live.rs`: the live detector: high-pass, McLeod Pitch Method,
  octave guard, the reading median and the attack hold.
- `src/tests.rs`: estimator tests on synthesized stems.
- `src/live_tests.rs`: live detector tests on synthesized plucks.
- `src/guess_tests.rs`: tuning guess tests on synthesized bass lines.

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

## Tuning guess

The offset only gives cents within ±50: a song in Eb standard at A440 and
a song in standard 100 cents flat are the same audio. `guess_song_tuning`
suggests the semitones from the lowest notes the bass plays, given the
measured offset. It is a suggestion; the user confirms it.

1. The bass stem goes through a `LiveDetector` (low note 30 Hz). Each
   reported pitch becomes a MIDI note after removing the offset; readings
   more than 35 cents from a semitone (slides, bends) are dropped, and a
   note counts once it holds for three readings in a row. The result is
   each note's share of the steady pitch time. A bass with less than 10
   seconds of steady pitch gives no guess.
2. The lowest note from B0 to Eb1 (five semitones under E1) that holds at
   least 5 % of the time, at least half as much as E1 and at least 0.3 of
   its upper octave is the lowest played note. The E1 condition keeps a
   few low passing notes (a five-string, a drop for one riff) from
   outvoting a song that sits on E1; the octave condition drops notes the
   detector read an octave low.
3. Without such a note, a song whose bass plays E1 at least 5 % of the
   time is standard; otherwise there is no guess (the bass never reaches
   its low string, so the audio can't tell).
4. `likely` (strong evidence) only for standard (E1 at least 10 %) and a
   half step down (Eb1 at least 10 % and E1 at most a quarter of it, or
   Eb1 at least 5 % and the guitar stem agreeing: its Eb2 at least 5 %
   and its E2 at most a quarter of that). Lower notes stay weak: D1 can
   be drop D or D standard, B0 a five-string in standard, and the audio
   doesn't tell them apart.

The guitar stem never guesses alone: it often holds bass bleed, which
reads as guitar notes under E2. Synth bass lines that go under E1 give
weak guesses that are wrong; the UI's wording ("Possibly") and the
user's confirmation cover that. On real songs a guess takes 1 to 2 s in a
release build.

## Live detector

`LiveDetector::new(sample_rate, low_hz)` takes the input rate and the
lowest target note. `push` takes any number of mono samples and appends a
`LiveReading { hz, clarity, level_db }` for each hop: the first once a
window is full, then one per hop (about 94 per second). Readings are
plain values; the caller throttles them for the UI.

1. Each sample goes through a 2nd-order Butterworth high-pass at 25 Hz
   (f64 state, kept across pushes) into a ring buffer.
2. The window is 4096 samples at 48 kHz, or 8192 when `low_hz` is below
   45 Hz (the low B of a 5-string bass, 30.87 Hz, needs about five
   periods). The hop is 512. Other rates scale both.
3. `level_db` is the RMS of the high-passed window in dBFS (a full-scale
   sine is −3), floored at −120.
4. The normalized square difference function (NSDF) up to a lag of half
   the window: the autocorrelation comes from the power spectrum of the
   window zero-padded to at least twice its length (one forward and one
   inverse real FFT), and the normalizing energy term is updated per lag.
5. Key maxima: the highest NSDF point between each positive-going zero
   crossing and the next negative-going one, after the first
   negative-going crossing, refined by parabolic interpolation. The chosen
   peak is the first key maximum at or above 0.9 of the highest. Its
   interpolated lag is the period; its value is the clarity.
6. Octave guard: a fundamental about 13 dB or more under a strong 2nd
   harmonic (with weak odd partials) puts a key maximum at half the period
   above the 0.9 threshold. If the key maximum near twice the chosen lag
   is higher and its distance from 1 is at most half that of the chosen
   one, it is taken instead. For a correct reading the longer lag is
   always a little less periodic (decay, inharmonicity), so the guard
   doesn't halve good readings, including with noise about 20 dB under
   the note.
7. A reading is accepted when its clarity is at least 0.85
   (`MIN_CLARITY`) and its level at least −60 dBFS (`QUIET_DB`, about 20
   dB above a quiet interface input with the gain up). Accepted
   frequencies go into a 5-reading median; `hz` is the median once three
   are in, and `None` for rejected readings. Eight rejected readings in a
   row (about 85 ms) clear the median, so a new note doesn't inherit the
   last one.
8. Attack hold: a plucked string starts sharp (often 20 to 40 cents) and
   noisy, and settles within a few tens of milliseconds. Each hop's mean
   square level is kept; when the latest span of hops is at least 4 dB
   louder than the span before it and above the quiet gate, a new attack
   has started. A span covers at least one period of `low_hz` (up to four
   hops), so a low note's waveform doesn't make the level wobble. After an
   attack the median is cleared and no pitch is reported until the window
   no longer holds the attack plus 60 ms (about 150 ms on the short
   window, 240 ms on the long one). 4 dB catches a string re-plucked
   while it still rings at half its starting level; natural decay and
   noise 20 dB under the note don't trigger it.

Notes below `low_hz` may read an octave high: their doubled period can
exceed the searched lag range, so the octave guard can't see it. Upper partials stretched by string stiffness pull the
reading slightly sharp (a fraction of a cent for normal strings), as they
do for the ear.

## Invariants and gotchas

- `VERSION` is stored with each result and older results are recomputed.
  Bump it with any change that can move the numbers or the guess (window,
  thresholds, weights, filters, guess rules, and changes to the live
  detector, which the guess runs on).
- Offsets are circular: −50 and +50 cents are the same tuning. Average
  them with `Circular`, never arithmetically.
- The peak interpolation exponent is tied to the Hann window. A different
  window needs a different exponent (or the bias check in the spectrum
  tests fails).
- Drums must not be passed: their peaks are not pitched and only blur
  the estimate.
- The UI repeats `MIN_CONFIDENCE` and the drift smoothing width; keep them
  in step.
- `LiveDetector::push` must not allocate: all buffers are sized in
  `new`, and readings go into the caller's `Vec`. It runs on the audio
  worker thread.
- The live detector has no version of its own: its readings are not
  stored, but the tuning guess uses it, so a change to it bumps
  `VERSION`.
- The NSDF search stops at half the window, and the window choice assumes
  the doubled period of the lowest target note still fits there (so the
  octave guard can check it). Keep that when changing the window lengths.

## Testing

```sh
cargo test -p shredlab-tuning
cargo test -p shredlab-tuning --release -- --nocapture five_minute
cargo test -p shredlab-tuning --release -- --nocapture live_tests
cargo run --release -p shredlab-tuning --example measure_library -- <library root>
```

The `measure_library` example runs the estimate and the guess on the stems
of every song in a ShredLab library (the `library` folder in the app's
data folder) and prints the offset, confidence, drift, guess and segment
offsets per song. Use it
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

The live detector tests synthesize plucks (decaying partials, mild
inharmonicity, white noise 40 dB under the note) from B0 to E6 at several
cent offsets, at 44.1 and 48 kHz, and require every reading after the
attack to be within 1 cent. They also cover weak and missing
fundamentals under a strong 2nd harmonic (the right octave), much noisier
plucks (never another octave), silence, white noise and a tone under the
quiet gate (no pitch), window sizes per rate, chunking, the median
restarting after a gap, and plucks with a noise burst that start 40 cents
sharp (from silence and re-plucked while ringing): no reading more than 2
cents off, and each note shown within 0.4 s. `per_hop_cost` prints the
time per hop and, in release builds only, fails above a tenth of the
hop's duration.

The guess tests synthesize bass lines (0.8 s plucked notes cycling
through a pattern) and cover Eb standard (likely, with an offset),
standard (likely, also with a few D1s under E1), drop D and drop C#
(possible only), a bass that never reaches its low string, too little
bass, a low note much rarer than its octave (no guess), the guitar
backing a weak Eb guess, and the guitar alone (no guess).

## Common changes

- Tuning a threshold or weight: change the constant in `src/song.rs` or
  `src/spectrum.rs`, run the tests, and bump `VERSION`.
- Changing a guess rule: change the constant in `src/guess.rs`, run the
  guess tests and `measure_library` on real songs, and bump `VERSION`.
- Adding a stem kind: add it to `StemKind` with a weight and lowest
  frequency; decide whether it is a primary stem or a fallback like vocals
  in `estimate_song_tuning`.
