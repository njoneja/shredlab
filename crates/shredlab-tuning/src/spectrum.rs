// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Hann-windowed magnitude spectra and spectral peak picking.

use std::f64::consts::TAU;
use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

/// A spectral peak: refined frequency and linear magnitude.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Peak {
    pub hz: f64,
    pub mag: f64,
}

/// Peaks quieter than this fraction of the frame's strongest bin (−40 dB)
/// are ignored.
const PEAK_FLOOR: f32 = 0.01;

/// Exponent applied to the magnitudes before quadratic peak interpolation.
/// With a Hann window, interpolating on `|X|^0.23` is nearly unbiased; on
/// the log magnitude the error reaches 0.016 bins, several cents for bass
/// notes.
const POWER: f64 = 0.2308;

/// Reusable FFT state for one window length.
pub struct Analyzer {
    n: usize,
    sample_rate: f64,
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    input: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    mags: Vec<f32>,
}

impl Analyzer {
    pub fn new(n: usize, sample_rate: f64) -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(n);
        // Periodic Hann.
        let window = (0..n)
            .map(|i| (0.5 - 0.5 * (TAU * i as f64 / n as f64).cos()) as f32)
            .collect();
        Self {
            n,
            sample_rate,
            input: fft.make_input_vec(),
            spectrum: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            mags: vec![0.0; n / 2 + 1],
            fft,
            window,
        }
    }

    /// Finds the peaks of `frame` (zero-padded to the window length) between
    /// `min_hz` and `max_hz`: local maxima within 40 dB of the strongest bin
    /// in that range, refined by quadratic interpolation on the log
    /// magnitude. Peaks are appended to `out` in ascending frequency.
    pub fn peaks(&mut self, frame: &[f32], min_hz: f64, max_hz: f64, out: &mut Vec<Peak>) {
        let n = self.n;
        for (i, x) in self.input.iter_mut().enumerate() {
            *x = frame.get(i).copied().unwrap_or(0.0) * self.window[i];
        }
        self.fft
            .process_with_scratch(&mut self.input, &mut self.spectrum, &mut self.scratch)
            .expect("buffer sizes come from the plan");

        let bin_hz = self.sample_rate / n as f64;
        let lo = ((min_hz / bin_hz).ceil() as usize).max(1);
        let hi = ((max_hz / bin_hz).floor() as usize).min(n / 2 - 1);
        if lo > hi {
            return;
        }
        // Only the band plus one neighbor on each side is needed.
        for k in lo - 1..=hi + 1 {
            self.mags[k] = self.spectrum[k].norm();
        }
        let band = &self.mags[lo..=hi];
        let max = band.iter().copied().fold(0.0f32, f32::max);
        if max <= f32::MIN_POSITIVE {
            return;
        }
        let floor = max * PEAK_FLOOR;
        let m = &self.mags;
        for k in lo..=hi {
            let b = m[k];
            if b < floor || b <= m[k - 1] || b < m[k + 1] {
                continue;
            }
            let (a, b, c) = (
                f64::from(m[k - 1]).powf(POWER),
                f64::from(b).powf(POWER),
                f64::from(m[k + 1]).powf(POWER),
            );
            let denom = a - 2.0 * b + c;
            let p = if denom < 0.0 {
                (0.5 * (a - c) / denom).clamp(-0.5, 0.5)
            } else {
                0.0
            };
            out.push(Peak {
                hz: (k as f64 + p) * bin_hz,
                mag: (b - 0.25 * (a - c) * p).powf(1.0 / POWER),
            });
        }
    }
}

/// Largest harmonic number the harmonic filter checks. Above it the ±20
/// cent windows of neighboring harmonics overlap, so the test means
/// nothing.
const MAX_HARMONIC: f64 = 34.0;

/// Removes peaks within ±20 cents of a non-octave multiple (3, 5, 6, 7, 9,
/// ...) of a stronger lower peak. `peaks` must be sorted by frequency.
pub fn drop_non_octave_harmonics(peaks: &mut Vec<Peak>) {
    let tolerance = 2f64.powf(20.0 / 1200.0);
    let mut keep = vec![true; peaks.len()];
    for j in 1..peaks.len() {
        let pj = peaks[j];
        for pi in &peaks[..j] {
            if pi.mag <= pj.mag {
                continue;
            }
            let ratio = pj.hz / pi.hz;
            let h = ratio.round();
            if !(3.0..=MAX_HARMONIC).contains(&h) || is_power_of_two(h) {
                continue;
            }
            let r = ratio / h;
            if r < tolerance && r > 1.0 / tolerance {
                keep[j] = false;
                break;
            }
        }
    }
    let mut i = 0;
    peaks.retain(|_| {
        i += 1;
        keep[i - 1]
    });
}

fn is_power_of_two(h: f64) -> bool {
    (h as u64).is_power_of_two()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f64, rate: f64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (TAU * hz * i as f64 / rate).sin() as f32)
            .collect()
    }

    #[test]
    fn single_sine_peak_is_refined() {
        let rate = 44_100.0;
        let mut a = Analyzer::new(8192, rate);
        // From 6 to 230 bins, at several fractional positions.
        for hz in [32.7, 41.2, 55.3, 61.1, 110.0, 261.63, 440.0, 1234.5] {
            let mut peaks = Vec::new();
            a.peaks(&sine(hz, rate, 8192), 30.0, 2000.0, &mut peaks);
            assert_eq!(peaks.len(), 1, "{hz}: {peaks:?}");
            let cents = 1200.0 * (peaks[0].hz / hz).log2();
            assert!(cents.abs() < 0.2, "{hz}: {cents} cents");
        }
    }

    #[test]
    fn harmonic_filter_keeps_octaves() {
        let mk = |hz, mag| Peak { hz, mag };
        let mut peaks = vec![
            mk(100.0, 1.0),
            mk(200.0, 0.5),
            mk(300.4, 0.3),
            mk(400.0, 0.2),
            mk(500.0, 0.1),
            mk(530.0, 0.1),
        ];
        drop_non_octave_harmonics(&mut peaks);
        let hz: Vec<f64> = peaks.iter().map(|p| p.hz).collect();
        assert_eq!(hz, vec![100.0, 200.0, 400.0, 530.0]);
    }
}
