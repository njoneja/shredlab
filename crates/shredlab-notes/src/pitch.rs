// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Frame-wise pitch tracking: probabilistic YIN candidates per frame and a
//! Viterbi path through pitch bins plus an unvoiced state (as in pYIN).

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

use crate::signal::Signal;
use crate::{StemKind, HOP_S};

/// A stem whose reference level is at or below this (dBFS) is silent.
pub(crate) const SILENT_DB: f32 = -90.0;
/// Frames more than this under the stem's reference level are unvoiced.
const GATE_BELOW_REF_DB: f32 = 40.0;
/// Absolute floor of the gate (dBFS).
const GATE_FLOOR_DB: f32 = -70.0;
/// Percentile of the frame levels taken as the stem's reference level.
const REFERENCE_PERCENTILE: f64 = 0.99;
/// YIN window: at least this long, and at least the longest period
/// searched times [`WINDOW_PERIODS`].
const MIN_WINDOW_S: f64 = 0.025;
const WINDOW_PERIODS: f64 = 1.2;
/// Beta distribution of the YIN thresholds (pYIN): shape (2, b), mean
/// 2/(2+b) = 0.2. Separated stems are noisier than the solo recordings
/// pYIN's default (mean 0.1) was tuned on.
const BETA_B: f64 = 8.0;
const THRESHOLDS: usize = 100;
/// Share of the threshold mass no dip falls under that still goes to the
/// lowest dip (pYIN's absolute-minimum probability).
const ABSOLUTE_MIN_SHARE: f32 = 0.01;
/// Octave guard (stems whose notes may lack a fundamental): a dip near
/// twice a candidate's period that is at most
/// this fraction of its CMNDF value, and lower by at least the margin,
/// takes [`OCTAVE_MOVE`] of the candidate's mass.
const OCTAVE_RATIO: f32 = 0.5;
const OCTAVE_MARGIN: f32 = 0.03;
const OCTAVE_MOVE: f32 = 0.7;
/// The guard leaves dips under this alone: a high note's period is a few
/// samples long, and the dip at twice it is cleaner only because it is
/// sampled better.
const OCTAVE_MIN_VALUE: f32 = 0.1;
/// Pitch bins in semitones (20 cents).
pub(crate) const BIN_ST: f64 = 0.2;
/// Viterbi costs in nats: per bin moved between frames, the most a jump
/// can cost, and a voiced/unvoiced switch.
const STEP_COST: f32 = 0.1;
const JUMP_COST: f32 = 4.0;
const SWITCH_COST: f32 = 4.0;
/// Emission floor for a voiced bin without candidates, and the factor on
/// the unvoiced probability.
const EMISSION_FLOOR: f32 = 1e-3;
const UNVOICED_FACTOR: f32 = 0.5;
/// A decoded bin takes the exact pitch of a candidate this close (bins).
const REFINE_BINS: f64 = 1.5;

/// The pitch track of one stem, one frame per [`HOP_S`].
pub(crate) struct Track {
    /// Fundamental as a fractional MIDI note (A440 = 69, before the song's
    /// offset); NaN where unvoiced.
    pub midi: Vec<f32>,
    /// Voicing probability: the YIN threshold mass with a dip under it.
    pub vprob: Vec<f32>,
    /// RMS level of the frame's window in dBFS.
    pub level_db: Vec<f32>,
    /// The stem's reference (loud) level, dBFS.
    pub reference_db: f32,
    /// Frames under this level are never voiced.
    pub gate_db: f32,
}

impl Track {
    pub fn len(&self) -> usize {
        self.midi.len()
    }

    pub fn voiced(&self, t: usize) -> bool {
        !self.midi[t].is_nan()
    }
}

/// A pitch candidate of one frame.
#[derive(Clone, Copy)]
struct Candidate {
    lag: f64,
    value: f32,
    mass: f32,
}

struct Yin {
    window: usize,
    min_lag: usize,
    max_lag: usize,
    n: usize,
    fft: Arc<dyn RealToComplex<f32>>,
    ifft: Arc<dyn ComplexToReal<f32>>,
    a: Vec<f32>,
    b: Vec<f32>,
    fa: Vec<Complex<f32>>,
    fb: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    scratch_inv: Vec<Complex<f32>>,
    corr: Vec<f32>,
    prefix: Vec<f64>,
    diff: Vec<f32>,
    cmndf: Vec<f32>,
    /// Whether a much cleaner dip at twice a candidate's period takes most
    /// of its mass.
    octave_guard: bool,
    /// Cumulative threshold weights: `cdf[k]` is the mass of the first `k`
    /// thresholds.
    cdf: Vec<f32>,
}

impl Yin {
    fn new(rate: f64, min_hz: f64, max_hz: f64) -> Self {
        let min_lag = ((rate / max_hz).floor() as usize).max(2);
        let max_lag = (rate / min_hz).ceil() as usize + 1;
        let window = ((MIN_WINDOW_S * rate).max(WINDOW_PERIODS * max_lag as f64)).ceil() as usize;
        let n = (window + max_lag + 1).next_power_of_two();
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(n);
        let ifft = planner.plan_fft_inverse(n);
        // Beta(2, b) weights on the thresholds (i + 0.5) / 100.
        let w: Vec<f64> = (0..THRESHOLDS)
            .map(|i| {
                let s = (i as f64 + 0.5) / THRESHOLDS as f64;
                s * (1.0 - s).powf(BETA_B - 1.0)
            })
            .collect();
        let total: f64 = w.iter().sum();
        let mut cdf = vec![0.0f32; THRESHOLDS + 1];
        let mut acc = 0.0;
        for (i, x) in w.iter().enumerate() {
            acc += x / total;
            cdf[i + 1] = acc as f32;
        }
        Self {
            window,
            min_lag,
            max_lag,
            n,
            a: fft.make_input_vec(),
            b: fft.make_input_vec(),
            fa: fft.make_output_vec(),
            fb: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            scratch_inv: ifft.make_scratch_vec(),
            corr: ifft.make_output_vec(),
            prefix: vec![0.0; window + max_lag + 2],
            diff: vec![0.0; max_lag + 2],
            cmndf: vec![0.0; max_lag + 2],
            cdf,
            octave_guard: true,
            fft,
            ifft,
        }
    }

    /// Threshold mass at or below `v`.
    fn mass_below(&self, v: f32) -> f32 {
        let k = ((f64::from(v) * THRESHOLDS as f64 + 0.5).floor() as isize)
            .clamp(0, THRESHOLDS as isize) as usize;
        self.cdf[k]
    }

    /// Candidates of the frame `x` (`window + max_lag + 1` samples), and
    /// the voicing probability.
    fn analyze(&mut self, x: &[f32], out: &mut Vec<Candidate>) -> f32 {
        out.clear();
        let w = self.window;
        let len = w + self.max_lag + 1;
        self.a[..w].copy_from_slice(&x[..w]);
        self.a[w..].fill(0.0);
        self.b[..len].copy_from_slice(&x[..len]);
        self.b[len..].fill(0.0);
        let mut acc = 0.0f64;
        self.prefix[0] = 0.0;
        for (i, &s) in x[..len].iter().enumerate() {
            acc += f64::from(s) * f64::from(s);
            self.prefix[i + 1] = acc;
        }
        let e0 = self.prefix[w];
        if e0 <= 1e-12 {
            return 0.0;
        }
        self.fft
            .process_with_scratch(&mut self.a, &mut self.fa, &mut self.scratch)
            .expect("buffer sizes come from the plan");
        self.fft
            .process_with_scratch(&mut self.b, &mut self.fb, &mut self.scratch)
            .expect("buffer sizes come from the plan");
        for (p, q) in self.fa.iter_mut().zip(&self.fb) {
            *p = p.conj() * q;
        }
        self.ifft
            .process_with_scratch(&mut self.fa, &mut self.corr, &mut self.scratch_inv)
            .expect("buffer sizes come from the plan");
        let norm = 1.0 / self.n as f64;
        // CMNDF d'(τ) = d(τ) τ / Σ_{j≤τ} d(j).
        self.cmndf[0] = 1.0;
        let mut sum = 0.0f64;
        for tau in 1..=self.max_lag {
            let et = self.prefix[tau + w] - self.prefix[tau];
            let d = (e0 + et - 2.0 * f64::from(self.corr[tau]) * norm).max(0.0);
            sum += d;
            self.diff[tau] = d as f32;
            self.cmndf[tau] = if sum > 0.0 {
                (d * tau as f64 / sum) as f32
            } else {
                1.0
            };
        }
        // Dips in the searched range, shortest period first.
        // The period is interpolated on the raw difference (YIN), the value
        // on the CMNDF.
        let f = &self.cmndf;
        let g = &self.diff;
        let parabola = |v: &[f32], tau: usize| {
            let (a, b, c) = (
                f64::from(v[tau - 1]),
                f64::from(v[tau]),
                f64::from(v[tau + 1]),
            );
            let denom = a - 2.0 * b + c;
            if denom > 0.0 {
                let p = (0.5 * (a - c) / denom).clamp(-0.5, 0.5);
                (p, b - 0.25 * (a - c) * p)
            } else {
                (0.0, b)
            }
        };
        for tau in self.min_lag.max(2)..self.max_lag {
            if f[tau] < f[tau - 1] && f[tau] <= f[tau + 1] && f[tau] < 1.0 {
                let (p, _) = parabola(g, tau);
                let (_, v) = parabola(f, tau);
                let v = v as f32;
                out.push(Candidate {
                    lag: tau as f64 + p,
                    value: v.max(0.0),
                    mass: 0.0,
                });
            }
        }
        if out.is_empty() {
            return 0.0;
        }
        // Dip k takes the thresholds above its value up to the lowest value
        // of the dips before it.
        let mut prev_min = f32::INFINITY;
        let mut voiced = 0.0;
        for c in out.iter_mut() {
            let v = c.value;
            if v < prev_min {
                let hi = if prev_min.is_finite() {
                    self.mass_below(prev_min)
                } else {
                    1.0
                };
                let m = (hi - self.mass_below(v)).max(0.0);
                c.mass = m;
                voiced += m;
                prev_min = v;
            }
        }
        let lowest = (0..out.len())
            .min_by(|&i, &j| out[i].value.total_cmp(&out[j].value))
            .expect("not empty");
        let rest = (1.0 - voiced).max(0.0) * ABSOLUTE_MIN_SHARE;
        out[lowest].mass += rest;
        voiced += rest;
        // Octave guard: a much cleaner dip at twice the period.
        for i in 0..out.len() {
            if !self.octave_guard {
                break;
            }
            if out[i].mass <= 0.0 {
                continue;
            }
            let target = 2.0 * out[i].lag;
            let Some(j) = (0..out.len())
                .filter(|&j| (out[j].lag / target - 1.0).abs() < 0.03)
                .min_by(|&a, &b| out[a].value.total_cmp(&out[b].value))
            else {
                continue;
            };
            if out[i].value >= OCTAVE_MIN_VALUE
                && out[j].value < OCTAVE_RATIO * out[i].value
                && out[j].value < out[i].value - OCTAVE_MARGIN
            {
                let moved = OCTAVE_MOVE * out[i].mass;
                out[i].mass -= moved;
                out[j].mass += moved;
            }
        }
        out.retain(|c| c.mass > 0.0);
        voiced.min(1.0)
    }
}

fn hz_to_midi(hz: f64) -> f64 {
    69.0 + 12.0 * (hz / 440.0).log2()
}

/// Tracks the pitch of `sig` over `n_frames` frames of [`HOP_S`].
pub(crate) fn track(sig: &Signal, n_frames: usize, kind: StemKind) -> Track {
    let (min_hz, max_hz) = kind.range_hz();
    let rate = sig.rate;
    let mut yin = Yin::new(rate, min_hz, max_hz);
    yin.octave_guard = !kind.has_fundamental();
    let w = yin.window;
    let len = w + yin.max_lag + 1;
    let lo = hz_to_midi(min_hz);
    let n_bins = ((hz_to_midi(max_hz) - lo) / BIN_ST).ceil() as usize + 1;

    // Zero-padded copy so every frame reads a full region.
    let pad = len;
    let mut x = vec![0.0f32; sig.x.len() + 2 * pad];
    x[pad..pad + sig.x.len()].copy_from_slice(&sig.x);

    let mut level_db = vec![0.0f32; n_frames];
    let mut vprob = vec![0.0f32; n_frames];
    let mut cands: Vec<Candidate> = Vec::new();
    let mut all: Vec<(f32, f32)> = Vec::new();
    let mut offsets = vec![0usize; n_frames + 1];
    for t in 0..n_frames {
        let c = (t as f64 * HOP_S * rate).round() as isize;
        let start = (c - (len / 2) as isize + pad as isize).max(0) as usize;
        let frame = &x[start..start + len];
        // Level over the window centered on the frame.
        let off = (len - w) / 2;
        let e: f64 = frame[off..off + w]
            .iter()
            .map(|&s| f64::from(s) * f64::from(s))
            .sum();
        level_db[t] = if e > 0.0 {
            ((10.0 * (e / w as f64).log10()) as f32).max(-120.0)
        } else {
            -120.0
        };
        let v = yin.analyze(frame, &mut cands);
        vprob[t] = v;
        for cand in &cands {
            all.push((hz_to_midi(rate / cand.lag) as f32, cand.mass));
        }
        offsets[t + 1] = all.len();
    }

    let mut sorted: Vec<f32> = level_db.iter().copied().filter(|&l| l > -119.0).collect();
    sorted.sort_unstable_by(f32::total_cmp);
    let reference_db = if sorted.is_empty() {
        -120.0
    } else {
        sorted[((sorted.len() - 1) as f64 * REFERENCE_PERCENTILE).round() as usize]
    };
    let gate_db = (reference_db - GATE_BELOW_REF_DB).max(GATE_FLOOR_DB);
    for t in 0..n_frames {
        if level_db[t] < gate_db {
            vprob[t] = 0.0;
        }
    }

    let path = viterbi(n_frames, n_bins, |t, emis: &mut [f32]| {
        let gated = level_db[t] < gate_db;
        let obs = &mut emis[..n_bins];
        obs.fill(0.0);
        if !gated {
            for &(m, mass) in &all[offsets[t]..offsets[t + 1]] {
                let pos = (f64::from(m) - lo) / BIN_ST;
                if pos < 0.0 || pos > (n_bins - 1) as f64 {
                    continue;
                }
                let i = pos.floor() as usize;
                let frac = (pos - i as f64) as f32;
                obs[i] += mass * (1.0 - frac);
                if i + 1 < n_bins {
                    obs[i + 1] += mass * frac;
                }
            }
        }
        for o in obs.iter_mut() {
            *o = -(*o + EMISSION_FLOOR).ln();
        }
        let v = if gated { 0.0 } else { vprob[t] };
        emis[n_bins] = -((1.0 - v).max(0.0) * UNVOICED_FACTOR + EMISSION_FLOOR).ln();
    });

    let mut midi = vec![f32::NAN; n_frames];
    for t in 0..n_frames {
        let s = path[t] as usize;
        if s >= n_bins {
            continue;
        }
        let center = lo + s as f64 * BIN_ST;
        let near = all[offsets[t]..offsets[t + 1]]
            .iter()
            .filter(|(m, _)| ((f64::from(*m) - center) / BIN_ST).abs() <= REFINE_BINS)
            .max_by(|a, b| a.1.total_cmp(&b.1));
        midi[t] = near.map_or(center as f32, |c| c.0);
    }
    Track {
        midi,
        vprob,
        level_db,
        reference_db,
        gate_db,
    }
}

/// Minimum-cost path through `n_bins` pitch states and one unvoiced state
/// (index `n_bins`). `emission(t, costs)` fills the frame's costs (negative
/// log likelihoods). Moving between pitch bins costs [`STEP_COST`] per bin,
/// at most [`JUMP_COST`]; switching voicing costs [`SWITCH_COST`].
fn viterbi(
    n_frames: usize,
    n_bins: usize,
    mut emission: impl FnMut(usize, &mut [f32]),
) -> Vec<u16> {
    let states = n_bins + 1;
    let unvoiced = n_bins;
    if n_frames == 0 {
        return Vec::new();
    }
    let mut back = vec![0u16; n_frames * states];
    let mut cost = vec![0.0f32; states];
    let mut emis = vec![0.0f32; states];
    let mut d = vec![0.0f32; n_bins];
    let mut arg = vec![0u16; n_bins];
    emission(0, &mut emis);
    // Start unvoiced is free; voiced pays a switch.
    for b in 0..n_bins {
        cost[b] = SWITCH_COST + emis[b];
    }
    cost[unvoiced] = emis[unvoiced];
    for t in 1..n_frames {
        emission(t, &mut emis);
        // L1 distance transform over the voiced bins.
        for b in 0..n_bins {
            d[b] = cost[b];
            arg[b] = b as u16;
        }
        for b in 1..n_bins {
            if d[b - 1] + STEP_COST < d[b] {
                d[b] = d[b - 1] + STEP_COST;
                arg[b] = arg[b - 1];
            }
        }
        for b in (0..n_bins - 1).rev() {
            if d[b + 1] + STEP_COST < d[b] {
                d[b] = d[b + 1] + STEP_COST;
                arg[b] = arg[b + 1];
            }
        }
        let (bmin, cmin) =
            cost[..n_bins]
                .iter()
                .enumerate()
                .fold(
                    (0, f32::INFINITY),
                    |acc, (i, &c)| if c < acc.1 { (i, c) } else { acc },
                );
        let cu = cost[unvoiced];
        let row = &mut back[t * states..(t + 1) * states];
        let mut best = f32::INFINITY;
        for b in 0..n_bins {
            let (mut v, mut a) = (d[b], arg[b]);
            if cmin + JUMP_COST < v {
                v = cmin + JUMP_COST;
                a = bmin as u16;
            }
            if cu + SWITCH_COST < v {
                v = cu + SWITCH_COST;
                a = unvoiced as u16;
            }
            row[b] = a;
            cost[b] = v + emis[b];
            best = best.min(cost[b]);
        }
        let (v, a) = if cmin + SWITCH_COST < cu {
            (cmin + SWITCH_COST, bmin as u16)
        } else {
            (cu, unvoiced as u16)
        };
        row[unvoiced] = a;
        cost[unvoiced] = v + emis[unvoiced];
        best = best.min(cost[unvoiced]);
        for c in &mut cost {
            *c -= best;
        }
    }
    let mut path = vec![0u16; n_frames];
    let mut s = (0..states)
        .min_by(|&a, &b| cost[a].total_cmp(&cost[b]))
        .expect("states") as u16;
    for t in (0..n_frames).rev() {
        path[t] = s;
        if t > 0 {
            s = back[t * states + s as usize];
        }
    }
    path
}
