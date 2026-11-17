// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Tuning analysis for ShredLab.
//!
//! Measures how far a song is from A440 (within ±50 cents) from its
//! separated stems: [`estimate_song_tuning`]. Pure DSP: mono `f32` samples
//! in, a [`TuningEstimate`] out; no paths, no devices.
//!
//! Guesses how the song is tuned (Eb standard, drop D…) from the lowest
//! notes of its bass: [`guess_song_tuning`].
//!
//! Also holds the live tuner's pitch detector, [`LiveDetector`]: samples
//! from an input stream in, a [`LiveReading`] per hop out.

mod cents;
mod guess;
mod live;
mod song;
mod spectrum;

pub use guess::{guess_song_tuning, TuningGuess, BASS_STANDARD_LOW, GUITAR_STANDARD_LOW};
pub use live::{LiveDetector, LiveReading, LEVEL_FLOOR_DB, MIN_CLARITY, QUIET_DB};
pub use song::estimate_song_tuning;

/// Algorithm version of [`estimate_song_tuning`] and [`guess_song_tuning`].
/// Stored results with an older version should be recomputed. Bump it
/// whenever a change can move the measured numbers or the guess.
pub const VERSION: u32 = 3;

/// Below this confidence (agreement between blocks) the estimate is not
/// usable.
pub const MIN_CONFIDENCE: f32 = 0.7;

/// Below this many spectral peaks the estimate is not usable.
pub const MIN_PEAKS: u32 = 200;

/// Length of a drift segment in seconds.
pub const SEGMENT_SECONDS: f32 = 10.0;

/// Length of a confidence block in seconds. Songs need at least four
/// blocks with pitched content to be measured.
pub const BLOCK_SECONDS: f32 = 5.0;

/// Separated stems the estimator understands. Drums are never used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StemKind {
    Bass,
    Guitar,
    /// Piano or keys.
    Keys,
    /// Everything the model didn't assign elsewhere ("misc").
    Other,
    /// Only used when every other stem is silent, at half weight.
    Vocals,
}

impl StemKind {
    /// Weight of this stem's peaks when stems are combined.
    pub fn weight(self) -> f64 {
        match self {
            StemKind::Bass | StemKind::Guitar => 1.0,
            StemKind::Keys => 0.8,
            StemKind::Other => 0.6,
            StemKind::Vocals => 0.5,
        }
    }

    /// Lowest frequency searched for peaks, in Hz.
    pub fn min_hz(self) -> f32 {
        match self {
            StemKind::Bass => 30.0,
            _ => 70.0,
        }
    }
}

/// One stem: mono samples at the song's sample rate.
#[derive(Debug, Clone, Copy)]
pub struct StemAudio<'a> {
    pub kind: StemKind,
    pub samples: &'a [f32],
}

/// The song's tuning offset over one [`SEGMENT_SECONDS`] stretch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    /// Start of the segment in seconds.
    pub start_s: f32,
    /// Offset in cents. Unwrapped around the song's offset, so it lies
    /// within 50 cents of [`TuningEstimate::offset_cents`] and can go
    /// slightly past ±50 when the song sits near the wrap.
    pub offset_cents: f32,
    /// The segment's share of the song's peak weight relative to the
    /// heaviest segment, 0..1.
    pub weight: f32,
}

/// How far a song is from A440.
#[derive(Debug, Clone, PartialEq)]
pub struct TuningEstimate {
    /// Cents from the nearest A440 semitone, −50..+50.
    pub offset_cents: f32,
    /// The A4 reference the offset implies, `440·2^(offset/1200)`.
    pub reference_hz: f32,
    /// How well the song's [`BLOCK_SECONDS`] blocks agree on the offset,
    /// 0..1: the weighted resultant length of the block means. 0 when
    /// fewer than four blocks have pitched content.
    pub confidence: f32,
    /// True when the segment offsets spread by more than 8 cents after a
    /// 5-segment moving median.
    pub drifts: bool,
    /// Usable segments in time order. Segments with too few peaks or little
    /// pitched content (fades, breaks) are left out, so there can be gaps.
    pub segments: Vec<Segment>,
    /// Spectral peaks that went into the estimate.
    pub peak_count: u32,
}

impl TuningEstimate {
    /// False when the song's tuning couldn't be measured reliably
    /// (confidence below [`MIN_CONFIDENCE`] or fewer than [`MIN_PEAKS`]
    /// peaks). The numbers are still filled in, for diagnostics.
    pub fn is_reliable(&self) -> bool {
        self.confidence >= MIN_CONFIDENCE && self.peak_count >= MIN_PEAKS
    }
}

#[cfg(test)]
mod guess_tests;
#[cfg(test)]
mod live_tests;
#[cfg(test)]
mod tests;
