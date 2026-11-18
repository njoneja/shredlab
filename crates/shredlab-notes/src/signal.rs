// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! The analysis signal: the stem low-passed and decimated to a rate that
//! fits the stem's range, then high-passed to remove rumble and DC.

use std::f64::consts::{PI, TAU};

use crate::StemKind;

/// Decimated mono signal at the analysis rate.
pub(crate) struct Signal {
    pub x: Vec<f32>,
    pub rate: f64,
}

/// Analysis rate the decimation aims for (it stays at or above it): high
/// enough for several harmonics of the stem's highest note, and for a
/// period of at least eight samples (the period is interpolated between
/// samples, and fewer cost precision).
fn target_rate(kind: StemKind) -> f64 {
    match kind {
        StemKind::Bass => 11_025.0,
        _ => 16_000.0,
    }
}

/// Low-pass taps per unit of the decimation factor, on each side.
const TAPS_PER_FACTOR: usize = 10;

pub(crate) fn prepare(samples: &[f32], rate: f64, kind: StemKind) -> Signal {
    let factor = ((rate / target_rate(kind)).floor() as usize).max(1);
    let out_rate = rate / factor as f64;
    let mut x = if factor == 1 {
        samples.to_vec()
    } else {
        decimate(samples, factor)
    };
    let (min_hz, _) = kind.range_hz();
    let mut hp = HighPass::new(out_rate, (0.7 * min_hz).min(40.0));
    for s in &mut x {
        *s = hp.process(*s);
    }
    Signal { x, rate: out_rate }
}

/// Windowed-sinc (Blackman) low-pass at 0.45 of the output rate, evaluated
/// only at the kept samples.
fn decimate(samples: &[f32], factor: usize) -> Vec<f32> {
    let half = TAPS_PER_FACTOR * factor;
    let cutoff = 0.45 / factor as f64; // cycles per input sample
    let taps: Vec<f32> = (0..=2 * half)
        .map(|i| {
            let k = i as f64 - half as f64;
            let sinc = if k == 0.0 {
                2.0 * cutoff
            } else {
                (TAU * cutoff * k).sin() / (PI * k)
            };
            let w = 0.42 - 0.5 * (PI * i as f64 / half as f64).cos()
                + 0.08 * (TAU * i as f64 / half as f64).cos();
            (sinc * w) as f32
        })
        .collect();
    let n_out = samples.len().div_ceil(factor);
    let mut out = Vec::with_capacity(n_out);
    for j in 0..n_out {
        let c = j * factor;
        let mut acc = 0.0f32;
        if c >= half && c + half < samples.len() {
            for (s, t) in samples[c - half..=c + half].iter().zip(&taps) {
                acc += s * t;
            }
        } else {
            for (i, t) in taps.iter().enumerate() {
                let k = c as isize + i as isize - half as isize;
                if k >= 0 && (k as usize) < samples.len() {
                    acc += samples[k as usize] * t;
                }
            }
        }
        out.push(acc);
    }
    out
}

/// 2nd-order Butterworth high-pass (RBJ biquad), direct form I in f64.
pub(crate) struct HighPass {
    b: [f64; 3],
    a: [f64; 2],
    x: [f64; 2],
    y: [f64; 2],
}

impl HighPass {
    pub fn new(rate: f64, cutoff: f64) -> Self {
        let w0 = TAU * cutoff / rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;
        Self {
            b: [
                (1.0 + cos) / 2.0 / a0,
                -(1.0 + cos) / a0,
                (1.0 + cos) / 2.0 / a0,
            ],
            a: [-2.0 * cos / a0, (1.0 - alpha) / a0],
            x: [0.0; 2],
            y: [0.0; 2],
        }
    }

    pub fn process(&mut self, x: f32) -> f32 {
        let x = f64::from(x);
        let mut y = self.b[0] * x + self.b[1] * self.x[0] + self.b[2] * self.x[1]
            - self.a[0] * self.y[0]
            - self.a[1] * self.y[1];
        if y.abs() < 1e-30 {
            y = 0.0;
        }
        self.x = [x, self.x[0]];
        self.y = [y, self.y[0]];
        y as f32
    }
}
