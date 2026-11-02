// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! `torch.stft` / `torch.istft` equivalent (center=True, reflect padding,
//! periodic Hann, not normalized). Reference: `stft` / `istft` in
//! `tools/reference/roformer_separate.py`.
//!
//! Like the numpy reference, frames are windowed in f32 and transformed in
//! f64, and spectra stay f64: the RoFormer graph amplifies tiny input
//! differences (a -138 dB STFT rounding difference became -67 dB in its
//! mask output), so matching the reference's precision matters.

use std::sync::Arc;

use realfft::num_complex::Complex64;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

use crate::{hann_periodic, reflect_index};

/// STFT with `center=True` framing. Spectra are `(bins, frames)` row-major
/// (`bins = n_fft/2 + 1`, frames fastest).
pub struct Stft {
    n_fft: usize,
    hop: usize,
    window: Vec<f32>,
    fwd: Arc<dyn RealToComplex<f64>>,
    inv: Arc<dyn ComplexToReal<f64>>,
}

impl Stft {
    pub fn new(n_fft: usize, hop: usize) -> Self {
        assert!(n_fft >= 2 && n_fft.is_multiple_of(2) && hop > 0);
        let mut planner = RealFftPlanner::<f64>::new();
        Self {
            n_fft,
            hop,
            window: hann_periodic(n_fft),
            fwd: planner.plan_fft_forward(n_fft),
            inv: planner.plan_fft_inverse(n_fft),
        }
    }

    pub fn n_fft(&self) -> usize {
        self.n_fft
    }

    pub fn hop(&self) -> usize {
        self.hop
    }

    /// Number of frequency bins, `n_fft/2 + 1`.
    pub fn bins(&self) -> usize {
        self.n_fft / 2 + 1
    }

    /// Number of frames for a signal of `len` samples: `1 + len / hop`.
    pub fn frames(&self, len: usize) -> usize {
        1 + len / self.hop
    }

    /// Forward STFT of one channel -> `(bins, frames)` complex.
    pub fn forward(&self, x: &[f32]) -> Vec<Complex64> {
        let (n_fft, hop) = (self.n_fft, self.hop);
        let pad = (n_fft / 2) as isize;
        let frames = self.frames(x.len());
        let bins = self.bins();
        let mut out = vec![Complex64::new(0.0, 0.0); bins * frames];
        let mut buf = self.fwd.make_input_vec();
        let mut spec = self.fwd.make_output_vec();
        let mut scratch = self.fwd.make_scratch_vec();
        for t in 0..frames {
            let start = (t * hop) as isize - pad;
            for (k, b) in buf.iter_mut().enumerate() {
                *b = (x[reflect_index(start + k as isize, x.len())] * self.window[k]) as f64;
            }
            self.fwd
                .process_with_scratch(&mut buf, &mut spec, &mut scratch)
                .expect("rfft size mismatch");
            for (f, v) in spec.iter().enumerate() {
                out[f * frames + t] = *v;
            }
        }
        out
    }

    /// Inverse STFT of a `(bins, frames)` spectrum -> `length` samples.
    /// irfft, window, overlap-add, divide by the summed squared window
    /// (floor 1e-8), drop `n_fft/2` at the start.
    pub fn inverse(&self, z: &[Complex64], frames: usize, length: usize) -> Vec<f32> {
        let (n_fft, hop) = (self.n_fft, self.hop);
        let bins = self.bins();
        assert_eq!(z.len(), bins * frames);
        let out_len = n_fft + hop * (frames.saturating_sub(1));
        let mut out = vec![0.0f32; out_len];
        let mut norm = vec![0.0f32; out_len];
        let mut spec = self.inv.make_input_vec();
        let mut buf = self.inv.make_output_vec();
        let mut scratch = self.inv.make_scratch_vec();
        let scale = 1.0 / n_fft as f64;
        for t in 0..frames {
            for (f, s) in spec.iter_mut().enumerate() {
                *s = z[f * frames + t];
            }
            // irfft ignores the imaginary parts of DC and Nyquist.
            spec[0].im = 0.0;
            spec[bins - 1].im = 0.0;
            self.inv
                .process_with_scratch(&mut spec, &mut buf, &mut scratch)
                .expect("irfft size mismatch");
            let o = t * hop;
            for k in 0..n_fft {
                let w = self.window[k];
                out[o + k] += (buf[k] * scale * w as f64) as f32;
                norm[o + k] += w * w;
            }
        }
        let pad = n_fft / 2;
        (0..length)
            .map(|i| {
                let j = pad + i;
                if j < out_len {
                    out[j] / norm[j].max(1e-8)
                } else {
                    0.0
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive_dft_bin(frame: &[f32], f: usize) -> (f64, f64) {
        let n = frame.len();
        let (mut re, mut im) = (0.0, 0.0);
        for (k, &v) in frame.iter().enumerate() {
            let a = -2.0 * std::f64::consts::PI * (f * k) as f64 / n as f64;
            re += v as f64 * a.cos();
            im += v as f64 * a.sin();
        }
        (re, im)
    }

    #[test]
    fn forward_matches_naive_dft() {
        let n_fft = 16;
        let hop = 4;
        let x: Vec<f32> = (0..37).map(|i| ((i * 7 % 11) as f32 - 5.0) / 5.0).collect();
        let stft = Stft::new(n_fft, hop);
        let z = stft.forward(&x);
        let frames = stft.frames(x.len());
        assert_eq!(frames, 10);
        let w = hann_periodic(n_fft);
        for t in [0, 3, frames - 1] {
            let frame: Vec<f32> = (0..n_fft)
                .map(|k| {
                    x[reflect_index((t * hop + k) as isize - (n_fft / 2) as isize, x.len())] * w[k]
                })
                .collect();
            for f in [0, 1, 5, 8] {
                let (re, im) = naive_dft_bin(&frame, f);
                let got = z[f * frames + t];
                assert!((got.re - re).abs() < 1e-4, "t{t} f{f}");
                assert!((got.im - im).abs() < 1e-4, "t{t} f{f}");
            }
        }
    }

    #[test]
    fn roundtrip() {
        let stft = Stft::new(64, 16);
        let x: Vec<f32> = (0..1000)
            .map(|i| (i as f32 * 0.05).sin() * 0.5 + (i as f32 * 0.31).cos() * 0.1)
            .collect();
        let z = stft.forward(&x);
        let y = stft.inverse(&z, stft.frames(x.len()), x.len());
        let err = x
            .iter()
            .zip(&y)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(err < 1e-5, "max err {err}");
    }
}
