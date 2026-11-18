// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Attacks: level rises of the whole stem and of its high band (pick and
//! key noise) per pitch frame, and the frames picked as onsets.

use crate::pitch::Track;
use crate::signal::{HighPass, Signal};
use crate::HOP_S;

/// The high band starts here (Hz); a pick or hammer noise lives above it,
/// a held note's fundamental mostly below.
const HIGH_BAND_HZ: f64 = 1500.0;
/// Most hops a level span covers. A span covers at least one period of the
/// lowest note searched, so a low note's waveform doesn't make it wobble.
const MAX_SPAN_HOPS: usize = 4;
/// Full-band rise in dB where the score starts, and the rise over that
/// which scores 1.
const RISE_START_DB: f32 = 3.0;
const RISE_RANGE_DB: f32 = 6.0;
/// The same for the high band, which moves more on its own.
const HIGH_START_DB: f32 = 6.0;
const HIGH_RANGE_DB: f32 = 9.0;
/// The high band counts only when it is within this much of the full band.
const HIGH_BELOW_FULL_DB: f32 = 40.0;
/// Score that makes an onset, and the lower score near a sixteenth of the
/// beat grid.
pub(crate) const PICK_SCORE: f32 = 0.35;
const PICK_SCORE_ON_GRID: f32 = 0.25;
/// Distance to a sixteenth that counts as on the grid (s).
const GRID_TOLERANCE_S: f64 = 0.035;
/// An onset is the highest score within this many frames either side.
const PEAK_RADIUS: usize = 3;

/// Onset score per frame, 0..1.
pub(crate) fn strengths(sig: &Signal, n_frames: usize, min_hz: f64, gate_db: f32) -> Vec<f32> {
    let rate = sig.rate;
    let hop = HOP_S * rate;
    let mut hp = HighPass::new(rate, HIGH_BAND_HZ.min(0.35 * rate));
    let mut full = vec![0.0f64; n_frames];
    let mut high = vec![0.0f64; n_frames];
    let mut counts = vec![0usize; n_frames];
    for (i, &s) in sig.x.iter().enumerate() {
        let h = hp.process(s);
        let t = ((i as f64 / hop) + 0.5).floor() as usize;
        if t >= n_frames {
            break;
        }
        full[t] += f64::from(s) * f64::from(s);
        high[t] += f64::from(h) * f64::from(h);
        counts[t] += 1;
    }
    for t in 0..n_frames {
        if counts[t] > 0 {
            full[t] /= counts[t] as f64;
            high[t] /= counts[t] as f64;
        }
    }
    let span = ((rate / min_hz / hop).ceil() as usize).clamp(1, MAX_SPAN_HOPS);
    let db = |p: f64| {
        if p > 1e-14 {
            (10.0 * p.log10()) as f32
        } else {
            -140.0
        }
    };
    let mut out = vec![0.0f32; n_frames];
    for t in 2 * span - 1..n_frames {
        let mean = |v: &[f64], a: usize| v[a..a + span].iter().sum::<f64>() / span as f64;
        let now = db(mean(&full, t + 1 - span));
        if now < gate_db {
            continue;
        }
        let before = db(mean(&full, t + 1 - 2 * span));
        let rise = ((now - before - RISE_START_DB) / RISE_RANGE_DB).clamp(0.0, 1.0);
        let high_now = db(mean(&high, t + 1 - span));
        let high_rise = if high_now >= now - HIGH_BELOW_FULL_DB {
            let high_before = db(mean(&high, t + 1 - 2 * span));
            ((high_now - high_before - HIGH_START_DB) / HIGH_RANGE_DB).clamp(0.0, 1.0)
        } else {
            0.0
        };
        // A step at hop a scores highest when the newer span starts at a.
        let at = t + 1 - span;
        out[at] = out[at].max(rise.max(high_rise));
    }
    out
}

/// Frames with an onset: local maxima of the score at or above
/// [`PICK_SCORE`] (lower near a sixteenth of the beat grid), in voiced or
/// loud frames.
pub(crate) fn pick(score: &[f32], beats: &[f64], track: &Track) -> Vec<usize> {
    let grid = sixteenths(beats);
    let mut out = Vec::new();
    for t in 0..score.len() {
        let s = score[t];
        if s <= 0.0 || track.level_db[t] < track.gate_db {
            continue;
        }
        let lo = t.saturating_sub(PEAK_RADIUS);
        let hi = (t + PEAK_RADIUS + 1).min(score.len());
        // Ties go to the earliest frame.
        if score[lo..t].iter().any(|&x| x >= s) || score[t + 1..hi].iter().any(|&x| x > s) {
            continue;
        }
        let time = t as f64 * HOP_S;
        let on_grid = {
            let i = grid.partition_point(|&g| g < time);
            [i.checked_sub(1), Some(i)]
                .into_iter()
                .flatten()
                .filter_map(|j| grid.get(j))
                .any(|&g| (g - time).abs() <= GRID_TOLERANCE_S)
        };
        let threshold = if on_grid {
            PICK_SCORE_ON_GRID
        } else {
            PICK_SCORE
        };
        if s >= threshold {
            out.push(t);
        }
    }
    out
}

/// Sixteenth positions between usable beats.
fn sixteenths(beats: &[f64]) -> Vec<f64> {
    let b: Vec<f64> = beats.iter().copied().filter(|x| x.is_finite()).collect();
    let mut out = Vec::new();
    for w in b.windows(2) {
        let len = w[1] - w[0];
        if !(0.1..=2.0).contains(&len) {
            continue;
        }
        for k in 0..4 {
            out.push(w[0] + len * k as f64 / 4.0);
        }
    }
    if let Some(&last) = b.last() {
        out.push(last);
    }
    out.sort_by(f64::total_cmp);
    out
}
