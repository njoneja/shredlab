// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Live pitch detection for the tuner: the McLeod Pitch Method on a stream
//! of mono samples, one reading per hop.

use std::f64::consts::TAU;
use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

/// Readings whose clarity (NSDF value at the chosen peak) is below this
/// have no pitch.
pub const MIN_CLARITY: f32 = 0.85;

/// Readings whose window is quieter than this (RMS, dBFS) have no pitch.
/// About 20 dB above the noise floor of a typical interface input with the
/// gain up, and well below a plucked string at a sensible input level.
pub const QUIET_DB: f32 = -60.0;

/// The level reported for digital silence.
pub const LEVEL_FLOOR_DB: f32 = -120.0;

/// Below this lowest target frequency the detector uses the long window.
const LONG_WINDOW_BELOW_HZ: f32 = 45.0;
/// Window lengths and hop at 48 kHz; other rates scale them.
const WINDOW_48K: f64 = 4096.0;
const LONG_WINDOW_48K: f64 = 8192.0;
const HOP_48K: f64 = 512.0;
/// High-pass cutoff in Hz (2nd order Butterworth).
const HIGH_PASS_HZ: f64 = 25.0;
/// The chosen key maximum is the first one at or above this fraction of
/// the highest.
const PEAK_THRESHOLD: f32 = 0.9;
/// Readings in the median.
const MEDIAN_LEN: usize = 5;
/// Accepted readings needed before a median is reported.
const MEDIAN_MIN: usize = 3;
/// Consecutive rejected readings after which the median starts over.
const DROPOUT_HOPS: u32 = 8;

/// One detector reading, produced every hop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiveReading {
    /// Detected fundamental in Hz: the median of the last few accepted
    /// readings. `None` when this reading was rejected (clarity below
    /// [`MIN_CLARITY`] or level below [`QUIET_DB`]) or fewer than three
    /// readings have been accepted since the last dropout.
    pub hz: Option<f32>,
    /// How periodic the window is, 0..1 (NSDF value at the chosen peak;
    /// 0 when no peak was found).
    pub clarity: f32,
    /// RMS level of the high-passed window in dBFS (a full-scale square
    /// wave is 0, a full-scale sine −3), at least [`LEVEL_FLOOR_DB`].
    pub level_db: f32,
}

/// Streaming pitch detector. Feed it samples with [`LiveDetector::push`];
/// it emits one [`LiveReading`] per hop once the first window is full.
/// Pure DSP: no devices, no threads, no allocation after construction.
pub struct LiveDetector {
    rate: f64,
    window: usize,
    hop: usize,
    /// Largest lag searched, half the window.
    max_lag: usize,
    high_pass: HighPass,
    /// High-passed samples, circular, `window` long.
    ring: Vec<f32>,
    pos: usize,
    filled: usize,
    since_hop: usize,
    fft: Arc<dyn RealToComplex<f32>>,
    ifft: Arc<dyn ComplexToReal<f32>>,
    frame: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    scratch_fwd: Vec<Complex<f32>>,
    scratch_inv: Vec<Complex<f32>>,
    acf: Vec<f32>,
    nsdf: Vec<f32>,
    /// Key maxima of the NSDF: (interpolated lag, value).
    maxima: Vec<(f64, f32)>,
    recent: [f32; MEDIAN_LEN],
    recent_len: usize,
    recent_next: usize,
    dropout: u32,
}

impl LiveDetector {
    /// A detector for `sample_rate` Hz input whose lowest target note is
    /// `low_hz`. The window is 4096 samples at 48 kHz, or 8192 when `low_hz`
    /// is below 45 Hz (the low B of a 5-string bass); the hop is 512. Other
    /// rates scale both, so the durations stay the same. Notes below
    /// `low_hz` may read an octave high.
    pub fn new(sample_rate: u32, low_hz: f32) -> Self {
        let rate = f64::from(sample_rate.max(1));
        let scale = rate / 48_000.0;
        let base = if low_hz < LONG_WINDOW_BELOW_HZ {
            LONG_WINDOW_48K
        } else {
            WINDOW_48K
        };
        let window = ((base * scale).round() as usize).max(64);
        let hop = ((HOP_48K * scale).round() as usize).max(1);
        let max_lag = window / 2;
        // Zero padding to at least twice the window keeps the circular
        // autocorrelation from wrapping.
        let n = (2 * window).next_power_of_two();
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(n);
        let ifft = planner.plan_fft_inverse(n);
        Self {
            rate,
            window,
            hop,
            max_lag,
            high_pass: HighPass::new(rate, HIGH_PASS_HZ),
            ring: vec![0.0; window],
            pos: 0,
            filled: 0,
            since_hop: 0,
            frame: fft.make_input_vec(),
            spectrum: fft.make_output_vec(),
            scratch_fwd: fft.make_scratch_vec(),
            scratch_inv: ifft.make_scratch_vec(),
            acf: ifft.make_output_vec(),
            nsdf: vec![0.0; max_lag + 1],
            maxima: Vec::with_capacity(max_lag / 2 + 1),
            recent: [0.0; MEDIAN_LEN],
            recent_len: 0,
            recent_next: 0,
            dropout: 0,
            fft,
            ifft,
        }
    }

    /// Analysis window length in samples.
    pub fn window_len(&self) -> usize {
        self.window
    }

    /// Samples between readings.
    pub fn hop_len(&self) -> usize {
        self.hop
    }

    /// Clears the filter, the buffered samples and the median, as for a
    /// newly started input.
    pub fn reset(&mut self) {
        self.high_pass.reset();
        self.ring.fill(0.0);
        self.pos = 0;
        self.filled = 0;
        self.since_hop = 0;
        self.recent_len = 0;
        self.recent_next = 0;
        self.dropout = 0;
    }

    /// Feeds mono samples (any count) and appends one reading to `out` for
    /// each hop completed. Reuse `out` between calls to avoid allocating.
    pub fn push(&mut self, samples: &[f32], out: &mut Vec<LiveReading>) {
        for &x in samples {
            self.ring[self.pos] = self.high_pass.process(x);
            self.pos += 1;
            if self.pos == self.window {
                self.pos = 0;
            }
            if self.filled < self.window {
                self.filled += 1;
                if self.filled < self.window {
                    continue;
                }
                // First full window: read at once.
                self.since_hop = self.hop;
            } else {
                self.since_hop += 1;
            }
            if self.since_hop >= self.hop {
                self.since_hop = 0;
                out.push(self.analyze());
            }
        }
    }

    fn analyze(&mut self) -> LiveReading {
        let (clarity, level_db, lag) = self.detect();
        let accepted = lag.filter(|_| clarity >= MIN_CLARITY && level_db >= QUIET_DB);
        let hz = match accepted {
            Some(lag) => {
                self.dropout = 0;
                self.recent[self.recent_next] = (self.rate / lag) as f32;
                self.recent_next = (self.recent_next + 1) % MEDIAN_LEN;
                self.recent_len = (self.recent_len + 1).min(MEDIAN_LEN);
                (self.recent_len >= MEDIAN_MIN).then(|| self.median())
            }
            None => {
                self.dropout += 1;
                if self.dropout >= DROPOUT_HOPS {
                    self.recent_len = 0;
                    self.recent_next = 0;
                }
                None
            }
        };
        LiveReading {
            hz,
            clarity,
            level_db,
        }
    }

    /// Median of the buffered readings (the upper middle one for an even
    /// count, so it is always a real reading).
    fn median(&self) -> f32 {
        let mut v = [0.0f32; MEDIAN_LEN];
        let v = &mut v[..self.recent_len];
        v.copy_from_slice(&self.recent[..self.recent_len]);
        v.sort_unstable_by(f32::total_cmp);
        v[v.len() / 2]
    }

    /// Runs MPM on the current window: (clarity, level in dBFS, period in
    /// samples).
    fn detect(&mut self) -> (f32, f32, Option<f64>) {
        let w = self.window;
        let (older, newer) = self.ring.split_at(self.pos);
        self.frame[..newer.len()].copy_from_slice(newer);
        self.frame[newer.len()..w].copy_from_slice(older);
        self.frame[w..].fill(0.0);

        let x = &self.frame[..w];
        let energy: f64 = x.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
        let level_db = if energy > 0.0 {
            ((10.0 * (energy / w as f64).log10()) as f32).max(LEVEL_FLOOR_DB)
        } else {
            LEVEL_FLOOR_DB
        };
        if level_db <= LEVEL_FLOOR_DB {
            return (0.0, level_db, None);
        }

        // Autocorrelation r(τ) = Σ x_j x_{j+τ} via the power spectrum.
        self.fft
            .process_with_scratch(&mut self.frame, &mut self.spectrum, &mut self.scratch_fwd)
            .expect("buffer sizes come from the plan");
        for c in &mut self.spectrum {
            *c = Complex::new(c.norm_sqr(), 0.0);
        }
        self.ifft
            .process_with_scratch(&mut self.spectrum, &mut self.acf, &mut self.scratch_inv)
            .expect("buffer sizes come from the plan");
        // The forward FFT used `frame` as scratch; m(τ) reads the ring.
        let n = self.acf.len() as f64;

        // NSDF n(τ) = 2 r(τ) / m(τ), m(τ) = Σ_{j<w−τ} x_j² + x_{j+τ}².
        let ring = &self.ring;
        let pos = self.pos;
        let sample = |i: usize| {
            let k = pos + i;
            f64::from(ring[if k >= w { k - w } else { k }])
        };
        let mut m = 2.0 * energy;
        for tau in 0..=self.max_lag {
            self.nsdf[tau] = if m > 0.0 {
                (2.0 * f64::from(self.acf[tau]) / n / m) as f32
            } else {
                0.0
            };
            let (a, b) = (sample(tau), sample(w - 1 - tau));
            m -= a * a + b * b;
        }

        self.find_key_maxima();
        if self.maxima.is_empty() {
            return (0.0, level_db, None);
        }
        let highest = self.maxima.iter().map(|m| m.1).fold(f32::MIN, f32::max);
        let threshold = PEAK_THRESHOLD * highest;
        let (mut lag, mut clarity) = *self
            .maxima
            .iter()
            .find(|m| m.1 >= threshold)
            .expect("the highest maximum passes");

        // Octave guard. A weak fundamental under a strong 2nd harmonic puts
        // a key maximum at half the period that can pass the threshold. If
        // the maximum near twice the chosen lag is clearly more periodic
        // (its distance from 1 at most half as large), the chosen lag was
        // the 2nd harmonic.
        if let Some(&(lag2, value2)) = self
            .maxima
            .iter()
            .filter(|m| (m.0 / (2.0 * lag) - 1.0).abs() < 0.03)
            .max_by(|a, b| a.1.total_cmp(&b.1))
        {
            if value2 > clarity + 0.01 && (1.0 - value2) < 0.5 * (1.0 - clarity) {
                lag = lag2;
                clarity = value2;
            }
        }
        (clarity.min(1.0), level_db, Some(lag))
    }

    /// Fills `maxima` with the NSDF key maxima: the highest point between
    /// each positive-going zero crossing and the next negative-going one,
    /// after the first negative-going crossing, refined by parabolic
    /// interpolation.
    fn find_key_maxima(&mut self) {
        self.maxima.clear();
        let f = &self.nsdf;
        let len = f.len();
        let mut t = 1;
        while t < len && f[t] > 0.0 {
            t += 1;
        }
        let mut best: Option<usize> = None;
        while t < len - 1 {
            if f[t] > 0.0 {
                match best {
                    Some(b) if f[b] >= f[t] => {}
                    _ => best = Some(t),
                }
            } else if let Some(b) = best.take() {
                self.maxima.push(interpolate(f, b));
            }
            t += 1;
        }
        // A region still open at the end counts only if its highest point is
        // a real local maximum.
        if let Some(b) = best {
            if b < len - 2 && f[b] >= f[b + 1] {
                self.maxima.push(interpolate(f, b));
            }
        }
    }
}

/// Parabolic interpolation around index `k`: (lag, value).
fn interpolate(f: &[f32], k: usize) -> (f64, f32) {
    let (a, b, c) = (f64::from(f[k - 1]), f64::from(f[k]), f64::from(f[k + 1]));
    let denom = a - 2.0 * b + c;
    if denom >= 0.0 {
        return (k as f64, f[k]);
    }
    let p = (0.5 * (a - c) / denom).clamp(-0.5, 0.5);
    (k as f64 + p, (b - 0.25 * (a - c) * p) as f32)
}

/// 2nd-order Butterworth high-pass (RBJ biquad), direct form I in f64.
struct HighPass {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl HighPass {
    fn new(rate: f64, cutoff: f64) -> Self {
        let w0 = TAU * cutoff / rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 + cos) / 2.0 / a0,
            b1: -(1.0 + cos) / a0,
            b2: (1.0 + cos) / 2.0 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }

    fn process(&mut self, x: f32) -> f32 {
        let x = f64::from(x);
        let mut y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        // Flush tiny values so silence never reaches denormals.
        if y.abs() < 1e-30 {
            y = 0.0;
        }
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y as f32
    }
}
