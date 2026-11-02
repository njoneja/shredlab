//! Refine panel DSP: pan/width mask, EQ, chain.

use std::f64::consts::PI;

use shredlab_dsp::refine::eq::{Coefs, EndMode, EqParams};
use shredlab_dsp::refine::pan::window_gain;
use shredlab_dsp::refine::*;

const SR: u32 = 44_100;

fn sine(f: f64, n: usize, amp: f64) -> Vec<f32> {
    (0..n)
        .map(|i| (amp * (2.0 * PI * f * i as f64 / SR as f64).sin()) as f32)
        .collect()
}

fn add(a: &[f32], b: &[f32]) -> Vec<f32> {
    a.iter().zip(b).map(|(x, y)| x + y).collect()
}

fn scaled(a: &[f32], k: f32) -> Vec<f32> {
    a.iter().map(|x| x * k).collect()
}

/// Amplitude of the `f` Hz component over `x[range]` (least-squares fit of
/// sin + cos; exact for a steady sine with an integer number of cycles, very
/// close otherwise).
fn amp_at(x: &[f32], f: f64, range: std::ops::Range<usize>) -> f64 {
    let (mut s, mut c) = (0.0, 0.0);
    let n = range.len() as f64;
    for i in range {
        let ph = 2.0 * PI * f * i as f64 / SR as f64;
        s += x[i] as f64 * ph.sin();
        c += x[i] as f64 * ph.cos();
    }
    2.0 * (s * s + c * c).sqrt() / n
}

/// Deterministic white-ish noise in [-a, a].
fn noise(n: usize, seed: u64, a: f32) -> Vec<f32> {
    let mut s = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (0..n)
        .map(|_| {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((s >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0) * a
        })
        .collect()
}

/// Left-only 440 Hz, right-only 1000 Hz, centre 2500 Hz (amplitude 0.3 each).
fn lcr_signal(n: usize) -> (Vec<f32>, Vec<f32>) {
    let lt = sine(440.0, n, 0.3);
    let rt = sine(1000.0, n, 0.3);
    let ct = sine(2500.0, n, 0.3);
    let l = add(&lt, &ct);
    let r = add(&rt, &ct);
    (l, r)
}

fn steady(n: usize) -> std::ops::Range<usize> {
    n / 4..3 * n / 4
}

fn db(x: f64) -> f64 {
    20.0 * x.max(1e-12).log10()
}

fn pan_only(p: PanParams) -> RefineParams {
    RefineParams {
        pan: p,
        ..RefineParams::default()
    }
}

#[test]
fn keep_left_keeps_the_left_tone_only() {
    let n = SR as usize;
    let (l, r) = lcr_signal(n);
    let [ol, or] = refine_offline(&pan_only(PanParams::left()), SR, &l, &r);
    let w = steady(n);
    assert!((amp_at(&ol, 440.0, w.clone()) - 0.3).abs() < 0.003);
    assert!(db(amp_at(&or, 1000.0, w.clone()) / 0.3) < -60.0);
    assert!(db(amp_at(&ol, 2500.0, w.clone()) / 0.3) < -60.0);
    assert!(db(amp_at(&or, 2500.0, w.clone()) / 0.3) < -60.0);
}

#[test]
fn inverted_left_removes_only_the_left_tone() {
    let n = SR as usize;
    let (l, r) = lcr_signal(n);
    let p = PanParams {
        invert: true,
        ..PanParams::left()
    };
    let [ol, or] = refine_offline(&pan_only(p), SR, &l, &r);
    let w = steady(n);
    assert!(db(amp_at(&ol, 440.0, w.clone()) / 0.3) < -60.0);
    assert!((amp_at(&or, 1000.0, w.clone()) - 0.3).abs() < 0.003);
    assert!((amp_at(&ol, 2500.0, w.clone()) - 0.3).abs() < 0.003);
    assert!((amp_at(&or, 2500.0, w.clone()) - 0.3).abs() < 0.003);
}

#[test]
fn center_preset_passes_centre_content() {
    let n = SR as usize;
    let (l, r) = lcr_signal(n);
    let [ol, or] = refine_offline(&pan_only(PanParams::center()), SR, &l, &r);
    let w = steady(n);
    assert!((amp_at(&ol, 2500.0, w.clone()) - 0.3).abs() < 0.003);
    assert!((amp_at(&or, 2500.0, w.clone()) - 0.3).abs() < 0.003);
    assert!(db(amp_at(&ol, 440.0, w.clone()) / 0.3) < -60.0);
    assert!(db(amp_at(&or, 1000.0, w.clone()) / 0.3) < -60.0);
}

#[test]
fn keep_plus_remove_and_lcr_sum_to_the_input() {
    let n = SR as usize / 2;
    let l = add(&noise(n, 1, 0.3), &sine(300.0, n, 0.2));
    let r = add(&noise(n, 2, 0.3), &scaled(&sine(300.0, n, 0.2), 0.4));
    let maxerr = |parts: &[[Vec<f32>; 2]]| {
        let mut e = 0.0f32;
        for i in 0..n {
            let sl: f32 = parts.iter().map(|p| p[0][i]).sum();
            let sr: f32 = parts.iter().map(|p| p[1][i]).sum();
            e = e.max((sl - l[i]).abs()).max((sr - r[i]).abs());
        }
        e
    };
    let keep = PanParams {
        enabled: true,
        center: 0.2,
        width: 0.5,
        softness: 0.3,
        invert: false,
    };
    let remove = PanParams {
        invert: true,
        ..keep
    };
    let parts = [
        refine_offline(&pan_only(keep), SR, &l, &r),
        refine_offline(&pan_only(remove), SR, &l, &r),
    ];
    assert!(
        maxerr(&parts) < 1e-5,
        "window+inverse err {}",
        maxerr(&parts)
    );
    let lcr = [
        refine_offline(&pan_only(PanParams::left()), SR, &l, &r),
        refine_offline(&pan_only(PanParams::center()), SR, &l, &r),
        refine_offline(&pan_only(PanParams::right()), SR, &l, &r),
    ];
    assert!(maxerr(&lcr) < 1e-5, "L+C+R err {}", maxerr(&lcr));
}

#[test]
fn mask_edges_are_smooth_raised_cosines() {
    let (c, w, s) = (0.1f32, 0.6f32, 0.25f32);
    // Half gain at the window edges, full inside, zero outside.
    assert!((window_gain(c + w / 2.0, c, w, s) - 0.5).abs() < 1e-6);
    assert!((window_gain(c - w / 2.0, c, w, s) - 0.5).abs() < 1e-6);
    assert_eq!(window_gain(c, c, w, s), 1.0);
    assert_eq!(window_gain(c + w / 2.0 + s / 2.0 + 1e-3, c, w, s), 0.0);
    // Continuous with bounded slope (π / (2 s) for a raised cosine).
    let step = 1e-4f32;
    let mut prev = window_gain(-1.0, c, w, s);
    let mut p = -1.0 + step;
    while p <= 1.0 {
        let g = window_gain(p, c, w, s);
        assert!((g - prev).abs() <= std::f32::consts::PI / (2.0 * s) * step * 1.01 + 1e-6);
        prev = g;
        p += step;
    }
    // A tone sitting on the edge comes out at half amplitude.
    let n = SR as usize;
    let pan = 0.33f32; // edge of the Center preset
    let gl = (1.0 - pan) / (1.0 + pan);
    let t = sine(700.0, n, 0.4);
    let (l, r) = (scaled(&t, gl), t.clone());
    let [ol, or] = refine_offline(&pan_only(PanParams::center()), SR, &l, &r);
    let wr = steady(n);
    assert!((amp_at(&or, 700.0, wr.clone()) / 0.4 - 0.5).abs() < 0.01);
    assert!((amp_at(&ol, 700.0, wr.clone()) / (0.4 * gl as f64) - 0.5).abs() < 0.01);
}

/// Runs a chain over `l, r` in blocks of varying size.
fn stream(chain: &mut RefineChain, l: &[f32], r: &[f32], seed: u64) -> (Vec<f32>, Vec<f32>) {
    let mut ol = vec![0.0; l.len()];
    let mut or = vec![0.0; r.len()];
    let sizes = noise(4096, seed, 1.0);
    let (mut i, mut k) = (0, 0);
    while i < l.len() {
        let b = (((sizes[k % sizes.len()] + 1.0) * 400.0) as usize).min(l.len() - i);
        chain.process(
            &l[i..i + b],
            &r[i..i + b],
            &mut ol[i..i + b],
            &mut or[i..i + b],
        );
        i += b;
        k += 1;
    }
    (ol, or)
}

#[test]
fn bypass_is_the_exactly_delayed_input() {
    let n = 30_000;
    let (l, r) = (noise(n, 3, 0.8), noise(n, 4, 0.8));
    let mut chain = RefineChain::new(SR);
    assert_eq!(chain.latency(), PAN_LATENCY);
    assert!(chain.is_bypassed());
    let (ol, or) = stream(&mut chain, &l, &r, 9);
    let lat = chain.latency();
    assert!(ol[..lat].iter().chain(&or[..lat]).all(|&x| x == 0.0));
    assert_eq!(&ol[lat..], &l[..n - lat]);
    assert_eq!(&or[lat..], &r[..n - lat]);
}

#[test]
fn chain_returns_to_exact_bypass_after_being_used() {
    let n = 80_000;
    let (l, r) = (noise(n, 5, 0.5), noise(n, 6, 0.5));
    let mut chain = RefineChain::new(SR);
    let mut on = pan_only(PanParams::left());
    on.eq = eq_preset("guitar-focus").unwrap().eq;
    let lat = chain.latency();
    let mut ol = vec![0.0; n];
    let mut or = vec![0.0; n];
    let half = n / 4;
    chain.set_params(&on);
    chain.process(&l[..half], &r[..half], &mut ol[..half], &mut or[..half]);
    assert!(!chain.is_bypassed());
    chain.set_params(&RefineParams::default());
    let (a, b) = (half, n);
    chain.process(&l[a..b], &r[a..b], &mut ol[a..b], &mut or[a..b]);
    assert!(chain.is_bypassed());
    // The last part is the delayed input again, exactly.
    let tail = n - 10_000..n;
    for i in tail {
        assert_eq!(ol[i], l[i - lat]);
        assert_eq!(or[i], r[i - lat]);
    }
}

#[test]
fn switching_the_stft_path_on_and_off_is_seamless() {
    // A mask that keeps everything: output must stay the delayed input while
    // the STFT path switches in and out (tests the overlap-add warm-up),
    // including a mid-hop `set_params_now`.
    let n = 60_000;
    let (l, r) = (noise(n, 7, 0.5), noise(n, 8, 0.5));
    let all = pan_only(PanParams {
        enabled: true,
        center: 0.0,
        width: 4.0,
        ..PanParams::default()
    });
    let mut chain = RefineChain::new(SR);
    let lat = chain.latency();
    let mut ol = vec![0.0; n];
    let mut or = vec![0.0; n];
    let cuts = [0, 5_000, 17_777, 30_001, 41_234, n];
    for (k, w) in cuts.windows(2).enumerate() {
        match k % 3 {
            0 => chain.set_params(&all),
            1 => chain.set_params(&RefineParams::default()),
            _ => chain.set_params_now(&all),
        }
        let (a, b) = (w[0], w[1]);
        chain.process(&l[a..b], &r[a..b], &mut ol[a..b], &mut or[a..b]);
    }
    let mut err = 0.0f32;
    for i in lat..n {
        err = err
            .max((ol[i] - l[i - lat]).abs())
            .max((or[i] - r[i - lat]).abs());
    }
    assert!(err < 1e-5, "max deviation {err}");
}

#[test]
fn streaming_matches_offline_for_any_block_size() {
    let n = 50_000;
    let (l0, r0) = lcr_signal(n);
    let l = add(&l0, &noise(n, 10, 0.1));
    let r = add(&r0, &noise(n, 11, 0.1));
    let mut p = pan_only(PanParams {
        enabled: true,
        center: -0.4,
        width: 0.7,
        softness: 0.3,
        invert: true,
    });
    p.eq = eq_preset("vocals-clarity").unwrap().eq;
    let [fl, fr] = refine_offline(&p, SR, &l, &r);
    for seed in [1, 2, 3] {
        let mut chain = RefineChain::new(SR);
        chain.set_params_now(&p);
        let lat = chain.latency();
        let (sl, sr) = stream(&mut chain, &l, &r, seed);
        assert_eq!(&sl[lat..], &fl[..n - lat], "seed {seed}");
        assert_eq!(&sr[lat..], &fr[..n - lat], "seed {seed}");
    }
}

#[test]
fn reset_and_prime_reproduce_the_offline_result() {
    // Seek pattern: reset, prime with the next `latency()` samples, then
    // the output is aligned with the play position from the first sample.
    let n = 40_000;
    let (l, r) = lcr_signal(n);
    let p = pan_only(PanParams::right());
    let [fl, fr] = refine_offline(&p, SR, &l, &r);
    let mut chain = RefineChain::new(SR);
    chain.set_params(&p);
    let mut junk = (vec![0.0; 3000], vec![0.0; 3000]);
    chain.process(&r[..3000], &l[..3000], &mut junk.0, &mut junk.1);
    chain.reset();
    let lat = chain.latency();
    chain.prime(&l[..lat], &r[..lat]);
    let m = n - lat;
    let mut ol = vec![0.0; m];
    let mut or = vec![0.0; m];
    chain.process(&l[lat..], &r[lat..], &mut ol, &mut or);
    assert_eq!(ol, fl[..m]);
    assert_eq!(or, fr[..m]);
}

/// Largest sample-to-sample step of `x`.
fn max_step(x: &[f32]) -> f32 {
    x.windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max)
}

#[test]
fn pan_parameter_changes_do_not_click() {
    let n = SR as usize * 3;
    let (f, a) = (440.0, 0.5);
    let l = sine(f, n, a);
    let r = vec![0.0; n];
    let mut chain = RefineChain::new(SR);
    let lat = chain.latency();
    let mut ol = vec![0.0; n];
    let mut or = vec![0.0; n];
    let changes: [(usize, RefineParams); 6] = [
        (0, RefineParams::default()),
        (20_000, pan_only(PanParams::left())),
        (
            40_000,
            pan_only(PanParams {
                invert: true,
                ..PanParams::left()
            }),
        ),
        (60_000, pan_only(PanParams::left())),
        (
            80_000,
            pan_only(PanParams {
                center: 0.0,
                width: 0.3,
                ..PanParams::left()
            }),
        ),
        (100_000, RefineParams::default()),
    ];
    let mut i = 0;
    let mut k = 0;
    while i < n {
        if k < changes.len() && i >= changes[k].0 {
            chain.set_params(&changes[k].1);
            k += 1;
        }
        let b = 256.min(n - i);
        chain.process(
            &l[i..i + b],
            &r[i..i + b],
            &mut ol[i..i + b],
            &mut or[i..i + b],
        );
        i += b;
    }
    let sine_step = (a * 2.0 * PI * f / SR as f64) as f32;
    let got = max_step(&ol[lat..]);
    assert!(
        got <= sine_step * 1.05,
        "max step {got} vs sine {sine_step}"
    );
    // The tone really was removed at some point (the test isn't vacuous).
    assert!(amp_at(&ol, f, 50_000..55_000) < 0.01);
    assert!(max_step(&or) < 1e-3);
}

#[test]
fn eq_parameter_changes_do_not_click() {
    let n = SR as usize * 2;
    let mut chain = RefineChain::new(SR);
    let lat = chain.latency();
    for (f, a, gain_max) in [(1000.0, 0.2, 4.0), (100.0, 0.5, 1.1)] {
        chain.reset();
        chain.set_params_now(&RefineParams::default());
        let l = sine(f, n, a);
        let r = sine(f, n, a);
        let mut ol = vec![0.0; n];
        let mut or = vec![0.0; n];
        let mut eq_boost = EqParams::default();
        eq_boost.bands[1] = BandParams::new(1000.0, 12.0, 1.0);
        let eq_cut = EqParams {
            low_cut: CutParams {
                enabled: true,
                freq: 300.0,
                ..CutParams::default()
            },
            ..EqParams::default()
        };
        let mut eq_sweep = eq_boost;
        eq_sweep.bands[1].freq = 3000.0;
        eq_sweep.low_cut = CutParams {
            enabled: true,
            freq: 150.0,
            ..CutParams::default()
        };
        let seq = [eq_boost, eq_cut, eq_sweep, EqParams::default(), eq_cut];
        let mut i = 0;
        let mut k = 0;
        while i < n {
            // First change after the sine's onset has passed the delay.
            if i >= lat + 2_000 + k * 15_000 && k < seq.len() {
                chain.set_params(&RefineParams {
                    eq: seq[k],
                    ..RefineParams::default()
                });
                k += 1;
            }
            let b = 256.min(n - i);
            chain.process(
                &l[i..i + b],
                &r[i..i + b],
                &mut ol[i..i + b],
                &mut or[i..i + b],
            );
            i += b;
        }
        let sine_step = (gain_max * a * 2.0 * PI * f / SR as f64) as f32;
        assert_eq!(k, seq.len());
        // From just before the first change (the sine's own onset excluded).
        let got = max_step(&ol[2 * lat + 1_000..]);
        // A click would be many times the sine's slope; gliding a steep cut
        // across the tone overshoots briefly (~10 %), which is fine.
        assert!(
            got <= sine_step * 1.2,
            "{f} Hz: max step {got} vs {sine_step}"
        );
    }
}

#[test]
fn eq_matches_the_analytic_biquad_response() {
    let fs = SR as f64;
    // RBJ textbook points.
    let pk = Coefs::peaking(fs, 1000.0, 1.0, 6.0);
    assert!((db(pk.magnitude(fs, 1000.0)) - 6.0).abs() < 1e-6);
    let hp = Coefs::highpass(fs, 200.0, std::f64::consts::FRAC_1_SQRT_2);
    assert!((db(hp.magnitude(fs, 200.0)) + 3.0103).abs() < 1e-3);
    assert!(db(hp.magnitude(fs, 20.0)) < -39.0);
    let cut = EqParams {
        low_cut: CutParams {
            enabled: true,
            freq: 120.0,
            ..CutParams::default()
        },
        high_cut: CutParams {
            enabled: true,
            freq: 8000.0,
            ..CutParams::default()
        },
        ..EqParams::default()
    };
    let mut out = [0.0f32; 3];
    response_db(&cut, SR as f32, &[120.0, 8000.0, 1000.0], &mut out);
    // 4th-order Butterworth: -3 dB at the corner, flat in between.
    assert!((out[0] + 3.01).abs() < 0.05, "{out:?}");
    assert!((out[1] + 3.01).abs() < 0.1, "{out:?}");
    assert!(out[2].abs() < 0.05, "{out:?}");

    // Steady-state sine gain of the running EQ equals the curve.
    for id in ["guitar-focus", "bass-notes", "vocals-clarity"] {
        let eq = eq_preset(id).unwrap().eq;
        let p = RefineParams {
            eq,
            ..RefineParams::default()
        };
        let freqs = [60.0, 150.0, 440.0, 1000.0, 2500.0, 6000.0, 12_000.0];
        let mut curve = [0.0f32; 7];
        response_db(&eq, SR as f32, &freqs, &mut curve);
        let n = SR as usize;
        for (&f, &want) in freqs.iter().zip(&curve) {
            let x = sine(f as f64, n, 0.25);
            let [ol, _] = refine_offline(&p, SR, &x, &x);
            let got = db(amp_at(&ol, f as f64, n / 2..n) / 0.25);
            assert!(
                (got - want as f64).abs() < 0.05,
                "{id} {f} Hz: {got} vs {want}"
            );
        }
    }
}

#[test]
fn stereo_field_puts_energy_in_the_right_bins() {
    let n = SR as usize;
    let lt = sine(440.0, n, 0.3); // mid band, hard left
    let ct = sine(100.0, n, 0.3); // low band, centre
    let rt = sine(5000.0, n, 0.3); // high band, hard right
    let l = add(&lt, &ct);
    let r = add(&rt, &ct);
    let mut chain = RefineChain::new(SR);
    chain.set_analysis(true);
    let mut ol = vec![0.0; n];
    let mut or = vec![0.0; n];
    chain.process(&l, &r, &mut ol, &mut or);
    let mut field = [0.0f32; FIELD_LEN];
    let hops = chain.take_field(&mut field);
    assert_eq!(hops as usize, n / PAN_HOP);
    // Still bypassed: analysis doesn't change the audio.
    assert!(chain.is_bypassed());
    let bin = |band: usize, b: usize| field[band * FIELD_BINS + b];
    let band_sum = |band: usize| -> f32 { (0..FIELD_BINS).map(|b| bin(band, b)).sum() };
    let centre = FIELD_BINS / 2;
    assert!(bin(1, 0) > 0.9 * band_sum(1));
    assert!(bin(0, centre) > 0.9 * band_sum(0));
    assert!(bin(2, FIELD_BINS - 1) > 0.9 * band_sum(2));
    // Level: a sine of amplitude A reads about 1.5·A² per channel it is in
    // (start-up hops with partial signal pull the average down a little).
    let a2 = 0.3f32 * 0.3;
    assert!(
        (band_sum(1) / (1.5 * a2) - 1.0).abs() < 0.1,
        "{}",
        band_sum(1)
    );
    assert!(
        (band_sum(0) / (2.0 * 1.5 * a2) - 1.0).abs() < 0.1,
        "{}",
        band_sum(0)
    );
    // Taken: the next read is empty.
    assert_eq!(chain.take_field(&mut field), 0);
    assert!(field.iter().all(|&v| v == 0.0));
}

#[test]
fn params_serialize_and_presets_are_sane() {
    let mut p = pan_only(PanParams::left());
    apply_preset(&mut p, eq_preset("bass-notes").unwrap());
    let s = serde_json::to_string(&p).unwrap();
    let back: RefineParams = serde_json::from_str(&s).unwrap();
    assert_eq!(back, p);
    // `invert` is a run-time setting: never saved, and an old saved `mode`
    // field is ignored.
    let pan_json = serde_json::to_string(&p.pan).unwrap();
    assert!(!pan_json.contains("invert") && !pan_json.contains("mode"));
    let mut inv = p;
    inv.pan.invert = true;
    let back: RefineParams = serde_json::from_str(&serde_json::to_string(&inv).unwrap()).unwrap();
    assert_eq!(back, p);
    let old: RefineParams = serde_json::from_str(
        r#"{"pan":{"enabled":true,"center":-1,"width":1.34,"mode":"remove"}}"#,
    )
    .unwrap();
    assert!(old.pan.enabled && !old.pan.invert && old.pan.center == -1.0);
    // Missing fields fall back to defaults (forward compatible saves).
    let partial: RefineParams = serde_json::from_str(r#"{"pan":{"enabled":true}}"#).unwrap();
    assert!(partial.pan.enabled && partial.eq.is_identity());
    assert!(RefineParams::default().is_identity());
    let mut ids: Vec<_> = EQ_PRESETS.iter().map(|p| p.id).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), EQ_PRESETS.len());
    for pr in &EQ_PRESETS {
        assert_eq!(pr.eq.is_identity(), pr.id == "flat", "{}", pr.id);
        // No preset boosts anything by more than 6 dB.
        let freqs = log_freqs(200, 20.0, 20_000.0);
        let mut c = vec![0.0; 200];
        response_db(&pr.eq, SR as f32, &freqs, &mut c);
        assert!(c.iter().all(|&v| v < 6.5), "{}", pr.id);
    }
}

fn shelf_end(freq: f32, gain_db: f32) -> CutParams {
    CutParams {
        enabled: true,
        freq,
        mode: EndMode::Shelf,
        gain_db,
    }
}

#[test]
fn shelf_matches_the_analytic_rbj_shelf() {
    let fs = SR as f64;
    // Independent evaluation of the RBJ shelf (S = 1) transfer function.
    let rbj = |f0: f64, g: f64, high: bool, f: f64| -> f64 {
        let a = 10f64.powf(g / 40.0);
        let w0 = 2.0 * PI * f0 / fs;
        let al = w0.sin() / 2.0 * 2f64.sqrt();
        let c = w0.cos();
        let k = 2.0 * a.sqrt() * al;
        let (b, aa) = if high {
            (
                [
                    a * ((a + 1.0) + (a - 1.0) * c + k),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
                    a * ((a + 1.0) + (a - 1.0) * c - k),
                ],
                [
                    (a + 1.0) - (a - 1.0) * c + k,
                    2.0 * ((a - 1.0) - (a + 1.0) * c),
                    (a + 1.0) - (a - 1.0) * c - k,
                ],
            )
        } else {
            (
                [
                    a * ((a + 1.0) - (a - 1.0) * c + k),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * c),
                    a * ((a + 1.0) - (a - 1.0) * c - k),
                ],
                [
                    (a + 1.0) + (a - 1.0) * c + k,
                    -2.0 * ((a - 1.0) + (a + 1.0) * c),
                    (a + 1.0) + (a - 1.0) * c - k,
                ],
            )
        };
        let w = 2.0 * PI * f / fs;
        let h = |co: [f64; 3]| {
            let re = co[0] + co[1] * w.cos() + co[2] * (2.0 * w).cos();
            let im = -co[1] * w.sin() - co[2] * (2.0 * w).sin();
            (re * re + im * im).sqrt()
        };
        20.0 * (h(b) / h(aa)).log10()
    };
    let freqs = [20.0f32, 100.0, 250.0, 1000.0, 4000.0, 10_000.0, 18_000.0];
    for (lo, hi) in [
        ((200.0, 6.0), (6000.0, -4.5)),
        ((80.0, -12.0), (10_000.0, 12.0)),
    ] {
        let p = EqParams {
            low_cut: shelf_end(lo.0, lo.1),
            high_cut: shelf_end(hi.0, hi.1),
            ..EqParams::default()
        };
        let mut out = [0.0f32; 7];
        response_db(&p, SR as f32, &freqs, &mut out);
        for (&f, &got) in freqs.iter().zip(&out) {
            let want = rbj(lo.0 as f64, lo.1 as f64, false, f as f64)
                + rbj(hi.0 as f64, hi.1 as f64, true, f as f64);
            assert!((got as f64 - want).abs() < 1e-3, "{f} Hz: {got} vs {want}");
        }
        // Textbook points: half the gain at the corner, full gain far away.
        let mut c = [0.0f32; 2];
        let low_only = EqParams {
            low_cut: shelf_end(lo.0, lo.1),
            ..EqParams::default()
        };
        response_db(&low_only, SR as f32, &[lo.0, 15.0], &mut c);
        assert!((c[0] - lo.1 / 2.0).abs() < 0.05, "{c:?}");
        assert!((c[1] - lo.1).abs() < 0.5, "{c:?}");
    }
    // The running EQ follows the curve (steady-state sine gain).
    let p = RefineParams {
        eq: EqParams {
            low_cut: shelf_end(150.0, 8.0),
            high_cut: shelf_end(5000.0, -6.0),
            ..EqParams::default()
        },
        ..RefineParams::default()
    };
    let n = SR as usize;
    for f in [60.0f32, 150.0, 1000.0, 5000.0, 12_000.0] {
        let mut want = [0.0f32];
        response_db(&p.eq, SR as f32, &[f], &mut want);
        let x = sine(f as f64, n, 0.2);
        let [ol, _] = refine_offline(&p, SR, &x, &x);
        let got = db(amp_at(&ol, f as f64, n / 2..n) / 0.2);
        assert!(
            (got - want[0] as f64).abs() < 0.05,
            "{f} Hz: {got} vs {}",
            want[0]
        );
    }
    // Shelf gain is clamped to ±12 dB; a 0 dB shelf is identity.
    assert_eq!(shelf_end(100.0, 30.0).shelf_gain(), 12.0);
    assert!(EqParams {
        low_cut: shelf_end(100.0, 0.0),
        ..EqParams::default()
    }
    .is_identity());
}

#[test]
fn switching_cut_and_shelf_does_not_click() {
    let n = SR as usize * 2;
    let mut chain = RefineChain::new(SR);
    let lat = chain.latency();
    for (f, a) in [(100.0, 0.5), (8000.0, 0.2)] {
        chain.reset();
        chain.set_params_now(&RefineParams::default());
        let l = sine(f, n, a);
        let mut ol = vec![0.0; n];
        let mut or = vec![0.0; n];
        let eq = |lo: CutParams, hi: CutParams| RefineParams {
            eq: EqParams {
                low_cut: lo,
                high_cut: hi,
                ..EqParams::default()
            },
            ..RefineParams::default()
        };
        let seq = [
            eq(CutParams::cut(300.0), CutParams::cut(4000.0)),
            eq(shelf_end(300.0, 9.0), shelf_end(4000.0, -9.0)),
            {
                // Low end off while a low bell glides in.
                let mut e = eq(CutParams::off(300.0), shelf_end(4000.0, 9.0));
                e.eq.bands[0] = BandParams::new(150.0, 9.0, 0.7);
                e
            },
            eq(shelf_end(300.0, -12.0), CutParams::cut(4000.0)),
            RefineParams::default(),
        ];
        let (mut i, mut k) = (0, 0);
        while i < n {
            if i >= lat + 2_000 + k * 12_000 && k < seq.len() {
                chain.set_params(&seq[k]);
                k += 1;
            }
            let b = 256.min(n - i);
            chain.process(
                &l[i..i + b],
                &l[i..i + b],
                &mut ol[i..i + b],
                &mut or[i..i + b],
            );
            i += b;
        }
        assert_eq!(k, seq.len());
        // The loudest setting (+9 dB shelf at 100 Hz, ~2.8x) bounds the
        // slope; a click or zipper step would exceed it.
        let sine_step = (2.9 * a * 2.0 * PI * f / SR as f64) as f32;
        let got = max_step(&ol[2 * lat + 1_000..]);
        assert!(
            got <= sine_step * 1.05,
            "{f} Hz: max step {got} vs {sine_step}"
        );
        // Back to flat: ends bypassed again, output = input.
        assert!(chain.is_bypassed());
    }
}

#[test]
fn old_params_and_presets_stay_cuts() {
    // Saved before shelves existed: no mode / gain_db fields.
    let p: CutParams = serde_json::from_str(r#"{"enabled":true,"freq":120.0}"#).unwrap();
    assert_eq!(p, CutParams::cut(120.0));
    assert!(p.cut_on() && p.shelf_gain() == 0.0);
    for pr in EQ_PRESETS {
        assert_eq!(pr.eq.low_cut.mode, EndMode::Cut, "{}", pr.id);
        assert_eq!(pr.eq.high_cut.mode, EndMode::Cut, "{}", pr.id);
    }
    // Default params: exact passthrough (delayed).
    let x = sine(440.0, 20_000, 0.3);
    let [ol, _] = refine_offline(&RefineParams::default(), SR, &x, &x);
    assert_eq!(ol, x);
}
