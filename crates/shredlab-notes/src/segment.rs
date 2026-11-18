// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Notes from the pitch track: voiced stretches cut at onsets into
//! attack segments, stable pitches inside each segment, and the moves
//! between them labeled as legato steps, slides or bends; then vibrato,
//! naming and confidence per note.

use crate::pitch::Track;
use crate::{Attack, Bend, Note, StemKind, HOP_S};

/// Unvoiced frames bridged inside a segment when no onset falls in them.
const GAP_FRAMES: usize = 3;
/// An onset this many frames before or after the start of a voiced
/// stretch marks where the stretch starts.
const ONSET_BEFORE: usize = 6;
const ONSET_AFTER: usize = 4;
/// An onset splits a segment only this many frames after its start.
const MIN_SPLIT: usize = 3;
/// Half-width of the median that smooths the pitch, in frames.
const SMOOTH_RADIUS: usize = 3;
/// Note labels: a frame within this of a semitone can start a note, and
/// the label moves only when the pitch is more than [`LEAVE_ST`] from the
/// current note and within [`ENTER_ST`] of the new one for
/// [`CONFIRM_FRAMES`] frames. Vibrato around a note keeps its label.
const ENTER_ST: f32 = 0.45;
const LEAVE_ST: f32 = 0.7;
const CONFIRM_FRAMES: usize = 3;
/// A run of one label is stable when it lasts at least the stable length
/// and drifts (least-squares slope times length) less than this.
const STABLE_DRIFT_ST: f32 = 0.5;
/// A glide starts and ends where the pitch is this close to the held note:
/// a share of the move, within limits.
const AT_NOTE_SHARE: f32 = 0.1;
const AT_NOTE_MIN_ST: f32 = 0.06;
const AT_NOTE_ST: f32 = 0.25;
/// Moves between stable pitches whose middle half takes at most this many
/// frames are steps (legato or picked); longer ones are glides (bends or
/// slides). Up to [`BROKEN_FRAMES`] still make a step when the voicing
/// drops under [`BROKEN_VPROB`] on the way.
const QUICK_FRAMES: usize = 2;
const BROKEN_FRAMES: usize = 4;
const BROKEN_VPROB: f32 = 0.5;
/// Onset score that counts as an attack for a step or glide (well under
/// the score that splits segments: legato is reported only when clear).
const WEAK_ONSET: f32 = 0.12;
/// Bends: at most this many semitones up, no faster than this (st/s), and
/// not a staircase.
const MAX_BEND_ST: f32 = 3.25;
const MAX_BEND_SPEED: f32 = 30.0;
/// Bends wider than this ("1½") are rare and slow; a faster glide that
/// wide is a slide.
const WIDE_BEND_ST: f32 = 2.6;
const WIDE_BEND_SPEED: f32 = 20.0;
/// Glides of at least this size are slides.
const MIN_SLIDE_ST: f32 = 1.5;
/// Staircase: semitone steps a glide pauses on (two frames within
/// [`STEP_ST`] of the step that move less than [`FLAT_ST`]).
const STAIR_STEPS: usize = 2;
const STEP_ST: f32 = 0.15;
const FLAT_ST: f32 = 0.1;
/// A segment without a stable stretch is still one note when the middle
/// half of its pitches spans at most this.
const FALLBACK_SPREAD_ST: f32 = 0.5;
/// Vibrato: moving-average length for the trend (frames), sign hysteresis
/// (st), half-period range (s), regularity and size (peak to peak, st).
const VIB_TREND: usize = 15;
const VIB_HYST: f32 = 0.03;
const VIB_HALF_MIN_S: f64 = 0.055;
const VIB_HALF_MAX_S: f64 = 0.14;
const VIB_IRREGULAR: f64 = 0.4;
const VIB_MIN_PP_ST: f32 = 0.25;
const VIB_MIN_S: f64 = 0.25;

/// Shortest note: a 32nd at the song's tempo, or 40 ms without beats.
pub(crate) fn min_note_len(beats: &[f64]) -> f64 {
    let mut d: Vec<f64> = beats
        .windows(2)
        .map(|w| w[1] - w[0])
        .filter(|x| x.is_finite() && (0.15..=2.0).contains(x))
        .collect();
    if d.len() < 2 {
        return 0.04;
    }
    d.sort_by(f64::total_cmp);
    (d[d.len() / 2] / 8.0).clamp(0.03, 0.15)
}

pub(crate) struct Context<'a> {
    pub track: &'a Track,
    pub onset: &'a [f32],
    pub picks: &'a [usize],
    pub kind: StemKind,
    pub offset_cents: f64,
    pub min_len: f64,
}

/// A note while it is being built: frames `[start, end)`.
struct Draft {
    start: usize,
    end: usize,
    attack: Attack,
    /// Frames of the note's held pitch (before any bend).
    base: Vec<(usize, usize)>,
    /// Pitch of the held note (offset removed).
    pitch: f32,
    bend: Option<DraftBend>,
    /// Bent and back: the note takes no further bends.
    released: bool,
    /// Built without a stable stretch.
    fallback: bool,
}

struct DraftBend {
    start: usize,
    peak: usize,
    release: Option<usize>,
    semitones: f32,
    /// Stable run at the bend's peak, for vibrato.
    hold: Option<(usize, usize)>,
}

pub(crate) fn notes(cx: &Context) -> Vec<Note> {
    let track = cx.track;
    let n = track.len();
    let offset = if cx.offset_cents.is_finite() {
        cx.offset_cents
    } else {
        0.0
    };
    let shift = (offset / 100.0) as f32;
    let p: Vec<f32> = track.midi.iter().map(|&m| m - shift).collect();
    let mut is_pick = vec![false; n];
    for &t in cx.picks {
        is_pick[t] = true;
    }
    let min_frames = (cx.min_len / HOP_S).round().max(1.0) as usize;
    let stable_frames = ((0.75 * cx.min_len).clamp(0.03, 0.08) / HOP_S).round() as usize;

    // Voiced stretches, bridging short gaps without an onset.
    let mut stretches: Vec<(usize, usize)> = Vec::new();
    let mut t = 0;
    while t < n {
        if !track.voiced(t) {
            t += 1;
            continue;
        }
        let a = t;
        let mut b = t + 1;
        loop {
            while b < n && track.voiced(b) {
                b += 1;
            }
            let mut g = b;
            while g < n && !track.voiced(g) && g - b < GAP_FRAMES + 1 {
                g += 1;
            }
            if g < n && track.voiced(g) && g - b <= GAP_FRAMES && !is_pick[b..=g].iter().any(|&x| x)
            {
                b = g;
                continue;
            }
            break;
        }
        stretches.push((a, b));
        t = b;
    }

    // Attack segments: stretches cut at onsets.
    let mut segments: Vec<(usize, usize)> = Vec::new();
    let mut prev_end = 0;
    for &(a, b) in &stretches {
        let lo = a.saturating_sub(ONSET_BEFORE).max(prev_end);
        let start_pick = (lo..(a + ONSET_AFTER).min(b)).find(|&i| is_pick[i]);
        let mut s = start_pick.unwrap_or(a);
        for &i in cx.picks.iter().filter(|&&i| i > a && i < b) {
            if i >= s + MIN_SPLIT {
                segments.push((s, i));
                s = i;
            }
        }
        segments.push((s, b));
        prev_end = b;
    }

    let mut out = Vec::new();
    for &(a, b) in &segments {
        if b - a < min_frames {
            continue;
        }
        let drafts = segment_notes(cx, &p, a, b, stable_frames);
        let mut drafts = merge_short(drafts, min_frames);
        drafts.retain(|d| d.end - d.start >= min_frames);
        for d in drafts {
            out.push(finish(cx, &p, &d));
        }
    }
    out
}

/// Median of the voiced values of `v` in `[a, b)`.
fn median(v: &[f32], a: usize, b: usize) -> f32 {
    let mut x: Vec<f32> = v[a..b].iter().copied().filter(|x| !x.is_nan()).collect();
    if x.is_empty() {
        return f32::NAN;
    }
    x.sort_unstable_by(f32::total_cmp);
    x[x.len() / 2]
}

/// Run of one note label.
#[derive(Clone, Copy)]
struct Run {
    start: usize,
    end: usize,
    stable: bool,
    pitch: f32,
}

fn segment_notes(cx: &Context, p: &[f32], a: usize, b: usize, stable_frames: usize) -> Vec<Draft> {
    // Smoothed pitch.
    let sm: Vec<f32> = (a..b)
        .map(|t| {
            let lo = t.saturating_sub(SMOOTH_RADIUS).max(a);
            let hi = (t + SMOOTH_RADIUS + 1).min(b);
            let k = p[lo..hi].iter().filter(|x| !x.is_nan()).count();
            if k >= 3 {
                median(p, lo, hi)
            } else {
                f32::NAN
            }
        })
        .collect();
    let at = |t: usize| sm[t - a];

    // Labels with hysteresis.
    let mut label: Vec<Option<i32>> = vec![None; b - a];
    let mut cur: Option<i32> = None;
    for t in a..b {
        let v = at(t);
        if !v.is_nan() {
            let r = v.round();
            let near = (v - r).abs() < ENTER_ST;
            let confirmed = || {
                (t..b)
                    .map(at)
                    .filter(|x| !x.is_nan())
                    .take(CONFIRM_FRAMES)
                    .filter(|&x| (x - r).abs() < ENTER_ST)
                    .count()
                    == CONFIRM_FRAMES
            };
            match cur {
                None if near && confirmed() => cur = Some(r as i32),
                Some(c) if (v - c as f32).abs() > LEAVE_ST && near && confirmed() => {
                    cur = Some(r as i32)
                }
                _ => {}
            }
        }
        label[t - a] = cur;
    }

    // Runs of one label; stable ones are held pitches.
    let mut runs: Vec<Run> = Vec::new();
    let mut s = a;
    for t in a + 1..=b {
        if t == b || label[t - a] != label[s - a] {
            if let Some(l) = label[s - a] {
                // The run's core: without the frames at either end that
                // are still on their way to or from the note.
                let off = |i: usize| at(i).is_nan() || (at(i) - l as f32).abs() >= ENTER_ST;
                let mut cs = s;
                let mut ce = t;
                while cs < ce && off(cs) {
                    cs += 1;
                }
                while ce > cs && off(ce - 1) {
                    ce -= 1;
                }
                if ce > cs {
                    let stable =
                        ce - cs >= stable_frames && drift(&sm[cs - a..ce - a]) < STABLE_DRIFT_ST;
                    runs.push(Run {
                        start: cs,
                        end: ce,
                        stable,
                        pitch: median(p, cs, ce),
                    });
                }
            }
            s = t;
        }
    }
    let stable: Vec<Run> = runs
        .into_iter()
        .filter(|r| r.stable && !r.pitch.is_nan())
        .collect();

    if stable.is_empty() {
        return fallback(p, a, b).into_iter().collect();
    }

    let mut drafts: Vec<Draft> = Vec::new();
    let new_draft = |start: usize, attack: Attack, run: &Run| Draft {
        start,
        end: run.end,
        attack,
        base: vec![(run.start, run.end)],
        pitch: run.pitch,
        bend: None,
        released: false,
        fallback: false,
    };
    drafts.push(new_draft(a, Attack::Picked, &stable[0]));
    for w in stable.windows(2) {
        let (r0, r1) = (w[0], w[1]);
        let cur = drafts.last_mut().expect("one draft");
        let base = cur.pitch;
        let delta = r1.pitch - r0.pitch;
        // Where the glide leaves r0's pitch and reaches r1's.
        let near = (AT_NOTE_SHARE * delta.abs()).clamp(AT_NOTE_MIN_ST, AT_NOTE_ST);
        let gs = (r0.start..r0.end)
            .rev()
            .find(|&t| !at(t).is_nan() && (at(t) - r0.pitch).abs() <= near)
            .map_or(r0.end, |t| t + 1);
        let ge = (gs..r1.end)
            .find(|&t| !at(t).is_nan() && (at(t) - r1.pitch).abs() <= near)
            .unwrap_or(r1.start)
            .max(gs);
        // How long the move takes: the frames in its middle half, doubled,
        // so a slow drift before a quick step doesn't count. A step breaks
        // the periodicity (the window holds two pitches); a glide keeps it.
        let (lo_mid, hi_mid) = {
            let (x, y) = (r0.pitch + 0.25 * delta, r0.pitch + 0.75 * delta);
            (x.min(y), x.max(y))
        };
        let middle: Vec<usize> = (gs..ge)
            .filter(|&t| (lo_mid..=hi_mid).contains(&at(t)))
            .collect();
        let broken = middle.iter().any(|&t| cx.track.vprob[t] < BROKEN_VPROB);
        let quick = middle.len() <= QUICK_FRAMES || (broken && middle.len() <= BROKEN_FRAMES);
        let glide = 2 * middle.len();
        let mid_frame = match (middle.first(), middle.last()) {
            (Some(&f), Some(&l)) => (f + l).div_ceil(2),
            _ => (gs + ge) / 2,
        };
        let lo = gs.saturating_sub(2).max(a);
        let hi = (ge + 3).min(b);
        let (onset_at, onset) = (lo..hi)
            .map(|t| (t, cx.onset[t]))
            .fold((lo, 0.0f32), |m, x| if x.1 > m.1 { x } else { m });
        let attacked = onset >= WEAK_ONSET;

        // Inside a bend: release, bend further, or leave the note.
        if let Some(bend) = cur.bend.as_mut().filter(|_| !cur.released) {
            let from_base = r1.pitch - base;
            if from_base.abs() < 0.5 && !attacked {
                bend.release = Some(ge);
                cur.released = true;
                cur.base.push((r1.start, r1.end));
                cur.end = r1.end;
                continue;
            }
            if from_base > bend.semitones + 0.4 && from_base <= MAX_BEND_ST && !attacked && !quick {
                bend.peak = ge;
                bend.semitones = from_base;
                bend.hold = Some((r1.start, r1.end));
                cur.end = r1.end;
                continue;
            }
            if from_base > 0.5 && from_base <= bend.semitones + 0.4 && !attacked {
                // Held at or under the peak: still the bent note.
                cur.end = r1.end;
                continue;
            }
        }

        if delta.abs() < 0.5 {
            // Back to the same note after an excursion.
            if attacked {
                cur.end = onset_at;
                drafts.push(new_draft(onset_at, Attack::Picked, &r1));
            } else {
                cur.base.push((r1.start, r1.end));
                cur.end = r1.end;
            }
            continue;
        }
        let speed = delta.abs() / (glide.max(1) as f64 * HOP_S) as f32;
        let stair = staircase(&p[gs..ge], r0.pitch);
        if quick {
            let (at_frame, attack) = if attacked {
                (onset_at, Attack::Picked)
            } else {
                (mid_frame, Attack::Legato)
            };
            cur.end = at_frame;
            drafts.push(new_draft(at_frame, attack, &r1));
        } else if cx.kind.bends()
            && !attacked
            && cur.bend.is_none()
            && (r0.pitch - base).abs() < 0.5
            && delta > 0.5
            && delta <= MAX_BEND_ST
            && speed <= MAX_BEND_SPEED
            && (delta <= WIDE_BEND_ST || speed <= WIDE_BEND_SPEED)
            && !stair
        {
            cur.bend = Some(DraftBend {
                start: gs,
                peak: ge,
                release: None,
                semitones: r1.pitch - base,
                hold: Some((r1.start, r1.end)),
            });
            cur.end = r1.end;
        } else if attacked {
            cur.end = onset_at;
            drafts.push(new_draft(onset_at, Attack::Picked, &r1));
        } else if delta.abs() >= MIN_SLIDE_ST {
            cur.end = ge;
            drafts.push(new_draft(ge, Attack::Slide, &r1));
        } else {
            let at_frame = mid_frame;
            cur.end = at_frame;
            drafts.push(new_draft(at_frame, Attack::Legato, &r1));
        }
    }
    if let Some(last) = drafts.last_mut() {
        last.end = b;
    }
    // A bend that returns without holding at its peak: find the top.
    for d in &mut drafts {
        if d.bend.is_none() && cx.kind.bends() {
            quick_bend(d, &sm, a);
        }
    }
    drafts
}

/// A bend up and back inside a note's held stretches without a stable
/// peak (a quick "bend and release"): an excursion above the held pitch
/// of 0.5 to [`MAX_BEND_ST`] semitones that rises over more than
/// [`QUICK_FRAMES`] frames.
fn quick_bend(d: &mut Draft, sm: &[f32], a: usize) {
    if d.base.len() < 2 {
        return;
    }
    let (_, e0) = d.base[0];
    let (s1, _) = d.base[1];
    if s1 <= e0 {
        return;
    }
    let seg = &sm[e0 - a..s1 - a];
    let Some((k, &top)) = seg
        .iter()
        .enumerate()
        .filter(|(_, x)| !x.is_nan())
        .max_by(|x, y| x.1.total_cmp(y.1))
    else {
        return;
    };
    let size = top - d.pitch;
    if size < 0.5 || size > MAX_BEND_ST || k < 2 * QUICK_FRAMES {
        return;
    }
    d.bend = Some(DraftBend {
        start: e0,
        peak: e0 + k,
        release: Some(s1),
        semitones: size,
        hold: None,
    });
    d.released = true;
}

/// Least-squares slope of the run times its length, in semitones.
fn drift(v: &[f32]) -> f32 {
    let pts: Vec<(f64, f64)> = v
        .iter()
        .enumerate()
        .filter(|(_, x)| !x.is_nan())
        .map(|(i, &x)| (i as f64, f64::from(x)))
        .collect();
    if pts.len() < 2 {
        return 0.0;
    }
    let n = pts.len() as f64;
    let mx = pts.iter().map(|p| p.0).sum::<f64>() / n;
    let my = pts.iter().map(|p| p.1).sum::<f64>() / n;
    let sxx: f64 = pts.iter().map(|p| (p.0 - mx).powi(2)).sum();
    let sxy: f64 = pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    if sxx <= 0.0 {
        return 0.0;
    }
    ((sxy / sxx) * v.len() as f64).abs() as f32
}

/// Whether a glide moves in semitone steps (a slide fret by fret): it
/// pauses on at least [`STAIR_STEPS`] semitones between its ends, each for
/// at least two frames.
fn staircase(glide: &[f32], from: f32) -> bool {
    let mut steps: Vec<i32> = Vec::new();
    for w in glide.windows(2) {
        if w[0].is_nan() || w[1].is_nan() || (w[1] - w[0]).abs() >= FLAT_ST {
            continue;
        }
        let d = w[1] - from;
        let k = d.round();
        if k != 0.0 && (d - k).abs() <= STEP_ST && !steps.contains(&(k as i32)) {
            steps.push(k as i32);
        }
    }
    steps.len() >= STAIR_STEPS
}

/// One note for a segment without a stable stretch, when its pitch is
/// steady enough.
fn fallback(p: &[f32], a: usize, b: usize) -> Option<Draft> {
    let skip = ((b - a) / 4).min(3);
    let mut v: Vec<f32> = p[a + skip..b]
        .iter()
        .copied()
        .filter(|x| !x.is_nan())
        .collect();
    if v.len() < 3 {
        return None;
    }
    v.sort_unstable_by(f32::total_cmp);
    let q = |f: f64| v[((v.len() - 1) as f64 * f).round() as usize];
    if q(0.75) - q(0.25) > FALLBACK_SPREAD_ST {
        return None;
    }
    Some(Draft {
        start: a,
        end: b,
        attack: Attack::Picked,
        base: vec![(a + skip, b)],
        pitch: q(0.5),
        bend: None,
        released: false,
        fallback: true,
    })
}

/// Merges notes shorter than `min_frames` into a neighbor in the same
/// segment: into the previous note, or the first into the next.
fn merge_short(drafts: Vec<Draft>, min_frames: usize) -> Vec<Draft> {
    let mut out: Vec<Draft> = Vec::new();
    let mut pending: Option<Draft> = None;
    for mut d in drafts {
        if let Some(p) = pending.take() {
            // A short first note joins the next one, keeping its attack.
            d.start = p.start;
            d.attack = p.attack;
        }
        if d.end - d.start >= min_frames {
            out.push(d);
        } else if let Some(prev) = out.last_mut() {
            prev.end = d.end;
        } else {
            pending = Some(d);
        }
    }
    if let Some(p) = pending {
        out.push(p);
    }
    out
}

fn finish(cx: &Context, p: &[f32], d: &Draft) -> Note {
    let track = cx.track;
    let t = |f: usize| f as f64 * HOP_S;
    let mut held: Vec<f32> = d
        .base
        .iter()
        .flat_map(|&(s, e)| p[s..e].iter().copied())
        .filter(|x| !x.is_nan())
        .collect();
    held.sort_unstable_by(f32::total_cmp);
    let pitch = if held.is_empty() {
        d.pitch
    } else {
        held[held.len() / 2]
    };
    let midi = pitch.round().clamp(0.0, 127.0);
    let cents = ((pitch - midi) * 100.0).clamp(-50.0, 50.0);

    // Confidence: voicing, steadiness of the held pitch, and level.
    let voiced: Vec<f32> = (d.start..d.end).map(|i| track.vprob[i]).collect();
    let vmean = voiced.iter().sum::<f32>() / voiced.len().max(1) as f32;
    let mad = if held.is_empty() {
        0.5
    } else {
        let mut dev: Vec<f32> = held.iter().map(|x| (x - pitch).abs()).collect();
        dev.sort_unstable_by(f32::total_cmp);
        dev[dev.len() / 2]
    };
    let steady = 1.0 - ((mad - 0.05) / 0.4).clamp(0.0, 0.6);
    let level = (d.start..d.end)
        .map(|i| track.level_db[i])
        .fold(f32::MIN, f32::max);
    let loud = 1.0 - ((track.reference_db - 20.0 - level) / 40.0).clamp(0.0, 0.5);
    let mut confidence = (vmean * steady * loud).clamp(0.0, 1.0);
    if d.fallback {
        confidence *= 0.6;
    }

    let mut vib_runs: Vec<(usize, usize)> = d.base.clone();
    if let Some(h) = d.bend.as_ref().and_then(|b| b.hold) {
        vib_runs.push(h);
    }
    let vibrato = vib_runs
        .iter()
        .filter(|(s, e)| (e - s) as f64 * HOP_S >= VIB_MIN_S)
        .any(|&(s, e)| has_vibrato(&p[s..e]));

    Note {
        start_s: t(d.start),
        end_s: t(d.end),
        midi: midi as u8,
        cents,
        confidence,
        attack: d.attack,
        bend: d.bend.as_ref().map(|b| Bend {
            start_s: t(b.start),
            peak_s: t(b.peak),
            release_s: b.release.map(t),
            semitones: b.semitones,
        }),
        vibrato,
    }
}

/// A regular 4 to 8 Hz oscillation of the pitch around its trend.
fn has_vibrato(v: &[f32]) -> bool {
    // Fill unvoiced frames from the previous value.
    let mut x: Vec<f32> = Vec::with_capacity(v.len());
    let mut last = v.iter().copied().find(|y| !y.is_nan());
    for &y in v {
        if !y.is_nan() {
            last = Some(y);
        }
        match last {
            Some(l) => x.push(l),
            None => return false,
        }
    }
    let n = x.len();
    if n < VIB_TREND * 2 {
        return false;
    }
    let half = VIB_TREND / 2;
    let r: Vec<f32> = (half..n - half)
        .map(|i| {
            let mean = x[i - half..=i + half].iter().sum::<f32>() / VIB_TREND as f32;
            x[i] - mean
        })
        .collect();
    let mut sign = 0i8;
    let mut crossings: Vec<usize> = Vec::new();
    let mut extremes: Vec<f32> = Vec::new();
    let mut extreme = 0.0f32;
    for (i, &y) in r.iter().enumerate() {
        let s = if y > VIB_HYST {
            1
        } else if y < -VIB_HYST {
            -1
        } else {
            0
        };
        if s != 0 && s != sign {
            if sign != 0 {
                crossings.push(i);
                extremes.push(extreme);
            }
            sign = s;
            extreme = 0.0;
        }
        extreme = extreme.max(y.abs());
    }
    if crossings.len() < 4 {
        return false;
    }
    let halves: Vec<f64> = crossings
        .windows(2)
        .map(|w| (w[1] - w[0]) as f64 * HOP_S)
        .collect();
    let mean = halves.iter().sum::<f64>() / halves.len() as f64;
    let sd = (halves.iter().map(|h| (h - mean).powi(2)).sum::<f64>() / halves.len() as f64).sqrt();
    let size = 2.0 * extremes[1..].iter().sum::<f32>() / (extremes.len() - 1).max(1) as f32;
    (VIB_HALF_MIN_S..=VIB_HALF_MAX_S).contains(&mean)
        && sd <= VIB_IRREGULAR * mean
        && size >= VIB_MIN_PP_ST
}

/// Drops notes mostly inside a chordal stretch and trims the rest to it.
pub(crate) fn remove_chordal(notes: &mut Vec<Note>, chordal: &[(f64, f64)], min_len: f64) {
    notes.retain_mut(|n| {
        for &(s, e) in chordal {
            if e <= n.start_s || s >= n.end_s {
                continue;
            }
            let overlap = e.min(n.end_s) - s.max(n.start_s);
            if overlap >= 0.5 * (n.end_s - n.start_s) {
                return false;
            }
            // Keep the side outside the stretch (the longer one when the
            // stretch lies inside the note).
            let keep_start = s > n.start_s && (e >= n.end_s || s - n.start_s >= n.end_s - e);
            if keep_start {
                n.end_s = s;
            } else {
                n.start_s = e;
            }
        }
        n.end_s - n.start_s >= min_len - 1e-9
    });
}
