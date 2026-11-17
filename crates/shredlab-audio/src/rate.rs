// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Playback-rate correction: the song plays `2^(cents/1200)` times faster,
//! pitch and tempo together (like a tape-speed change). Applied in the
//! engine's device-rate resampler and, for export, by resampling the
//! finished render.

use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

/// Largest rate correction in cents, either way (about ±12% tempo).
pub const MAX_RATE_CENTS: i32 = 200;

/// Headroom of a resampler built for a variable ratio; covers
/// `2^(MAX_RATE_CENTS/1200)` (1.1225).
pub(crate) const RATIO_HEADROOM: f64 = 1.13;

/// Clamps to `-MAX_RATE_CENTS..=MAX_RATE_CENTS`.
pub(crate) fn clamp_cents(cents: i32) -> i32 {
    cents.clamp(-MAX_RATE_CENTS, MAX_RATE_CENTS)
}

/// Playback rate for a correction: song seconds per output second.
pub(crate) fn rate(cents: i32) -> f64 {
    2f64.powf(clamp_cents(cents) as f64 / 1200.0)
}

/// Sinc parameters shared by playback and export.
pub(crate) fn sinc_params() -> SincInterpolationParameters {
    SincInterpolationParameters {
        sinc_len: 128,
        f_cutoff: 0.95,
        oversampling_factor: 128,
        interpolation: SincInterpolationType::Cubic,
        window: WindowFunction::BlackmanHarris2,
    }
}

const OFFLINE_CHUNK: usize = 16_384;

/// Resamples a finished stereo render so it plays `rate(cents)` times
/// faster at the same sample rate: `round(len / rate)` frames, aligned
/// (rubato's `SincFixedIn` output starts at input time 0; the end is
/// flushed with silence). `progress` gets 0..=1. Returns the input
/// unchanged at 0.
pub(crate) fn resample_offline(
    input: [Vec<f32>; 2],
    cents: i32,
    progress: &mut dyn FnMut(f32),
) -> [Vec<f32>; 2] {
    let cents = clamp_cents(cents);
    let n = input[0].len().min(input[1].len());
    if cents == 0 || n == 0 {
        return input;
    }
    let ratio = 1.0 / rate(cents);
    let want = (n as f64 * ratio).round() as usize;
    let Ok(mut rs) = SincFixedIn::<f32>::new(ratio, 1.0, sinc_params(), OFFLINE_CHUNK, 2) else {
        return input;
    };
    let mut buf = rs.output_buffer_allocate(true);
    let mut out = [
        Vec::with_capacity(want + rs.output_frames_max()),
        Vec::with_capacity(want + rs.output_frames_max()),
    ];
    let mut pos = 0;
    let mut last = 0.0f32;
    while out[0].len() < want {
        let res = if pos + OFFLINE_CHUNK <= n {
            let chunk = [
                &input[0][pos..pos + OFFLINE_CHUNK],
                &input[1][pos..pos + OFFLINE_CHUNK],
            ];
            rs.process_into_buffer(&chunk, &mut buf, None)
        } else if pos < n {
            let chunk = [&input[0][pos..n], &input[1][pos..n]];
            rs.process_partial_into_buffer(Some(&chunk), &mut buf, None)
        } else {
            rs.process_partial_into_buffer(None::<&[&[f32]]>, &mut buf, None)
        };
        let Ok((_, got)) = res else { break };
        pos += OFFLINE_CHUNK;
        for (o, b) in out.iter_mut().zip(&buf) {
            o.extend_from_slice(&b[..got]);
        }
        let f = (pos as f32 / n as f32).min(1.0);
        if f - last >= 0.1 {
            progress(f);
            last = f;
        }
    }
    for o in out.iter_mut() {
        o.resize(want, 0.0);
    }
    out
}

#[cfg(test)]
pub(crate) mod test_util {
    /// Frequency of a steady sine from its interpolated upward zero
    /// crossings (first to last).
    pub fn sine_hz(x: &[f32], sr: f64) -> f64 {
        let mut first = None;
        let mut last = 0.0;
        let mut count = 0usize;
        for i in 1..x.len() {
            let (a, b) = (x[i - 1] as f64, x[i] as f64);
            if a <= 0.0 && b > 0.0 {
                let t = (i - 1) as f64 + a / (a - b);
                if first.is_none() {
                    first = Some(t);
                } else {
                    count += 1;
                }
                last = t;
            }
        }
        let first = first.expect("no zero crossing");
        assert!(count > 0, "one zero crossing only");
        count as f64 * sr / (last - first)
    }

    /// Normalized autocorrelation of `x` at integer lag `k`.
    fn acf(x: &[f32], k: usize) -> f64 {
        let n = x.len() - k;
        let (mut xy, mut xx, mut yy) = (0.0f64, 0.0f64, 0.0f64);
        for i in 0..n {
            let (a, b) = (x[i] as f64, x[i + k] as f64);
            xy += a * b;
            xx += a * a;
            yy += b * b;
        }
        xy / (xx * yy).sqrt().max(1e-30)
    }

    /// Best lag (parabolic interpolation) in `lo..=hi` and its correlation.
    fn best_lag(x: &[f32], lo: usize, hi: usize) -> (f64, f64) {
        let v: Vec<f64> = (lo - 1..=hi + 1).map(|k| acf(x, k)).collect();
        let i = (1..v.len() - 1)
            .max_by(|&a, &b| v[a].total_cmp(&v[b]))
            .unwrap();
        let (a, b, c) = (v[i - 1], v[i], v[i + 1]);
        let d = a - 2.0 * b + c;
        let off = if d.abs() > 1e-12 {
            0.5 * (a - c) / d
        } else {
            0.0
        };
        ((lo - 1 + i) as f64 + off, b)
    }

    /// Fundamental of a periodic signal expected near `expect` Hz (within a
    /// quarter tone or so), from the autocorrelation. Checks the period is
    /// strong and the half period is not (no octave error), then refines
    /// the estimate over many periods.
    pub fn periodic_hz(x: &[f32], sr: f64, expect: f64) -> f64 {
        let t = sr / expect;
        let (p, c) = best_lag(x, (t * 0.8) as usize, (t * 1.25) as usize + 1);
        assert!(c > 0.8, "weak periodicity {c} near {expect} Hz");
        let half = acf(x, (p / 2.0).round() as usize);
        assert!(half < 0.6, "octave error? acf at half period {half}");
        let m = ((0.2 * sr / p).floor() as usize).max(1);
        let center = m as f64 * p;
        let (pm, _) = best_lag(x, (center - 0.4 * p) as usize, (center + 0.4 * p) as usize);
        sr * m as f64 / pm
    }

    pub fn cents(a: f64, b: f64) -> f64 {
        1200.0 * (a / b).log2()
    }
}

#[cfg(test)]
mod tests {
    use super::test_util::*;
    use super::*;

    fn sine(hz: f64, sr: f64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (0.5 * (2.0 * std::f64::consts::PI * hz * i as f64 / sr).sin()) as f32)
            .collect()
    }

    #[test]
    fn rate_follows_cents_and_clamps() {
        assert_eq!(rate(0), 1.0);
        assert!((rate(1200 / 12) - 2f64.powf(1.0 / 12.0)).abs() < 1e-12);
        assert_eq!(rate(500), rate(MAX_RATE_CENTS));
        assert_eq!(rate(-500), rate(-MAX_RATE_CENTS));
        assert!(rate(MAX_RATE_CENTS) < RATIO_HEADROOM);
        assert!(1.0 / rate(-MAX_RATE_CENTS) < RATIO_HEADROOM);
    }

    #[test]
    fn offline_resample_shifts_pitch_and_length_and_keeps_alignment() {
        let n = 3 * 44_100 + 17;
        let x = sine(440.0, 44_100.0, n);
        let slow = sine(30.0, 44_100.0, n);
        for c in [-200, -37, 18, 200] {
            let r = rate(c);
            let out = resample_offline([x.clone(), x.clone()], c, &mut |_| {});
            assert_eq!(out[0].len(), (n as f64 / r).round() as usize);
            assert_eq!(out[0], out[1]);
            let f = sine_hz(&out[0][2000..out[0].len() - 2000], 44_100.0);
            assert!(cents(f, 440.0 * r).abs() < 0.2, "{c}: {f}");
            // Aligned: output frame j is input time j * r, start to end.
            let out = resample_offline([slow.clone(), slow.clone()], c, &mut |_| {});
            let len = out[0].len();
            for j in [200, len / 2, len - 200] {
                let t = j as f64 * r / 44_100.0;
                let want = 0.5 * (2.0 * std::f64::consts::PI * 30.0 * t).sin();
                assert!((out[0][j] as f64 - want).abs() < 1e-3, "{c} at {j}");
            }
        }
        let same = resample_offline([x.clone(), x.clone()], 0, &mut |_| {});
        assert_eq!(same[0], x);
    }
}
