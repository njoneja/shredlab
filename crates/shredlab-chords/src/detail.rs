// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! On top of the decoded segments: passing chords inside a segment, and a
//! more detailed name for each segment (A6, Aadd9, E9). Neither moves a
//! segment boundary.

use crate::chroma::CellFeatures;
use crate::decode::{self, state_chord, Run, CHORDS, NO_CHORD, QUIET_SCORE, STATES};
use crate::grid::{Cell, Pos};
use crate::{Chord, ChordSegment, Quality};

/// A passing chord may last at most this many cells (a beat)...
const PASS_MAX_CELLS: usize = 4;
/// ...and at least this many (an eighth), unless it is a single cell
/// with strong evidence.
const PASS_MIN_CELLS: usize = 2;
/// Its first and last cells beat the segment's chord by at least this
/// much on their own (cell scores, biases included): the chord starts
/// where it is struck and lasts to its end, and a single loud cell next
/// to the segment's chord does not carry a pooled window.
const PASS_EDGE_MARGIN: f32 = 0.03;
/// On the window's pooled chroma it beats the segment's chord by this
/// much. Real recordings swing about 0.1 from cell to cell, so a smaller
/// margin finds passing chords everywhere.
const PASS_MARGIN: f32 = 0.15;
/// It scores at least this much on the pooled chroma: a clear chord, not
/// the best reading of a few riff notes.
const PASS_MIN_SCORE: f32 = 0.8;
/// Added to the pooled score of a passing chord with a seventh (7, m7,
/// maj7) on top of the decoder's bias: a quick chord is named as a triad
/// unless its seventh is clearly there.
const PASS_SEVENTH_BIAS: f32 = -0.05;
/// It starts on a beat or the off-beat eighth with an onset of at least
/// this strength: it is struck, not a note of an arpeggio ringing on.
const PASS_ONSET: f32 = 0.5;
/// A single cell counts only with an onset of at least this strength...
const SINGLE_ONSET: f32 = 0.8;
/// ...and this margin.
const SINGLE_MARGIN: f32 = 0.25;
/// The cells of a passing chord have at least this share of the song's
/// reference level on average: a decaying tail reads as anything.
const PASS_LEVEL: f32 = 0.3;
/// The held chord lets go: a tone of the segment's chord that is not in
/// the passing chord has at most this share of its level in the rest of
/// the segment (as shares of the pooled chroma, so loudness does not
/// count). A riff over a ringing chord keeps the chord's tones.
const LET_GO: f32 = 0.55;
/// A tone with less than this share of the rest of the segment's chroma
/// was hardly there, so it cannot let go.
const LET_GO_MIN_SHARE: f32 = 0.05;
/// The rest of the segment needs at least this many cells to compare
/// against.
const LET_GO_MIN_CELLS: usize = 2;
/// Instead of the held chord letting go, at least this many of the
/// passing chord's new notes (not in the segment's chord) arrive together,
/// its root among them, each strong: at least [`ARRIVE_FLOOR`] of the
/// window's chroma (above
/// its expected 3rd-harmonic leak) and [`ARRIVE_RATIO`] times its share
/// in the rest of the segment. G over D (G and B) passes while the D's A
/// rings on; a riff brings one new note at a time.
const ARRIVE_NOTES: usize = 2;
const ARRIVE_FLOOR: f32 = 0.11;
const ARRIVE_RATIO: f32 = 2.0;
/// Added to an extension's cell score on top of the decoded chord's bias:
/// the added note has to be there through most of the segment, not now
/// and then. With three equal chord tones, a ninth or sixth at about 60 %
/// of their strength breaks even.
const DETAIL_BIAS: f32 = -0.04;
/// A detailed name replaces the plain one when its mean cell score over
/// the segment is higher and it scores higher in at least this share of
/// the segment's cells.
const DETAIL_SHARE: f32 = 0.5;

/// A passing chord found in the merged run `run`: cells `start..end`,
/// chord state `state`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Pass {
    pub run: usize,
    pub start: usize,
    pub end: usize,
    pub state: usize,
}

/// Qualities that can name a decoded quality in more detail, the decoded
/// one first. A sus2 can be an add9 whose third is weak (the 2nd and 3rd
/// sit a tone apart, and the third is often quieter). Power, maj7, m7 and
/// dim keep their name.
fn refinements(q: Quality) -> &'static [Quality] {
    use Quality::*;
    match q {
        Major => &[Major, Six, Add9],
        Sus2 => &[Sus2, Add9],
        Minor => &[Minor, Min6],
        Dom7 => &[Dom7, Dom9],
        Sus4 => &[Sus4, SevenSus4],
        _ => &[],
    }
}

/// Passing chords are triads or sevenths with a third: power chords,
/// sus and dim readings of a few notes are mostly riffs and arpeggios.
fn has_third(c: usize) -> bool {
    state_chord(c).is_some_and(|(_, q)| third(q) != 0)
}

fn pitch_set(root: u8, q: Quality) -> u16 {
    q.intervals().iter().fold(0u16, |m, &i| {
        m | 1 << ((usize::from(root) + usize::from(i)) % 12)
    })
}

/// Major (1), minor (2) or no third (0).
fn third(q: Quality) -> u8 {
    match q {
        Quality::Major | Quality::Dom7 | Quality::Maj7 => 1,
        Quality::Minor | Quality::Min7 => 2,
        _ => 0,
    }
}

/// Whether chord state `c` is a chord of its own next to state `s`, not
/// `s` with notes added or left out. On a different root, `c` needs a
/// note outside `s`, its ninth, seventh and major seventh (C#m7 over A is
/// an Amaj9 voicing), and must not contain all of `s` (F#m7 over A is an
/// A6). On the same root, it is the other triad (Am over A, not A7 or A5).
/// Bm over D counts: B is D's sixth, and a quick Bm is what players hear.
fn distinct(c: usize, s: usize) -> bool {
    let (Some((rc, qc)), Some((rs, qs))) = (state_chord(c), state_chord(s)) else {
        return false;
    };
    if rc == rs {
        return third(qc) != 0 && third(qs) != 0 && third(qc) != third(qs);
    }
    let (pc, ps) = (pitch_set(rc, qc), pitch_set(rs, qs));
    let tensions = [2u8, 10, 11].iter().fold(ps, |m, &i| {
        m | 1 << ((usize::from(rs) + usize::from(i)) % 12)
    });
    pc & !tensions != 0 && pc & ps != ps
}

/// Pitch classes of chord state `c` (none for "no chord").
fn state_pitches(c: usize) -> u16 {
    state_chord(c).map_or(0, |(r, q)| pitch_set(r, q))
}

/// Sum of the chroma of `cells`.
fn pooled(features: &[CellFeatures], cells: impl Iterator<Item = usize>) -> [f32; 12] {
    let mut v = [0.0f32; 12];
    for k in cells {
        for (x, c) in v.iter_mut().zip(&features[k].chroma) {
            *x += c;
        }
    }
    v
}

/// Sum of the expected 3rd-harmonic leak of `cells`.
fn pooled_leak(features: &[CellFeatures], cells: std::ops::Range<usize>) -> [f32; 12] {
    let mut v = [0.0f32; 12];
    for f in &features[cells] {
        for (x, c) in v.iter_mut().zip(&f.leak) {
            *x += c;
        }
    }
    v
}

/// Sum of the bass readings of `cells`.
fn pooled_bass(features: &[CellFeatures], cells: std::ops::Range<usize>) -> [u32; 12] {
    let mut v = [0u32; 12];
    for f in &features[cells] {
        for (x, &y) in v.iter_mut().zip(&f.bass) {
            *x += u32::from(y);
        }
    }
    v
}

/// The name of a passing chord found as state `c` on cells `cells` of a
/// segment with state `s`: among `c` and the chords with a third made of
/// its notes (G and Bm in Gmaj7), the one that scores best on the pooled
/// window. With [`PASS_SEVENTH_BIAS`], a seventh chord keeps its name only
/// when its fourth note is about as strong as the others; otherwise the
/// triad whose notes are strongest names it.
fn pass_name(
    c: usize,
    s: usize,
    features: &[CellFeatures],
    cells: std::ops::Range<usize>,
) -> usize {
    let notes = state_pitches(c);
    let chroma = pooled(features, cells.clone());
    let leak = pooled_leak(features, cells.clone());
    let bass = pooled_bass(features, cells);
    (0..CHORDS)
        .filter(|&x| x == c || (state_pitches(x) & !notes == 0 && has_third(x)))
        .filter(|&x| x == c || (distinct(x, s) && !borrowed_third(x, s)))
        .map(|x| (x, pooled_score(&chroma, &leak, &bass, x, true)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map_or(c, |(x, _)| x)
}

/// Score of chord state `c` on pooled chroma, leak and bass readings, as the
/// decoder scores a cell, plus [`PASS_SEVENTH_BIAS`] when `pass` and `c`
/// has a seventh.
fn pooled_score(
    chroma: &[f32; 12],
    leak: &[f32; 12],
    bass: &[u32; 12],
    c: usize,
    pass: bool,
) -> f32 {
    let Some((root, q)) = state_chord(c) else {
        return f32::NEG_INFINITY;
    };
    let norm = chroma.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm <= 0.0 {
        return f32::NEG_INFINITY;
    }
    let cos = decode::cosine(root, q, &decode::template(root, q), chroma, leak, norm);
    let total: u32 = bass.iter().sum();
    let max = bass.iter().copied().max().unwrap_or(0);
    let root_bass =
        total >= 2 && max as f32 >= 0.6 * total as f32 && bass[usize::from(root)] == max;
    let bonus = if root_bass {
        decode::BASS_ROOT_BONUS
    } else {
        0.0
    };
    let seventh = if pass && matches!(q, Quality::Dom7 | Quality::Min7 | Quality::Maj7) {
        PASS_SEVENTH_BIAS
    } else {
        0.0
    };
    cos + decode::bias(q) + bonus + seventh
}

/// Whether some tone of segment state `s` that passing state `c` lacks
/// is clearly weaker in `window` (pooled chroma) than in `rest`, both as
/// shares of their chroma, so the song getting louder or quieter does not
/// count. A tone a semitone from one of the passing chord's new notes is
/// not judged: in the analysis windows the loud neighbor masks it (a
/// riff's C hides a ringing A chord's C#), so its drop proves nothing.
fn lets_go(s: usize, c: usize, window: &[f32; 12], rest: &[f32; 12]) -> bool {
    let (wt, rt) = (window.iter().sum::<f32>(), rest.iter().sum::<f32>());
    if wt <= 0.0 || rt <= 0.0 {
        return false;
    }
    let (held, pass) = (state_pitches(s), state_pitches(c));
    let new = pass & !held;
    let masked = (new << 1 | new >> 1 | new << 11 | new >> 11) & 0xfff;
    let gone = held & !pass & !masked;
    (0..12).filter(|&pc| gone & 1 << pc != 0).any(|pc| {
        let share = rest[pc] / rt;
        share >= LET_GO_MIN_SHARE && window[pc] / wt <= LET_GO * share
    })
}

/// Whether at least [`ARRIVE_NOTES`] of passing state `c`'s notes that
/// segment state `s` lacks are strong in the window, its root among them:
/// pooled chroma `window` with leak `wleak`, against `rest` with leak
/// `rleak`. A chord whose root the held chord already has (C# or C#7 over
/// A, its third) is more often a riff's power chord on that note.
fn new_notes_arrive(
    s: usize,
    c: usize,
    (window, wleak): (&[f32; 12], &[f32; 12]),
    (rest, rleak): (&[f32; 12], &[f32; 12]),
) -> bool {
    let (wt, rt) = (window.iter().sum::<f32>(), rest.iter().sum::<f32>());
    if wt <= 0.0 || rt <= 0.0 {
        return false;
    }
    let Some((root, _)) = state_chord(c) else {
        return false;
    };
    let new = state_pitches(c) & !state_pitches(s);
    let strong: Vec<usize> = (0..12)
        .filter(|&pc| new & 1 << pc != 0)
        .filter(|&pc| {
            let w = (window[pc] - wleak[pc]).max(0.0) / wt;
            let r = (rest[pc] - rleak[pc]).max(0.0) / rt;
            w >= ARRIVE_FLOOR && w >= ARRIVE_RATIO * r
        })
        .collect();
    strong.len() >= ARRIVE_NOTES && strong.contains(&usize::from(root))
}

/// Whether chord state `c` over segment state `s` is a power chord
/// moving under the held chord's tones: its root and fifth are not in the
/// held chord's triad and its third is (C over A or A7, F over D). Riffs
/// move like this (A, C, D), and the third the chord seems to have is the
/// held chord's. The held chord's seventh does not count: over A7, the G
/// is often the riff's own note.
fn borrowed_third(c: usize, s: usize) -> bool {
    let (Some((rc, qc)), Some((rs, qs))) = (state_chord(c), state_chord(s)) else {
        return false;
    };
    let held = qs
        .intervals()
        .iter()
        .filter(|&&i| i < 10)
        .fold(0u16, |m, &i| {
            m | 1 << ((usize::from(rs) + usize::from(i)) % 12)
        });
    let pc = |i: u8| 1u16 << ((usize::from(rc) + usize::from(i)) % 12);
    let third = match third(qc) {
        1 => 4,
        2 => 3,
        _ => return false,
    };
    held & pc(0) == 0 && held & pc(7) == 0 && held & pc(third) != 0
}

/// Short chords inside chord segments that beat the segment's chord
/// clearly on their pooled chroma while a tone of the segment's chord
/// lets go, in time order. Not the neighbor's chord arriving early or
/// leaving late, not "no chord".
pub(crate) fn passing(
    runs: &[Run],
    scores: &[[f32; STATES]],
    cells: &[Cell],
    features: &[CellFeatures],
    reference: f32,
    has_beats: bool,
) -> Vec<Pass> {
    let mut found: Vec<(f32, Pass)> = Vec::new();
    for (i, r) in runs.iter().enumerate() {
        if r.state == NO_CHORD {
            continue;
        }
        let prev = i.checked_sub(1).map(|j| runs[j].state);
        let next = runs.get(i + 1).map(|n| n.state);
        let mut cand: Vec<(f32, Pass)> = Vec::new();
        for a in r.start..r.end {
            // Struck: it starts on the beat or the off-beat eighth with an
            // onset.
            let struck =
                (!has_beats || cells[a].pos != Pos::Sixteenth) && features[a].onset >= PASS_ONSET;
            if !struck {
                continue;
            }
            for b in a + 1..=(a + PASS_MAX_CELLS).min(r.end) {
                let len = b - a;
                // Shorter than an eighth only as a single, clearly struck
                // cell.
                let single = len < PASS_MIN_CELLS;
                if single && !(len == 1 && has_beats && features[a].onset >= SINGLE_ONSET) {
                    continue;
                }
                let n = len as f32;
                let level: f32 = features[a..b].iter().map(|f| f.level).sum::<f32>() / n;
                if level < PASS_LEVEL * reference {
                    continue;
                }
                let chroma = pooled(features, a..b);
                let leak = pooled_leak(features, a..b);
                let bass = pooled_bass(features, a..b);
                let held = pooled_score(&chroma, &leak, &bass, r.state, false);
                let need = if single { SINGLE_MARGIN } else { PASS_MARGIN };
                for c in (0..CHORDS).filter(|&c| {
                    has_third(c) && distinct(c, r.state) && !borrowed_third(c, r.state)
                }) {
                    let edge = |k: usize| scores[k][c] - scores[k][r.state] >= PASS_EDGE_MARGIN;
                    if !edge(a) || !edge(b - 1) {
                        continue;
                    }
                    let score = pooled_score(&chroma, &leak, &bass, c, true);
                    let margin = score - held;
                    // The neighbor's chord arriving early or ringing on is
                    // not a chord of its own.
                    let early =
                        b == r.end && next.is_some_and(|x| x != NO_CHORD && !distinct(c, x));
                    let late =
                        a == r.start && prev.is_some_and(|x| x != NO_CHORD && !distinct(c, x));
                    if margin >= need && score >= PASS_MIN_SCORE && !early && !late {
                        cand.push((
                            margin * n,
                            Pass {
                                run: i,
                                start: a,
                                end: b,
                                state: c,
                            },
                        ));
                    }
                }
            }
        }
        // The held chord lets go, compared with the rest of the segment:
        // its cells outside every candidate window, quiet ones left out...
        let rest_cells: Vec<usize> = (r.start..r.end)
            .filter(|&k| scores[k][NO_CHORD] < QUIET_SCORE)
            .filter(|&k| !cand.iter().any(|(_, p)| (p.start..p.end).contains(&k)))
            .collect();
        let rest = pooled(features, rest_cells.iter().copied());
        let mut rest_leak = [0.0f32; 12];
        for &k in &rest_cells {
            for (x, l) in rest_leak.iter_mut().zip(&features[k].leak) {
                *x += l;
            }
        }
        // ...or at least two strong new notes arrive together.
        cand.retain(|(_, p)| {
            let window = pooled(features, p.start..p.end);
            let wleak = pooled_leak(features, p.start..p.end);
            rest_cells.len() >= LET_GO_MIN_CELLS
                && (lets_go(r.state, p.state, &window, &rest)
                    || new_notes_arrive(r.state, p.state, (&window, &wleak), (&rest, &rest_leak)))
        });
        // The clearest first; overlapping ones lose.
        cand.sort_by(|x, y| y.0.total_cmp(&x.0));
        for (t, p) in cand {
            if found
                .iter()
                .all(|(_, q)| q.run != p.run || q.end <= p.start || p.end <= q.start)
            {
                found.push((
                    t,
                    Pass {
                        state: pass_name(p.state, r.state, features, p.start..p.end),
                        ..p
                    },
                ));
            }
        }
    }
    let mut out: Vec<Pass> = found.into_iter().map(|(_, p)| p).collect();
    out.sort_by_key(|p| p.start);
    out
}

/// Names each chord segment in more detail: the decoded quality or one of
/// its refinements, same root and bass, scored over the segment's cells
/// outside passing chords with the same templates as the decoder.
pub(crate) fn refine(
    segments: &mut [ChordSegment],
    runs: &[Run],
    passes: &[Pass],
    scores: &[[f32; STATES]],
    features: &[CellFeatures],
) {
    for (i, (seg, r)) in segments.iter_mut().zip(runs).enumerate() {
        let Some(chord) = seg.chord else {
            seg.detailed = None;
            continue;
        };
        seg.detailed = Some(chord);
        let cands = refinements(chord.quality);
        if cands.len() < 2 {
            continue;
        }
        let used: Vec<usize> = (r.start..r.end)
            .filter(|&k| scores[k][NO_CHORD] < QUIET_SCORE)
            .filter(|&k| {
                !passes
                    .iter()
                    .any(|p| p.run == i && (p.start..p.end).contains(&k))
            })
            .collect();
        if used.is_empty() {
            continue;
        }
        // Cosines only: the bass bonus is the same for every candidate
        // (same root), and the decoded chord's bias is added to both.
        // The leak is judged over the segment: per pitch class, the share
        // of the pooled chroma that exceeds the pooled leak is kept in
        // every cell.
        let (mut sc, mut sl) = ([0.0f32; 12], [0.0f32; 12]);
        for &k in &used {
            for pc in 0..12 {
                sc[pc] += features[k].chroma[pc];
                sl[pc] += features[k].leak[pc];
            }
        }
        let lost: [f32; 12] = std::array::from_fn(|pc| {
            if sc[pc] > 0.0 {
                sl[pc].min(sc[pc]) / sc[pc]
            } else {
                0.0
            }
        });
        let cell = |k: usize, q: Quality| -> f32 {
            let f = &features[k];
            let norm = f.chroma.iter().map(|x| x * x).sum::<f32>().sqrt();
            let t = decode::template(chord.root, q);
            let extra = if q == chord.quality { 0.0 } else { DETAIL_BIAS };
            let leak: [f32; 12] = std::array::from_fn(|pc| f.chroma[pc] * lost[pc]);
            decode::cosine(chord.root, q, &t, &f.chroma, &leak, norm) + extra
        };
        let mut best = (chord.quality, 0.0f32);
        for &q in &cands[1..] {
            let d: Vec<f32> = used
                .iter()
                .map(|&k| cell(k, q) - cell(k, chord.quality))
                .collect();
            let mean = d.iter().sum::<f32>() / d.len() as f32;
            let share = d.iter().filter(|&&x| x > 0.0).count() as f32 / d.len() as f32;
            if mean > best.1 && share >= DETAIL_SHARE {
                best = (q, mean);
            }
        }
        seg.detailed = Some(Chord {
            quality: best.0,
            ..chord
        });
    }
}

/// A figure lasts at least this many cells (a beat)...
const FIGURE_MIN_CELLS: usize = 4;
/// ...and is found in windows of at most this many (two beats); adjacent
/// windows with the same figure join.
const FIGURE_MAX_CELLS: usize = 8;
/// The figure's added note has at least this share of the window's
/// chroma (above its expected 3rd-harmonic leak)...
const FIGURE_FLOOR: f32 = 0.08;
/// ...and at least this many times its share in the rest of the segment.
const FIGURE_RATIO: f32 = 2.0;
/// The figure's pooled score (with [`DETAIL_BIAS`]) beats the plain
/// chord's by at least this much.
const FIGURE_MARGIN: f32 = 0.02;
/// A window whose expected 3rd-harmonic leak is more than this share of
/// its chroma comes from overdriven stems: only sixths count there
/// (6ths are no harmonic of the chord's notes; a 7th or 9th can be, and
/// a b7 over a held major chord is often a riff note).
const FIGURE_DRIVEN: f32 = 0.1;
/// A sus figure replaces the chord's third: its share of the window's
/// chroma is at most this much of its share in the rest of the segment
/// (an A with a melody D over a ringing C# is no Asus4).
const SUS_THIRD_DROP: f32 = 0.55;
/// The figures of one quality cover at most this share of the segment's
/// cells; more, and the extension belongs in the segment's detailed name.
const FIGURE_MAX_SHARE: f32 = 0.6;

/// A candidate figure window: margin times length, start, end, the shares
/// of the added notes and of the notes the extension removes.
type FigureWindow = (f32, usize, usize, Vec<f32>, Vec<f32>);

/// A same-root extension figure: cells `start..end` of run `run`, the
/// segment's root with quality `quality`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Figure {
    pub run: usize,
    pub start: usize,
    pub end: usize,
    pub quality: Quality,
}

/// Extensions a figure can add to a decoded quality: a note on top of
/// the chord, same root. Power chords get none (a 6th or 9th over a
/// fifth is as often a riff note as a chord).
fn figure_qualities(q: Quality) -> &'static [Quality] {
    use Quality::*;
    match q {
        Major => &[Six, Add9, Maj7, Dom7, Dom9, Sus4, Sus2],
        Minor => &[Min6, Min7, Sus4, Sus2],
        Dom7 => &[Dom9],
        Sus4 => &[SevenSus4],
        _ => &[],
    }
}

/// Chroma and leak summed over `cells`.
fn sums(features: &[CellFeatures], cells: impl Iterator<Item = usize>) -> ([f32; 12], [f32; 12]) {
    let (mut c, mut l) = ([0.0f32; 12], [0.0f32; 12]);
    for k in cells {
        for pc in 0..12 {
            c[pc] += features[k].chroma[pc];
            l[pc] += features[k].leak[pc];
        }
    }
    (c, l)
}

/// Share of pitch class `pc` in pooled chroma `c`, counted above its
/// pooled leak `l` when `discount`.
fn note_share(c: &[f32; 12], l: &[f32; 12], pc: usize, discount: bool) -> f32 {
    let total: f32 = c.iter().sum();
    let kept = if discount {
        (c[pc] - l[pc]).max(0.0)
    } else {
        c[pc]
    };
    if total > 0.0 {
        kept / total
    } else {
        0.0
    }
}

/// Stretches of at least a beat inside chord segments where the chord
/// gains a note on its root (A to A6 and back within one A segment), in
/// time order. Outside passing chords and quiet cells, never the
/// segment's detailed name.
#[allow(clippy::too_many_arguments)]
pub(crate) fn figures(
    segments: &[ChordSegment],
    runs: &[Run],
    passes: &[Pass],
    scores: &[[f32; STATES]],
    cells: &[Cell],
    features: &[CellFeatures],
    reference: f32,
    has_beats: bool,
) -> Vec<Figure> {
    let mut out: Vec<Figure> = Vec::new();
    for (i, (seg, r)) in segments.iter().zip(runs).enumerate() {
        let (Some(chord), Some(detailed)) = (seg.chord, seg.detailed) else {
            continue;
        };
        let usable = |k: usize| {
            scores[k][NO_CHORD] < QUIET_SCORE
                && !passes
                    .iter()
                    .any(|p| p.run == i && (p.start..p.end).contains(&k))
        };
        let used = (r.start..r.end).filter(|&k| usable(k)).count();
        if used < 2 * FIGURE_MIN_CELLS {
            continue;
        }
        let base = pitch_set(chord.root, chord.quality);
        let mut all: Vec<(f32, Figure)> = Vec::new();
        for &q in figure_qualities(chord.quality) {
            if q == detailed.quality {
                continue;
            }
            let added = pitch_set(chord.root, q) & !base;
            // A sus2's second is the fifth's 3rd harmonic like a 9th (the
            // decoder leaves it alone; see `decode::leak_tones`).
            let second = 1u16 << ((usize::from(chord.root) + 2) % 12);
            let discounted: u16 = decode::leak_tones(q)
                .fold(if q == Quality::Sus2 { second } else { 0 }, |m, i| {
                    m | 1 << ((usize::from(chord.root) + usize::from(i)) % 12)
                });
            // A sus replaces the third, which has to drop in the window.
            let removed = base & !pitch_set(chord.root, q);
            let removed_pcs: Vec<usize> = (0..12).filter(|&pc| removed & 1 << pc != 0).collect();
            let plain = |c: &[f32; 12]| -> Vec<f32> {
                let total: f32 = c.iter().sum();
                removed_pcs
                    .iter()
                    .map(|&pc| if total > 0.0 { c[pc] / total } else { 0.0 })
                    .collect()
            };
            let added_pcs: Vec<usize> = (0..12).filter(|&pc| added & 1 << pc != 0).collect();
            let shares = |c: &[f32; 12], l: &[f32; 12]| -> Vec<f32> {
                added_pcs
                    .iter()
                    .map(|&pc| note_share(c, l, pc, discounted & 1 << pc != 0))
                    .collect()
            };
            let mut cand: Vec<FigureWindow> = Vec::new();
            for a in r.start..r.end {
                let struck = (!has_beats || cells[a].pos != Pos::Sixteenth)
                    && features[a].onset >= PASS_ONSET;
                if !struck {
                    continue;
                }
                for b in a + FIGURE_MIN_CELLS..=(a + FIGURE_MAX_CELLS).min(r.end) {
                    if !(a..b).all(usable) {
                        break;
                    }
                    let n = (b - a) as f32;
                    let level: f32 = features[a..b].iter().map(|f| f.level).sum::<f32>() / n;
                    if level < PASS_LEVEL * reference {
                        continue;
                    }
                    let (c, l) = sums(features, a..b);
                    let norm = c.iter().map(|x| x * x).sum::<f32>().sqrt();
                    if norm <= 0.0 {
                        continue;
                    }
                    let score = |q: Quality| {
                        let t = decode::template(chord.root, q);
                        decode::cosine(chord.root, q, &t, &c, &l, norm)
                    };
                    let margin = score(q) + DETAIL_BIAS - score(chord.quality);
                    let s = shares(&c, &l);
                    // On overdriven stems only sixths and fourths: a 7th, 9th
                    // or 2nd there is too often a harmonic or a riff note.
                    let driven = !matches!(q, Quality::Six | Quality::Min6 | Quality::Sus4)
                        && l.iter().sum::<f32>() > FIGURE_DRIVEN * c.iter().sum::<f32>();
                    if !driven && margin >= FIGURE_MARGIN && s.iter().all(|&x| x >= FIGURE_FLOOR) {
                        cand.push((margin * n, a, b, s, plain(&c)));
                    }
                }
            }
            // The added note stands out against the rest of the segment:
            // its usable cells outside every candidate window.
            let rest: Vec<usize> = (r.start..r.end)
                .filter(|&k| usable(k) && !cand.iter().any(|x| (x.1..x.2).contains(&k)))
                .collect();
            if rest.len() < FIGURE_MIN_CELLS {
                continue;
            }
            let (rc, rl) = sums(features, rest.iter().copied());
            let held = shares(&rc, &rl);
            let kept = plain(&rc);
            cand.retain(|x| {
                x.3.iter().zip(&held).all(|(&w, &h)| w >= FIGURE_RATIO * h)
                    && x.4
                        .iter()
                        .zip(&kept)
                        .all(|(&w, &h)| w <= SUS_THIRD_DROP * h)
            });
            cand.sort_by(|x, y| y.0.total_cmp(&x.0));
            let windows: Vec<(f32, Figure)> = cand
                .into_iter()
                .map(|(m, a, b, _, _)| {
                    let f = Figure {
                        run: i,
                        start: a,
                        end: b,
                        quality: q,
                    };
                    (m, f)
                })
                .collect();
            // How much of the segment this quality's best windows cover.
            let mut picked: Vec<Figure> = Vec::new();
            for (_, f) in &windows {
                if picked.iter().all(|p| p.end <= f.start || f.end <= p.start) {
                    picked.push(*f);
                }
            }
            let covered: usize = picked.iter().map(|f| f.end - f.start).sum();
            if covered as f32 > FIGURE_MAX_SHARE * used as f32 {
                continue;
            }
            all.extend(windows);
        }
        // Across qualities the clearest windows win (an Asus2 window is an
        // Aadd9 too, but scores better without the third).
        all.sort_by(|x, y| y.0.total_cmp(&x.0));
        let mut found: Vec<Figure> = Vec::new();
        for (_, f) in all {
            if found.iter().all(|p| p.end <= f.start || f.end <= p.start) {
                found.push(f);
            }
        }
        let mut figs = found;
        figs.sort_by_key(|f| f.start);
        // Adjacent windows with the same figure join.
        for f in figs {
            match out.last_mut() {
                Some(last)
                    if last.run == f.run && last.quality == f.quality && last.end == f.start =>
                {
                    last.end = f.end;
                }
                _ => out.push(f),
            }
        }
    }
    out
}
