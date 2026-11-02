//! STFT glue of the `demucs-hybrid` adapter (== `HTDemucs._spec` / `_ispec`).
//! Reference: `spec` / `ispec` in `tools/reference/demucs_hybrid.py`.

use std::sync::Arc;

use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

use crate::{hann_periodic_f64, reflect_pad};

/// Demucs hybrid spectrogram: `n_fft/2` bins (Nyquist dropped) and
/// `ceil(L / hop)` frames. Planes are `(bins, frames)` row-major.
pub struct HybridStft {
    n_fft: usize,
    hop: usize,
    /// window / sqrt(n_fft), f32 (forward, `normalized=True`)
    win_fwd: Vec<f32>,
    /// f32(sqrt(n_fft)) * window, f32 (inverse)
    win_inv: Vec<f32>,
    /// windowÂ², f32 (overlap-add envelope)
    win_sq: Vec<f32>,
    fwd: Arc<dyn RealToComplex<f32>>,
    inv: Arc<dyn ComplexToReal<f32>>,
}

impl HybridStft {
    pub fn new(n_fft: usize, hop: usize) -> Self {
        assert!(n_fft.is_multiple_of(2) && hop > 0 && hop * 2 <= n_fft);
        let w = hann_periodic_f64(n_fft);
        let sq = (n_fft as f64).sqrt();
        let w32: Vec<f32> = w.iter().map(|&v| v as f32).collect();
        let mut planner = RealFftPlanner::<f32>::new();
        Self {
            n_fft,
            hop,
            win_fwd: w.iter().map(|&v| (v / sq) as f32).collect(),
            win_inv: w32.iter().map(|&v| sq as f32 * v).collect(),
            win_sq: w32.iter().map(|&v| v * v).collect(),
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

    /// Frequency bins in the model's spectrogram: `n_fft/2`.
    pub fn bins(&self) -> usize {
        self.n_fft / 2
    }

    /// Frames for `len` samples: `ceil(len / hop)`.
    pub fn frames(&self, len: usize) -> usize {
        len.div_ceil(self.hop)
    }

    /// Forward spectrogram of one channel into real/imag planes, each
    /// `bins * frames` long.
    ///
    /// Reflect-pad `3*hop/2` left and `3*hop/2 + (le*hop - L)` right, then
    /// reflect-pad `n_fft/2` both sides (center=True), frame with hop,
    /// window / sqrt(n_fft), rfft; drop the Nyquist bin and keep frames
    /// `[2, 2 + le)`.
    pub fn spec(&self, x: &[f32], re: &mut [f32], im: &mut [f32]) {
        let (n_fft, hop) = (self.n_fft, self.hop);
        let len = x.len();
        let le = self.frames(len);
        let bins = self.bins();
        assert_eq!(re.len(), bins * le);
        assert_eq!(im.len(), bins * le);
        let pad = hop / 2 * 3;
        let p1 = reflect_pad(x, pad, pad + le * hop - len);
        let p2 = reflect_pad(&p1, n_fft / 2, n_fft / 2);
        let mut buf = self.fwd.make_input_vec();
        let mut spec = self.fwd.make_output_vec();
        let mut scratch = self.fwd.make_scratch_vec();
        for t in 0..le {
            let start = (t + 2) * hop;
            for (k, b) in buf.iter_mut().enumerate() {
                *b = p2[start + k] * self.win_fwd[k];
            }
            self.fwd
                .process_with_scratch(&mut buf, &mut spec, &mut scratch)
                .expect("rfft size mismatch");
            for f in 0..bins {
                re[f * le + t] = spec[f].re;
                im[f * le + t] = spec[f].im;
            }
        }
    }

    /// Inverse of [`spec`](Self::spec): `(bins, frames)` planes -> `length`
    /// samples. Append a zero Nyquist bin, zero-pad 2 frames each side,
    /// irfft, multiply by sqrt(n_fft)Â·window, overlap-add, divide by the
    /// summed squared window (floor 1e-11), drop `n_fft/2` then `3*hop/2`.
    pub fn ispec(&self, re: &[f32], im: &[f32], length: usize) -> Vec<f32> {
        let (n_fft, hop) = (self.n_fft, self.hop);
        let bins = self.bins();
        let frames = re.len() / bins;
        assert_eq!(re.len(), bins * frames);
        assert_eq!(im.len(), re.len());
        let n = frames + 4;
        let out_len = (n - 1) * hop + n_fft;
        let mut out = vec![0.0f32; out_len];
        let mut env = vec![0.0f32; out_len];
        for j in 0..n {
            let o = j * hop;
            for (e, w) in env[o..o + n_fft].iter_mut().zip(&self.win_sq) {
                *e += w;
            }
        }
        let mut spec = self.inv.make_input_vec();
        let mut buf = self.inv.make_output_vec();
        let mut scratch = self.inv.make_scratch_vec();
        let scale = 1.0 / n_fft as f32;
        for t in 0..frames {
            for f in 0..bins {
                spec[f] = Complex32::new(re[f * frames + t], im[f * frames + t]);
            }
            spec[0].im = 0.0;
            spec[bins] = Complex32::new(0.0, 0.0);
            self.inv
                .process_with_scratch(&mut spec, &mut buf, &mut scratch)
                .expect("irfft size mismatch");
            let o = (t + 2) * hop;
            for k in 0..n_fft {
                out[o + k] += buf[k] * scale * self.win_inv[k];
            }
        }
        let start = n_fft / 2 + hop / 2 * 3;
        (0..length)
            .map(|i| {
                let j = start + i;
                out[j] / env[j].max(1e-11)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_small() {
        let s = HybridStft::new(64, 16);
        let len = 1000;
        let x: Vec<f32> = (0..len)
            .map(|i| (i as f32 * 0.07).sin() * 0.4 + ((i * 13 % 7) as f32 - 3.0) * 0.01)
            .collect();
        let fr = s.frames(len);
        let mut re = vec![0.0; s.bins() * fr];
        let mut im = vec![0.0; s.bins() * fr];
        s.spec(&x, &mut re, &mut im);
        let y = s.ispec(&re, &im, len);
        // Like demucs, frames 0-1 and the last two are dropped, so the edges
        // don't reconstruct; the dropped Nyquist bin makes the rest
        // approximate, but close.
        let mid = 128..len - 128;
        let err: f32 = x[mid.clone()]
            .iter()
            .zip(&y[mid.clone()])
            .map(|(a, b)| (a - b) * (a - b))
            .sum();
        let pow: f32 = x[mid].iter().map(|a| a * a).sum();
        assert!(10.0 * (pow / err).log10() > 40.0);
    }
}
