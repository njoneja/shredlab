// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Note detection for ShredLab.
//!
//! [`detect_notes`] turns one pitched stem into note events: start, end,
//! MIDI note and cents at concert pitch relative to the song's tuning
//! offset, how each note was reached (picked, legato, slide), bends and a
//! vibrato flag, plus the stretches where the stem plays several notes at
//! once. Pure DSP: mono `f32` samples in, a [`NoteAnalysis`] out; no paths,
//! no devices, no threads.
//!
//! [`detect_notes_with_model`] does the same with Spotify's Basic Pitch
//! model for every stem but the bass: the caller runs the network through a
//! closure (one window in, a [`WindowOutput`] out), so this crate stays
//! free of ONNX Runtime.
//!
//! [`Excerpt`] runs either on one stretch of a stem (a loop): the caller
//! detects the padded stretch's audio and [`Excerpt::finish`] moves the
//! result back to song time, clipped to the stretch.

pub mod basic_pitch;
mod excerpt;
mod lead;
mod onset;
mod pitch;
mod poly;
mod quick_bend;
mod segment;
mod signal;

/// Algorithm version of [`detect_notes`]. Stored results with an older
/// version are recomputed. Bump it whenever a change can move the result.
pub const VERSION: u32 = 7;

/// Pitched stems the detector understands. Drums are never passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StemKind {
    Bass,
    Guitar,
    Vocals,
    Keys,
    Other,
}

impl StemKind {
    /// Lowest and highest fundamental searched, in Hz.
    pub fn range_hz(self) -> (f64, f64) {
        match self {
            // A 5-string bass's low B is 31 Hz.
            StemKind::Bass => (29.0, 400.0),
            StemKind::Guitar => (72.0, 2100.0),
            // E2 to C#6; a lower reading is more often two voices a
            // fifth apart than a low voice.
            StemKind::Vocals => (80.0, 1100.0),
            StemKind::Keys | StemKind::Other => (29.0, 2000.0),
        }
    }

    /// Above this (Hz) the model path keeps only notes in a high run (a solo
    /// bent past the top of the neck); `range_hz` reaches further for them.
    pub fn usual_top_hz(self) -> Option<f64> {
        match self {
            StemKind::Guitar => Some(1400.0),
            _ => None,
        }
    }

    /// Whether every note has its fundamental. Voices do; a low string or
    /// key, or overdrive, can leave it weak or missing. Where a voice's
    /// fundamental is missing, two voices sound in a harmonic interval.
    pub fn has_fundamental(self) -> bool {
        self == StemKind::Vocals
    }

    /// Whether [`detect_notes_with_model`] uses the model for this stem. The
    /// bass, which plays one note at a time, keeps the pitch tracker.
    pub fn uses_model(self) -> bool {
        self != StemKind::Bass
    }

    /// Whether a glide up from a held note can be a bend. Keys and voices
    /// glide (slides) but don't bend a fretted note.
    pub fn bends(self) -> bool {
        matches!(self, StemKind::Bass | StemKind::Guitar | StemKind::Other)
    }
}

/// How the model path splits quick runs into notes (the Notes panel's Fast
/// runs). More splits fast runs into separate notes but can also chop held
/// notes into repeats; Fewer keeps only clearly attacked notes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fast {
    Fewer,
    #[default]
    Normal,
    More,
}

/// The note detection settings of the model path. The pitch tracker (the
/// bass) ignores them. The default (Normal fast runs, quiet notes skipped)
/// is the detector's standard tuning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Detail {
    /// How quick runs are split into notes.
    pub fast: Fast,
    /// Whether softer notes take part in the lead line (the Notes panel's
    /// Quiet notes: Include).
    pub quiet: bool,
}

/// The thresholds a [`Detail`] sets on the model path.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Tuning {
    /// Shortest note event kept from the model (s); also the shortest part
    /// of a lead note's event outside the note reported as heard.
    pub min_event_s: f64,
    /// An onset peak at least this high starts a note (upstream uses 0.5).
    pub onset_threshold: f32,
    /// A note lasts while its activation stays at least this high.
    pub frame_threshold: f32,
    /// A note takes part in the lead line from this mean activation up.
    pub lead_min_amp: f32,
    /// A note in a high run (a solo far above the part under it) takes
    /// part from this mean activation up.
    pub high_run_min_amp: f32,
}

impl Detail {
    pub(crate) fn tuning(self) -> Tuning {
        // Measured on sparse and dense solos: the two thresholds act
        // independently. A lower onset threshold splits fast runs but also
        // chops held notes into repeats; a lower strength floor lets quiet
        // notes into the lead line with little noise, and going under 0.25
        // adds nothing. A lower frame threshold merges notes and turns lines
        // into chordal stretches, so it stays put.
        let (min_event_s, onset_threshold) = match self.fast {
            Fast::Fewer => (0.07, 0.5),
            Fast::Normal => (0.05, 0.4),
            Fast::More => (0.05, 0.35),
        };
        let (lead_min_amp, high_run_min_amp) = if self.quiet { (0.25, 0.2) } else { (0.3, 0.24) };
        Tuning {
            min_event_s,
            onset_threshold,
            frame_threshold: 0.3,
            lead_min_amp,
            high_run_min_amp,
        }
    }
}

/// One stem to analyze.
#[derive(Debug, Clone, Copy)]
pub struct NoteInput<'a> {
    /// Mono stem samples (the app downmixes, applies separation strength and
    /// Refine's stereo mask before this; not the EQ).
    pub samples: &'a [f32],
    pub sample_rate: u32,
    pub kind: StemKind,
    /// The song's measured offset from A440 in cents (−50..50); notes are
    /// named relative to it.
    pub offset_cents: f64,
    /// Detected beat times in seconds (may be empty). Used for the minimum
    /// note length and onset weighting, never to move notes.
    pub beats: &'a [f64],
    /// Note detection settings (model path only).
    pub detail: Detail,
}

/// How a note was reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attack {
    /// A new pluck, pick, key press or sung onset.
    Picked,
    /// Pitch change without a new attack: hammer-on (up) or pull-off (down).
    Legato,
    /// Reached by a continuous glide from the previous note.
    Slide,
}

/// A bend inside a note.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bend {
    /// When the pitch starts rising (or falling) from the note.
    pub start_s: f64,
    /// When it reaches its target.
    pub peak_s: f64,
    /// When it returns to the note, if it does inside the note.
    pub release_s: Option<f64>,
    /// Size of the bend in semitones, positive up: 1.0 = half step ("½" in
    /// tab), 2.0 = whole step ("full"), 3.0 = "1½". The measured size, not
    /// rounded.
    pub semitones: f32,
}

/// One note event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Note {
    pub start_s: f64,
    pub end_s: f64,
    /// MIDI note number at concert pitch after removing `offset_cents`
    /// (the fretted / sung note, before any bend).
    pub midi: u8,
    /// Median deviation from `midi` in cents after the offset (−50..50).
    pub cents: f32,
    /// 0..1.
    pub confidence: f32,
    pub attack: Attack,
    pub bend: Option<Bend>,
    /// Stored now, shown later.
    pub vibrato: bool,
}

/// A note the model heard outside the lead line ([`NoteAnalysis::others`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Heard {
    pub start_s: f64,
    pub end_s: f64,
    /// MIDI note number at concert pitch after removing `offset_cents`, as
    /// for [`Note::midi`].
    pub midi: u8,
    /// The model's mean activation over the note, 0..1.
    pub confidence: f32,
}

/// The result for one stem.
#[derive(Debug, Clone, Default)]
pub struct NoteAnalysis {
    /// Sorted by start, non-overlapping.
    pub notes: Vec<Note>,
    /// Stretches (start_s, end_s) where the stem plays several notes at once;
    /// no notes are reported inside them.
    pub chordal: Vec<(f64, f64)>,
    /// Model path only: every other note the model heard, at its detected
    /// length, sorted by start; they may overlap each other and the lead
    /// notes. Notes that are not lead notes appear whole; a lead note that
    /// rang on after the next lead note started (or was trimmed by a
    /// chordal stretch) appears as the part outside its lead note. Empty
    /// from [`detect_notes`].
    pub others: Vec<Heard>,
}

/// Seconds between pitch frames.
pub const HOP_S: f64 = 0.01;

/// Detects the notes of one monophonic (or mostly monophonic) stem.
pub fn detect_notes(input: &NoteInput) -> NoteAnalysis {
    if input.sample_rate == 0 || input.samples.is_empty() {
        return NoteAnalysis::default();
    }
    let (min_hz, _) = input.kind.range_hz();
    let sig = signal::prepare(input.samples, f64::from(input.sample_rate), input.kind);
    let n_frames =
        (input.samples.len() as f64 / f64::from(input.sample_rate) / HOP_S).ceil() as usize;
    let track = pitch::track(&sig, n_frames, input.kind);
    if track.reference_db <= pitch::SILENT_DB {
        return NoteAnalysis::default();
    }
    let onsets = onset::strengths(&sig, n_frames, min_hz, track.gate_db);
    let min_len = segment::min_note_len(input.beats);
    let picks = onset::pick(&onsets, input.beats, &track);
    let mut notes = segment::notes(&segment::Context {
        track: &track,
        onset: &onsets,
        picks: &picks,
        kind: input.kind,
        offset_cents: input.offset_cents,
        min_len,
    });
    let chordal = poly::stretches(&sig, &track, input.kind);
    segment::remove_chordal(&mut notes, &chordal, min_len);
    NoteAnalysis {
        notes,
        chordal,
        others: Vec::new(),
    }
}

pub use basic_pitch::WindowOutput;
pub use excerpt::{Excerpt, MIN_LEN_S as EXCERPT_MIN_S, PAD_S as EXCERPT_PAD_S};

/// Detects the notes of one stem with the Basic Pitch model, run through
/// `model`: it gets one window of [`basic_pitch::WINDOW`] samples at
/// [`basic_pitch::SAMPLE_RATE`] (the input `[1, WINDOW, 1]`) and returns
/// that window's activations. The notes are the stem's lead line, one at a
/// time; chordal stretches are where it plays chords without one. Stems
/// that don't use the model ([`StemKind::uses_model`]) go to
/// [`detect_notes`] and never call `model`. Errors are the model's.
pub fn detect_notes_with_model(
    input: &NoteInput,
    model: &mut dyn FnMut(&[f32]) -> Result<WindowOutput, String>,
) -> Result<NoteAnalysis, String> {
    if !input.kind.uses_model() {
        return Ok(detect_notes(input));
    }
    if input.sample_rate == 0 || input.samples.is_empty() {
        return Ok(NoteAnalysis::default());
    }
    let audio = basic_pitch::resample(input.samples, input.sample_rate)?;
    let act = basic_pitch::infer(&audio, model)?;
    Ok(lead::transcribe(
        &act,
        input.kind,
        input.offset_cents,
        input.detail,
        Some((input.samples, input.sample_rate)),
    ))
}

#[cfg(test)]
mod tests;
