// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Detecting the notes of one stretch of a stem (a loop) on its own: the
//! stretch plus at least [`PAD_S`] of context on each side is detected as if
//! it were the whole stem, and the result is moved back to song time and
//! clipped to the stretch.
//!
//! The context before the stretch reaches back to the start of a model
//! window of the whole stem (Basic Pitch runs in windows of about 2 s that
//! start every 1.64 s), so the model sees the same windows as in a
//! song-wide run and hears the stretch the same way: on a 23 s guitar solo
//! at More fast runs with quiet notes, 82 notes against the song-wide run's
//! 81, where a context of exactly 1 s gave 62 (the model's activations move
//! with the window positions, and borderline notes go either way).

use std::ops::Range;

use crate::{Heard, Note, NoteAnalysis};

/// Least context detected on each side of the stretch (clamped to the
/// song): the lead line's register, a note ringing in from before the
/// stretch and the fast-bend check's look back all need some audio before
/// it. Before the stretch it reaches further, to a model window's start.
pub const PAD_S: f64 = 1.0;

/// Shortest stretch [`Excerpt::new`] accepts (s).
pub const MIN_LEN_S: f64 = 0.05;

/// A stretch of a stem to detect on its own: the stretch (`start_s` to
/// `end_s`, song time) and the audio actually detected (`from_s` to `to_s`:
/// from the start of the model window at or before [`PAD_S`] ahead of the
/// stretch, to [`PAD_S`] after it, within the song).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Excerpt {
    pub start_s: f64,
    pub end_s: f64,
    pub from_s: f64,
    pub to_s: f64,
}

/// The latest start of a model window of the whole stem at or before `t`
/// (s), at least 0: the windows of an excerpt starting there fall where the
/// whole stem's do. Exact in samples at 44.1 kHz (twice the model's rate).
fn grid_start(t: f64) -> f64 {
    use crate::basic_pitch::{HOP, SAMPLE_RATE};
    let hop = HOP as f64 / f64::from(SAMPLE_RATE);
    let k = (t / hop).floor().max(0.0);
    k * HOP as f64 / f64::from(SAMPLE_RATE)
}

impl Excerpt {
    /// The stretch `start_s`..`end_s` of a song `duration_s` long, clamped to
    /// the song; `None` when less than [`MIN_LEN_S`] of it is inside.
    pub fn new(start_s: f64, end_s: f64, duration_s: f64) -> Option<Excerpt> {
        if !(start_s.is_finite() && end_s.is_finite() && duration_s.is_finite()) {
            return None;
        }
        let start_s = start_s.max(0.0);
        let end_s = end_s.min(duration_s);
        if end_s - start_s < MIN_LEN_S {
            return None;
        }
        Some(Excerpt {
            start_s,
            end_s,
            from_s: grid_start(start_s - PAD_S),
            to_s: (end_s + PAD_S).min(duration_s),
        })
    }

    /// The samples to detect at `sample_rate`, within a stem of `len`
    /// samples.
    pub fn samples(&self, sample_rate: u32, len: usize) -> Range<usize> {
        let at = |t: f64| ((t * f64::from(sample_rate)).round().max(0.0) as usize).min(len);
        at(self.from_s)..at(self.to_s)
    }

    /// The beats inside the detected audio, in its own time (0 = `from_s`).
    pub fn beats(&self, beats: &[f64]) -> Vec<f64> {
        beats
            .iter()
            .filter(|&&b| b >= self.from_s && b <= self.to_s)
            .map(|b| b - self.from_s)
            .collect()
    }

    /// The analysis of the detected audio back in song time, clipped to the
    /// stretch: notes, chordal stretches and other notes heard that overlap
    /// it, cut at its edges. A bend that starts outside its clipped note is
    /// dropped; one cut short keeps its start and ends its rise at the cut.
    pub fn finish(&self, a: NoteAnalysis) -> NoteAnalysis {
        let off = self.from_s;
        let (s, e) = (self.start_s, self.end_s);
        let clip = |a: f64, b: f64| -> Option<(f64, f64)> {
            let (a, b) = (a + off, b + off);
            (b > s && a < e).then(|| (a.max(s), b.min(e)))
        };
        let notes = a
            .notes
            .iter()
            .filter_map(|n| {
                let (start_s, end_s) = clip(n.start_s, n.end_s)?;
                let bend = n.bend.and_then(|mut b| {
                    b.start_s += off;
                    b.peak_s += off;
                    b.release_s = b.release_s.map(|r| r + off).filter(|&r| r < end_s);
                    if b.start_s < start_s || b.start_s >= end_s {
                        return None;
                    }
                    b.peak_s = b.peak_s.min(end_s);
                    Some(b)
                });
                Some(Note {
                    start_s,
                    end_s,
                    bend,
                    ..*n
                })
            })
            .collect();
        let chordal = a.chordal.iter().filter_map(|&(a, b)| clip(a, b)).collect();
        let others = a
            .others
            .iter()
            .filter_map(|h| {
                let (start_s, end_s) = clip(h.start_s, h.end_s)?;
                Some(Heard {
                    start_s,
                    end_s,
                    ..*h
                })
            })
            .collect();
        NoteAnalysis {
            notes,
            chordal,
            others,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Attack, Bend};

    fn note(start_s: f64, end_s: f64, midi: u8) -> Note {
        Note {
            start_s,
            end_s,
            midi,
            cents: 0.0,
            confidence: 0.8,
            attack: Attack::Picked,
            bend: None,
            vibrato: false,
        }
    }

    #[test]
    fn pads_and_clamps_to_the_song() {
        use crate::basic_pitch::{HOP, SAMPLE_RATE};
        let hop = HOP as f64 / f64::from(SAMPLE_RATE);
        let x = Excerpt::new(10.0, 20.0, 100.0).unwrap();
        // Back to the model window starting at or before 9 s (5 hops).
        assert!((x.from_s - 5.0 * hop).abs() < 1e-9 && x.from_s <= 9.0 && x.from_s > 9.0 - hop);
        assert_eq!(x.to_s, 21.0);
        // In samples at 44.1 kHz, a whole number of the model's hops.
        let r = x.samples(44_100, 100 * 44_100);
        assert_eq!(r.start, 10 * HOP);
        assert_eq!(r.end, 21 * 44_100);
        let x = Excerpt::new(0.4, 5.0, 5.5).unwrap();
        assert_eq!((x.start_s, x.end_s, x.from_s, x.to_s), (0.4, 5.0, 0.0, 5.5));
        let x = Excerpt::new(-3.0, 9.0, 8.0).unwrap();
        assert_eq!((x.start_s, x.end_s, x.from_s, x.to_s), (0.0, 8.0, 0.0, 8.0));
        assert!(Excerpt::new(7.99, 9.0, 8.0).is_none());
        assert!(Excerpt::new(3.0, 3.01, 8.0).is_none());
        assert!(Excerpt::new(f64::NAN, 3.0, 8.0).is_none());
    }

    #[test]
    fn sample_range_and_beats() {
        let x = Excerpt {
            start_s: 2.0,
            end_s: 3.0,
            from_s: 1.0,
            to_s: 4.0,
        };
        assert_eq!(x.samples(1000, 10_000), 1000..4000);
        assert_eq!(x.samples(1000, 3500), 1000..3500);
        assert_eq!(x.beats(&[0.5, 1.0, 2.5, 4.0, 4.5]), vec![0.0, 1.5, 3.0]);
    }

    #[test]
    fn finish_moves_back_and_clips() {
        // Detected audio from 9 s; the loop is 10 to 20 s.
        let x = Excerpt {
            start_s: 10.0,
            end_s: 20.0,
            from_s: 9.0,
            to_s: 21.0,
        };
        let mut bent = note(1.5, 2.5, 64);
        bent.bend = Some(Bend {
            start_s: 1.6,
            peak_s: 1.8,
            release_s: Some(2.3),
            semitones: 2.0,
        });
        // Bend starts before the loop: dropped with the clipped part.
        let mut early_bend = note(0.5, 1.5, 60);
        early_bend.bend = Some(Bend {
            start_s: 0.7,
            peak_s: 0.9,
            release_s: None,
            semitones: 1.0,
        });
        // A bend cut by the loop's end: rise ends at the cut, no release.
        let mut late_bend = note(10.8, 11.5, 67);
        late_bend.bend = Some(Bend {
            start_s: 10.9,
            peak_s: 11.2,
            release_s: Some(11.4),
            semitones: 2.0,
        });
        let a = NoteAnalysis {
            notes: vec![
                note(0.0, 0.8, 59),
                early_bend,
                bent,
                note(5.0, 6.0, 62),
                late_bend,
                note(11.0, 12.0, 69),
            ],
            chordal: vec![(0.2, 3.0), (6.0, 7.0), (11.5, 12.0)],
            others: vec![
                Heard {
                    start_s: 0.5,
                    end_s: 1.2,
                    midi: 50,
                    confidence: 0.4,
                },
                Heard {
                    start_s: 0.2,
                    end_s: 0.9,
                    midi: 52,
                    confidence: 0.4,
                },
                Heard {
                    start_s: 10.5,
                    end_s: 11.5,
                    midi: 55,
                    confidence: 0.4,
                },
            ],
        };
        let f = x.finish(a);
        let r = |t: f64| (t * 1e6).round() / 1e6;
        let spans: Vec<_> = f
            .notes
            .iter()
            .map(|n| (r(n.start_s), r(n.end_s), n.midi))
            .collect();
        assert_eq!(
            spans,
            vec![
                (10.0, 10.5, 60),
                (10.5, 11.5, 64),
                (14.0, 15.0, 62),
                (19.8, 20.0, 67),
            ]
        );
        assert!(f.notes[0].bend.is_none());
        let b = f.notes[1].bend.unwrap();
        assert!((b.start_s - 10.6).abs() < 1e-9 && (b.peak_s - 10.8).abs() < 1e-9);
        assert!((b.release_s.unwrap() - 11.3).abs() < 1e-9);
        let b = f.notes[3].bend.unwrap();
        assert!((b.start_s - 19.9).abs() < 1e-9 && b.peak_s == 20.0 && b.release_s.is_none());
        let chordal: Vec<_> = f.chordal.iter().map(|&(a, b)| (r(a), r(b))).collect();
        assert_eq!(chordal, vec![(10.0, 12.0), (15.0, 16.0)]);
        let heard: Vec<_> = f
            .others
            .iter()
            .map(|h| (r(h.start_s), r(h.end_s), h.midi))
            .collect();
        assert_eq!(heard, vec![(10.0, 10.2, 50), (19.5, 20.0, 55)]);
    }
}
