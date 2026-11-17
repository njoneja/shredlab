// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! What the detector saw, cell by cell, for checking a stretch of a real
//! song: the decoded chord, the best-scoring chords, the bass note, the
//! onset, the chroma, the segment's detailed name, passing chords, and
//! the scores of the extensions the detector does not decode (6, m6,
//! add9, 9, 7sus4). Not used by the app.

use std::cell::RefCell;

use crate::chroma::CellFeatures;
use crate::decode::{cosine, state_chord, template, BASS_ROOT_BONUS, NO_CHORD, STATES};
use crate::grid::{Cell, Pos};
use crate::{detect_chords, ChordSegment, PassingChord, Quality, StemAudio};

/// One grid cell (a sixteenth of a beat).
#[derive(Debug, Clone)]
pub struct CellReport {
    pub start_s: f64,
    /// "beat", "8th" or "16th".
    pub pos: &'static str,
    /// Pitched level relative to the song's reference level.
    pub level: f32,
    /// Onset strength 0..1.
    pub onset: f32,
    /// Change cost at this cell.
    pub penalty: f32,
    /// Chroma normalized to its maximum (C = 0).
    pub chroma: [f32; 12],
    pub bass: Option<u8>,
    /// The Viterbi state here, before short runs are merged.
    pub decoded: Option<(u8, Quality)>,
    /// The final segment's chord here (after merging and slash chords).
    pub segment: Option<(u8, &'static str, Option<u8>)>,
    /// The final segment's detailed name here.
    pub detailed: Option<(u8, &'static str, Option<u8>)>,
    /// The cell score of the final segment's chord (without slash bass).
    pub segment_score: Option<f32>,
    /// The passing chord this cell lies in, if any.
    pub passing: Option<(u8, &'static str)>,
    /// The figure this cell lies in, if any.
    pub figure: Option<(u8, &'static str)>,
    /// Best chords by cell score, best first: (root, suffix) or None for no chord.
    pub top: Vec<(Option<(u8, &'static str)>, f32)>,
    /// Best chords of the extensions, scored the same way without a bias.
    pub extra: Vec<((u8, &'static str), f32)>,
}

thread_local! {
    static REQUEST: RefCell<Option<(f64, f64)>> = const { RefCell::new(None) };
    static OUT: RefCell<Vec<CellReport>> = const { RefCell::new(Vec::new()) };
}

/// Runs [`detect_chords`] and reports the cells between `from_s` and `to_s`.
pub fn cell_report(
    stems: &[StemAudio],
    sample_rate: u32,
    beats: &[f64],
    offset_cents: f64,
    duration_s: f64,
    from_s: f64,
    to_s: f64,
) -> Vec<CellReport> {
    REQUEST.with(|r| *r.borrow_mut() = Some((from_s, to_s)));
    OUT.with(|o| o.borrow_mut().clear());
    detect_chords(stems, sample_rate, beats, offset_cents, duration_s);
    REQUEST.with(|r| *r.borrow_mut() = None);
    OUT.with(|o| std::mem::take(&mut *o.borrow_mut()))
}

pub(crate) fn requested() -> Option<(f64, f64)> {
    REQUEST.with(|r| *r.borrow())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn fill(
    (from, to): (f64, f64),
    cells: &[Cell],
    features: &[CellFeatures],
    reference: f32,
    scores: &[[f32; STATES]],
    penalties: &[f32],
    path: &[usize],
    segments: &[ChordSegment],
    passing: &[PassingChord],
    figures: &[PassingChord],
) {
    let extra_tpl: Vec<((u8, Quality), [f32; 12])> = (0..12u8)
        .flat_map(|root| {
            Quality::EXTENDED
                .iter()
                .map(move |&q| ((root, q), template(root, q)))
        })
        .collect();
    let mut out = Vec::new();
    for (k, c) in cells.iter().enumerate() {
        if c.end < from || c.start > to {
            continue;
        }
        let f = &features[k];
        let max = f.chroma.iter().cloned().fold(0.0f32, f32::max);
        let norm = f.chroma.iter().map(|x| x * x).sum::<f32>().sqrt();
        let mut chroma = f.chroma;
        if max > 0.0 {
            chroma.iter_mut().for_each(|x| *x /= max);
        }
        let mut order: Vec<usize> = (0..STATES).collect();
        order.sort_by(|&a, &b| scores[k][b].total_cmp(&scores[k][a]));
        let top = order
            .iter()
            .take(5)
            .map(|&s| {
                let chord = (s != NO_CHORD)
                    .then(|| state_chord(s).map(|(r, q)| (r, q.suffix())))
                    .flatten();
                (chord, scores[k][s])
            })
            .collect();
        let bass = f.bass_note();
        let mut extra: Vec<((u8, &'static str), f32)> = if norm > 0.0 {
            extra_tpl
                .iter()
                .map(|&((root, q), ref t)| {
                    let id = (root, q.suffix());
                    let cos = cosine(root, q, t, &f.chroma, &f.leak, norm);
                    let bonus = if bass == Some(id.0) {
                        BASS_ROOT_BONUS
                    } else {
                        0.0
                    };
                    (id, cos + bonus)
                })
                .collect()
        } else {
            Vec::new()
        };
        extra.sort_by(|a, b| b.1.total_cmp(&a.1));
        extra.truncate(2);
        let mid = 0.5 * (c.start + c.end);
        let seg = segments.iter().find(|s| mid >= s.start_s && mid < s.end_s);
        let named = |c: Option<crate::Chord>| c.map(|ch| (ch.root, ch.quality.suffix(), ch.bass));
        let segment = named(seg.and_then(|s| s.chord));
        let detailed = named(seg.and_then(|s| s.detailed));
        let segment_score = seg.and_then(|s| s.chord).map(|ch| {
            let q = Quality::ALL
                .iter()
                .position(|&q| q == ch.quality)
                .unwrap_or(0);
            scores[k][q * 12 + usize::from(ch.root)]
        });
        let inside = |list: &[PassingChord]| {
            list.iter()
                .find(|p| mid >= p.start_s && mid < p.end_s)
                .map(|p| (p.chord.root, p.chord.quality.suffix()))
        };
        let passing = inside(passing);
        let figure = inside(figures);
        out.push(CellReport {
            start_s: c.start,
            pos: match c.pos {
                Pos::Beat => "beat",
                Pos::Eighth => "8th",
                Pos::Sixteenth => "16th",
            },
            level: if reference > 0.0 {
                f.level / reference
            } else {
                0.0
            },
            onset: f.onset,
            penalty: penalties[k],
            chroma,
            bass,
            decoded: state_chord(path[k]),
            segment,
            detailed,
            segment_score,
            passing,
            figure,
            top,
            extra,
        });
    }
    OUT.with(|o| *o.borrow_mut() = out);
}
