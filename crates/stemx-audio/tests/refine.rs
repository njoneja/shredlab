//! Refined stems in the mixer, playback pipeline and export (M5c).
//!
//! The mix rule (crate `mix` docs): `out = w·original + Σ (g_i·r_i − w·s_i)`
//! with `s_i` the whole stem and `r_i` the heard part after EQ. The stereo
//! window is an exception to the row's on switch: on → the stem minus the
//! window (`invert`), off → the window alone (heard at the row's volume).

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use stemx_audio::mix::{MixMode, Mixer};
use stemx_audio::pipeline::{Meta, Pipeline, XFADE};
use stemx_audio::{render, ClickConfig, RenderRequest, Track};
use stemx_dsp::refine::{eq_preset, pan_offline, refine_offline, PanParams, RefineParams};

const SR: u32 = 44_100;
const NAMES: [&str; 3] = ["guitar", "bass", "drums"];

fn tone(f: f32, n: usize, a: f32, ph: f32) -> Vec<f32> {
    (0..n)
        .map(|i| a * (2.0 * std::f32::consts::PI * f * i as f32 / SR as f32 + ph).sin())
        .collect()
}

fn track(name: &str, l: Vec<f32>, r: Vec<f32>) -> Track {
    Track {
        name: name.into(),
        channels: [Arc::new(l), Arc::new(r)],
        sample_rate: SR,
    }
}

/// Guitar: a hard-left 440 Hz part plus a hard-right 660 Hz part. Bass and
/// drums centred. The original adds a leftover the stems don't explain.
fn song(n: usize) -> (Vec<Track>, Track) {
    let gl = tone(440.0, n, 0.3, 0.0);
    let gr = tone(660.0, n, 0.25, 0.3);
    let guitar = track("guitar", gl, gr);
    let b = tone(82.0, n, 0.4, 0.1);
    let bass = track("bass", b.clone(), b);
    let d = tone(2000.0, n, 0.05, 0.7);
    let drums = track("drums", d.clone(), d);
    let stems = vec![guitar, bass, drums];
    let res = tone(5100.0, n, 0.02, 0.0);
    let o = [0, 1].map(|c| {
        (0..n)
            .map(|i| stems.iter().map(|t| t.channels[c][i]).sum::<f32>() + res[i])
            .collect::<Vec<f32>>()
    });
    let [ol, or] = o;
    (stems, track("original", ol, or))
}

fn gains(v: [f32; 3]) -> Vec<(String, f32)> {
    NAMES
        .iter()
        .zip(v)
        .map(|(n, g)| (n.to_string(), g))
        .collect()
}

/// The stereo window "left" as an exception to a row whose switch is `on`:
/// on → everything but the left part, off → only the left part.
fn except_left(on: bool) -> RefineParams {
    RefineParams {
        pan: PanParams {
            invert: on,
            ..PanParams::left()
        },
        ..RefineParams::default()
    }
}

fn mix_all(m: &mut Mixer, n: usize, block: usize) -> [Vec<f32>; 2] {
    let mut out = [Vec::with_capacity(n), Vec::with_capacity(n)];
    let (mut l, mut r) = (vec![0.0; block], vec![0.0; block]);
    let mut pos = 0;
    while pos < n {
        let k = block.min(n - pos);
        m.mix(pos as i64, &mut l[..k], &mut r[..k], true);
        out[0].extend_from_slice(&l[..k]);
        out[1].extend_from_slice(&r[..k]);
        pos += k;
    }
    out
}

fn max_diff(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

#[test]
fn identity_refine_is_bit_identical_to_the_plain_mix() {
    let n = 30_000;
    let (stems, orig) = song(n);
    for (mode, g) in [
        (MixMode::Original, [1.0, 1.0, 1.0]),
        (MixMode::Original, [0.0, 1.0, 0.7]),
        (MixMode::Stems, [0.8, 0.0, 1.5]),
    ] {
        let mut plain = Mixer::new(vec![], &gains(g));
        plain.set_mode(mode);
        plain.load_with_original(stems.clone(), Some(orig.clone()), false);
        let a = mix_all(&mut plain, n, 333);
        // Explicit identity params on every stem, and a different block size.
        let mut refined = Mixer::new(vec![], &gains(g));
        refined.set_mode(mode);
        for nm in NAMES {
            refined.set_refine(nm, RefineParams::default());
        }
        refined.load_with_original(stems.clone(), Some(orig.clone()), false);
        let b = mix_all(&mut refined, n, 256);
        for c in 0..2 {
            assert!(
                a[c].iter()
                    .zip(&b[c])
                    .all(|(x, y)| x.to_bits() == y.to_bits()),
                "{mode:?} {g:?}"
            );
        }
        if mode == MixMode::Original && g == [1.0; 3] {
            assert!(a[0]
                .iter()
                .zip(orig.channels[0].iter())
                .all(|(x, y)| x == y));
        }
    }
}

#[test]
fn an_exception_flips_with_the_switch() {
    let n = 60_000;
    let (stems, orig) = song(n);
    let g = &stems[0];
    let left = pan_offline(&PanParams::left(), SR, &g.channels[0], &g.channels[1]);
    let mid = 10_000..n - 10_000;
    let resid = |out: &[f32], want: &dyn Fn(usize) -> f32| {
        rms(&mid.clone().map(|i| out[i] - want(i)).collect::<Vec<_>>())
    };

    // Guitar ON except left, everything on: the original minus the left
    // guitar (the unheard part leaves the mix). The right guitar stays.
    let mut m = Mixer::new(vec![], &gains([1.0, 1.0, 1.0]));
    m.set_mode(MixMode::Original);
    m.set_refine("guitar", except_left(true));
    m.load_with_original(stems.clone(), Some(orig.clone()), false);
    let out = mix_all(&mut m, n, 512);
    for c in 0..2 {
        let want: Vec<f32> = (0..n).map(|i| orig.channels[c][i] - left[c][i]).collect();
        assert!(max_diff(&out[c], &want) < 1e-5, "on except left, ch {c}");
    }
    let r = resid(&out[0], &|i| orig.channels[0][i] - g.channels[0][i]);
    assert!(r < 0.01, "left guitar remains: {r}");
    let r = resid(&out[1], &|i| orig.channels[1][i]);
    assert!(r < 0.01, "right channel changed: {r}");

    // Guitar OFF except left (heard at its volume), the rest on: the
    // original minus the right guitar.
    let mut m = Mixer::new(vec![], &gains([1.0, 1.0, 1.0]));
    m.set_mode(MixMode::Original);
    m.set_refine("guitar", except_left(false));
    m.load_with_original(stems.clone(), Some(orig.clone()), false);
    let out = mix_all(&mut m, n, 700);
    for c in 0..2 {
        let want: Vec<f32> = (0..n)
            .map(|i| orig.channels[c][i] - g.channels[c][i] + left[c][i])
            .collect();
        assert!(max_diff(&out[c], &want) < 1e-5, "off except left, ch {c}");
    }
    let r = resid(&out[0], &|i| orig.channels[0][i]);
    assert!(r < 0.01, "left channel changed: {r}");
    let r = resid(&out[1], &|i| orig.channels[1][i] - g.channels[1][i]);
    assert!(r < 0.01, "right guitar remains: {r}");

    // Solo (plain sum) plays exactly what the row plays: ON except left at
    // 0.5 = half the right guitar; OFF except left = the left guitar.
    for (on, gain) in [(true, 0.5f32), (false, 1.0)] {
        let mut m = Mixer::new(vec![], &gains([gain, 0.0, 0.0]));
        m.set_mode(MixMode::Stems);
        m.set_refine("guitar", except_left(on));
        m.load_with_original(stems.clone(), Some(orig.clone()), false);
        let out = mix_all(&mut m, n, 256);
        for c in 0..2 {
            let want: Vec<f32> = (0..n)
                .map(|i| {
                    let heard = if on {
                        g.channels[c][i] - left[c][i]
                    } else {
                        left[c][i]
                    };
                    gain * heard
                })
                .collect();
            assert!(max_diff(&out[c], &want) < 1e-5, "solo on={on}, ch {c}");
        }
    }
}

#[test]
fn flipping_the_switch_with_an_exception_crossfades() {
    let n = 2 * SR as usize;
    let (stems, orig) = song(n);
    let g = gains([1.0, 1.0, 1.0]);
    let mut p = Pipeline::new(vec![], &g, false);
    p.set_mix(g.clone(), MixMode::Original);
    p.set_refine("guitar", except_left(true));
    p.load_with_original(stems.clone(), Some(orig.clone()));
    p.play();
    let (a, _) = run(&mut p, 20_000, 256);
    // Switch off (the gain stays: the left part is now heard), then on.
    p.set_refine("guitar", except_left(false));
    let (b, _) = run(&mut p, 20_000, 256);
    p.set_refine("guitar", except_left(true));
    let (c, _) = run(&mut p, 20_000, 256);
    let mut all = a[0].clone();
    all.extend_from_slice(&b[0]);
    all.extend_from_slice(&c[0]);
    let sig = orig.channels[0]
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f32, f32::max);
    let st = all
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f32, f32::max);
    assert!(st < sig * 1.5, "step {st} vs {sig}");
    // Settled after each flip: left channel without, then with the left guitar.
    let gl = &stems[0].channels[0];
    let tail = 15_000..20_000;
    let d = |x: &[f32], off: usize, with: bool| {
        rms(&tail
            .clone()
            .map(|i| x[i] - (orig.channels[0][off + i] - if with { 0.0 } else { gl[off + i] }))
            .collect::<Vec<_>>())
    };
    assert!(d(&a[0], 0, false) < 0.01, "{}", d(&a[0], 0, false));
    assert!(d(&b[0], 20_000, true) < 0.01, "{}", d(&b[0], 20_000, true));
    assert!(
        d(&c[0], 40_000, false) < 0.01,
        "{}",
        d(&c[0], 40_000, false)
    );
}

#[test]
fn eq_on_a_stem_is_heard_in_the_full_mix_and_in_solo() {
    let n = 50_000;
    let (stems, orig) = song(n);
    let p = RefineParams {
        eq: eq_preset("bass-notes").unwrap().eq,
        ..RefineParams::default()
    };
    let b = &stems[1];
    let r = refine_offline(&p, SR, &b.channels[0], &b.channels[1]);
    // All on: original − bass + EQ(bass).
    let mut m = Mixer::new(vec![], &gains([1.0, 1.0, 1.0]));
    m.set_mode(MixMode::Original);
    m.set_refine("bass", p);
    m.load_with_original(stems.clone(), Some(orig.clone()), false);
    let out = mix_all(&mut m, n, 256);
    for c in 0..2 {
        let want: Vec<f32> = (0..n)
            .map(|i| orig.channels[c][i] - b.channels[c][i] + r[c][i])
            .collect();
        assert!(max_diff(&out[c], &want) < 1e-5);
    }
    assert!(
        max_diff(&out[0], &orig.channels[0]) > 0.05,
        "EQ not audible"
    );
    // Solo bass at 0.8 (plain sum): 0.8 · EQ(bass), nothing else.
    let mut m = Mixer::new(vec![], &gains([0.0, 0.8, 0.0]));
    m.set_mode(MixMode::Stems);
    m.set_refine("bass", p);
    m.load_with_original(stems.clone(), Some(orig), false);
    let out = mix_all(&mut m, n, 1000);
    let want: Vec<f32> = r[0].iter().map(|x| 0.8 * x).collect();
    assert!(max_diff(&out[0], &want) < 1e-6);
}

fn run(p: &mut Pipeline, frames: usize, block: usize) -> ([Vec<f32>; 2], Vec<Meta>) {
    let mut out = [vec![], vec![]];
    let mut m = vec![];
    let (mut bl, mut br, mut bm) = (
        vec![0.0; block],
        vec![0.0; block],
        vec![Meta::default(); block],
    );
    let mut done = 0;
    while done < frames {
        let k = block.min(frames - done);
        p.render(&mut bl[..k], &mut br[..k], &mut bm[..k]);
        out[0].extend_from_slice(&bl[..k]);
        out[1].extend_from_slice(&br[..k]);
        m.extend_from_slice(&bm[..k]);
        done += k;
    }
    (out, m)
}

/// Guitar OFF except left (the left part heard) with EQ; bass EQ'd.
fn refined_list() -> Vec<(String, RefineParams)> {
    let mut eq = except_left(false);
    eq.eq = eq_preset("guitar-focus").unwrap().eq;
    vec![
        ("guitar".to_string(), eq),
        (
            "bass".to_string(),
            RefineParams {
                eq: eq_preset("bass-notes").unwrap().eq,
                ..RefineParams::default()
            },
        ),
    ]
}

fn request(stems: &[Track], orig: &Track, speed: f64, semis: f64) -> RenderRequest {
    RenderRequest {
        tracks: stems.to_vec(),
        original: Some(orig.clone()),
        mode: MixMode::Original,
        gains: gains([1.0, 1.0, 1.0]),
        speed,
        semitones: semis,
        click: ClickConfig::default(),
        start: 0.0,
        end: None,
        refine: refined_list(),
    }
}

#[test]
fn playback_equals_export_with_refine_on() {
    let n = 3 * SR as usize;
    let (stems, orig) = song(n);
    for (speed, semis) in [(1.0, 0.0), (0.8, 2.0)] {
        let req = request(&stems, &orig, speed, semis);
        let export = render(&req, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        // Playback path: the same pipeline in small real-time-sized blocks
        // (offline flavour: no start fade, no limiter, like the engine's
        // pipeline minus those two).
        let mut p = Pipeline::new(vec![], &req.gains, false);
        p.set_mix(req.gains.clone(), req.mode);
        p.set_refine_all(req.refine.clone());
        p.load_with_original(stems.clone(), Some(orig.clone()));
        p.set_speed(speed);
        p.set_pitch(semis);
        p.seek(0.0);
        p.play();
        let (play, _) = run(&mut p, export[0].len(), 256);
        for c in 0..2 {
            if speed == 1.0 {
                assert_eq!(play[c], export[c], "unstretched must be bit-identical");
            } else {
                // The stretcher itself depends on how its input is chunked;
                // the refine chains do not. Compare against the same check
                // without refine to show refine adds nothing.
                let d = max_diff(&play[c], &export[c]);
                let mut plain_req = req.clone();
                plain_req.refine.clear();
                let plain_export =
                    render(&plain_req, &mut |_| {}, &AtomicBool::new(false)).unwrap();
                let mut q = Pipeline::new(vec![], &req.gains, false);
                q.set_mix(req.gains.clone(), req.mode);
                q.load_with_original(stems.clone(), Some(orig.clone()));
                q.set_speed(speed);
                q.set_pitch(semis);
                q.play();
                let (plain_play, _) = run(&mut q, plain_export[0].len(), 256);
                let d0 = max_diff(&plain_play[c], &plain_export[c]);
                assert!(
                    d <= d0.max(1e-6) * 1.5 + 1e-6,
                    "stretched: {d} vs plain {d0}"
                );
            }
        }
        // And the refined guitar really is in it: unstretched export equals
        // original − guitar + EQ(left guitar) − bass + EQ(bass).
        if speed == 1.0 {
            let rl = refined_list();
            let gr = refine_offline(&rl[0].1, SR, &stems[0].channels[0], &stems[0].channels[1]);
            let br = refine_offline(&rl[1].1, SR, &stems[1].channels[0], &stems[1].channels[1]);
            let want: Vec<f32> = (0..n)
                .map(|i| {
                    orig.channels[0][i] - stems[0].channels[0][i] + gr[0][i]
                        - stems[1].channels[0][i]
                        + br[0][i]
                })
                .collect();
            assert!(max_diff(&export[0], &want) < 1e-5);
        }
    }
}

#[test]
fn seek_and_loop_with_refine_stay_aligned_and_click_free() {
    let n = 4 * SR as usize;
    let (stems, orig) = song(n);
    let g = gains([0.0, 1.0, 1.0]);
    let refine = refined_list();
    // Reference: the whole refined mix, rendered offline from the start.
    let mut req = request(&stems, &orig, 1.0, 0.0);
    req.gains = g.clone();
    let whole = render(&req, &mut |_| {}, &AtomicBool::new(false)).unwrap();

    let mut p = Pipeline::new(vec![], &g, true);
    p.set_mix(g.clone(), MixMode::Original);
    p.set_refine_all(refine);
    p.load_with_original(stems.clone(), Some(orig.clone()));
    p.play();
    let (a, _) = run(&mut p, 20_000, 256);
    // Seek while playing: after the crossfade, aligned with the reference.
    // Not bit-exact: after a restart the mask's STFT frames sit at a
    // different hop phase, so compare within -40 dB of the signal.
    let close = |x: &[f32], y: &[f32]| {
        let d: Vec<f32> = x.iter().zip(y).map(|(a, b)| a - b).collect();
        rms(&d) / rms(y)
    };
    p.seek(2.0);
    let (b, mb) = run(&mut p, 20_000, 256);
    let at = 2 * SR as usize;
    let e = close(&b[0][XFADE + 10..], &whole[0][at + XFADE + 10..at + 20_000]);
    assert!(e < 0.01, "after seek: {e}");
    for (i, m) in mb.iter().enumerate() {
        assert!((m.pos - (at + i) as f64 / SR as f64).abs() < 1e-9);
    }
    // Loop 1.0-1.5 s: every pass after the wrap matches the reference.
    p.set_loop(Some((1.0, 1.5)));
    p.seek(1.0);
    let (c, mc) = run(&mut p, SR as usize + 1000, 256);
    let wraps: Vec<usize> = (0..mc.len())
        .filter(|&i| mc[i].flags & stemx_audio::pipeline::F_WRAP != 0)
        .collect();
    assert_eq!(wraps.len(), 2, "{wraps:?}");
    for &w in &wraps {
        let r = w + XFADE + 10..(w + 10_000).min(c[0].len());
        let src = (mc[r.start].pos * SR as f64).round() as usize;
        let e = close(&c[0][r.clone()], &whole[0][src..src + r.len()]);
        assert!(e < 0.01, "after wrap at {w}: {e}");
    }
    // No clicks anywhere: the steepest step stays within the signal's own.
    let sig = whole[0]
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f32, f32::max);
    for x in [&a[0], &b[0], &c[0]] {
        let st = x
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(st < sig * 1.5 + 0.01, "step {st} vs {sig}");
    }
}

#[test]
fn turning_refine_on_while_playing_is_seamless() {
    let n = 2 * SR as usize;
    let (stems, orig) = song(n);
    let g = gains([1.0, 1.0, 1.0]);
    let mut p = Pipeline::new(vec![], &g, false);
    p.set_mix(g.clone(), MixMode::Original);
    p.load_with_original(stems, Some(orig.clone()));
    p.play();
    let (a, _) = run(&mut p, 10_000, 256);
    // Bass EQ on mid-play, then guitar selection: smoothed, no jump.
    p.set_refine("bass", refined_list()[1].1);
    let (b, _) = run(&mut p, 10_000, 256);
    p.set_refine("guitar", except_left(true));
    let (c, _) = run(&mut p, 20_000, 256);
    let mut all = a[0].clone();
    all.extend_from_slice(&b[0]);
    all.extend_from_slice(&c[0]);
    let sig = orig.channels[0]
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f32, f32::max);
    let st = all
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f32, f32::max);
    assert!(st < sig * 1.5, "step {st} vs {sig}");
    // Before any change: exactly the original.
    assert_eq!(&a[0][..], &orig.channels[0][..10_000]);
}

#[test]
fn stereo_field_comes_from_the_analysed_stem_only() {
    let n = SR as usize;
    let (stems, orig) = song(n);
    let g = gains([1.0, 1.0, 1.0]);
    let mut p = Pipeline::new(vec![], &g, false);
    p.set_mix(g, MixMode::Original);
    p.load_with_original(stems, Some(orig));
    p.play();
    let mut f = [0.0f32; stemx_audio::FIELD_LEN];
    run(&mut p, 8192, 256);
    assert_eq!(p.take_field(&mut f), 0, "analysis is off by default");
    p.set_analysis(Some("guitar"));
    run(&mut p, 8192, 256);
    assert!(p.take_field(&mut f) > 0);
    // Guitar: hard left and hard right parts, nothing in the centre.
    let bin = |k: usize| f[41 + k]; // mid band
    assert!(
        bin(0) > 100.0 * bin(20) && bin(40) > 100.0 * bin(20),
        "{:?}",
        &f[41..82]
    );
}
