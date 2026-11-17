// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Guessing the song's tuning (Eb standard, drop D…) from the lowest notes
//! the bass plays, with the guitar as a second opinion. The measured offset
//! only gives the cents; the semitones come from here, as a suggestion.

use crate::{LiveDetector, StemAudio, StemKind};

/// MIDI note of the lowest open string in standard tuning: E1 on a
/// four-string bass, E2 on guitar.
pub const BASS_STANDARD_LOW: i32 = 28;
pub const GUITAR_STANDARD_LOW: i32 = 40;

/// Seconds of steady pitch a stem needs before its notes count.
const MIN_SECONDS: f64 = 10.0;
/// A note counts as played when it holds at least this share of the
/// stem's steady pitch time.
const PLAYED_SHARE: f64 = 0.05;
/// A note this common is strong evidence.
const STRONG_SHARE: f64 = 0.10;
/// A low note below standard must have at least this share relative to
/// the standard low note (E): a few D1 readings in a song that sits on E1
/// are passing notes, a five-string or octave errors, not a tuning.
const BELOW_E_RATIO: f64 = 0.5;
/// …and relative to the note an octave above it: a note far rarer than
/// its upper octave is likely the detector reading an octave low.
const OCTAVE_RATIO: f64 = 0.3;
/// "Likely" a half step down needs the standard low note to be at most
/// this share of the lowered one.
const NO_E_RATIO: f64 = 0.25;
/// Lowest note searched below standard, in semitones (B0 on bass).
const MAX_BELOW: i32 = 5;
/// Readings in a row on the same note before they count.
const STEADY_READINGS: u32 = 3;
/// Readings further than this from the nearest semitone (after the
/// offset) are slides or bends and don't count.
const MAX_DEVIATION: f64 = 0.35;

/// A guess at how the song is tuned: the lowest note the bass plays
/// steadily, relative to its standard low string (E1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TuningGuess {
    /// The lowest steadily played bass note as a MIDI number, after
    /// removing the measured offset (so Eb1 = 27 in a song in Eb standard
    /// at +20 cents).
    pub lowest_midi: i32,
    /// `lowest_midi` relative to E1, 0..=−5: 0 is standard, −1 a half step
    /// down, −2 drop D or D standard, and so on.
    pub below_standard: i32,
    /// Strong evidence ("Likely") rather than weak ("Possibly").
    pub likely: bool,
}

/// Guesses the song's tuning from its bass stem; the guitar stem can only
/// back a weak bass guess. `offset_cents` is the measured offset; notes
/// are read relative to it. `None` when no tuning stands out: no bass,
/// too little of it, or it never reaches its low string. The guitar stem
/// alone isn't used: it often holds bass bleed, which reads as guitar
/// notes below the guitar's range.
pub fn guess_song_tuning(
    stems: &[StemAudio],
    sample_rate: u32,
    offset_cents: f32,
) -> Option<TuningGuess> {
    let notes = |kind: StemKind, low_hz: f32| {
        stems
            .iter()
            .find(|s| s.kind == kind)
            .map(|s| NoteShares::measure(s.samples, sample_rate, low_hz, offset_cents))
            .filter(|n| n.seconds >= MIN_SECONDS)
    };
    let mut g = guess_from(&notes(StemKind::Bass, 30.0)?)?;
    // The guitar agreeing on a half step down (its low Eb played, its low
    // E not) makes a weak bass guess strong. Bass bleed in the guitar stem
    // sits an octave lower, so it can't fake this.
    if g.below_standard == -1 && !g.likely {
        g.likely = notes(StemKind::Guitar, 70.0)
            .is_some_and(|gt| half_step_down(&gt, GUITAR_STANDARD_LOW, PLAYED_SHARE));
    }
    Some(g)
}

/// The rules on the bass notes.
fn guess_from(n: &NoteShares) -> Option<TuningGuess> {
    let std_low = BASS_STANDARD_LOW;
    let e = n.share(std_low);
    let lowest = (std_low - MAX_BELOW..std_low).find(|&m| {
        let s = n.share(m);
        s >= PLAYED_SHARE && s >= BELOW_E_RATIO * e && s >= OCTAVE_RATIO * n.share(m + 12)
    });
    let (lowest, likely) = match lowest {
        // Only a half step down has a single reading; lower notes can be a
        // drop tuning, a lowered standard or a five-string, so they stay
        // weak.
        Some(m) => (
            m,
            m == std_low - 1 && half_step_down(n, std_low, STRONG_SHARE),
        ),
        None if e >= PLAYED_SHARE => (std_low, e >= STRONG_SHARE),
        None => return None,
    };
    Some(TuningGuess {
        lowest_midi: lowest,
        below_standard: lowest - std_low,
        likely,
    })
}

/// The note a half step under the standard low string holds at least
/// `min_share`, and the standard low note is rare next to it.
fn half_step_down(n: &NoteShares, standard_low: i32, min_share: f64) -> bool {
    let low = n.share(standard_low - 1);
    low >= min_share && n.share(standard_low) <= NO_E_RATIO * low
}

/// Share of a stem's steady pitch time on each MIDI note.
pub(crate) struct NoteShares {
    shares: [f64; 128],
    pub(crate) seconds: f64,
}

impl NoteShares {
    pub(crate) fn measure(
        samples: &[f32],
        sample_rate: u32,
        low_hz: f32,
        offset_cents: f32,
    ) -> Self {
        let mut det = LiveDetector::new(sample_rate, low_hz);
        let hop_s = det.hop_len() as f64 / f64::from(sample_rate.max(1));
        let mut readings = Vec::new();
        let mut counts = [0u32; 128];
        let mut total = 0u32;
        let mut prev: Option<usize> = None;
        let mut run = 0u32;
        for chunk in samples.chunks(8192) {
            readings.clear();
            det.push(chunk, &mut readings);
            for r in &readings {
                let note = r.hz.and_then(|hz| {
                    let x = 69.0 + 12.0 * (f64::from(hz) / 440.0).log2()
                        - f64::from(offset_cents) / 100.0;
                    let m = x.round();
                    ((x - m).abs() <= MAX_DEVIATION && (0.0..128.0).contains(&m))
                        .then_some(m as usize)
                });
                match note {
                    Some(m) if prev == Some(m) => run += 1,
                    Some(_) => run = 1,
                    None => run = 0,
                }
                prev = note;
                if let Some(m) = note.filter(|_| run >= STEADY_READINGS) {
                    counts[m] += 1;
                    total += 1;
                }
            }
        }
        let mut shares = [0.0; 128];
        if total > 0 {
            for (s, &c) in shares.iter_mut().zip(&counts) {
                *s = f64::from(c) / f64::from(total);
            }
        }
        Self {
            shares,
            seconds: f64::from(total) * hop_s,
        }
    }

    pub(crate) fn share(&self, midi: i32) -> f64 {
        usize::try_from(midi)
            .ok()
            .and_then(|m| self.shares.get(m))
            .copied()
            .unwrap_or(0.0)
    }
}
