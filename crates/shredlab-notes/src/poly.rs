// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Chordal stretches: frames whose steady spectral peaks are not all
//! harmonics of one fundamental, held long enough to be more than a
//! ringing note or a noise burst.

use shredlab_tuning::spectrum::{Analyzer, Peak};

use crate::pitch::Track;
use crate::signal::Signal;
use crate::{StemKind, HOP_S};

/// Analysis frames are this many pitch frames apart.
const STEP: usize = 2;
/// Window length in seconds (rounded up to a power of two in samples):
/// long for stems that reach the lowest notes.
const WINDOW_LOW_S: f64 = 0.17;
const WINDOW_S: f64 = 0.085;
/// Peaks within this of the frame's strongest (amplitude ratio, −24 dB)
/// take part.
const STRONG: f64 = 0.063;
/// When a frame is chordal: the share of its steady peaks' power outside
/// the fundamental's harmonics together with the strongest unexplained
/// peak (amplitude relative to the frame's strongest), or that peak alone.
/// Alone it catches chords whose notes are mostly harmonics of the root
/// (an open E leaves little unexplained power, but its other
/// fundamentals are loud).
struct Limits {
    share: f64,
    share_loud: f64,
    alone: f64,
}

/// Guitar, bass and keys: −15 dB with a quarter of the power, or −11 dB.
/// Vocals and other stems carry a lead over quieter backing parts and
/// bleed, which should not hide the lead: −10 dB with a third, or −4 dB.
fn limits(kind: StemKind) -> Limits {
    match kind {
        StemKind::Vocals | StemKind::Other => Limits {
            share: 0.35,
            share_loud: 0.3,
            alone: 0.6,
        },
        _ => Limits {
            share: 0.25,
            share_loud: 0.18,
            alone: 0.28,
        },
    }
}

/// A peak is steady when the previous analysis frame had one within this
/// many cents (or half a bin).
const STEADY_CENTS: f64 = 40.0;
/// Harmonic match tolerance in cents, and the highest harmonic matched.
const HARMONIC_CENTS: f64 = 40.0;
const MAX_HARMONIC: f64 = 12.0;
/// Peaks this close to a harmonic of the tracked pitch refine it.
const REFINE_CENTS: f64 = 80.0;
/// A frame's voicing probability must reach this for its tracked pitch to
/// be the fundamental; otherwise the lowest strong peak is.
const TRUST_PITCH: f32 = 0.2;
/// Stretch building: sliding window (s), share of chordal frames in it,
/// shortest stretch, and the longest gap joined.
const SMOOTH_S: f64 = 0.3;
const MIN_SHARE: f64 = 0.6;
const MIN_STRETCH_S: f64 = 0.4;
const JOIN_GAP_S: f64 = 0.25;

fn cents(a: f64, b: f64) -> f64 {
    1200.0 * (a / b).log2()
}

/// Chordal stretches (start_s, end_s) in time order.
pub(crate) fn stretches(sig: &Signal, track: &Track, kind: StemKind) -> Vec<(f64, f64)> {
    let (min_hz, max_hz) = kind.range_hz();
    let limits = limits(kind);
    let rate = sig.rate;
    let secs = if min_hz < 60.0 {
        WINDOW_LOW_S
    } else {
        WINDOW_S
    };
    let n = ((secs * rate).ceil() as usize).next_power_of_two();
    let mut analyzer = Analyzer::new(n, rate);
    let bin_hz = rate / n as f64;
    let top = (max_hz * 4.0).min(0.45 * rate);
    let mut frame = vec![0.0f32; n];
    let mut peaks: Vec<Peak> = Vec::new();
    let mut prev: Vec<Peak> = Vec::new();
    // Per analysis frame: None (quiet), Some(chordal?).
    let mut state: Vec<Option<bool>> = Vec::new();
    let mut t = 0;
    while t < track.len() {
        let c = (t as f64 * HOP_S * rate).round() as isize - (n / 2) as isize;
        for (i, x) in frame.iter_mut().enumerate() {
            let k = c + i as isize;
            *x = if k >= 0 && (k as usize) < sig.x.len() {
                sig.x[k as usize]
            } else {
                0.0
            };
        }
        peaks.clear();
        let loud = track.level_db[t] >= track.gate_db;
        if loud {
            analyzer.peaks(&frame, 0.9 * min_hz, top, &mut peaks);
        }
        let s = if loud && !peaks.is_empty() {
            Some(chordal_frame(
                &peaks,
                &prev,
                track,
                t,
                bin_hz,
                &limits,
                kind.has_fundamental(),
            ))
        } else {
            None
        };
        state.push(s);
        std::mem::swap(&mut prev, &mut peaks);
        t += STEP;
    }

    // Smooth and build stretches.
    let dt = STEP as f64 * HOP_S;
    let half = ((SMOOTH_S / dt / 2.0).round() as usize).max(1);
    let mut on = vec![false; state.len()];
    for i in 0..state.len() {
        let lo = i.saturating_sub(half);
        let hi = (i + half + 1).min(state.len());
        let (mut yes, mut total) = (0usize, 0usize);
        for s in state[lo..hi].iter().flatten() {
            total += 1;
            yes += usize::from(*s);
        }
        on[i] =
            state[i].is_some() && total * 2 >= hi - lo && yes as f64 >= MIN_SHARE * total as f64;
    }
    let mut out: Vec<(f64, f64)> = Vec::new();
    let mut i = 0;
    while i < on.len() {
        if !on[i] {
            i += 1;
            continue;
        }
        let a = i;
        while i < on.len() && on[i] {
            i += 1;
        }
        let (s, e) = (a as f64 * dt - dt / 2.0, i as f64 * dt - dt / 2.0);
        let s = s.max(0.0);
        match out.last_mut() {
            Some(last) if s - last.1 <= JOIN_GAP_S => last.1 = e,
            _ => out.push((s, e)),
        }
    }
    out.retain(|(s, e)| e - s >= MIN_STRETCH_S);
    out
}

/// Whether one frame's steady peaks hold more than one fundamental.
fn chordal_frame(
    peaks: &[Peak],
    prev: &[Peak],
    track: &Track,
    t: usize,
    bin_hz: f64,
    limits: &Limits,
    fundamental: bool,
) -> bool {
    let max = peaks.iter().map(|p| p.mag).fold(0.0, f64::max);
    let steady: Vec<Peak> = peaks
        .iter()
        .filter(|p| p.mag >= STRONG * max)
        .filter(|p| {
            prev.iter().any(|q| {
                (p.hz - q.hz).abs() <= 0.5 * bin_hz || cents(p.hz, q.hz).abs() <= STEADY_CENTS
            })
        })
        .copied()
        .collect();
    if steady.len() < 2 {
        return false;
    }
    let tracked = (!track.midi[t].is_nan() && track.vprob[t] >= TRUST_PITCH)
        .then(|| 440.0 * 2f64.powf((f64::from(track.midi[t]) - 69.0) / 12.0));
    let mut f0 = tracked.unwrap_or(steady[0].hz);
    // A voice without its fundamental is two voices a fifth or an octave
    // apart; the lowest of them is the fundamental to explain.
    if fundamental
        && !steady
            .iter()
            .any(|p| cents(p.hz, f0).abs() <= REFINE_CENTS && p.mag >= 0.1 * max)
    {
        f0 = steady[0].hz;
    }
    // The tracked pitch is the frame's center; a moving pitch (vibrato, a
    // bend) puts the window's harmonics a little off it. Refine it from
    // the peaks near its harmonics.
    let mut ratios: Vec<(f64, f64)> = steady
        .iter()
        .filter_map(|p| {
            let k = (p.hz / f0).round();
            (1.0..=MAX_HARMONIC)
                .contains(&k)
                .then(|| p.hz / (k * f0))
                .filter(|r| (1200.0 * r.log2()).abs() <= REFINE_CENTS)
                .map(|r| (r, p.mag))
        })
        .collect();
    if !ratios.is_empty() {
        ratios.sort_by(|a, b| a.0.total_cmp(&b.0));
        let half = ratios.iter().map(|r| r.1).sum::<f64>() / 2.0;
        let mut acc = 0.0;
        for (r, m) in &ratios {
            acc += m;
            if acc >= half {
                f0 *= r;
                break;
            }
        }
    }
    // A real peak an octave under the fundamental: the tracker took the
    // 2nd harmonic.
    if steady
        .iter()
        .any(|p| cents(p.hz, f0 / 2.0).abs() <= HARMONIC_CENTS && p.mag >= 0.1 * max)
    {
        f0 /= 2.0;
    }
    let (mut total, mut unexplained, mut loud) = (0.0, 0.0, 0.0f64);
    for p in &steady {
        let k = (p.hz / f0).round();
        if k > MAX_HARMONIC {
            continue;
        }
        let power = p.mag * p.mag;
        total += power;
        if k < 1.0 || cents(p.hz, k * f0).abs() > HARMONIC_CENTS {
            unexplained += power;
            loud = loud.max(p.mag);
        }
    }
    total > 0.0
        && (loud >= limits.alone * max
            || (unexplained / total >= limits.share && loud >= limits.share_loud * max))
}
