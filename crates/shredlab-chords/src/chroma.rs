// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Per-stem analysis: chroma from interpolated spectral peaks, frame
//! levels and onsets; the bass notes from the monophonic detector; and
//! their sums per grid cell.

use shredlab_tuning::spectrum::{drop_non_octave_harmonics, Analyzer, Peak};
use shredlab_tuning::{LiveDetector, LiveReading, StemAudio, StemKind};

use crate::grid::Cell;

/// Hop between frames at 44.1 kHz (23 ms); other rates scale it.
const HOP_44K: f64 = 1024.0;
/// Long window (186 ms) for the low band, where notes are a few Hz apart.
const LONG_WINDOW_44K: f64 = 8192.0;
/// Short window (93 ms) for the band above [`SPLIT_HZ`], where timing
/// matters more than resolution.
const SHORT_WINDOW_44K: f64 = 4096.0;
/// Boundary between the long-window and the short-window band.
const SPLIT_HZ: f64 = 260.0;
/// Highest frequency searched for peaks.
const MAX_HZ: f64 = 2000.0;
/// Highest frequency searched in the bass stem.
const BASS_MAX_HZ: f64 = 1000.0;
/// Peaks weaker than this fraction of the frame's strongest peak (−34 dB)
/// are left out.
const PEAK_FLOOR: f64 = 0.02;
/// Peaks further than this from an equal-tempered semitone (after the
/// offset) are slides, bends, noise or the 7th harmonic (31 cents flat);
/// closer ones are weighted down the further off they are.
const MAX_DEVIATION: f64 = 0.35;
/// Frames more than 40 dB under the stem's loudest frame are skipped.
const FRAME_GATE: f32 = 0.01;
/// A stem whose loudest frame is below −90 dBFS RMS is silent.
const SILENT_RMS: f32 = 3.0e-5;
/// Lowest note the bass detector targets. 45 Hz keeps its short window;
/// notes below read an octave high, which leaves the pitch class alone.
const BASS_LOW_HZ: f32 = 45.0;
/// Bass readings further than this from a semitone are slides.
const BASS_MAX_DEVIATION: f64 = 0.35;
/// Level onset: rise in dB over the mean of the previous frames that
/// starts to count, and the rise that counts fully on top of it.
const ONSET_RISE_DB: f32 = 3.0;
const ONSET_FULL_DB: f32 = 6.0;
const ONSET_HISTORY: usize = 3;
/// Chroma onset: the share of a frame's chroma that is new compared with
/// two frames earlier, where it starts to count and its full range.
const FLUX_START: f32 = 0.3;
const FLUX_RANGE: f32 = 0.4;
/// An onset helps a cell when it falls this long before or after the
/// cell's start (the windows smear attacks by about this much).
const ONSET_BEFORE_S: f64 = 0.04;
const ONSET_AFTER_S: f64 = 0.06;

/// Weight of a stem's chroma in the cell sum.
pub(crate) fn chroma_weight(kind: StemKind) -> f32 {
    match kind {
        StemKind::Guitar | StemKind::Keys => 1.0,
        StemKind::Other => 0.5,
        StemKind::Bass => 0.3,
        StemKind::Vocals => 0.5,
    }
}

/// Weight of a stem's onsets. The bass gives clean onsets, but it often
/// moves away from the harmony's changes (syncopation, walks, pedals), so
/// the harmony stems count more.
fn onset_weight(kind: StemKind) -> f32 {
    match kind {
        StemKind::Guitar | StemKind::Keys => 1.0,
        StemKind::Other => 0.7,
        StemKind::Bass | StemKind::Vocals => 0.5,
    }
}

/// Frame timing shared by all stems: frame `t` is centered at `t·hop`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Frames {
    pub rate: f64,
    pub hop: usize,
    pub count: usize,
}

impl Frames {
    pub fn new(rate: f64, duration: f64) -> Self {
        let hop = ((HOP_44K * rate / 44_100.0).round() as usize).max(1);
        let count = (duration * rate / hop as f64).ceil() as usize + 1;
        Self { rate, hop, count }
    }

    /// Index of the first frame centered at or after `s`.
    fn at_or_after(&self, s: f64) -> usize {
        ((s * self.rate / self.hop as f64).ceil().max(0.0) as usize).min(self.count)
    }
}

/// One stem's analysis per frame.
pub(crate) struct StemFrames {
    pub kind: StemKind,
    /// Peak weight per pitch class (0 for gated frames).
    pub chroma: Vec<[f32; 12]>,
    /// RMS of the hop around the frame center.
    pub level: Vec<f32>,
    /// Onset strength 0..1.
    pub onset: Vec<f32>,
    /// Share of each pitch class's chroma expected a fifth above it as
    /// its 3rd harmonic (see [`third_harmonic_leak`]).
    pub leak: f32,
}

impl StemFrames {
    pub fn is_silent(&self) -> bool {
        self.chroma.iter().all(|c| c.iter().all(|&x| x == 0.0))
    }
}

/// Window length for `base` at 44.1 kHz scaled to `rate`, rounded to a
/// multiple of 256 so the FFT size has small factors.
fn window_len(base: f64, rate: f64) -> usize {
    (((base * rate / 44_100.0) / 256.0).round() as usize).max(1) * 256
}

/// Fractional MIDI note of `hz` with the tuning offset removed.
fn midi(hz: f64, offset_cents: f64) -> f64 {
    69.0 + 12.0 * (hz / 440.0).log2() - offset_cents / 100.0
}

/// Analyzes one stem: chroma, levels and onsets per frame.
pub(crate) fn analyze_stem(stem: &StemAudio, frames: Frames, offset_cents: f64) -> StemFrames {
    let x = stem.samples;
    let hop = frames.hop;
    let n = frames.count;

    let level: Vec<f32> = (0..n)
        .map(|t| {
            let c = t * hop;
            let lo = c.saturating_sub(hop / 2).min(x.len());
            let hi = (c + hop / 2).min(x.len());
            if hi <= lo {
                return 0.0;
            }
            let e: f64 = x[lo..hi].iter().map(|&v| f64::from(v) * f64::from(v)).sum();
            (e / (hi - lo) as f64).sqrt() as f32
        })
        .collect();
    let mut out = StemFrames {
        kind: stem.kind,
        chroma: vec![[0.0; 12]; n],
        onset: vec![0.0; n],
        level,
        leak: 0.0,
    };
    let loudest = out.level.iter().copied().fold(0.0f32, f32::max);
    if loudest < SILENT_RMS {
        return out;
    }
    let gate = (loudest * FRAME_GATE).max(SILENT_RMS);

    let rate = frames.rate;
    let long_n = window_len(LONG_WINDOW_44K, rate);
    let short_n = window_len(SHORT_WINDOW_44K, rate);
    let mut long = Analyzer::new(long_n, rate);
    let mut short = Analyzer::new(short_n, rate);
    let mut buf = vec![0.0f32; long_n];
    let mut peaks: Vec<Peak> = Vec::with_capacity(128);
    let mut short_peaks: Vec<Peak> = Vec::with_capacity(128);
    let mut counts: Vec<usize> = Vec::new();
    let min_hz = f64::from(stem.kind.min_hz());
    let max_hz = if stem.kind == StemKind::Bass {
        BASS_MAX_HZ
    } else {
        MAX_HZ
    };

    for t in 0..n {
        if out.level[t] < gate {
            continue;
        }
        let c = (t * hop) as isize;
        let frame = |len: usize, buf: &mut [f32]| {
            let start = c - (len / 2) as isize;
            for (i, v) in buf[..len].iter_mut().enumerate() {
                let j = start + i as isize;
                *v = if j >= 0 && (j as usize) < x.len() {
                    x[j as usize]
                } else {
                    0.0
                };
            }
        };
        peaks.clear();
        frame(long_n, &mut buf);
        long.peaks(&buf[..long_n], min_hz, SPLIT_HZ, &mut peaks);
        // Magnitude of a sinusoid of amplitude A under a Hann window of
        // length N is A·N/4; scale both bands to amplitudes.
        let long_scale = 4.0 / long_n as f64;
        for p in &mut peaks {
            p.mag *= long_scale;
        }
        short_peaks.clear();
        frame(short_n, &mut buf);
        short.peaks(&buf[..short_n], SPLIT_HZ, max_hz, &mut short_peaks);
        let short_scale = 4.0 / short_n as f64;
        peaks.extend(short_peaks.iter().map(|p| Peak {
            hz: p.hz,
            mag: p.mag * short_scale,
        }));
        let max = peaks.iter().map(|p| p.mag).fold(0.0, f64::max);
        peaks.retain(|p| p.mag >= max * PEAK_FLOOR);
        drop_non_octave_harmonics(&mut peaks);
        soften_weak_fundamental_overtones(&mut peaks);
        counts.push(peaks.len());
        let chroma = &mut out.chroma[t];
        for p in &peaks {
            let m = midi(p.hz, offset_cents);
            let r = m.round();
            let dev = m - r;
            if dev.abs() > MAX_DEVIATION {
                continue;
            }
            let w = p.mag.sqrt() * (1.0 - (dev / MAX_DEVIATION).powi(2));
            chroma[(r as i64).rem_euclid(12) as usize] += w as f32;
        }
    }

    out.leak = third_harmonic_leak(stem.kind, &mut counts);

    // Onsets: a level jump or new pitch classes.
    let db: Vec<f32> = out
        .level
        .iter()
        .map(|&l| 20.0 * l.max(1e-6).log10())
        .collect();
    for t in 1..n {
        if out.level[t] < gate {
            continue;
        }
        let h = &db[t.saturating_sub(ONSET_HISTORY)..t];
        let prev = h.iter().sum::<f32>() / h.len() as f32;
        let rise = ((db[t] - prev - ONSET_RISE_DB) / ONSET_FULL_DB).clamp(0.0, 1.0);
        let a = &out.chroma[t];
        let b = &out.chroma[t.saturating_sub(2)];
        let total: f32 = a.iter().sum();
        let flux = if total > 0.0 {
            let new: f32 = a.iter().zip(b).map(|(x, y)| (x - y).max(0.0)).sum();
            ((new / total - FLUX_START) / FLUX_RANGE).clamp(0.0, 1.0)
        } else {
            0.0
        };
        out.onset[t] = rise.max(flux);
    }
    out
}

/// Median peaks per frame (after the floor and the overtone filters) of
/// a clean guitar stem and of an overdriven one in the library songs:
/// overdrive adds harmonics and intermodulation products all over the
/// spectrum.
const CLEAN_PEAKS: f32 = 24.0;
const DRIVEN_PEAKS: f32 = 28.0;
/// Leak share of an overdriven stem.
const DRIVEN_LEAK: f32 = 0.3;

/// How much of a pitch class's chroma shows up a fifth above it as its
/// 3rd harmonic (an octave and a fifth up) after the peak filters, from
/// how overdriven the stem sounds: 0 up to [`CLEAN_PEAKS`] median peaks
/// per frame, rising to [`DRIVEN_LEAK`] at [`DRIVEN_PEAKS`]. Overdrive
/// makes a note's 3rd harmonic louder than its fundamental and octaves,
/// which the peak filters leave alone; in an A chord the third's then
/// reads as a major seventh, the fifth's as a ninth, a minor third's as
/// a minor seventh. Only guitar and other stems: keys mostly don't
/// distort, and a dense piano chord has many peaks too.
fn third_harmonic_leak(kind: StemKind, counts: &mut [usize]) -> f32 {
    if !matches!(kind, StemKind::Guitar | StemKind::Other) || counts.is_empty() {
        return 0.0;
    }
    let mid = counts.len() / 2;
    let median = *counts.select_nth_unstable(mid).1 as f32;
    let drive = ((median - CLEAN_PEAKS) / (DRIVEN_PEAKS - CLEAN_PEAKS)).clamp(0.0, 1.0);
    DRIVEN_LEAK * drive
}

/// Largest harmonic number [`soften_weak_fundamental_overtones`] checks.
const MAX_OVERTONE: f64 = 16.0;
/// Its tolerance around each harmonic, in cents.
const OVERTONE_CENTS: f64 = 20.0;
/// Magnitude factor for a likely overtone of a weak fundamental.
const OVERTONE_FACTOR: f64 = 0.5;

/// Weakens peaks within ±20 cents of a non-octave multiple of a lower
/// peak when that peak or its 2nd or 4th harmonic is stronger. The tuning
/// crate's filter only drops overtones of a stronger peak, but a guitar's
/// low strings have weak fundamentals: the 3rd harmonic of a minor chord's
/// third, louder than that fundamental, reads as a minor seventh. The
/// note's octave partials vouch for the overtone. The peak is weakened
/// rather than dropped because it can also be a played note (the major
/// seventh of a maj7 chord is the 3rd harmonic of its third).
fn soften_weak_fundamental_overtones(peaks: &mut [Peak]) {
    let tol = 2f64.powf(OVERTONE_CENTS / 1200.0);
    let near = |a: f64, b: f64| a / b < tol && b / a < tol;
    // Strongest of each peak and its octaves 2 and 4.
    let series: Vec<f64> = peaks
        .iter()
        .map(|p| {
            peaks
                .iter()
                .filter(|q| near(q.hz, p.hz) || near(q.hz, 2.0 * p.hz) || near(q.hz, 4.0 * p.hz))
                .map(|q| q.mag)
                .fold(p.mag, f64::max)
        })
        .collect();
    for j in 1..peaks.len() {
        let pj = peaks[j];
        let overtone = peaks[..j].iter().zip(&series).any(|(pi, &strength)| {
            let ratio = pj.hz / pi.hz;
            let h = ratio.round();
            strength > pj.mag
                && (3.0..=MAX_OVERTONE).contains(&h)
                && !(h as u64).is_power_of_two()
                && near(ratio, h)
        });
        if overtone {
            peaks[j].mag *= OVERTONE_FACTOR;
        }
    }
}

/// Bass note readings: pitch class and time of each steady reading.
pub(crate) fn bass_notes(samples: &[f32], rate: u32, offset_cents: f64) -> Vec<(f64, u8)> {
    let mut det = LiveDetector::new(rate, BASS_LOW_HZ);
    let hop = det.hop_len();
    let half = det.window_len() / 2;
    let rate_f = f64::from(rate);
    let mut readings: Vec<LiveReading> = Vec::new();
    let mut out = Vec::new();
    let mut k = 0usize;
    for chunk in samples.chunks(16_384) {
        readings.clear();
        det.push(chunk, &mut readings);
        for r in &readings {
            if let Some(hz) = r.hz {
                let m = midi(f64::from(hz), offset_cents);
                let rm = m.round();
                if (m - rm).abs() <= BASS_MAX_DEVIATION {
                    let t = (half + k * hop) as f64 / rate_f;
                    out.push((t, (rm as i64).rem_euclid(12) as u8));
                }
            }
            k += 1;
        }
    }
    out
}

/// What the stems hold in one grid cell.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CellFeatures {
    /// Weighted chroma of the pitched stems.
    pub chroma: [f32; 12],
    /// Weighted mean level of the pitched stems.
    pub level: f32,
    /// Onset strength 0..1 at the cell's start.
    pub onset: f32,
    /// Bass readings per pitch class.
    pub bass: [u16; 12],
    /// Chroma expected per pitch class as the 3rd harmonic of the class a
    /// fifth below, from the overdriven stems.
    pub leak: [f32; 12],
}

impl CellFeatures {
    /// The bass note when one pitch class holds most of the cell's readings.
    pub fn bass_note(&self) -> Option<u8> {
        let total: u32 = self.bass.iter().map(|&c| u32::from(c)).sum();
        let (pc, &max) = self.bass.iter().enumerate().max_by_key(|(_, &c)| c)?;
        (total >= 2 && f32::from(max) >= 0.6 * total as f32).then_some(pc as u8)
    }
}

/// Sums the stems' frames into the grid cells.
pub(crate) fn cell_features(
    cells: &[Cell],
    frames: Frames,
    stems: &[StemFrames],
    bass: &[(f64, u8)],
) -> Vec<CellFeatures> {
    let mut out = vec![CellFeatures::default(); cells.len()];
    let mut b = 0;
    for (cell, f) in cells.iter().zip(&mut out) {
        let mut lo = frames.at_or_after(cell.start);
        let mut hi = frames.at_or_after(cell.end);
        if hi <= lo {
            // Shorter than a hop: the frame nearest the middle.
            let mid = 0.5 * (cell.start + cell.end);
            let t = ((mid * frames.rate / frames.hop as f64).round() as usize)
                .min(frames.count.saturating_sub(1));
            lo = t;
            hi = t + 1;
        }
        let inv = 1.0 / (hi - lo) as f32;
        let on_lo = frames.at_or_after(cell.start - ONSET_BEFORE_S);
        let on_hi = frames
            .at_or_after(cell.start + ONSET_AFTER_S)
            .max(on_lo + 1);
        for s in stems {
            let w = chroma_weight(s.kind);
            for t in lo..hi.min(s.chroma.len()) {
                for (a, &c) in f.chroma.iter_mut().zip(&s.chroma[t]) {
                    *a += w * c * inv;
                }
                if s.leak > 0.0 {
                    for (pc, &c) in s.chroma[t].iter().enumerate() {
                        f.leak[(pc + 7) % 12] += s.leak * w * c * inv;
                    }
                }
                f.level += w * s.level[t] * inv;
            }
            let on = s.onset[on_lo.min(s.onset.len())..on_hi.min(s.onset.len())]
                .iter()
                .copied()
                .fold(0.0f32, f32::max);
            f.onset = f.onset.max(on * onset_weight(s.kind));
        }
        while b < bass.len() && bass[b].0 < cell.start {
            b += 1;
        }
        let mut j = b;
        while j < bass.len() && bass[j].0 < cell.end {
            let c = &mut f.bass[usize::from(bass[j].1)];
            *c = c.saturating_add(1);
            j += 1;
        }
    }
    out
}
