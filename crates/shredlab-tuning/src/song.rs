// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Offline estimate of a whole song's offset from A440.

use crate::cents::{cents_from_a440, fold, Circular};
use crate::spectrum::{drop_non_octave_harmonics, Analyzer, Peak};
use crate::{Segment, StemAudio, StemKind, TuningEstimate, SEGMENT_SECONDS};

/// Window length at 44.1 kHz; other rates scale it to keep the same
/// duration and frequency resolution.
const WINDOW_44K: f64 = 8192.0;
/// Highest frequency searched for peaks, in Hz.
const MAX_HZ: f64 = 2000.0;
/// Frames whose RMS is more than 40 dB below the stem's loudest frame are
/// skipped.
const FRAME_GATE: f32 = 0.01;
/// A stem whose loudest frame is below −90 dBFS RMS counts as silent.
const SILENT_RMS: f32 = 3.0e-5;
/// Segments need at least this resultant length to count for drift.
const SEGMENT_MIN_CONFIDENCE: f64 = 0.5;
/// Segments need at least this many peaks to count for drift.
const SEGMENT_MIN_PEAKS: u32 = 40;
/// Spread of the smoothed segment offsets above which the song drifts.
const DRIFT_CENTS: f64 = 8.0;

/// Measures how far a song is from A440 from its separated stems (mono, at
/// `sample_rate`). Bass, guitar, keys and other stems are combined with
/// their [`StemKind::weight`]; vocals are used only if all of those are
/// silent. Drums must not be passed. Stems are analyzed in parallel.
///
/// Returns `None` when no stem has a usable spectral peak (silence, no
/// pitched stems, or a zero sample rate). Check
/// [`TuningEstimate::is_reliable`] before showing the result.
pub fn estimate_song_tuning(stems: &[StemAudio], sample_rate: u32) -> Option<TuningEstimate> {
    if sample_rate == 0 {
        return None;
    }
    let rate = f64::from(sample_rate);
    let (pitched, vocals): (Vec<_>, Vec<_>) =
        stems.iter().partition(|s| s.kind != StemKind::Vocals);
    let mut results = analyze_all(&pitched, rate);
    if results.iter().all(|r| r.total.count == 0) {
        results = analyze_all(&vocals, rate);
    }

    let mut total = Circular::default();
    let segment_count = results.iter().map(|r| r.segments.len()).max()?;
    let mut segments = vec![Circular::default(); segment_count];
    for r in &results {
        let w = r.kind.weight();
        total.merge(&r.total, w);
        for (sum, s) in segments.iter_mut().zip(&r.segments) {
            sum.merge(s, w);
        }
    }
    if total.count == 0 {
        return None;
    }

    let offset = total.mean_cents();
    let max_weight = segments.iter().map(|s| s.weight).fold(0.0, f64::max);
    let segments: Vec<Segment> = segments
        .iter()
        .enumerate()
        .filter(|(_, s)| s.count >= SEGMENT_MIN_PEAKS && s.resultant() >= SEGMENT_MIN_CONFIDENCE)
        .map(|(i, s)| Segment {
            start_s: i as f32 * SEGMENT_SECONDS,
            offset_cents: (offset + fold(s.mean_cents() - offset)) as f32,
            weight: (s.weight / max_weight) as f32,
        })
        .collect();

    Some(TuningEstimate {
        offset_cents: offset as f32,
        reference_hz: (440.0 * 2f64.powf(offset / 1200.0)) as f32,
        confidence: total.resultant() as f32,
        drifts: drifts(&segments),
        segments,
        peak_count: total.count,
    })
}

/// True when the 3-segment moving median of the offsets spreads by more
/// than [`DRIFT_CENTS`].
fn drifts(segments: &[Segment]) -> bool {
    let v: Vec<f64> = segments.iter().map(|s| f64::from(s.offset_cents)).collect();
    if v.len() < 2 {
        return false;
    }
    let smoothed: Vec<f64> = (0..v.len())
        .map(|i| {
            let window = &v[i.saturating_sub(1)..(i + 2).min(v.len())];
            median(window)
        })
        .collect();
    let lo = smoothed.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = smoothed.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    hi - lo > DRIFT_CENTS
}

fn median(values: &[f64]) -> f64 {
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    }
}

/// Per-stem sums: the whole song and each segment.
struct StemResult {
    kind: StemKind,
    total: Circular,
    segments: Vec<Circular>,
}

fn analyze_all(stems: &[&StemAudio], rate: f64) -> Vec<StemResult> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = stems
            .iter()
            .map(|s| scope.spawn(move || analyze_stem(s, rate)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("stem analysis panicked"))
            .collect()
    })
}

fn analyze_stem(stem: &StemAudio, rate: f64) -> StemResult {
    // Even window, a quarter of it as hop: 8192 / 2048 at 44.1 kHz.
    let n = (((WINDOW_44K * rate / 44_100.0) / 2.0).round() as usize * 2).max(64);
    let hop = n / 4;
    let samples = stem.samples;
    let segment_len = SEGMENT_SECONDS as f64 * rate;
    let segment_count = (samples.len() as f64 / segment_len).ceil() as usize;
    let mut result = StemResult {
        kind: stem.kind,
        total: Circular::default(),
        segments: vec![Circular::default(); segment_count],
    };

    let starts: Vec<usize> = (0..samples.len()).step_by(hop).collect();
    let rms: Vec<f32> = starts
        .iter()
        .map(|&s| {
            let frame = &samples[s..(s + n).min(samples.len())];
            let energy: f64 = frame.iter().map(|&x| f64::from(x) * f64::from(x)).sum();
            (energy / n as f64).sqrt() as f32
        })
        .collect();
    let loudest = rms.iter().copied().fold(0.0f32, f32::max);
    if loudest < SILENT_RMS {
        return result;
    }
    let gate = loudest * FRAME_GATE;

    let mut analyzer = Analyzer::new(n, rate);
    let mut peaks: Vec<Peak> = Vec::new();
    for (&start, &frame_rms) in starts.iter().zip(&rms) {
        if frame_rms < gate {
            continue;
        }
        let frame = &samples[start..(start + n).min(samples.len())];
        peaks.clear();
        analyzer.peaks(frame, f64::from(stem.kind.min_hz()), MAX_HZ, &mut peaks);
        drop_non_octave_harmonics(&mut peaks);
        let center = start as f64 + n as f64 / 2.0;
        let segment = ((center / segment_len) as usize).min(segment_count - 1);
        for p in &peaks {
            let cents = cents_from_a440(p.hz);
            let w = p.mag.sqrt();
            result.total.add(cents, w);
            result.segments[segment].add(cents, w);
        }
    }
    result
}
