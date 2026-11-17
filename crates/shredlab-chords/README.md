# shredlab-chords

Chord detection: a chord per stretch of a song, from its separated stems
and its beat grid, at concert pitch, plus an estimate of the song's key.
Pure DSP: mono `f32` samples in, a `ChordAnalysis` (contiguous
`ChordSegment`s, each with a plain and a detailed name, the passing chords
and figures inside them, and an optional `Key`) out.

## Boundaries

Owns `detect_chords` and the result types (`Quality`, `Chord`,
`ChordSegment`, `PassingChord`, `Key`, `ChordAnalysis`). It does not spell chords
(sharps or flats), transpose them to the player's tuning, or decide how
the UI shows pushes; the UI does that from the pitch classes and the key.
It does not decode or downmix stems, track beats, measure the tuning
offset, run as a job or store the result in `song.json`; those belong to
the app, `shredlab-beats`, `shredlab-tuning` and `shredlab-library`. It
takes no paths and opens no audio device.

Dependencies: realfft and `shredlab-tuning`, whose `spectrum` module (the
Hann analyzer with interpolated peaks and the non-octave harmonic filter)
and `LiveDetector` (monophonic pitch) it builds on. `StemAudio` and
`StemKind` are re-exported from there. Results are plain structs without
serde; callers map them to their own storage types.

## Layout

- `src/lib.rs`: public types, the algorithm `VERSION` and `detect_chords`,
  which runs the stages below.
- `src/grid.rs`: the analysis grid: beats cleaned up, extended and
  gap-filled, each beat split into sixteenths.
- `src/chroma.rs`: per-stem frames (chroma from spectral peaks, levels,
  onsets), the bass notes, and the sums per grid cell.
- `src/decode.rs`: chord templates and biases, the per-cell scores, the
  Viterbi decoding with position-dependent change costs, short-segment
  merging, slash chords and pushes.
- `src/detail.rs`: passing chords and figures inside segments, and the detailed names.
- `src/key.rs`: the key estimate.
- `src/diag.rs`: a cell-by-cell report of what the detector saw, for the
  example; not used by the app.
- `src/tests.rs`: tests on synthesized stems.
- `examples/chords_library.rs`: runs the detector over a ShredLab library.

## How it works

1. Grid. Beats that are not finite, outside the song or closer than
   0.1 s to the previous one are dropped. The median interval extends the
   grid before the first beat and after the last, and splits any interval
   longer than 1.5 medians (a break the beat tracker skipped) into beats of
   about that length. Each beat interval is split into four cells, which
   start on the beat, on a sixteenth or on the off-beat eighth. With fewer
   than two beats a fixed grid of 0.5 s beats from the first beat (or 0)
   is used, every cell costs the same to change on, and no segment is a
   push.
2. Stems. Guitar and keys count fully, other at half weight, bass at 0.3
   (it mostly serves the root and slash chords). Vocals are analyzed only
   when every other stem is silent. Each stem runs on its own thread, and
   the bass detector on one more.
3. Frames. A hop of 1024 samples at 44.1 kHz (23 ms; other rates scale
   it). Each frame takes peaks from two Hann windows centered on it: 8192
   samples (186 ms) below 260 Hz, where notes are a few Hz apart, and 4096
   (93 ms) from 260 Hz to 2 kHz (1 kHz for bass), where timing matters
   more. Frames more than 40 dB under the stem's loudest are skipped.
   Magnitudes are scaled to amplitudes so the two bands compare; peaks
   more than 34 dB under the frame's strongest are dropped.
4. Overtones. The tuning crate's filter drops peaks near a non-octave
   multiple of a stronger lower peak; without it the 5th harmonic reads as
   a major third and power chords turn major. A second pass weakens (×0.5)
   peaks near a non-octave multiple of a lower peak when that peak, its
   2nd or its 4th harmonic is stronger: low guitar strings have weak
   fundamentals, and the 3rd harmonic of a chord's third otherwise reads
   as a seventh.
5. Chroma. Each peak adds `sqrt(amplitude)` to its pitch class, after
   removing the tuning offset, weighted down with its distance from the
   semitone (0 beyond 35 cents, which also drops the 31-cent-flat 7th
   harmonic). Overdrive makes a note's 3rd harmonic (an octave and a
   fifth up) louder than its fundamental and octaves, which the filters
   above leave alone. Guitar and other stems therefore get a leak share:
   0 up to a median of 24 peaks per frame (after the filters), rising to
   0.3 at 28 peaks, since overdrive adds harmonics and intermodulation
   all over the spectrum. On the library, clean and moderately driven
   guitar stems sit at 15 to 23 and get none; only heavy overdrive (28
   to 30) gets the full share. Each cell keeps, per pitch class,
   that share of the chroma a fifth below it (weighted like the chroma)
   as the leak expected there. Keys, bass and vocals have none.
6. Onsets. Per stem and frame, the larger of a level rise (3 to 9 dB over
   the previous three frames) and chroma flux (the share of the frame's
   chroma that is new compared with two frames earlier). A cell's onset is
   the strongest stem onset from 40 ms before to 60 ms after its start,
   scaled by stem: guitar and keys 1, other 0.7, bass and vocals 0.5.
7. Bass notes. The bass stem goes through a `LiveDetector` with a 45 Hz
   low note (its short window; lower notes may read an octave high, which
   keeps the pitch class). Readings within 35 cents of a semitone count,
   per cell and pitch class.
8. Scores. Each cell scores every chord (12 roots × 9 qualities) by the
   cosine between its chroma and the chord's template (root weight 1.2,
   other tones 1), plus a bias by quality (power, major, minor 0; 7 and
   m7 −0.03; sus −0.04; maj7 −0.05; dim −0.06) so simpler chords win
   close calls, plus 0.06 when the cell's bass note (one pitch class with
   at least 60 % of its readings) is the root. "No chord" scores 0.6, or
   2.0 when the cell's pitched level is under a tenth of the song's
   reference (the 80th percentile of its non-silent cells). A template
   tone other than the root, third and fifth that sits a fifth above
   another tone of the chord (a maj7's seventh over its third, an m7's
   over its minor third, a 9th over the fifth) counts only for what its
   chroma exceeds the cell's leak there; a sus2's second does not get
   this, since an Asus2 discounted that way reads as the Esus4 with the
   same notes. Passing chords score with the same cosine on their
   window's pooled chroma and leak.
9. Decoding. Viterbi over the cells maximizes the summed scores minus a
   cost per change: 0.8 on a beat, 1.3 on the off-beat eighth, 1.8 on a
   sixteenth, each lowered by up to 30 % by an onset. Staying costs
   nothing. A push an eighth early survives when the new chord is clearly
   ahead in its two early cells; a single odd cell does not pay for two
   changes.
10. Segments. Runs shorter than two cells (an eighth) merge into the
    neighbor whose chord scores better on them. A chord segment of at
    least a beat gets a slash bass when one pitch class other than the
    root holds at least 60 % of at least 20 bass readings (about 0.2 s).
    `push` is set on chord segments (not the first) that start off the
    beat.
11. Passing chords. Inside each chord segment, a window of two to four
    cells (an eighth to a beat) that starts on a beat or the off-beat
    eighth with an onset of at least 0.5 is scored on its pooled chroma
    and bass readings, so an arpeggio with one note per sixteenth reads
    as its chord. A chord from the decoded set passes by when, on the
    pooled window, it beats the segment's chord by at least 0.15 and
    scores at least 0.8, it also beats it by 0.03 in the window's first
    and last cells on their own (a single loud cell next to the segment's
    chord does not carry a window), and the window averages at least 0.3
    of the song's reference level. A single cell also counts with an
    onset of 0.8 and a margin of 0.25. The held chord must let go: some
    tone of the segment's chord that the passing chord lacks has at most
    55 % of its share of the chroma in the rest of the segment (outside
    every candidate window and quiet cells), where it held at least 5 %.
    Shares, not levels, so a louder bar does not count. Tones a semitone
    from one of the passing chord's new notes are not judged, since the
    loud neighbor masks them in the analysis windows. Instead of letting
    go, the passing chord may bring at least two new notes (not in the
    segment's chord) at once, its root among them, each holding at least
    11 % of the window's chroma above its expected 3rd-harmonic leak and
    twice its share in the rest of the segment: G over D (G and B) passes
    while the D's A rings on, and a riff brings one new note at a time.
    The root has to be new because a chord on a held chord's note (C# or
    C#7 over A) is more often a riff's power chord. Only chords with a
    third qualify (power, sus and dim readings are mostly riff notes),
    and only chords of their own: another root with a note outside the
    segment's chord and its 9th, 7th and maj7, not containing the whole
    segment chord (Bm or Gmaj7 over D count; Bm7 over D is a D6, C#m7
    over A an Amaj9), or the other triad on the same root (Am over A).
    A chord whose root and fifth are outside the segment chord's triad
    while its third is in it (C over A or A7, F over D) is a power chord
    moving under the held chord, a riff, and does not count. A window at
    the segment's start or end is dropped when it is the neighbor's
    chord (or the neighbor with notes added or left out) arriving early
    or ringing on. Overlapping candidates go to the larger margin times
    length. The winner is then named by the best pooled score among
    itself and the chords with a third made of its notes, with 7, m7 and
    maj7 passing chords 0.05 further behind than in the decoder: Gmaj7
    over D becomes G or Bm unless its fourth note is about as strong as
    the others. Passing chords never change the segments.
12. Detailed names. Each chord segment is scored against refinements of
    its decoded quality on the same root: major against 6 and add9, minor
    against m6, 7 against 9, sus4 against 7sus4, and sus2 against add9
    (whose third is often weak); power, maj7, m7 and dim keep their name.
    Over the segment's cells that are neither quiet nor in a passing
    chord, a refinement wins when its mean cosine minus 0.04 beats the
    decoded chord's and it is ahead in at least half the cells: the added
    note must be at roughly 60 % of the chord tones' strength through the
    segment. The leak is judged over the segment, not per cell: for
    each pitch class, the share of the segment's pooled chroma covered by
    its pooled leak is taken off that class in every cell, so a seventh
    or ninth clearly present across the segment survives cells where it
    is weak. Root and bass stay, so notes that spell two chords (A6 and
    F#m7, D6 and Bm7) are told apart by the decoded root, which the bass
    bonus usually sets.
13. Figures. Inside a chord segment, a stretch where the chord gains a
    note on its own root: major to 6, add9, maj7, 7, 9, sus4 or sus2;
    minor to m6, m7, sus4 or sus2; 7 to 9; sus4 to 7sus4 (power chords
    get none). A sus figure replaces the third, so the third's share of
    the window also has to fall to at most 55 % of its share in the rest
    of the segment (an A with a melody D over a ringing C# is no Asus4). A window of four
    to eight cells (a beat to two) outside passing chords and quiet cells
    that starts on a beat or the off-beat eighth with an onset of at
    least 0.5 and averages at least 0.3 of the reference level counts
    when, on its pooled chroma and leak, the extension's cosine minus
    0.04 beats the plain chord's by 0.02, and each added note holds at
    least 8 % of the window's chroma and at least twice its share in the
    rest of the segment (its cells outside every candidate window). An
    added 7th, 9th or sus2's second counts only above its expected leak,
    and in a window whose leak is over a tenth of its chroma (overdriven
    stems) only 6ths and sus4s count: a 6th or 4th is no harmonic of the
    chord's notes, while a 7th, 9th or 2nd can be, and a b7 over a held
    major chord is often a riff note. The clearest windows win across
    qualities (an Asus2 window also reads as Aadd9, but scores better
    without the third), adjacent windows with the same figure
    join, the segment's detailed name is never a figure, and a quality
    whose figures would cover more than 60 % of the segment gets none
    (that belongs in the detailed name).
14. Key. Krumhansl-Kessler profiles correlated with a pitch-class
    histogram: 60 % from the chords (each tone weighted by duration, the
    root twice) and 40 % from the cell chroma.

A five-minute song with four pitched stems takes about 0.6 s in a release
build; library songs take 0.4 to 1.3 s.

## Invariants and gotchas

- `VERSION` is stored with each result and older results are recomputed.
  Bump it with any change that can move segments or the key (grid,
  windows, weights, biases, penalties, thresholds, and changes to the
  tuning crate's `spectrum` or `LiveDetector`), and with any change to
  the passing-chord, detailed-name or figure rules.
- Segments are decoded from `Quality::ALL` only. The extensions in
  `Quality::EXTENDED` appear only in `detailed`, which has the root and
  bass of `chord` and is `None` exactly when `chord` is. The plain and
  detailed views share the same boundaries.
- Passing chords are in time order, each inside one chord segment, named
  from `Quality::ALL` without a slash bass. They are found before the
  detailed names, whose scores leave their cells out, so a quick Bm in a
  D segment does not turn the D into a D6.
- Figures are in time order, each inside one chord segment with that
  segment's root and an extension of its quality, never overlapping a
  passing chord and never the segment's detailed name. They don't change
  segments, detailed names or passing chords.
- Pitch classes are concert pitch relative to the song's own reference:
  the tuning offset is removed before rounding, so a song 30 cents flat
  reads as its intended notes. A guitar tuned down a half step still reads
  a half step down (an E shape reads Eb); the UI can use the key to
  suggest that.
- Segments are contiguous, in time order, and cover exactly 0 to the
  duration, with no two neighbors equal. A duration that is not positive
  gives no segments; a zero sample rate gives one segment without a chord.
- A bass onset alone never creates a change: onsets only lower the change
  cost, and the chroma decides. Harmony stems' onsets count twice as much
  as the bass's, so when guitar and bass disagree on where a change is,
  the guitar wins.
- Drums must not be passed: their noise flattens the chroma and adds
  onsets everywhere.
- The change costs are in units of per-cell scores. Changing the grid
  resolution, the score scale (template weights, biases) or the hop
  changes how they balance.

Known weaknesses: maj7 and m7 chords whose seventh is mostly the 3rd
harmonic of their third often read as the triad, more so on overdriven
stems, where a real seventh or ninth has to beat the expected leak (an
m7 voiced with a quiet seventh reads as minor). The leak share comes
from the peak density, so a very busy clean part could get some of it,
moderate overdrive gets none, and the 0.3 share is mild: the hardest
overdrive leaks more than it covers, and an occasional fake maj7, m7 or
add9 comes back there. A dominant 7 is not
discounted (its seventh is not a fifth above a chord tone), so riff
notes over a held chord can still read as 7. Without a bass, a sus4 can read
as the sus2 a fourth up (same notes). Riffs of single notes give chords
that follow the riff. The key can be its relative (Gb for Eb minor) or,
over power chords, its parallel.

Passing chords on real songs number from none to a few dozen per song.
Riffs that move against a held chord are still sometimes reported when
the riff's chord shares its root with the held chord (C#7 over Amaj7):
on separated stems the held chord often stops sounding while the riff
plays, so letting go does not tell them apart. A passing chord whose
missing tone keeps ringing is found only when two strong new notes,
its root among them, arrive (a G over a ringing D is; a quick Bm, with
one new note, is not). The let-go test never judges a tone a semitone from the
passing chord's new notes, so Am over A and A over Am are not reported.
A let-ring arpeggio is found as long as the segment chord's missing
tone stops; if that tone rings on too, it is missed. A 7sus4 voicing usually decodes
as the sus2 a tone lower (its fifth is the root's 3rd harmonic and gets
weakened), so it rarely gets its detailed name. A 6 needs its sixth
nearly as strong as the chord tones through the segment; a sixth played
now and then within a riff reads as plain major.

## Testing

```sh
cargo test -p shredlab-chords
cargo test -p shredlab-chords --release -- --nocapture five_minute
cargo run --release -p shredlab-chords --example chords_library -- <library root> [title filter] [segments]
```

The `chords_library` example runs the detector on every song of a
ShredLab library (the `library` folder in the app's data folder) with the
beats and tuning offset from `song.json` (measuring the offset when it is
missing), and prints per song the key, segment, chord and push counts,
the runtime, the share of chord time per quality, where the pushes sit,
and the first segments, with detailed names in parentheses when they
differ, passing chords in brackets and figures in braces under their
segment; the header counts all three. Use it to check a change on real recordings before bumping
`VERSION`. With a `from-to` range in seconds it lists that stretch and
every cell in it: the decoded and final chord, the detailed name, the
segment chord's score, the passing chord or figure the cell lies in, the
best-scoring chords and extensions, and the chroma.

The tests synthesize strummed guitar chords (weak fundamental, mild
inharmonicity), plucked bass lines and keys on known beat grids, and need
no fixtures. They cover a progression on the beat (major, minor, power,
7, m7, sus4), power chords staying power chords, eighth pushes found at
the right time and marked, a loud odd sixteenth not changing the chord, a
sparse syncopated bass not moving the changes, a bass pedal giving slash
chords (D/E, A/E) while a root bass gives none, songs 30 cents flat and 40
cents sharp, silence and noise bursts giving no chord, the key of a major
and a minor progression, beats slowing down with a push, 48 kHz input, no
beat grid, degenerate inputs, keys alone and the vocals fallback. For
detailed names: A, D and E stay plain while A6, D6 (over a D bass, not
Bm7), E9 and an add9 (decoded as sus2) get their names; on clean
guitar an Amaj7 (open shape), Am7, A7 and Aadd9 keep their notes, and
on guitar with strong 3rd harmonics through a tanh clipper, A, Am, E5, E,
D, Dm and Em stay plain chords with plain detailed names. For passing
chords: a Bm for an eighth and for a beat over a D pedal is reported
without splitting the D, and so is a Bm arpeggiated over a beat, one
note per sixteenth; clean progressions, a loud odd sixteenth, a steady
strummed chord with drum bleed and hiss, chords arriving a sixteenth
early, and single riff notes (C, C#) over a ringing A give none; a G
strummed over a ringing D (its A still sounding) is one. For figures: an A riff alternating A and A6 every two beats stays one A
segment with A6 figures on the A6 beats, a beat of a clean open Amaj7
inside an A gives one Amaj7 figure, the steady strummed chord gives
none, the overdriven chords give only 6th figures, a beat of Asus4 or
Asus2 inside an A gives that figure without splitting the A, and a
melody D over a ringing A gives no Asus4. Every
test checks that the segments are contiguous and cover the song, that
detailed names match their chords, that passing chords are sorted and
lie inside chord segments, and that figures are sorted, lie inside a
segment with its root, are not its detailed name and don't overlap a
passing chord. The five-minute test prints the runtime and the passing-chord count; its
bound is loose so it doesn't depend on the machine.

## Common changes

- Tuning a weight, bias, penalty or threshold: change the constant in
  `src/chroma.rs` or `src/decode.rs`, run the tests and `chords_library`
  on real songs, and bump `VERSION`.
- Adding a decoded chord quality: add it to `Quality` (with `ALL`,
  `suffix` and `intervals`), give it a bias in `src/decode.rs`, decide its
  third and refinements in `src/detail.rs`, add it to a test progression,
  and bump `VERSION`.
- Adding an extension: add it to `Quality` (with `EXTENDED`, `suffix`,
  `intervals` and an arm in the bias function in `src/decode.rs`, which
  never decodes it), add it to a decoded quality's refinements in
  `src/detail.rs`, cover it in the detailed-names test, and bump
  `VERSION`.
- Tuning passing chords, detailed names or figures: change the
  constants in `src/detail.rs`, run the tests and `chords_library` on
  several songs, check that the passing and figure counts stay low, and
  bump `VERSION`.
- Adding a stem kind: give it chroma and onset weights in `src/chroma.rs`
  and decide whether it is a primary stem or a fallback like vocals.
