// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Chord templates, the per-cell scores and the decoding of the cell
//! sequence into segments.

use crate::chroma::CellFeatures;
use crate::grid::{Cell, Pos};
use crate::{Chord, ChordSegment, Quality};

/// Chord states (12 roots × the qualities), then "no chord".
pub(crate) const CHORDS: usize = 12 * Quality::ALL.len();
pub(crate) const NO_CHORD: usize = CHORDS;
pub(crate) const STATES: usize = CHORDS + 1;

/// Template weight of the root relative to the other chord tones: voicings
/// double the root and the bass plays it, so it is usually the strongest.
pub(crate) const ROOT_WEIGHT: f32 = 1.2;
/// Added to the score of each quality. Simpler chords win when scores are
/// close: a fifth with a faint third stays a power chord, a triad with a
/// faint seventh stays a triad.
/// Extensions are never decoded: `detail` scores them against the decoded
/// chord with the decoded chord's bias plus its own.
pub(crate) fn bias(q: Quality) -> f32 {
    match q {
        Quality::Power | Quality::Major | Quality::Minor => 0.0,
        Quality::Dom7 | Quality::Min7 => -0.03,
        Quality::Sus2 | Quality::Sus4 => -0.04,
        Quality::Maj7 => -0.05,
        Quality::Dim => -0.06,
        Quality::Six | Quality::Min6 | Quality::Add9 | Quality::Dom9 | Quality::SevenSus4 => {
            f32::NEG_INFINITY
        }
    }
}
/// Added to chords whose root is the cell's bass note.
pub(crate) const BASS_ROOT_BONUS: f32 = 0.06;
/// Score of "no chord" in a cell with pitched content. Noise and unpitched
/// sound give flat chroma, whose best chord scores about 0.5.
const NO_CHORD_SCORE: f32 = 0.6;
/// Score of "no chord" in a cell whose pitched level is below
/// [`QUIET_LEVEL`] of the song's: higher than any chord can score.
pub(crate) const QUIET_SCORE: f32 = 2.0;
/// Cells quieter than this share of the song's reference level (−20 dB)
/// have no chord.
pub(crate) const QUIET_LEVEL: f32 = 0.1;
/// The song's reference level is this quantile of its non-silent cells.
const REFERENCE_QUANTILE: f64 = 0.8;

/// Cost of changing chord at a cell, by its position. A change on the beat
/// is cheapest; an eighth off the beat (a push) costs more; a sixteenth
/// the most. Units are per-cell scores (cosines): on clean audio a right
/// and a wrong chord differ by about 0.2 to 0.4 per cell, and real
/// recordings swing about that much from cell to cell, so a change has to
/// pay off over a few cells. An eighth push survives when the new chord is
/// clearly ahead in its two early cells; a single noisy cell never pays
/// for the two changes it would need.
const BEAT_PENALTY: f32 = 0.8;
const EIGHTH_PENALTY: f32 = 1.3;
const SIXTEENTH_PENALTY: f32 = 1.8;
/// An onset at the cell's start lowers the change cost by up to this share.
/// Small enough that an off-beat eighth with an onset still costs more than
/// a beat without one: riffs have onsets on every eighth.
const ONSET_RELIEF: f32 = 0.3;
/// Segments shorter than this many cells (an eighth) are merged into a
/// neighbor.
const MIN_CELLS: usize = 2;
/// A slash chord needs the segment to last this many cells (a beat)...
const SLASH_MIN_CELLS: usize = 4;
/// ...one bass pitch class holding this share of its bass readings...
const SLASH_SHARE: f32 = 0.6;
/// ...and at least this many readings (about 0.2 s).
const SLASH_MIN_READINGS: u32 = 20;

pub(crate) fn state_chord(s: usize) -> Option<(u8, Quality)> {
    (s < CHORDS).then(|| ((s % 12) as u8, Quality::ALL[s / 12]))
}

/// Unit-length template of a chord: root weight [`ROOT_WEIGHT`], other
/// tones 1.
pub(crate) fn template(root: u8, q: Quality) -> [f32; 12] {
    let mut v = [0.0f32; 12];
    for &i in q.intervals() {
        v[(usize::from(root) + usize::from(i)) % 12] = if i == 0 { ROOT_WEIGHT } else { 1.0 };
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    for x in &mut v {
        *x /= norm;
    }
    v
}

/// Template tones (intervals) whose evidence can be the 3rd harmonic of
/// another tone of the same chord: tones other than the root, third and
/// fifth that sit a fifth above a chord tone. A maj7's seventh over its
/// third, an m7's over its minor third, a 9th or a sus2's second over the
/// fifth. Root, third and fifth are never discounted (the root's leak
/// lands on the fifth, which is there anyway).
pub(crate) fn leak_tones(q: Quality) -> impl Iterator<Item = u8> {
    let iv = q.intervals();
    iv.iter().copied().filter(move |&i| {
        q != Quality::Sus2 && !matches!(i, 0 | 3 | 4 | 7) && iv.contains(&((i + 5) % 12))
    })
}

/// Cosine between a chord's template and a chroma vector, with each of
/// the chord's [`leak_tones`] counted only for what exceeds `leak`, the
/// 3rd harmonics expected there from the overdriven stems. Normalized by
/// the measured chroma, so templates without such tones score the plain
/// cosine.
pub(crate) fn cosine(
    root: u8,
    q: Quality,
    t: &[f32; 12],
    chroma: &[f32; 12],
    leak: &[f32; 12],
    norm: f32,
) -> f32 {
    let mut dot: f32 = t.iter().zip(chroma).map(|(a, c)| a * c).sum();
    for i in leak_tones(q) {
        let pc = (usize::from(root) + usize::from(i)) % 12;
        dot -= t[pc] * chroma[pc].min(leak[pc]);
    }
    dot / norm
}

/// Unit-length templates and biases for every chord state.
struct Templates {
    t: Vec<[f32; 12]>,
    bias: Vec<f32>,
}

impl Templates {
    fn new() -> Self {
        let mut t = Vec::with_capacity(CHORDS);
        let mut b = Vec::with_capacity(CHORDS);
        for s in 0..CHORDS {
            let (root, q) = state_chord(s).expect("chord state");
            t.push(template(root, q));
            b.push(bias(q));
        }
        Self { t, bias: b }
    }
}

/// The song's reference pitched level.
pub(crate) fn reference_level(features: &[CellFeatures]) -> f32 {
    let mut v: Vec<f32> = features
        .iter()
        .map(|f| f.level)
        .filter(|&l| l > 0.0)
        .collect();
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f32::total_cmp);
    v[((v.len() - 1) as f64 * REFERENCE_QUANTILE).round() as usize]
}

/// Scores of every state in every cell.
pub(crate) fn scores(features: &[CellFeatures], reference: f32) -> Vec<[f32; STATES]> {
    let tpl = Templates::new();
    features
        .iter()
        .map(|f| {
            let mut e = [0.0f32; STATES];
            let norm = f.chroma.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm <= 0.0 || f.level < QUIET_LEVEL * reference {
                e[NO_CHORD] = QUIET_SCORE;
                return e;
            }
            let bass = f.bass_note();
            for (s, (t, b)) in tpl.t.iter().zip(&tpl.bias).enumerate() {
                let (root, q) = state_chord(s).expect("chord state");
                let cos = cosine(root, q, t, &f.chroma, &f.leak, norm);
                let bonus = if bass == Some((s % 12) as u8) {
                    BASS_ROOT_BONUS
                } else {
                    0.0
                };
                e[s] = cos + b + bonus;
            }
            e[NO_CHORD] = NO_CHORD_SCORE;
            e
        })
        .collect()
}

/// Change cost at each cell (the first is unused).
pub(crate) fn penalties(cells: &[Cell], features: &[CellFeatures], has_beats: bool) -> Vec<f32> {
    cells
        .iter()
        .zip(features)
        .map(|(c, f)| {
            let base = if !has_beats {
                EIGHTH_PENALTY
            } else {
                match c.pos {
                    Pos::Beat => BEAT_PENALTY,
                    Pos::Eighth => EIGHTH_PENALTY,
                    Pos::Sixteenth => SIXTEENTH_PENALTY,
                }
            };
            base * (1.0 - ONSET_RELIEF * f.onset)
        })
        .collect()
}

/// Best state sequence: the sum of cell scores minus the change costs,
/// maximized. Staying costs nothing; any change costs the cell's penalty,
/// so the best predecessor is either the same state or the overall best.
pub(crate) fn viterbi(scores: &[[f32; STATES]], penalty: &[f32]) -> Vec<usize> {
    let n = scores.len();
    if n == 0 {
        return Vec::new();
    }
    let mut from = vec![0u8; n * STATES];
    let mut dp = scores[0];
    for k in 1..n {
        let (best, best_v) = argmax(&dp);
        let switch = best_v - penalty[k];
        let mut next = [0.0f32; STATES];
        for s in 0..STATES {
            let (v, f) = if dp[s] >= switch {
                (dp[s], s)
            } else {
                (switch, best)
            };
            next[s] = v + scores[k][s];
            from[k * STATES + s] = f as u8;
        }
        dp = next;
    }
    let mut path = vec![0usize; n];
    path[n - 1] = argmax(&dp).0;
    for k in (1..n).rev() {
        path[k - 1] = usize::from(from[k * STATES + path[k]]);
    }
    path
}

fn argmax(v: &[f32; STATES]) -> (usize, f32) {
    let mut best = (0, f32::NEG_INFINITY);
    for (i, &x) in v.iter().enumerate() {
        if x > best.1 {
            best = (i, x);
        }
    }
    best
}

/// A run of cells with one state: cells `start..end`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Run {
    pub state: usize,
    pub start: usize,
    pub end: usize,
}

fn runs(path: &[usize]) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    for (k, &s) in path.iter().enumerate() {
        match out.last_mut() {
            Some(r) if r.state == s => r.end = k + 1,
            _ => out.push(Run {
                state: s,
                start: k,
                end: k + 1,
            }),
        }
    }
    out
}

/// Merges runs shorter than [`MIN_CELLS`] into the neighbor whose state
/// scores better on their cells, shortest first.
fn merge_short(mut runs: Vec<Run>, scores: &[[f32; STATES]]) -> Vec<Run> {
    loop {
        if runs.len() < 2 {
            return runs;
        }
        let Some((i, _)) = runs
            .iter()
            .enumerate()
            .filter(|(_, r)| r.end - r.start < MIN_CELLS)
            .min_by_key(|(_, r)| r.end - r.start)
        else {
            return runs;
        };
        let r = runs[i];
        let fit = |s: usize| -> f32 { scores[r.start..r.end].iter().map(|e| e[s]).sum() };
        let left = i.checked_sub(1).map(|j| (j, fit(runs[j].state)));
        let right = (i + 1 < runs.len()).then(|| (i + 1, fit(runs[i + 1].state)));
        let target = match (left, right) {
            (Some(l), Some(rt)) => {
                if rt.1 > l.1 {
                    rt.0
                } else {
                    l.0
                }
            }
            (Some(l), None) => l.0,
            (None, Some(rt)) => rt.0,
            (None, None) => return runs,
        };
        if target < i {
            runs[target].end = r.end;
        } else {
            runs[target].start = r.start;
        }
        runs.remove(i);
        // Neighbors with the same state join.
        let mut j = 1;
        while j < runs.len() {
            if runs[j].state == runs[j - 1].state {
                runs[j - 1].end = runs[j].end;
                runs.remove(j);
            } else {
                j += 1;
            }
        }
    }
}

/// The decoded path as runs, those shorter than [`MIN_CELLS`] merged.
pub(crate) fn merged_runs(path: &[usize], scores: &[[f32; STATES]]) -> Vec<Run> {
    merge_short(runs(path), scores)
}

/// Turns the merged runs into segments, one each: slash chords from held
/// bass notes, pushes marked. `detailed` is the chord itself until
/// `detail::refine` names it.
pub(crate) fn segments(
    runs: &[Run],
    cells: &[Cell],
    features: &[CellFeatures],
    has_beats: bool,
) -> Vec<ChordSegment> {
    runs.iter()
        .map(|r| {
            let chord = state_chord(r.state).map(|(root, quality)| Chord {
                root,
                quality,
                bass: slash_bass(root, &features[r.start..r.end]),
            });
            let first = &cells[r.start];
            ChordSegment {
                start_s: first.start,
                end_s: cells[r.end - 1].end,
                chord,
                push: has_beats && r.start > 0 && chord.is_some() && first.pos != Pos::Beat,
                detailed: chord,
            }
        })
        .collect()
}

/// The bass note of a slash chord: one pitch class, other than the root,
/// holding most of the bass readings of a segment at least a beat long.
fn slash_bass(root: u8, cells: &[CellFeatures]) -> Option<u8> {
    if cells.len() < SLASH_MIN_CELLS {
        return None;
    }
    let mut counts = [0u32; 12];
    for f in cells {
        for (c, &b) in counts.iter_mut().zip(&f.bass) {
            *c += u32::from(b);
        }
    }
    let total: u32 = counts.iter().sum();
    let (pc, &max) = counts.iter().enumerate().max_by_key(|(_, &c)| c)?;
    let pc = pc as u8;
    (total >= SLASH_MIN_READINGS && max as f32 >= SLASH_SHARE * total as f32 && pc != root)
        .then_some(pc)
}
