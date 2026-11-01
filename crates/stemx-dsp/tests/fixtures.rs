//! Golden tests against `tests/fixtures/` (Python reference vectors, see
//! `tests/fixtures/README.md`). Skipped with a message if absent.

use std::path::PathBuf;

use stemx_dsp::chunk::{
    apply_demucs, apply_windowed, chunk_offsets, padded_window, roformer_chunk_starts, song_stats,
    triangular_weights,
};
use stemx_dsp::{hamming_symmetric, HybridStft, Stft};

fn fixtures_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    if dir.join("index.json").is_file() {
        Some(dir)
    } else {
        eprintln!(
            "skipping: {} not found (run tools/make_fixtures.py)",
            dir.display()
        );
        None
    }
}

fn read_bin(dir: &std::path::Path, name: &str) -> Vec<f32> {
    let bytes = std::fs::read(dir.join(format!("{name}.bin"))).unwrap();
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}

fn json(dir: &std::path::Path, name: &str) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(dir.join(name)).unwrap()).unwrap()
}

fn max_abs_diff(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

fn max_abs(a: &[f32]) -> f32 {
    a.iter().map(|x| x.abs()).fold(0.0, f32::max)
}

fn synth_input(dir: &std::path::Path) -> (Vec<f32>, Vec<f32>) {
    let x = read_bin(dir, "synth_input");
    let l = x.len() / 2;
    (x[..l].to_vec(), x[l..].to_vec())
}

fn f32s(v: &serde_json::Value) -> Vec<f32> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap() as f32)
        .collect()
}

fn usizes(v: &serde_json::Value) -> Vec<usize> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_u64().unwrap() as usize)
        .collect()
}

#[test]
fn demucs_stft_matches_reference() {
    let Some(dir) = fixtures_dir() else { return };
    let (l, r) = synth_input(&dir);
    let s = HybridStft::new(4096, 1024);
    let frames = s.frames(l.len());
    let bins = s.bins();
    let want = read_bin(&dir, "demucs_stft"); // (2, 2048, 22, 2)
    assert_eq!(want.len(), 2 * bins * frames * 2);
    let mut got = Vec::with_capacity(want.len());
    for ch in [&l, &r] {
        let mut re = vec![0.0; bins * frames];
        let mut im = vec![0.0; bins * frames];
        s.spec(ch, &mut re, &mut im);
        for i in 0..bins * frames {
            got.push(re[i]);
            got.push(im[i]);
        }
    }
    let rel = max_abs_diff(&got, &want) / max_abs(&want);
    eprintln!("demucs stft rel max err {rel:.2e}");
    assert!(rel < 1e-5, "rel err {rel}");

    // ispec of the reference spectrogram
    let want_i = read_bin(&dir, "demucs_istft");
    let plane = bins * frames;
    let mut got_i = Vec::new();
    for c in 0..2 {
        let z = &want[c * plane * 2..(c + 1) * plane * 2];
        let re: Vec<f32> = z.iter().step_by(2).copied().collect();
        let im: Vec<f32> = z.iter().skip(1).step_by(2).copied().collect();
        got_i.extend(s.ispec(&re, &im, l.len()));
    }
    let err = max_abs_diff(&got_i, &want_i);
    eprintln!("demucs istft max abs err {err:.2e}");
    assert!(err < 1e-5, "abs err {err}");
}

#[test]
fn roformer_stft_matches_reference() {
    let Some(dir) = fixtures_dir() else { return };
    let (l, r) = synth_input(&dir);
    let s = Stft::new(2048, 512);
    let frames = s.frames(l.len());
    let want = read_bin(&dir, "roformer_stft"); // (2, 1025, 44, 2)
    assert_eq!(want.len(), 2 * s.bins() * frames * 2);
    let mut got = Vec::new();
    for ch in [&l, &r] {
        for z in s.forward(ch) {
            got.push(z.re as f32);
            got.push(z.im as f32);
        }
    }
    let rel = max_abs_diff(&got, &want) / max_abs(&want);
    eprintln!("roformer stft rel max err {rel:.2e}");
    assert!(rel < 1e-5, "rel err {rel}");

    let want_i = read_bin(&dir, "roformer_istft");
    let plane = s.bins() * frames;
    let mut got_i = Vec::new();
    for c in 0..2 {
        let z: Vec<stemx_dsp::Complex64> = want[c * plane * 2..(c + 1) * plane * 2]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| stemx_dsp::Complex64::new(p[0] as f64, p[1] as f64))
            .collect();
        got_i.extend(s.inverse(&z, frames, l.len()));
    }
    let err = max_abs_diff(&got_i, &want_i);
    eprintln!("roformer istft max abs err {err:.2e}");
    assert!(err < 1e-5, "abs err {err}");
}

#[test]
fn song_normalization_matches_reference() {
    let Some(dir) = fixtures_dir() else { return };
    let (l, r) = synth_input(&dir);
    let idx = json(&dir, "index.json");
    let want = &idx["song_normalization"]["synth_input_22050"];
    let (mean, std) = song_stats(&[&l, &r]);
    let (wm, ws) = (
        want["mean"].as_f64().unwrap() as f32,
        want["std"].as_f64().unwrap() as f32,
    );
    assert!(
        (mean - wm).abs() <= 1e-6 * wm.abs().max(1e-3),
        "mean {mean} vs {wm}"
    );
    assert!((std - ws).abs() <= 1e-6 * ws, "std {std} vs {ws}");
}

#[test]
fn demucs_chunking_matches_reference() {
    let Some(dir) = fixtures_dir() else { return };
    let ch = json(&dir, "chunking.json");
    for (k, w) in ch["demucs"]["triangular_weights"].as_object().unwrap() {
        let s: usize = k.parse().unwrap();
        assert_eq!(triangular_weights(s), f32s(w), "weights S={s}");
    }
    for case in ch["demucs"]["cases"].as_array().unwrap() {
        let s = case["segment"].as_u64().unwrap() as usize;
        let len = case["length"].as_u64().unwrap() as usize;
        let overlap = case["overlap"].as_f64().unwrap();
        let offsets = chunk_offsets(len, s, overlap);
        let chunks = case["chunks"].as_array().unwrap();
        assert_eq!(offsets.len(), chunks.len(), "S={s} L={len}");
        let input: Vec<Vec<f32>> = case["input"].as_array().unwrap().iter().map(f32s).collect();
        let views: Vec<&[f32]> = input.iter().map(Vec::as_slice).collect();
        for (o, c) in offsets.iter().zip(chunks) {
            assert_eq!(*o as u64, c["offset"].as_u64().unwrap());
            let n = c["n"].as_u64().unwrap() as usize;
            let ws = c["window_start"].as_i64().unwrap();
            let (win, left) = padded_window(&views, *o, n, s);
            assert_eq!(*o as i64 - left as i64, ws);
            // first channel matches x[ws..ws+S] with zeros outside
            for (j, v) in win[..s].iter().enumerate() {
                let i = ws + j as i64;
                let want = if (0..len as i64).contains(&i) {
                    input[0][i as usize]
                } else {
                    0.0
                };
                assert_eq!(*v, want);
            }
        }
        // fake model y[j] = window[j] * (1 + j / S), one stem
        let out = apply_demucs(
            &views,
            s,
            overlap,
            1,
            |w: &[f32]| -> Result<Vec<f32>, ()> {
                Ok(w.iter()
                    .enumerate()
                    .map(|(i, v)| v * (1.0 + (i % s) as f32 / s as f32))
                    .collect())
            },
            &mut |_, _| {},
        )
        .unwrap();
        let want: Vec<f32> = case["output"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(f32s)
            .collect();
        let err = max_abs_diff(&out, &want);
        assert!(err < 1e-6, "demucs chunking S={s} L={len}: err {err}");
    }
}

#[test]
fn roformer_chunking_matches_reference() {
    let Some(dir) = fixtures_dir() else { return };
    let ch = json(&dir, "chunking.json");
    for (k, w) in ch["roformer"]["hamming"].as_object().unwrap() {
        let n: usize = k.parse().unwrap();
        let err = max_abs_diff(&hamming_symmetric(n), &f32s(w));
        assert!(err < 1e-7, "hamming {n}: {err}");
    }
    for case in ch["roformer"]["cases"].as_array().unwrap() {
        let c = case["chunk"].as_u64().unwrap() as usize;
        let k = case["num_overlap"].as_u64().unwrap() as usize;
        let len = case["length"].as_u64().unwrap() as usize;
        let starts = roformer_chunk_starts(len, c, c / k);
        assert_eq!(starts, usizes(&case["starts"]), "C={c} K={k} L={len}");
        let input: Vec<Vec<f32>> = case["input"].as_array().unwrap().iter().map(f32s).collect();
        let views: Vec<&[f32]> = input.iter().map(Vec::as_slice).collect();
        let out = apply_windowed(
            &views,
            c,
            &starts,
            &hamming_symmetric(c),
            1,
            |w: &[f32]| -> Result<Vec<f32>, ()> {
                Ok(w.iter()
                    .enumerate()
                    .map(|(i, v)| v * (1.0 + (i % c) as f32 / c as f32))
                    .collect())
            },
            &mut |_, _| {},
        )
        .unwrap();
        let want: Vec<f32> = case["output"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(f32s)
            .collect();
        let err = max_abs_diff(&out, &want);
        assert!(err < 1e-6, "roformer chunking C={c} L={len}: err {err}");
    }
}
