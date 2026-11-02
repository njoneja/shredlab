// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Separation strength (mask exponent re-derivation).

use std::f64::consts::PI;
use std::sync::atomic::AtomicBool;

use shredlab_dsp::strength::{apply_strength, strength_stream, Cancelled, StrengthOptions};

const SR: f64 = 44_100.0;

fn tone(f: f64, n: usize, amp: f64, ph: f64) -> Vec<f32> {
    (0..n)
        .map(|i| (amp * (2.0 * PI * f * i as f64 / SR + ph).sin()) as f32)
        .collect()
}

fn noise(n: usize, seed: u64, a: f32) -> Vec<f32> {
    let mut s = seed.wrapping_mul(0x9E3779B97F4A7C15) | 1;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            ((s >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0) * a
        })
        .collect()
}

fn snr_db(reference: &[f32], x: &[f32]) -> f64 {
    let (mut p, mut e) = (0.0f64, 0.0f64);
    for (a, b) in reference.iter().zip(x) {
        p += (*a as f64).powi(2);
        e += (*a as f64 - *b as f64).powi(2);
    }
    10.0 * (p / e.max(1e-30)).log10()
}

/// Three "stems" with mostly disjoint spectra plus a broadband one, stereo.
fn stems(n: usize) -> Vec<[Vec<f32>; 2]> {
    vec![
        [tone(110.0, n, 0.4, 0.0), tone(110.0, n, 0.3, 0.5)],
        [tone(880.0, n, 0.2, 1.0), tone(1320.0, n, 0.2, 0.0)],
        [tone(5000.0, n, 0.1, 0.0), tone(4000.0, n, 0.1, 2.0)],
        [noise(n, 1, 0.01), noise(n, 2, 0.01)],
    ]
}

fn mix(st: &[[Vec<f32>; 2]], extra: Option<&[Vec<f32>; 2]>) -> [Vec<f32>; 2] {
    let n = st[0][0].len();
    let mut o = [vec![0.0f32; n], vec![0.0f32; n]];
    for s in st.iter().chain(extra) {
        for ch in 0..2 {
            for (a, b) in o[ch].iter_mut().zip(&s[ch]) {
                *a += b;
            }
        }
    }
    o
}

fn views(st: &[[Vec<f32>; 2]]) -> Vec<[&[f32]; 2]> {
    st.iter().map(|s| [&s[0][..], &s[1][..]]).collect()
}

#[test]
fn derived_stems_sum_to_the_original_for_any_p() {
    let n = 50_000;
    let st = stems(n);
    // The original has a leftover the stems don't explain.
    let residual = [noise(n, 7, 0.02), noise(n, 8, 0.02)];
    let orig = mix(&st, Some(&residual));
    let cancel = AtomicBool::new(false);
    for p in [1.0, 1.7, 2.0, 3.3, 4.0] {
        let out = apply_strength(
            [&orig[0], &orig[1]],
            &views(&st),
            p,
            &StrengthOptions {
                threads: 3,
                chunk_frames: 7,
                ..Default::default()
            },
            &mut |_| {},
            &cancel,
        )
        .unwrap();
        let sum = mix(&out, None);
        for ch in 0..2 {
            let err = orig[ch]
                .iter()
                .zip(&sum[ch])
                .map(|(a, b)| (a - b).abs())
                .fold(0.0, f32::max);
            assert!(err < 2e-5, "p {p} ch {ch}: max err {err}");
        }
    }
}

#[test]
fn p1_is_close_to_the_input_stems_when_they_explain_the_mix() {
    let n = 60_000;
    let st = stems(n);
    let orig = mix(&st, None);
    let cancel = AtomicBool::new(false);
    let out = apply_strength(
        [&orig[0], &orig[1]],
        &views(&st),
        1.0,
        &StrengthOptions::default(),
        &mut |_| {},
        &cancel,
    )
    .unwrap();
    // Tonal stems with separate spectra come back nearly unchanged.
    for s in 0..3 {
        for ch in 0..2 {
            let snr = snr_db(&st[s][ch][5000..n - 5000], &out[s][ch][5000..n - 5000]);
            assert!(snr > 25.0, "stem {s} ch {ch}: {snr:.1} dB");
        }
    }
    // Higher p separates more: the noise stem loses the tones' leakage.
    let tight = apply_strength(
        [&orig[0], &orig[1]],
        &views(&st),
        4.0,
        &StrengthOptions::default(),
        &mut |_| {},
        &cancel,
    )
    .unwrap();
    let r = 5000..n - 5000;
    let snr1 = snr_db(&st[0][0][r.clone()], &out[0][0][r.clone()]);
    let snr4 = snr_db(&st[0][0][r.clone()], &tight[0][0][r]);
    assert!(snr4 >= snr1 - 1.0, "p4 {snr4:.1} vs p1 {snr1:.1}");
}

#[test]
fn stream_covers_the_song_in_order_and_reports_progress() {
    for n in [1usize, 1000, 4096, 33_333] {
        let st = stems(n);
        let orig = mix(&st, None);
        let cancel = AtomicBool::new(false);
        let mut next = 0usize;
        let mut last = 0.0f32;
        strength_stream(
            [&orig[0], &orig[1]],
            &views(&st),
            2.0,
            &StrengthOptions {
                chunk_frames: 3,
                threads: 4,
                ..Default::default()
            },
            &mut |p| {
                assert!(p >= last);
                last = p;
            },
            &cancel,
            &mut |start, part| {
                assert_eq!(start, next);
                assert_eq!(part.len(), st.len());
                next += part[0][0].len();
            },
        )
        .unwrap();
        assert_eq!(next, n);
        assert_eq!(last, 1.0);
    }
}

#[test]
fn cancel_stops_the_run() {
    let n = 100_000;
    let st = stems(n);
    let orig = mix(&st, None);
    let cancel = AtomicBool::new(true);
    let r = apply_strength(
        [&orig[0], &orig[1]],
        &views(&st),
        2.0,
        &StrengthOptions::default(),
        &mut |_| {},
        &cancel,
    );
    assert_eq!(r.unwrap_err(), Cancelled);
    // Cancelled from the progress callback mid-run.
    let cancel = AtomicBool::new(false);
    let r = apply_strength(
        [&orig[0], &orig[1]],
        &views(&st),
        2.0,
        &StrengthOptions {
            chunk_frames: 4,
            threads: 2,
            ..Default::default()
        },
        &mut |p| {
            if p > 0.3 {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        },
        &cancel,
    );
    assert_eq!(r.unwrap_err(), Cancelled);
}

#[test]
fn single_thread_equals_multi_thread() {
    let n = 20_000;
    let st = stems(n);
    let orig = mix(&st, None);
    let cancel = AtomicBool::new(false);
    let run = |threads, chunk_frames| {
        apply_strength(
            [&orig[0], &orig[1]],
            &views(&st),
            2.5,
            &StrengthOptions {
                threads,
                chunk_frames,
                ..Default::default()
            },
            &mut |_| {},
            &cancel,
        )
        .unwrap()
    };
    let a = run(1, 64);
    let b = run(5, 2);
    for (x, y) in a.iter().zip(&b) {
        for ch in 0..2 {
            let err = x[ch]
                .iter()
                .zip(&y[ch])
                .map(|(p, q)| (p - q).abs())
                .fold(0.0, f32::max);
            assert!(err < 1e-6, "{err}");
        }
    }
}

#[test]
fn smooth_keeps_the_stems_at_p1_and_never_boosts() {
    let n = 40_000;
    let st = stems(n);
    let residual = [noise(n, 7, 0.02), noise(n, 8, 0.02)];
    let orig = mix(&st, Some(&residual));
    let cancel = AtomicBool::new(false);
    let run = |p| {
        apply_strength(
            [&orig[0], &orig[1]],
            &views(&st),
            p,
            &StrengthOptions {
                smooth: true,
                ..Default::default()
            },
            &mut |_| {},
            &cancel,
        )
        .unwrap()
    };
    // p = 1: factor 1 everywhere, so the model's stems come back (STFT
    // round trip only), leftover and all; the residual is NOT in the sum.
    let one = run(1.0);
    let r = 5000..n - 5000;
    for s in 0..st.len() {
        for ch in 0..2 {
            let snr = snr_db(&st[s][ch][r.clone()], &one[s][ch][r.clone()]);
            assert!(snr > 60.0, "stem {s} ch {ch}: {snr:.1} dB");
        }
    }
    // p > 1: each stem only loses energy (the factor is at most 1), and the
    // sum stays the stems' sum minus what was removed (not the original).
    for p in [1.5, 2.0, 4.0] {
        let out = run(p);
        let e = |x: &[f32]| {
            x[r.clone()]
                .iter()
                .map(|v| (*v as f64).powi(2))
                .sum::<f64>()
        };
        for s in 0..st.len() {
            assert!(e(&out[s][0]) <= e(&st[s][0]) * 1.001, "p {p} stem {s} grew");
        }
        // The broadband stem loses the tones' leakage more as p grows.
        assert!(e(&out[3][0]) < e(&st[3][0]));
    }
}
