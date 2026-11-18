// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! The lead line, bends and chordal stretches from Basic Pitch's note
//! events (`basic_pitch.rs`): one note at a time, the line a player would
//! hear as the melody over whatever else the stem plays.

use std::collections::VecDeque;

use crate::basic_pitch::{self as bp, Activations, Event};
use crate::{segment, Attack, Bend, Heard, Note, NoteAnalysis, StemKind};

/// Shortest note event kept from the model (s); a lead note trimmed by the
/// next one may end up shorter, down to [`MIN_LEAD_S`]. Also the shortest
/// part of a lead note's event outside the note reported as heard.
const MIN_EVENT_S: f64 = 0.07;
const MIN_LEAD_S: f64 = 0.03;
/// Notes whose loudest frame is more than this under the stem's reference
/// level, or under the floor (dBFS), are dropped: the model transcribes
/// faint bleed as readily as the stem's own part.
const GATE_BELOW_REF_DB: f32 = 40.0;
const GATE_FLOOR_DB: f32 = -60.0;
/// A note takes part in the lead line from this mean activation up.
const LEAD_MIN_AMP: f32 = 0.3;
/// The model's onset activation from which a note counts as attacked.
const PICK_ONSET: f32 = 0.5;
/// Notes starting within this of each other start together (s); three or
/// more are a strum.
const TOGETHER_S: f64 = 0.05;
const STRUM_NOTES: usize = 3;
/// Inside a group of notes starting together, a higher note is ignored
/// when its activation is under this share of the stronger note (an
/// overtone the model also heard).
const GHOST_SHARE: f32 = 0.6;
/// The lead's register: the median of the last few lead notes, forgotten
/// this long after the lead's last note (s).
const REGISTER_NOTES: usize = 6;
const REGISTER_KEEP_S: f64 = 4.0;
/// Notes this far under the register (semitones) join the lead only when
/// they are clearly the lead: strong and fairly long, long, or the only
/// candidate within [`ALONE_S`].
const FLOOR_ST: f32 = 7.0;
/// Notes this far over the register (semitones) need the same: an
/// overtone the model took for a note (an octave and a fifth or more up).
const CEILING_ST: f32 = 19.0;
/// Strum tops need to be at most this far under the register.
const STRUM_FLOOR_ST: f32 = 3.0;
const CLEAR_AMP: f32 = 0.6;
const CLEAR_S: f64 = 0.25;
const LONG_S: f64 = 0.45;
const ALONE_S: f64 = 0.5;
/// A run: at least [`RUN_NOTES`] single notes (not strums), each starting
/// within [`RUN_GAP_S`] of the previous one's end and within [`RUN_STEP_ST`]
/// of its pitch. A run is a lick, so it joins the lead line wherever it
/// sits (a solo dropping to the low strings).
const RUN_NOTES: usize = 3;
const RUN_GAP_S: f64 = 0.15;
const RUN_STEP_ST: f32 = 5.0;
// A run outside the register does not move it (see `select_lead`).
/// A new lead note within this of the previous one's end (s) continues it
/// (legato or slide when not attacked); after a longer gap it is picked.
const LEGATO_GAP_S: f64 = 0.06;
/// Contour following: a peak at least this strong within this many contour
/// bins (thirds of a semitone) of the previous frame's pitch.
const FOLLOW_FLOOR: f32 = 0.1;
const FOLLOW_STEP_BINS: f32 = 2.0;
/// Bends: size range (semitones) and frames the target must hold.
const BEND_MIN_ST: f32 = 0.5;
const BEND_MAX_ST: f32 = 3.5;
const BEND_HOLD: usize = 4;
/// A bend glides no faster than this (semitones per second), and its
/// target may carry an onset up to this (the model often hears one where
/// the pitch arrives).
const BEND_MAX_SPEED: f64 = 30.0;
const BEND_ONSET_MAX: f32 = 0.8;
/// Slides: a glide of at least this many semitones into a new note.
const SLIDE_MIN_ST: f32 = 1.0;
/// Chordal stretches: frames where at least `STRUM_NOTES` notes sound make
/// up this share of the sounding frames in a window this long, for at least
/// this long, with gaps up to this joined (s); they are reported (and
/// their lead notes dropped) when lead notes that are not a strum's top
/// cover less than `LEAD_SHARE` of them.
const CHORD_WINDOW_S: f64 = 0.3;
const CHORD_SHARE: f64 = 0.6;
const CHORD_MIN_S: f64 = 0.4;
const CHORD_GAP_S: f64 = 0.25;
const LEAD_SHARE: f64 = 0.25;

/// A note event with its pitch and, once merged, its bend.
#[derive(Debug, Clone, Copy)]
struct Cand {
    ev: Event,
    /// Fractional MIDI pitch from the contour (concert, before the offset).
    pitch: f32,
    /// Bend in frames: start, peak, release, and size in semitones.
    bend: Option<(usize, usize, Option<usize>, f32)>,
    /// Notes starting together with this one, itself included.
    group: usize,
    /// The highest (non-ghost) note of its group.
    top: bool,
    alive: bool,
}

/// Turns stitched activations into the lead line and chordal stretches.
pub(crate) fn transcribe(a: &Activations, kind: StemKind, offset_cents: f64) -> NoteAnalysis {
    let min_frames = (MIN_EVENT_S / bp::FRAME_S).round() as usize;
    let events = bp::note_events(a, min_frames);
    // The stem's range, as for the pitch tracker (a quarter tone of slack).
    let (lo_hz, hi_hz) = kind.range_hz();
    let hz_midi = |hz: f64| (69.0 + 12.0 * (hz / 440.0).log2()) as f32;
    let (lo, hi) = (hz_midi(lo_hz) - 0.5, hz_midi(hi_hz) + 0.5);
    let mut cands: Vec<Cand> = events
        .into_iter()
        .map(|ev| Cand {
            pitch: event_pitch(a, &ev),
            ev,
            bend: None,
            group: 1,
            top: true,
            alive: true,
        })
        .collect();
    // Level gate: the model hears notes in faint bleed, so events whose
    // loudest frame is under the gate are dropped.
    let gate = level_gate(&a.level_db);
    cands.retain(|c| {
        (lo..=hi).contains(&c.pitch)
            && a.level_db[c.ev.start..c.ev.end.min(a.n)]
                .iter()
                .any(|&l| l >= gate)
    });
    if kind.bends() {
        merge_bends(a, &mut cands);
    }
    cands.retain(|c| c.alive);
    cands.sort_by_key(|c| (c.ev.start, c.ev.bin));
    groups(a, &mut cands);
    let picked = select_lead(a, &cands);
    let built = build_notes(a, &cands, &picked, offset_cents);
    let chordal = chordal(a, &cands, &built);
    let mut notes: Vec<Note> = built.iter().map(|&(_, n)| n).collect();
    segment::remove_chordal(&mut notes, &chordal, MIN_LEAD_S);
    let others = others(a, &cands, &built, &notes, offset_cents);
    NoteAnalysis {
        notes,
        chordal,
        others,
    }
}

/// Every note the model heard outside the lead line, sorted by start: a
/// candidate that is not a lead note whole, and of a lead note's candidate
/// the parts outside the lead note (it rang on under the next lead note, or
/// a chordal stretch trimmed it) from [`MIN_EVENT_S`] up. `built` are the
/// lead notes before chordal stretches removed or trimmed them into `notes`.
fn others(
    a: &Activations,
    cands: &[Cand],
    built: &[(usize, Note)],
    notes: &[Note],
    offset_cents: f64,
) -> Vec<Heard> {
    let offset = (offset_cents / 100.0) as f32;
    // The lead note each candidate became, if it survived.
    let mut kept: Vec<Option<(f64, f64)>> = vec![None; cands.len()];
    for &(i, n0) in built {
        let k = notes.partition_point(|n| n.start_s < n0.start_s - 1e-9);
        kept[i] = notes
            .get(k)
            .filter(|n| n.midi == n0.midi && n.end_s <= n0.end_s + 1e-9)
            .map(|n| (n.start_s, n.end_s));
    }
    let mut out = Vec::new();
    for (c, lead) in cands.iter().zip(&kept) {
        let (s, e) = (a.time_of(c.ev.start), a.time_of(c.ev.end));
        let parts = match *lead {
            Some((ls, le)) => [(s, ls), (le, e)],
            None => [(s, e), (e, e)],
        };
        let midi = (c.pitch - offset).round().clamp(0.0, 127.0) as u8;
        for (start_s, end_s) in parts {
            if end_s - start_s >= MIN_EVENT_S - 1e-9 {
                out.push(Heard {
                    start_s,
                    end_s,
                    midi,
                    confidence: c.ev.amp.clamp(0.0, 1.0),
                });
            }
        }
    }
    out.sort_by(|x, y| x.start_s.total_cmp(&y.start_s).then(x.midi.cmp(&y.midi)));
    out
}

/// The level gate (dBFS): [`GATE_BELOW_REF_DB`] under the stem's reference
/// level (the 99th percentile of its frame levels), at least
/// [`GATE_FLOOR_DB`].
fn level_gate(level_db: &[f32]) -> f32 {
    let mut v: Vec<f32> = level_db.iter().copied().filter(|&l| l > -119.0).collect();
    if v.is_empty() {
        return f32::INFINITY;
    }
    v.sort_by(f32::total_cmp);
    let reference = v[((v.len() - 1) as f64 * 0.99).round() as usize];
    (reference - GATE_BELOW_REF_DB).max(GATE_FLOOR_DB)
}

fn median(v: &mut [f32]) -> Option<f32> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    Some(v[v.len() / 2])
}

/// The note's pitch: the median contour peak near its bin over its first
/// half (before any bend).
fn event_pitch(a: &Activations, ev: &Event) -> f32 {
    let midi = ev.midi() as f32;
    let near = bp::contour_bin(midi);
    let half = ((ev.end - ev.start) / 2).max(1);
    let mut v: Vec<f32> = (ev.start..ev.start + half)
        .filter_map(|t| bp::contour_peak(a, t, near, 2.0, FOLLOW_FLOOR / 2.0))
        .map(|(b, _)| bp::contour_midi(b))
        .collect();
    median(&mut v).unwrap_or(midi)
}

/// Follows the contour from pitch `from` (MIDI) over frames `t0..t1`: per
/// frame the peak nearest the last pitch, at most [`FOLLOW_STEP_BINS`] away.
/// A frame without one keeps the last pitch; two in a row end the track.
fn follow(a: &Activations, t0: usize, t1: usize, from: f32) -> Vec<f32> {
    let mut bin = bp::contour_bin(from);
    let mut out = Vec::new();
    let mut misses = 0;
    for t in t0..t1.min(a.n) {
        match bp::contour_peak(a, t, bin, FOLLOW_STEP_BINS, FOLLOW_FLOOR) {
            Some((b, _)) => {
                bin = b;
                misses = 0;
            }
            None => {
                misses += 1;
                if misses >= 2 {
                    break;
                }
            }
        }
        out.push(bp::contour_midi(bin));
    }
    out
}

/// A glide of the contour from one pitch to another.
#[derive(Debug, Clone, Copy)]
struct Glide {
    /// Last frame at the start pitch.
    leave: usize,
    /// First frame at 80 % of the way.
    reach: usize,
    /// Semitones per second between the two.
    speed: f64,
    /// Median pitch (MIDI) over the frames from `reach` on.
    held: f32,
}

/// Whether the contour glides from `from` to `to` (MIDI) over frames
/// `t0..t1`, arriving within a third of a semitone of `to`.
fn glide(a: &Activations, t0: usize, t1: usize, from: f32, to: f32) -> Option<Glide> {
    let track = follow(a, t0, t1, from);
    let dir = if to > from { 1.0 } else { -1.0 };
    let rel: Vec<f32> = track.iter().map(|p| (p - from) * dir).collect();
    let size = (to - from) * dir;
    let near = rel.iter().position(|&r| (r - size).abs() <= 0.34)?;
    let peak = rel[near..(near + 6).min(rel.len())]
        .iter()
        .copied()
        .fold(f32::MIN, f32::max);
    let reach = rel.iter().position(|&r| r >= 0.8 * peak)?;
    let leave = rel[..reach].iter().rposition(|&r| r <= 0.25)?;
    let dt = a.time_of(t0 + reach) - a.time_of(t0 + leave);
    let speed = f64::from(rel[reach] - rel[leave]) / dt.max(1e-6);
    let mut tail: Vec<f32> = track[reach..].to_vec();
    Some(Glide {
        leave: t0 + leave,
        reach: t0 + reach,
        speed,
        held: median(&mut tail).unwrap_or(to),
    })
}

/// Merges bends: a note reached by a glide up of [`BEND_MIN_ST`] to
/// [`BEND_MAX_ST`] from a note, no faster than [`BEND_MAX_SPEED`] and
/// without a strong onset of its own, becomes that note's bend; a later
/// glide back to the held pitch is its release. Bends the model kept on
/// one note's bin are found on its contour.
fn merge_bends(a: &Activations, cands: &mut [Cand]) {
    let order: Vec<usize> = {
        let mut o: Vec<usize> = (0..cands.len()).collect();
        o.sort_by_key(|&i| (cands[i].ev.start, cands[i].ev.bin));
        o
    };
    for &i in &order {
        if !cands[i].alive {
            continue;
        }
        let base = cands[i].pitch;
        let target = order.iter().copied().find(|&j| {
            let (x, y) = (&cands[i], &cands[j]);
            let d = y.pitch - base;
            j != i
                && y.alive
                && y.ev.start >= x.ev.start + 3
                && y.ev.start <= x.ev.end + 4
                && y.ev.onset < BEND_ONSET_MAX
                && (BEND_MIN_ST..=BEND_MAX_ST).contains(&d)
        });
        let merged = target.and_then(|j| {
            let y = cands[j];
            let g = glide(
                a,
                cands[i].ev.start + 1,
                y.ev.end.min(y.ev.start + 12),
                base,
                y.pitch,
            )?;
            (g.speed <= BEND_MAX_SPEED).then_some((j, g))
        });
        if let Some((j, g)) = merged {
            let y = cands[j];
            cands[j].alive = false;
            let x = &mut cands[i];
            x.ev.end = x.ev.end.max(y.ev.end);
            x.bend = Some((g.leave, g.reach, None, g.held - base));
            // Release: a glide back to the held pitch into a note without
            // its own onset.
            let rel = order.iter().copied().find(|&k| {
                let z = &cands[k];
                k != i
                    && z.alive
                    && z.ev.start >= y.ev.start + 3
                    && z.ev.start <= cands[i].ev.end + 4
                    && z.ev.onset < BEND_ONSET_MAX
                    && (z.pitch - base).abs() <= 0.5
            });
            if let Some(k) = rel {
                let z = cands[k];
                if let Some(r) = glide(a, g.reach, z.ev.start + 8, g.held, base) {
                    cands[k].alive = false;
                    let x = &mut cands[i];
                    x.ev.end = x.ev.end.max(z.ev.end);
                    if let Some(b) = x.bend.as_mut() {
                        b.2 = Some(r.leave);
                    }
                }
            }
            continue;
        }
        let x = cands[i];
        let track = follow(a, x.ev.start, x.ev.end, base);
        cands[i].bend = bend_in(&track, base).map(|(s, p, r, size)| {
            (
                x.ev.start + s,
                x.ev.start + p,
                r.map(|r| x.ev.start + r),
                size,
            )
        });
    }
}

/// A bend inside one note's contour track: a rise of [`BEND_MIN_ST`] to
/// [`BEND_MAX_ST`] over the held pitch that holds [`BEND_HOLD`] frames.
/// Frames (relative to the track) of its start, peak and release, and size.
fn bend_in(track: &[f32], base: f32) -> Option<(usize, usize, Option<usize>, f32)> {
    let rel: Vec<f32> = track.iter().map(|p| p - base).collect();
    let n = rel.len();
    let first = (0..n.saturating_sub(BEND_HOLD))
        .find(|&i| rel[i..i + BEND_HOLD].iter().all(|&v| v >= BEND_MIN_ST))?;
    let mut start = first;
    while start > 0 && rel[start - 1] > 0.15 && rel[start - 1] < rel[start] {
        start -= 1;
    }
    let mut peak = first;
    let mut release = None;
    for i in first..n {
        if rel[i] > rel[peak] {
            peak = i;
        }
        if i > peak && rel[i] < 0.25 {
            release = Some(i);
            break;
        }
    }
    let size = rel[peak];
    (size <= BEND_MAX_ST).then_some((start, peak, release, size))
}

/// Fills `group` and `top`: a note is not its group's top when a higher
/// note at least [`GHOST_SHARE`] as strong starts with it.
fn groups(a: &Activations, cands: &mut [Cand]) {
    let starts: Vec<f64> = cands.iter().map(|c| a.time_of(c.ev.start)).collect();
    for i in 0..cands.len() {
        let mut group = 0;
        let mut top = true;
        for j in 0..cands.len() {
            if (starts[j] - starts[i]).abs() > TOGETHER_S {
                continue;
            }
            group += 1;
            if j != i
                && cands[j].pitch > cands[i].pitch
                && cands[j].ev.amp >= GHOST_SHARE * cands[i].ev.amp
            {
                top = false;
            }
        }
        cands[i].group = group;
        cands[i].top = top;
    }
}

/// Picks the lead line: candidates (indices into `cands`) in time order.
///
/// Only the highest note of notes starting together is a candidate. A
/// candidate under the lead's register by more than [`FLOOR_ST`] or over it
/// by more than [`CEILING_ST`] joins only when it is clearly the lead
/// (strong and fairly long, long, or the only candidate around), and never
/// as the top of a strum; a strum's top joins only within
/// [`STRUM_FLOOR_ST`] under the register and never cuts a sounding lead
/// note above it. Every other candidate takes over from the sounding lead
/// note. The register is forgotten after [`REGISTER_KEEP_S`] without a
/// lead note.
fn select_lead(a: &Activations, cands: &[Cand]) -> Vec<usize> {
    let mut lead: Vec<usize> = Vec::new();
    let mut recent: VecDeque<f32> = VecDeque::new();
    // End of the last lead note (frames), as trimmed by the next.
    let mut cur_end = 0usize;
    let in_run = runs(a, cands);
    for (i, c) in cands.iter().enumerate() {
        if !c.top || c.ev.amp < LEAD_MIN_AMP {
            continue;
        }
        let t = a.time_of(c.ev.start);
        let since = if lead.is_empty() {
            f64::INFINITY
        } else {
            t - a.time_of(cur_end)
        };
        if since > REGISTER_KEEP_S {
            recent.clear();
        }
        let register = median(&mut recent.iter().copied().collect::<Vec<_>>());
        let strum = c.group >= STRUM_NOTES;
        let dur = a.time_of(c.ev.end) - t;
        // The only candidate around: no other lead candidate starts within
        // ALONE_S.
        let alone = !cands.iter().enumerate().any(|(j, o)| {
            j != i
                && o.top
                && o.ev.amp >= LEAD_MIN_AMP
                && (a.time_of(o.ev.start) - t).abs() <= ALONE_S
        });
        let clear = !strum
            && ((c.ev.amp >= CLEAR_AMP && dur >= CLEAR_S) || dur >= LONG_S || alone || in_run[i]);
        if let Some(r) = register {
            let d = c.pitch - r;
            if (d < -FLOOR_ST || d > CEILING_ST) && !clear {
                continue;
            }
            if strum && d < -STRUM_FLOOR_ST {
                continue;
            }
        }
        let sounding = lead.last().is_some_and(|_| cur_end > c.ev.start);
        if sounding && strum {
            let prev = &cands[*lead.last().expect("sounding")];
            if c.pitch < prev.pitch + 0.5 {
                continue;
            }
        }
        lead.push(i);
        cur_end = c.ev.end;
        // A run below or above the register is a detour: the line returns
        // to its register after it.
        let detour = in_run[i] && register.is_some_and(|r| (c.pitch - r).abs() > FLOOR_ST);
        if !detour {
            recent.push_back(c.pitch);
            if recent.len() > REGISTER_NOTES {
                recent.pop_front();
            }
        }
    }
    lead
}

/// Which candidates belong to a run (see [`RUN_NOTES`]).
fn runs(a: &Activations, cands: &[Cand]) -> Vec<bool> {
    let single: Vec<usize> = (0..cands.len())
        .filter(|&i| {
            let c = &cands[i];
            c.top && c.group < STRUM_NOTES && c.ev.amp >= LEAD_MIN_AMP
        })
        .collect();
    let mut out = vec![false; cands.len()];
    let mut chain: Vec<usize> = Vec::new();
    let flush = |chain: &mut Vec<usize>, out: &mut Vec<bool>| {
        if chain.len() >= RUN_NOTES {
            for &k in chain.iter() {
                out[k] = true;
            }
        }
        chain.clear();
    };
    for &i in &single {
        if let Some(&p) = chain.last() {
            let (x, y) = (&cands[p], &cands[i]);
            let gap = a.time_of(y.ev.start) - a.time_of(x.ev.end);
            if gap > RUN_GAP_S || (y.pitch - x.pitch).abs() > RUN_STEP_ST {
                flush(&mut chain, &mut out);
            }
        }
        chain.push(i);
    }
    flush(&mut chain, &mut out);
    out
}

/// Lead notes from the picked candidates, with the candidate each came
/// from: each ends where the next starts.
fn build_notes(
    a: &Activations,
    cands: &[Cand],
    picked: &[usize],
    offset_cents: f64,
) -> Vec<(usize, Note)> {
    let offset = (offset_cents / 100.0) as f32;
    let mut notes = Vec::with_capacity(picked.len());
    let mut prev: Option<(&Cand, usize)> = None; // candidate and its trimmed end frame
    for (n, &i) in picked.iter().enumerate() {
        let c = &cands[i];
        let end = picked
            .get(n + 1)
            .map_or(c.ev.end, |&j| c.ev.end.min(cands[j].ev.start));
        let (start_s, end_s) = (a.time_of(c.ev.start), a.time_of(end));
        let attack = match prev {
            Some((p, p_end))
                if c.ev.onset < PICK_ONSET && start_s - a.time_of(p_end) <= LEGATO_GAP_S =>
            {
                // Where the previous note's pitch was when it ended.
                let from = match p.bend {
                    Some((_, peak, release, size))
                        if peak < p_end && release.is_none_or(|r| r >= p_end) =>
                    {
                        p.pitch + size
                    }
                    _ => p.pitch,
                };
                let t0 = p_end.saturating_sub(12).max(p.ev.start);
                let slid = (c.pitch - from).abs() >= SLIDE_MIN_ST
                    && glide(a, t0, c.ev.start + 8, from, c.pitch).is_some();
                if slid {
                    Attack::Slide
                } else {
                    Attack::Legato
                }
            }
            _ => Attack::Picked,
        };
        prev = Some((c, end));
        if end_s - start_s < MIN_LEAD_S {
            continue;
        }
        let pitch = c.pitch - offset;
        let midi = pitch.round().clamp(0.0, 127.0);
        let bend = c.bend.and_then(|(s, p, r, size)| {
            (s < end).then(|| Bend {
                start_s: a.time_of(s),
                peak_s: a.time_of(p.min(end)),
                release_s: r.filter(|&r| r < end).map(|r| a.time_of(r)),
                semitones: size,
            })
        });
        notes.push((
            i,
            Note {
                start_s,
                end_s,
                midi: midi as u8,
                cents: ((pitch - midi) * 100.0).clamp(-50.0, 50.0),
                confidence: c.ev.amp.clamp(0.0, 1.0),
                attack,
                bend,
                vibrato: false,
            },
        ));
    }
    notes
}

/// Stretches where the stem plays chords without a lead line of its own.
fn chordal(a: &Activations, cands: &[Cand], notes: &[(usize, Note)]) -> Vec<(f64, f64)> {
    let n = a.n;
    if n == 0 {
        return vec![];
    }
    let mut count = vec![0u16; n];
    for c in cands {
        for k in count.iter_mut().take(c.ev.end.min(n)).skip(c.ev.start) {
            *k += 1;
        }
    }
    let half = ((CHORD_WINDOW_S / bp::FRAME_S / 2.0).round() as usize).max(1);
    let mut chord = vec![false; n];
    for (t, ch) in chord.iter_mut().enumerate() {
        let w = &count[t.saturating_sub(half)..(t + half + 1).min(n)];
        let sounding = w.iter().filter(|&&k| k > 0).count();
        let poly = w.iter().filter(|&&k| usize::from(k) >= STRUM_NOTES).count();
        *ch = count[t] > 0 && sounding > 0 && poly as f64 >= CHORD_SHARE * sounding as f64;
    }
    // Runs of chordal frames, gaps joined.
    let mut runs: Vec<(f64, f64)> = Vec::new();
    let mut t = 0;
    while t < n {
        if !chord[t] {
            t += 1;
            continue;
        }
        let s = t;
        while t < n && chord[t] {
            t += 1;
        }
        let (s_s, e_s) = (a.time_of(s), a.time_of(t));
        match runs.last_mut() {
            Some(last) if s_s - last.1 <= CHORD_GAP_S => last.1 = e_s,
            _ => runs.push((s_s, e_s)),
        }
    }
    runs.retain(|(s, e)| e - s >= CHORD_MIN_S);
    // Lead notes of their own (not a strum's top) in each stretch.
    let own: Vec<(f64, f64)> = notes
        .iter()
        .filter(|(i, _)| cands[*i].group < STRUM_NOTES)
        .map(|(_, n)| (n.start_s, n.end_s))
        .collect();
    runs.retain(|&(s, e)| {
        let covered: f64 = own
            .iter()
            .map(|&(ns, ne)| (ne.min(e) - ns.max(s)).max(0.0))
            .sum();
        covered < LEAD_SHARE * (e - s)
    });
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::basic_pitch::{N_CONTOUR, N_NOTES};

    const K: usize = bp::MIDI_OFFSET;

    fn act(n: usize) -> Activations {
        Activations {
            n,
            note: vec![0.0; n * N_NOTES],
            onset: vec![0.0; n * N_NOTES],
            contour: vec![0.0; n * N_CONTOUR],
            time: (0..n).map(|f| f as f64 * bp::FRAME_S).collect(),
            level_db: vec![-20.0; n],
        }
    }

    /// Contour peak at a (fractional) MIDI pitch in frame `t`.
    fn pitch_at(a: &mut Activations, t: usize, midi: f32) {
        let b = bp::contour_bin(midi);
        let i = b.floor() as usize;
        let f = b - i as f32;
        let row = &mut a.contour[t * N_CONTOUR..(t + 1) * N_CONTOUR];
        row[i] = row[i].max(0.9 * (1.0 - f) + 0.2);
        row[i + 1] = row[i + 1].max(0.9 * f + 0.2);
        row[i - 1] = row[i - 1].max(0.15);
        row[i + 2] = row[i + 2].max(0.15);
    }

    /// A note `midi` over frames `s..e` with activation `amp`, attacked
    /// (model onset) when `onset`, its contour at the note.
    fn note(a: &mut Activations, midi: usize, s: usize, e: usize, amp: f32, onset: bool) {
        let k = midi - K;
        if onset {
            a.onset[s * N_NOTES + k] = 0.9;
        }
        for t in s..e {
            a.note[t * N_NOTES + k] = amp;
            pitch_at(a, t, midi as f32);
        }
    }

    fn check(r: &NoteAnalysis) {
        for w in r.notes.windows(2) {
            assert!(w[0].end_s <= w[1].start_s + 1e-9, "{:?}", r.notes);
        }
        for n in &r.notes {
            assert!(n.end_s > n.start_s);
            assert!((-50.0..=50.0).contains(&n.cents));
            assert!((0.0..=1.0).contains(&n.confidence));
            assert!(!r
                .chordal
                .iter()
                .any(|&(s, e)| n.start_s >= s && n.end_s <= e));
        }
        for w in r.others.windows(2) {
            assert!(w[0].start_s <= w[1].start_s, "{:?}", r.others);
        }
        for h in &r.others {
            assert!(h.end_s - h.start_s >= MIN_EVENT_S - 1e-9);
            assert!((0.0..=1.0).contains(&h.confidence));
        }
    }

    fn midis(r: &NoteAnalysis) -> Vec<u8> {
        r.notes.iter().map(|n| n.midi).collect()
    }

    #[test]
    fn single_notes_decode_with_times_cents_and_attacks() {
        let mut a = act(300);
        note(&mut a, 69, 20, 80, 0.8, true);
        note(&mut a, 72, 100, 160, 0.7, true);
        let r = transcribe(&a, StemKind::Guitar, 0.0);
        check(&r);
        assert_eq!(midis(&r), vec![69, 72]);
        assert!((r.notes[0].start_s - 20.0 * bp::FRAME_S).abs() < 1e-6);
        assert!(r.notes.iter().all(|n| n.attack == Attack::Picked));
        assert!(r
            .notes
            .iter()
            .all(|n| n.cents.abs() < 10.0 && n.bend.is_none()));
        assert!(r.chordal.is_empty());
        // A song 30 cents flat reads as its intended notes.
        let flat = transcribe(&a, StemKind::Guitar, -30.0);
        assert_eq!(midis(&flat), vec![69, 72]);
        assert!(flat.notes.iter().all(|n| (n.cents - 30.0).abs() < 10.0));
    }

    #[test]
    fn notes_outside_the_range_or_under_the_gate_are_dropped() {
        let mut a = act(300);
        note(&mut a, 100, 20, 80, 0.8, true); // over the guitar's range
        note(&mut a, 64, 100, 160, 0.8, true);
        note(&mut a, 67, 200, 260, 0.8, true);
        for l in &mut a.level_db[200..260] {
            *l = -75.0;
        }
        let r = transcribe(&a, StemKind::Guitar, 0.0);
        assert_eq!(midis(&r), vec![64]);
    }

    /// Strummed G chords (G3 B3 D4) every 45 frames from `from`.
    fn strums(a: &mut Activations, from: usize, count: usize) {
        for i in 0..count {
            let s = from + 45 * i;
            for m in [55, 59, 62] {
                note(a, m, s, s + 42, 0.7, true);
            }
        }
    }

    #[test]
    fn pure_strumming_is_chordal() {
        let mut a = act(600);
        strums(&mut a, 20, 10);
        let r = transcribe(&a, StemKind::Guitar, 0.0);
        check(&r);
        assert!(r.notes.is_empty(), "{:?}", midis(&r));
        let total: f64 = r.chordal.iter().map(|(s, e)| e - s).sum();
        assert!(total > 4.0, "{:?}", r.chordal);
    }

    #[test]
    fn a_lead_over_strumming_keeps_its_notes() {
        let mut a = act(600);
        let lead = [71, 74, 76, 74, 71, 74, 79, 76, 74, 71];
        for (i, &m) in lead.iter().enumerate() {
            let s = 10 + 45 * i;
            note(&mut a, m, s, s + 20, 0.8, true);
        }
        strums(&mut a, 30, 10);
        let r = transcribe(&a, StemKind::Guitar, 0.0);
        check(&r);
        assert!(r.chordal.is_empty(), "{:?}", r.chordal);
        let want: Vec<u8> = lead.iter().map(|&m| m as u8).collect();
        assert_eq!(midis(&r), want);
    }

    #[test]
    fn low_notes_between_phrases_are_skipped_unless_clearly_the_lead() {
        let mut a = act(700);
        // A phrase around D5, a short low G2 and B2 in the gap, the phrase
        // again, a long strong E3 alone, then the phrase again.
        let mut s = 10;
        for m in [74, 76, 74, 71] {
            note(&mut a, m, s, s + 15, 0.8, true);
            s += 17;
        }
        note(&mut a, 43, 90, 102, 0.5, true);
        note(&mut a, 47, 106, 118, 0.5, true);
        let mut s = 125;
        for m in [74, 79, 76] {
            note(&mut a, m, s, s + 15, 0.8, true);
            s += 17;
        }
        note(&mut a, 52, 250, 320, 0.8, true);
        let mut s = 360;
        for m in [74, 76] {
            note(&mut a, m, s, s + 15, 0.8, true);
            s += 17;
        }
        let r = transcribe(&a, StemKind::Guitar, 0.0);
        check(&r);
        assert_eq!(midis(&r), vec![74, 76, 74, 71, 74, 79, 76, 52, 74, 76]);
    }

    #[test]
    fn a_low_run_joins_the_lead() {
        let mut a = act(500);
        // A phrase around D5, then a lick on the low strings (G3 F#3 G3 E3,
        // short notes close together), then the phrase again.
        let mut s = 10;
        for m in [74, 76, 74, 71] {
            note(&mut a, m, s, s + 15, 0.8, true);
            s += 17;
        }
        for m in [55, 54, 55, 52] {
            note(&mut a, m, s, s + 9, 0.5, true);
            s += 10;
        }
        for m in [74, 76] {
            note(&mut a, m, s, s + 15, 0.8, true);
            s += 17;
        }
        let r = transcribe(&a, StemKind::Guitar, 0.0);
        check(&r);
        assert_eq!(midis(&r), vec![74, 76, 74, 71, 55, 54, 55, 52, 74, 76]);
    }

    #[test]
    fn a_bend_and_release_is_one_note() {
        let mut a = act(300);
        // B4 picked, bent a whole step over 9 frames, held, released over 9
        // frames; the model splits it into B4, C#5, B4 without onsets.
        for t in 10..44 {
            a.note[t * N_NOTES + 71 - K] = 0.8;
        }
        a.onset[10 * N_NOTES + 71 - K] = 0.9;
        for t in 46..95 {
            a.note[t * N_NOTES + 73 - K] = 0.8;
        }
        for t in 97..140 {
            a.note[t * N_NOTES + 71 - K] = 0.7;
        }
        for t in 10..140 {
            let p = match t {
                ..40 => 71.0,
                40..49 => 71.0 + 2.0 * (t - 40) as f32 / 9.0,
                49..90 => 73.0,
                90..99 => 73.0 - 2.0 * (t - 90) as f32 / 9.0,
                _ => 71.0,
            };
            pitch_at(&mut a, t, p);
        }
        let r = transcribe(&a, StemKind::Guitar, 0.0);
        check(&r);
        assert_eq!(midis(&r), vec![71], "{:?}", r.notes);
        let n = r.notes[0];
        let b = n.bend.expect("bend");
        assert!((b.semitones - 2.0).abs() < 0.25, "{b:?}");
        assert!((b.start_s - 40.0 * bp::FRAME_S).abs() < 0.03, "{b:?}");
        let rel = b.release_s.expect("release");
        assert!((rel - 90.0 * bp::FRAME_S).abs() < 0.03, "{b:?}");
        assert!(n.end_s > 130.0 * bp::FRAME_S);
        // Keys don't bend: the glide gives separate notes.
        let k = transcribe(&a, StemKind::Keys, 0.0);
        check(&k);
        assert!(k.notes.len() >= 2 && k.notes.iter().all(|n| n.bend.is_none()));
    }

    #[test]
    fn notes_outside_the_lead_line_are_heard_at_their_length() {
        let mut a = act(300);
        // An arpeggio C4 E4 G4 C5, every note ringing to frame 100, then a
        // short low G2 the lead line skips under a D5.
        for (k, m) in [60, 64, 67, 72].into_iter().enumerate() {
            note(&mut a, m, 10 + 15 * k, 100, 0.8, true);
        }
        note(&mut a, 43, 105, 117, 0.5, true);
        note(&mut a, 74, 120, 160, 0.8, true);
        let r = transcribe(&a, StemKind::Guitar, 0.0);
        check(&r);
        assert_eq!(midis(&r), vec![60, 64, 67, 72, 74]);
        let t = |f: usize| a.time_of(f);
        // The arpeggio's notes ring on from where the next one starts.
        let heard: Vec<(u8, f64, f64)> = r
            .others
            .iter()
            .map(|h| (h.midi, h.start_s, h.end_s))
            .collect();
        let want = [
            (60, t(25), t(100)),
            (64, t(40), t(100)),
            (67, t(55), t(100)),
            (43, t(105), t(117)),
        ];
        assert_eq!(heard.len(), want.len(), "{heard:?}");
        for (h, w) in heard.iter().zip(want) {
            assert_eq!(h.0, w.0, "{heard:?}");
            assert!(
                (h.1 - w.1).abs() < 1e-9 && (h.2 - w.2).abs() < 1e-9,
                "{heard:?}"
            );
        }
        assert!(r
            .others
            .iter()
            .all(|h| (h.confidence - 0.8).abs() < 1e-6 || (h.confidence - 0.5).abs() < 1e-6));
        // No heard note repeats a lead note.
        for n in &r.notes {
            assert!(!r.others.iter().any(|h| h.midi == n.midi
                && h.start_s < n.end_s - 1e-9
                && h.end_s > n.start_s + 1e-9));
        }
        // Named after the offset, as lead notes are.
        let flat = transcribe(&a, StemKind::Guitar, -100.0);
        assert_eq!(flat.others[0].midi, 61);
    }

    #[test]
    fn a_quick_step_is_not_a_bend() {
        let mut a = act(200);
        note(&mut a, 71, 10, 50, 0.8, true);
        note(&mut a, 73, 50, 100, 0.8, false);
        let r = transcribe(&a, StemKind::Guitar, 0.0);
        check(&r);
        assert_eq!(midis(&r), vec![71, 73]);
        assert!(r.notes[0].bend.is_none());
        assert_eq!(r.notes[1].attack, Attack::Legato);
    }

    #[test]
    fn a_slow_glide_into_a_new_note_is_a_slide() {
        let mut a = act(200);
        note(&mut a, 64, 10, 50, 0.8, true);
        for t in 54..120 {
            a.note[t * N_NOTES + 69 - K] = 0.8;
        }
        for t in 50..120 {
            let p = if t < 58 {
                64.0 + 5.0 * (t - 50) as f32 / 8.0
            } else {
                69.0
            };
            pitch_at(&mut a, t, p);
        }
        let r = transcribe(&a, StemKind::Guitar, 0.0);
        check(&r);
        assert_eq!(midis(&r), vec![64, 69]);
        assert_eq!(r.notes[1].attack, Attack::Slide);
    }
}
