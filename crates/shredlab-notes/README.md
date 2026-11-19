# shredlab-notes

Note detection: the notes one pitched stem plays, one at a time, with how
each note was reached (picked, hammered or pulled, slid into), bends, a
vibrato flag, and the stretches where the stem plays chords. Mono `f32`
samples in, a `NoteAnalysis` (sorted, non-overlapping `Note`s, chordal
stretches and, from the model, the other notes it heard) out.

Two detectors share the result types. The bass, which plays one note at a
time, always uses a pitch tracker (probabilistic YIN). The other stems use
Spotify's Basic Pitch model when the caller passes it, and the pitch
tracker otherwise: the model hears several notes at once, and the crate
picks the lead line from them, so a solo over rhythm guitar in the same
stem keeps its notes where the tracker would mark it all chordal.

## Boundaries

Owns `detect_notes` (the pitch tracker), `detect_notes_with_model` (Basic
Pitch for every stem but the bass), the input and result types
(`NoteInput`, `StemKind`, `Detail`, `Fast`, `Attack`, `Bend`, `Note`, `Heard`,
`NoteAnalysis`, `WindowOutput`), the model's window and frame constants (`basic_pitch`),
`Excerpt` (detecting one stretch of a stem, such as a loop, on its own)
and the algorithm `VERSION`. It
does not name notes (sharps or flats, octaves), transpose them to the
player's tuning, choose neck positions, quantize rhythm, or store results;
the UI and `shredlab-library` do that. It does not decode, downmix or mask
stems, track beats or measure the tuning offset: the caller passes mono
samples, the beat times and the offset. It does not run the network: the
caller passes a closure that runs one window through the model
(`shredlab_separation::NoteModel::run` in the app), so the crate stays free
of ONNX Runtime, like `shredlab-beats`. It takes no paths, opens no audio
device and starts no threads (the caller may run stems in parallel).

Dependencies: realfft (the fast-bend check's spectra), `shredlab-tuning`,
whose `spectrum` module (the Hann analyzer with interpolated peaks) the
tracker's chordal check uses,
and `shredlab-beats`, whose resampler brings audio to the model's
22,050 Hz. Results are plain structs without serde. The `notes_library`
example also uses `ort` (same pin as `shredlab-separation`),
`shredlab-dsp`, `shredlab-library` and `serde_json` as dev-dependencies.

## Layout

- `src/lib.rs`: public types, `VERSION`, the stem ranges, the
  thresholds the note settings set (`Detail::tuning`), `detect_notes`, which runs the tracker's stages below, and
  `detect_notes_with_model`.
- `src/basic_pitch.rs`: the model side: resampling, windows and stitching,
  upstream's note events, the contour helpers and the frame levels.
- `src/lead.rs`: the lead line, bends, slides and chordal stretches from
  the model's note events, with tests on synthetic activations.
- `src/quick_bend.rs`: the fast-bend check on the stem's audio at a lead
  note's start.
- `src/excerpt.rs`: `Excerpt`: the padded stretch to detect, its beats,
  and the result moved back to song time and clipped to the stretch.
- `src/signal.rs`: the analysis signal (low-passed, decimated, high-passed).
- `src/pitch.rs`: probabilistic YIN candidates per frame and the Viterbi
  pitch track.
- `src/onset.rs`: attack scores and onset picking.
- `src/segment.rs`: segments, held pitches, steps, slides, bends, vibrato,
  naming and confidence; removing notes inside chordal stretches.
- `src/poly.rs`: chordal stretches.
- `src/tests.rs`: tests on synthesized stems, and the model path with a
  fake model.
- `examples/notes_library.rs`: runs the detector over a ShredLab library.

## How it works: the pitch tracker

The bass always, and every stem when no model is passed.

1. Signal. The stem is low-passed and decimated to at least 11 kHz (bass)
   or 16 kHz (other stems), then high-passed at 70 % of the lowest note
   searched (at most 40 Hz). Ranges: bass 29 to 400 Hz (a 5-string's low B
   is 31 Hz), guitar 72 to 2100 Hz (bends past the top of the neck reach
   A6 and beyond; the model path keeps guitar notes over 1400 Hz only in a
   high run, see below), vocals 80 to 1100 Hz, keys and other 29 to
   2000 Hz.
2. Pitch candidates (pYIN). One frame every 10 ms. The YIN difference
   function over a window of at least 25 ms and 1.2 of the longest period
   gives the cumulative mean normalized difference (CMNDF). Each of its
   dips is a candidate; over 100 thresholds weighted by a Beta(2, 8)
   distribution (mean 0.2), each threshold goes to the first dip under it,
   and the mass no dip takes goes 1 % to the lowest dip. The candidates'
   summed mass is the voicing probability. The period is interpolated on
   the raw difference, the dip's value on the CMNDF. Octave guard (all
   stems but vocals): when a dip near twice a candidate's period is at
   most half its value and 0.03 lower, and the candidate's own value is
   at least 0.1, 70 % of its mass moves there (a weak fundamental under a
   strong 2nd harmonic). Frames more than 40 dB under the stem's reference
   level (its 99th percentile frame level) are unvoiced.
3. Pitch track. Viterbi through 20-cent bins over the stem's range plus
   one unvoiced state. Emissions are the candidates' masses split between
   the two nearest bins; the unvoiced state scores half the unvoiced
   probability. Moving costs 0.1 nats per bin, at most 4 (any jump), and
   switching voicing 4. A decoded bin takes the exact pitch of the
   strongest candidate within 1.5 bins.
4. Onsets. Per frame, the level rise of the whole stem (3 dB starts to
   count, 9 dB counts fully) and of its band above 1.5 kHz (6 to 15 dB:
   pick and key noise), each over spans long enough to hold a period of
   the lowest note. Onsets are local maxima (±30 ms) scoring at least
   0.35, or 0.25 within 35 ms of a sixteenth of the beat grid.
5. Segments. Voiced stretches, bridging up to 3 unvoiced frames without an
   onset, are cut at onsets. An onset up to 60 ms before a stretch marks
   its start.
6. Held pitches. Inside a segment the pitch is smoothed by a 70 ms median
   and labeled with hysteresis: a label moves to a new semitone only when
   the pitch is more than 0.7 semitones from the current one and within
   0.45 of the new one for three frames, so vibrato keeps its note. A
   label's run, without the frames at either end that are still moving, is
   a held pitch when it lasts three quarters of the minimum note length
   (30 to 80 ms) and drifts less than half a semitone.
7. Moves between held pitches. The move's length is the time its middle
   half takes (between 25 % and 75 % of the way), doubled, so a slow drift
   before a quick step does not count.
   - Step: the middle half takes at most two frames, or four when the
     voicing drops under 0.5 on the way (the window holds two pitches; a
     glide stays periodic). A new note, `Legato` without an attack.
   - Attack: an onset score of at least 0.12 around the move makes the
     next note `Picked` instead (legato is reported only when clear).
   - Bend (guitar, bass and other stems): a glide up from the note's held
     pitch of 0.5 to 3.25 semitones, no faster than 30 semitones per
     second, without pauses on two or more semitone steps on the way, and
     wider than 2.6 semitones only when slower than 20 per second. The
     note keeps its pitch and gets a `Bend` (start, peak, measured size).
     A later return within half a semitone of the held pitch is the
     release; a higher target extends the bend.
   - Bend and release without a held peak: an excursion of 0.5 to 3.25
     semitones above the held pitch between two of its held stretches,
     rising over at least four frames.
   - Slide: any other glide of at least 1.5 semitones. The next note
     starts where the glide arrives, `Slide`.
   - Anything else (a smaller glide, or any glide on vocals and keys
     under 1.5 semitones) is a `Legato` step.
   A segment without a held pitch is one note when the middle half of its
   pitches spans at most half a semitone (confidence × 0.6), else nothing.
8. Notes. Notes shorter than the minimum length (a 32nd at the median beat
   interval, 30 to 150 ms; 40 ms without beats) join the previous note of
   their segment; a short first note joins the next. Segments shorter than
   the minimum give no note. A note's pitch is the median of its held
   frames after removing the song's offset: `midi` is the nearest
   semitone, `cents` the rest. Confidence is the mean voicing probability
   times a steadiness factor (from the median deviation of the held
   pitch) times a level factor (from the note's peak level under the
   stem's reference).
9. Vibrato. On each held stretch of at least 250 ms (and a bend's held
   peak), the pitch minus a 150 ms moving average crosses zero at least
   four times, with half-periods of 55 to 140 ms (about 3.6 to 9 Hz) that
   vary by at most 40 %, and swings at least 25 cents peak to peak.
10. Chordal stretches. Every 20 ms, peaks of a Hann spectrum (170 ms
    window for stems that reach below 60 Hz, 85 ms otherwise) within
    24 dB of the frame's strongest that also appear in the previous frame
    (within 40 cents or half a bin) are checked against the harmonics
    (within 40 cents, up to the 12th) of the tracked pitch, refined from
    the peaks near its harmonics; with no trusted pitch, the lowest peak.
    A real peak an octave under the pitch takes over (the tracker took the
    2nd harmonic). For vocals, a pitch without a peak of its own gives way
    to the lowest peak: a voice has its fundamental, so a missing one is
    two voices a fifth or an octave apart. A frame is chordal when the
    unexplained peaks hold a quarter of the power with one at −15 dB, or
    one reaches −11 dB on its own (an open E chord is mostly harmonics of
    its root, but its other fundamentals are loud). Vocals and other
    stems, which carry a lead over backing parts and bleed, need a third
    of the power with −10 dB, or −4 dB alone. A stretch is where at least
    60 % of the non-quiet frames in a 300 ms window are chordal, at least
    400 ms long, with gaps up to 250 ms joined. Notes at least half inside
    a stretch are dropped; the rest are trimmed to it.

A four-minute guitar stem takes about 0.3 s in a release build; library
stems take 0.2 to 1 s each on an idle machine, keys and other stems
(the widest range) the longest.

## How it works: Basic Pitch

Guitar, vocals, keys and other stems when the caller passes the model
(`detect_notes_with_model`; the bass goes to the tracker and the closure is
never called).

`NoteInput::detail` holds two independent settings for this path; the
numbers below are the defaults'. `fast` (Fast runs) sets the onset
threshold and the shortest event: Fewer keeps only clearly attacked notes
(onsets of 0.5, events from 70 ms), More splits quick runs into separate
notes (onsets of 0.35) but also chops held notes into repeats. `quiet`
(Quiet notes: Include) lets softer notes into the lead line: lead
candidates from a strength of 0.25 (high runs 0.2). On real solos the two
added different notes and little noise came with the lower strength; a
strength under 0.25 added nothing, a frame threshold under 0.3 merged
notes and turned lines into chordal stretches, and shorter events or
onsets under 0.35 mostly split held notes, so those stay. The tracker
ignores both.

1. Model. The stem is resampled to 22,050 Hz and cut into windows of
   43,844 samples (2 s) after 3,840 samples of zeros, overlapping by 30
   frames of 256 samples, the last one zero-padded, as upstream
   `inference.py`. The closure returns each window's `note` and `onset`
   activations (88 semitone bins from A0) and `contour` (264 bins, a third
   of a semitone each), 172 frames each (11.6 ms). 15 frames are trimmed
   from both ends of every window and the rest concatenated; each frame's
   time comes from its window's real position. The contour bin of a note
   bin's center is the middle of its three (A0 is contour bin 1).
2. Note events, as upstream `output_to_notes_polyphonic`: onsets (the
   model's, or a rise of the note activation scaled to them) of at least
   0.4 (upstream 0.5; lower splits fast and legato runs) start notes that
   last while the activation stays at 0.3 or more, allowing 11 frames
   under it; then the remaining activation is tracked from its maxima both
   ways. Events shorter than 50 ms are dropped (fast runs keep their
   notes). An
   event's strength is its mean activation, its onset the model's onset
   activation at its start, and its pitch the median contour peak near its
   bin over its first half.
3. Gate. Events outside the stem's range (step 1 of the tracker) and
   events whose loudest frame (1,024 samples) is more than 40 dB under the
   stem's 99th percentile level, or under −60 dBFS, are dropped: the model
   transcribes faint bleed as readily as the stem's own part.
4. Bends (guitar, bass and other stems). The contour is followed from a
   note's pitch, frame by frame, to the nearest peak within two contour
   bins. A later event that starts within the note or 4 frames after it,
   0.5 to 3.5 semitones higher, with an onset under 0.8 (the model often
   hears one where a bend arrives), that the followed contour reaches no
   faster than 30 semitones per second, is the note's bend: the two merge,
   with the bend from the frame the pitch leaves the note to the frame it
   is 80 % of the way, its size the held pitch after that. A later event
   back at the note's pitch that the contour glides down to is the
   release. A note whose own contour rises 0.5 to 3.5 semitones for 4
   frames has a bend inside it.
5. Lead line. Events starting within 50 ms of each other start together
   (three or more are a strum); only the highest is a candidate, unless the
   higher one has less than 0.6 of its strength (an overtone). Candidates
   need a strength of 0.3. In time order, each candidate takes over from
   the sounding lead note, except:
   - The register is the median of the last six lead notes, forgotten after
     4 s without one. A candidate more than 7 semitones under it or 19 over
     it joins only when it is clearly the lead: strong (0.6) and at least
     250 ms long, at least 450 ms long, or the only candidate within
     0.5 s, or part of a run (three or more single notes, each within
     150 ms of the last one's end and 5 semitones of its pitch); never when
     it tops a strum. This keeps low strummed chord tones between phrases
     out and lets real jumps and licks on the low strings in. A run outside
     the register does not move it, so the line can return afterward.
   - A high run is the same from C6 up with looser limits: three or more
     notes of strength 0.24 or more, each within 500 ms of the last one's
     end and 7 semitones of its pitch. The model hears a solo bent far
     above a riff as short, weak notes; these join the lead line like a
     run. For guitar, notes over 1400 Hz that are not in a high run are
     dropped before notes starting together are compared, so an overtone
     up there can't push the real lead note under it out.
   - A strum's top joins only within 3 semitones under the register, and
     never cuts a sounding lead note above it.
   Each lead note ends where the next starts; notes under 30 ms are
   dropped.
6. Attacks. A lead note with an onset of 0.5 or more, or more than 60 ms
   after the previous one, is `Picked`; otherwise it is a `Slide` when the
   contour glides from the previous note's last pitch (its bend's, if
   still bent) at least a semitone into it, else `Legato`. `cents` is the
   pitch's distance from `midi` after the offset, `confidence` the
   strength. Vibrato is not detected on this path (always false).
7. Fast bends (guitar, bass and other stems). The model misses a bend
   that reaches its target within a few tens of milliseconds of the pick:
   its contour stays flat under the target and it reports the target as a
   plain note, often starting where the pitch arrives. So each lead note
   from E4 up without a bend, not reached by a slide, is checked on the
   stem's own audio from 150 ms before its start to 150 ms after (or its
   end): a Hann spectrum of about 46 ms (2,048 samples at 44.1 or 48 kHz)
   every 5 ms, and in each the strongest peak from 3 semitones under the
   note to half a semitone over it, when within 20 dB of the frame's
   strongest bin (60 Hz to 5 kHz). It is a fast bend when:
   - a lower pitch 0.75 to 2.6 semitones under the note is that peak for
     3 to 30 hops in a row, steady within 0.35 semitones, and new: not
     that pitch in the hop before, and at least 6 dB louder at its loudest
     than at its quietest in the 15 hops before;
   - within 12 hops after it, without dipping under it, the note takes
     over (within 0.3 semitones), no earlier than 80 ms before the model's
     start, and holds for 4 hops at least as strong as the lower pitch
     was;
   - the lower pitch fades by 10 dB within 3 to 10 hops after that (the
     string that sounded it now sounds the note; a note ringing on another
     string does not fade);
   - and the pitch moves: a hop on the way is between the two, or the
     lower pitch lasts at most 12 hops and sits at least 20 cents off a
     semitone after the offset (the string was already rising; a fretted
     note sits on one).
   Not when the lower pitch belongs to another note: an event at it
   sounding since 50 ms before it, or a previous lead note ending there
   when this one is not picked. The note becomes the semitone at or under
   the lower pitch (allowing 20 cents flat): a string only bends up, and
   the window catches the start of the rise, so the lower pitch reads
   sharp. Its bend goes from where the pitch leaves the lower pitch to
   where it arrives, its size from that semitone to the pitch reached
   (`cents` keeps the model note's deviation). The note starts with the
   lower pitch when that is more than a frame earlier, but not before the
   previous lead note's end; the previous note may give up its end to it,
   keeping 100 ms and its own bend's peak. The check computes about 60
   spectra per lead note.
8. Chordal stretches. A frame is chordal when at least 60 % of the
   sounding frames in a 300 ms window around it have three or more events
   sounding; runs at least 400 ms long, with gaps up to 250 ms joined, are
   chordal stretches when lead notes that are not a strum's top cover less
   than a quarter of them. So a lead over strumming keeps its notes (its
   notes start on their own), while pure strumming, whose only lead
   candidates are strum tops, shows as chordal. Lead notes at least half
   inside a stretch are dropped, the rest trimmed to it.
9. Other notes heard (`others`). Every event left after the gates and
   bend merging (step 4) that is not a lead note, at its full length:
   overtones, quiet notes, chord tones, a rhythm part, and lead notes
   dropped as too short or inside a chordal stretch. Of an event that is a
   lead note, the parts outside the lead note of at least 50 ms: a note
   ringing on after the next lead note starts, or the part a chordal
   stretch trimmed. Named like lead notes (`midi` after the offset), with
   the event's strength as `confidence`. A guitar stem of four to five
   minutes has 1,000 to 2,500.

The model and decoding take about 1.3 to 1.6 s for a four- to
five-minute stem on a desktop CPU with four threads, nearly all of it in
the model; the fast-bend check adds under 0.1 s.

## How it works: one stretch (a loop)

`Excerpt` lets the caller detect one stretch of a stem with other settings
than the rest. The caller cuts the stem's samples to `Excerpt::samples`,
passes `Excerpt::beats` (the beats inside, in the excerpt's time), runs
either detector on that as if it were the whole stem, and
`Excerpt::finish` moves the result back to song time and keeps the notes,
chordal stretches and other notes heard that overlap the stretch, cut at
its edges (a bend stays when it starts inside the part kept; a rise cut
short ends at the cut). The detected audio reaches at least `PAD_S` (1 s)
past each side of the stretch, clamped to the song, so the lead line has a
register, a note ringing into the stretch is heard from its attack, and
the fast-bend check can look back. Before the stretch it reaches further,
back to the start of the whole stem's model window at or before that
point (windows start every 36,164 samples, about 1.64 s at the model's
rate): the model then sees the same windows as in a song-wide run, and the
lead line inside the stretch matches the song-wide one with the same
settings. With exactly 1 s of context the activations shifted with the
window positions and borderline notes went the other way (on a 23 s solo
at More fast runs with quiet notes, 62 notes against 81 song-wide; aligned,
82, and the same 60 at the defaults). The alignment is exact in samples at
44.1 kHz. The level gate's reference (step 3) is the excerpt's own level.
A 23 s stretch takes about 0.2 s.

## Invariants and gotchas

- `VERSION` is stored with each result and older results are recomputed.
  Bump it with any change that can move notes, attacks, bends, vibrato
  flags or chordal stretches: ranges, rates, windows, costs, thresholds,
  the model file, which stems use it, and changes to the tuning crate's
  `spectrum` module or the beats crate's resampler.
- Notes are sorted by start and never overlap; none lies inside a chordal
  stretch. `others` are sorted by start and may overlap each other, the
  notes and the chordal stretches; none repeats a lead note's span, and
  the pitch tracker leaves them empty. Chordal stretches are sorted and disjoint. The tracker's times
  are multiples of its 10 ms frame, the model's on its frames of about
  11.6 ms (trimmed notes end on a stretch's edge).
- The bass never uses the model: Basic Pitch misses quick repeated low
  notes the tracker finds. `StemKind::uses_model` says which stems do.
- The model's window, output sizes and output order are fixed by
  `nmp.onnx`; a closure returning other sizes is an error. A model error
  is returned as is (the app then falls back to `detect_notes`).
- `midi` is the held (fretted or sung) note, never the bend's target;
  `cents` is within ±50 and relative to the song's offset, so a song 30
  cents flat reads as its intended notes. A guitar tuned down a half step
  still reads a half step down; the UI names shapes.
- `Bend::semitones` is the measured size, not rounded, except for fast
  bends found on the audio (step 7 of the model path), whose start is not
  measured precisely: their size is from the note's semitone to the pitch
  reached. `release_s` is set only when the pitch returns inside the note.
- Beats only set the minimum note length and lower the onset threshold on
  the grid; they never move notes.
- `Legato` and `Slide` notes follow the previous note of the same attack
  segment. When that note was dropped (too short, or inside a chordal
  stretch), the legato or slide note has no visible predecessor.
- Drums must not be passed.

Known weaknesses of the model path. The lead line is a guess at which of
the notes the model hears is the melody: a rhythm part that moves into the
lead's register, or a lead that drops more than 7 semitones under its
register for one or two short notes, is taken for the other. A single-note
rhythm riff counts as a run and joins the line. The first notes of a
section have no register yet, so a strum's top can start the line. Held
notes are sometimes split into repeated notes where the model hears a new
onset (vibrato, pick noise). Bends whose target carries a strong onset, or
whose contour follows a rhythm note at the same pitch, stay two notes.
The fast-bend check is conservative: a bend whose lower pitch sounds for
under 15 ms, or that the model starts more than 80 ms after the pitch
arrives, stays a plain note, and so do fast bends under E4 and bends of a
minor third. Repeated bends where the string is not fully released
between picks read from wherever the pick catches it, so a whole-step
bend can read as a half-step bend from the semitone in between. A short
grace note a half or whole step under the next note, picked between two
semitones, can read as a fast bend.

Known weaknesses of the tracker. The bend rule is a guess between bends and slides: a
fast slide of up to 2.6 semitones whose fret steps the 10 ms frames blur
can read as a bend, and a bend with a pause on a semitone step as a
slide. Pre-bends (picked bent, then released) read as a downward legato
step or slide. Two voices or instruments a fifth or an octave apart can
read as one note an octave under the lower one when the stem is not
vocals; a third or fourth apart they are chordal. Vocals in close
harmony give notes that jump between the voices and large cents.
Separated stems with bleed of another instrument at −10 dB or louder mark
chordal stretches on guitar and keys stems; reverb tails of a previous
note do the same when they stay that loud for 400 ms. A rhythm part
under a lead in one stem is chordal throughout (Refine's stereo mask
separates them before detection). Palm-muted power chords often read as
their root. Notes under 30 ms, grace notes and fast trills are merged.

## Testing

```sh
cargo test -p shredlab-notes
cargo test -p shredlab-notes --release -- --nocapture four_minute
cargo run --release -p shredlab-notes --example notes_library -- <library root> [title filter] [from-to] [stem] [--model <nmp.onnx>] [--pan <center>,<width>] [--fast fewer|normal|more] [--quiet] [--loop]
```

The `notes_library` example runs the detector on every pitched stem of
every song of a ShredLab library (the `library` folder in the app's data
folder; read only) with the beats and tuning offset from `song.json`, and
prints per stem the note count, the chordal share of the song, the bend,
slide, legato and vibrato counts, the mean confidence, the count of other
notes heard and the bytes they add to `song.json`, and the runtime.
With a `from-to` range in seconds it also lists the chordal stretches,
notes (time, name, cents, confidence, attack, bend, vibrato) and other
notes heard in that stretch and sums them up (count, notes from C4 up,
bends, the major key holding most of them, other notes heard); a stem
name limits it to that stem. `--model` runs
Basic Pitch through ONNX Runtime on the CPU for the stems that use it, as
the app does (the repo's `models/basic_pitch/nmp.onnx` after
`tools/setup_models.py`); `--pan` applies Refine's stereo mask (softness
0.2) first, as the app does when the mask is on; `--fast` sets Fast runs
(Normal by default) and `--quiet` includes quiet notes. `--loop` detects
only the `from-to` stretch through `Excerpt`, as the app does for a loop
with its own settings; compare its summary with the same run without it.
Use it before and after a change, and
before bumping `VERSION`.

The tests synthesize plucked notes (harmonics, exponential decay, a pick
noise burst) and sustained tones with pitch contours, and need no
fixtures. They cover a clean melody (names, attacks, start times, cents,
no vibrato or bends) up to E6, the same through a tanh clipper staying
single notes, a bass line down to the low B (31 Hz), a bass line without
fundamentals, octave traps (a weak fundamental under a strong 2nd
harmonic, clean and overdriven), a whole-step bend and release (size and
times), a held half-step bend, a three-semitone slide fret by fret, a
hammer-on and pull-off without attacks, vibrato, a vocal glide (a slide),
a double stop and a strummed open E (chordal, while a single note before
the strum stays a note), silence and hiss, a song 30 cents flat named with
and without its offset, the minimum note length with and without beats,
keys at 48 kHz, and a four-minute stem (runtime printed, a loose bound).
Every test checks that notes are sorted, don't overlap, have cents within
±50 and confidence within 0..1, and that none lies in a chordal stretch.

The model path is tested without the model. `src/lead.rs` builds
activations by hand: single notes (times, cents with and without an
offset, attacks), the range and level gates, pure strumming (chordal, no
notes), a lead over strumming (every lead note, no chordal stretch), low
notes between phrases (skipped when short, kept when long and strong), a
bend and release split by the model into three events (one note with the
bend's size, start and release; separate notes on keys), a quick step (no
bend, legato), a slow glide into a new note (slide), a fast bend in
synthesized audio under a plain model note (renamed to the lower note
with a whole-step bend, not on keys or without the audio) and a steady
note with a loud pick attack (no bend), and the other notes
heard (an arpeggio's ringing notes from where the next starts to their
end, a skipped low note whole, no lead note repeated, named after the
offset), and the note settings (a short note dropped at Fewer fast runs, a
weak note kept only with quiet notes included). `src/tests.rs` also checks
that the default settings keep the standard thresholds. `src/tests.rs`
runs `detect_notes_with_model` with a fake model (window size and count,
errors passed back) and checks that the bass never calls it (and hears no
other notes).
`src/basic_pitch.rs` checks the window count, frame count and times of
the stitching, and that wrong output sizes are an error.
`src/excerpt.rs` checks the padding (aligned to the model's windows,
clamped to the song), the sample range and beats, and the clipping of
notes, bends, chordal stretches and other notes heard; `src/tests.rs`
checks that an excerpt of a synthesized melody gives the whole stem's
notes cut to the stretch.
`src/quick_bend.rs` checks the fast-bend check on synthesized tones: a
glide from C6 to D6 over 40 ms (a bend from C6 with its times), a steady
D6 with a loud pick (no bend), and a C6 still ringing when a D6 is picked
(no bend).

## Common changes

- Tuning a threshold, cost or window: change the constant in its module
  (the model path's event and lead thresholds follow the note settings,
  in `Detail::tuning`), run the tests and `notes_library` on several songs
  (compare the counts and look at a few stretches by ear; with `--model`
  for the model path, and `--fast` and `--quiet` for each setting), and
  bump `VERSION`, also when only a setting other than the defaults
  changes: saved results don't record the thresholds they were found
  with.
- Changing the lead, chordal or other-notes rules of the model path:
  update `src/lead.rs`, step 5, 8 or 9 above and the tests in
  `src/lead.rs`, check a solo over rhythm guitar with and without Refine's
  stereo mask in `notes_library`, and bump `VERSION`.
- Moving a stem between the detectors: `StemKind::uses_model`, this
  README, and bump `VERSION`.
- Adding a stem kind: give it a range in `StemKind::range_hz`, decide
  whether it bends and whether its notes always have a fundamental, its
  analysis rate in `src/signal.rs` and its chordal limits in
  `src/poly.rs`, and add it to the library example.
- Changing the bend and slide rules: update the rule in `src/segment.rs`,
  this README's step 7, the bend, slide and legato tests, and bump
  `VERSION`. On the model path, bends the model hears are in
  `src/lead.rs` (step 4) and fast ones in `src/quick_bend.rs` (step 7);
  check a solo with `notes_library` and compare the bend counts per stem
  before and after.
