// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! End-to-end check: decode a song, run the full pipeline with ONNX Runtime
//! (CPU EP) and compare with the Python reference fixtures.
//!
//! ```text
//! .venv/Scripts/python.exe tools/beat_this_fixtures.py      # once, for the comparison
//! cargo run --release -p shredlab-beats --example beat_e2e -- \
//!     [--model models/onnx/beat_this_final0.onnx] [--threads 8] <audio>...
//! ```
//!
//! The ORT closure here is what the app's separation crate should provide.

use std::path::{Path, PathBuf};
use std::time::Instant;

use ort::session::Session;
use ort::value::TensorRef;
use shredlab_beats::{infer, postprocess, preprocess, BeatGrid, N_MELS};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Debug)]
struct E(String);
impl std::fmt::Display for E {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for E {}
impl From<shredlab_beats::Error> for E {
    fn from(e: shredlab_beats::Error) -> Self {
        E(e.to_string())
    }
}
impl From<ort::Error> for E {
    fn from(e: ort::Error) -> Self {
        E(e.to_string())
    }
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn main() -> Res<()> {
    let mut model = root().join("models/onnx/beat_this_final0.onnx");
    let mut threads = 8usize;
    let mut files = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--model" => model = args.next().ok_or("--model needs a path")?.into(),
            "--threads" => threads = args.next().ok_or("--threads needs n")?.parse()?,
            _ => files.push(PathBuf::from(a)),
        }
    }
    if files.is_empty() {
        return Err("usage: beat_e2e [--model m.onnx] [--threads n] <audio>...".into());
    }

    let t0 = Instant::now();
    let env = ort::init()
        .with_name("shredlab-beats")
        .build()
        .map_err(|e| E(e.to_string()))?;
    let mut session = Session::builder(&env)?
        .with_intra_threads(threads)
        .map_err(|e| E(e.to_string()))?
        .commit_from_file(&model)?;
    println!(
        "model {} | ORT CPU, {threads} threads | session {:.2}s",
        model.file_name().unwrap().to_string_lossy(),
        t0.elapsed().as_secs_f64()
    );

    for path in &files {
        let t0 = Instant::now();
        let (channels, sr) = decode(path)?;
        let t1 = Instant::now();
        let refs: Vec<&[f32]> = channels.iter().map(Vec::as_slice).collect();
        let spec = preprocess(&refs, sr)?;
        let t2 = Instant::now();
        let mut n_chunks = 0;
        let logits = infer(&spec, |input: &[f32], shape: [usize; 3]| -> Result<_, E> {
            n_chunks += 1;
            let t = TensorRef::from_array_view((shape, input))?;
            let out = session.run(ort::inputs!["spect" => t])?;
            let (_, b) = out["beat"].try_extract_tensor::<f32>()?;
            let (_, d) = out["downbeat"].try_extract_tensor::<f32>()?;
            Ok((b.to_vec(), d.to_vec()))
        })?;
        let t3 = Instant::now();
        let grid = postprocess(&logits);
        let t4 = Instant::now();

        let stem = path.file_stem().unwrap().to_string_lossy();
        let secs = channels[0].len() as f64 / sr as f64;
        println!("== {stem} ({secs:.0}s, {sr} Hz, {} ch)", channels.len());
        println!(
            "  decode {:.2}s | preprocess {:.2}s | net {:.2}s ({n_chunks} chunks) | post {:.1}ms",
            (t1 - t0).as_secs_f64(),
            (t2 - t1).as_secs_f64(),
            (t3 - t2).as_secs_f64(),
            (t4 - t3).as_secs_f64() * 1e3
        );
        println!(
            "  rust: {} beats, {} downbeats, {:.1} BPM, {:?} beats/bar",
            grid.beats.len(),
            grid.downbeats.len(),
            grid.tempo_bpm().unwrap_or(0.0),
            grid.most_common_beats_per_bar()
        );
        compare(&stem, &spec.data, &grid);
    }
    Ok(())
}

/// Same key as tools/beat_this_fixtures.py.
fn fixture_key(stem: &str) -> String {
    let s: String = stem
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    format!("song_{}", s.trim_matches('_'))
}

fn read<T, const N: usize>(p: &Path, f: fn([u8; N]) -> T) -> Option<Vec<T>> {
    let b = std::fs::read(p).ok()?;
    Some(b.as_chunks::<N>().0.iter().map(|c| f(*c)).collect())
}

fn compare(stem: &str, spect: &[f32], grid: &BeatGrid) {
    let dir = root().join("tests/fixtures/local/beat_this");
    let key = fixture_key(stem);
    let (Some(rb), Some(rd)) = (
        read(
            &dir.join(format!("{key}_beats.float64")),
            f64::from_le_bytes,
        ),
        read(
            &dir.join(format!("{key}_downs.float64")),
            f64::from_le_bytes,
        ),
    ) else {
        println!("  (no reference fixture {key}; run tools/beat_this_fixtures.py)");
        return;
    };
    if let Some(rs) = read(
        &dir.join(format!("{key}_spect.float32")),
        f32::from_le_bytes,
    ) {
        let n = rs.len().min(spect.len());
        let s: f64 = rs[..n].iter().map(|&v| (v as f64).powi(2)).sum();
        let e: f64 = rs[..n]
            .iter()
            .zip(&spect[..n])
            .map(|(&a, &b)| (a as f64 - b as f64).powi(2))
            .sum();
        println!(
            "  spect: {} frames vs ref {}, SNR {:.1} dB",
            spect.len() / N_MELS,
            rs.len() / N_MELS,
            10.0 * (s / e).log10()
        );
    }
    for (what, got, want) in [
        ("beats", &grid.beats, &rb),
        ("downbeats", &grid.downbeats, &rd),
    ] {
        let hit = |a: &[f64], b: &[f64]| {
            a.iter()
                .filter(|&&x| b.iter().any(|&y| (x - y).abs() <= 0.020 + 1e-9))
                .count()
        };
        let moved = want
            .iter()
            .filter(|&&y| !got.iter().any(|&x| (x - y).abs() < 1e-6))
            .count();
        let exact = got.len() == want.len()
            && got
                .iter()
                .zip(want.iter())
                .all(|(a, b)| (a - b).abs() < 1e-9);
        let max_off = want
            .iter()
            .map(|&y| {
                got.iter()
                    .map(|&x| (x - y).abs())
                    .fold(f64::INFINITY, f64::min)
            })
            .fold(0.0, f64::max);
        println!(
            "  {what}: rust {} vs ref {} | ref matched ±20ms {:.2}% | rust matched {:.2}% | identical {exact} ({moved} ref not hit exactly) | max |offset| {:.0} ms",
            got.len(),
            want.len(),
            100.0 * hit(want, got) as f64 / want.len().max(1) as f64,
            100.0 * hit(got, want) as f64 / got.len().max(1) as f64,
            max_off * 1e3
        );
    }
}

/// Minimal symphonia decode to planar f32 at the native rate.
fn decode(path: &Path) -> Res<(Vec<Vec<f32>>, u32)> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
    use symphonia::core::errors::Error as SymError;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let mss = MediaSourceStream::new(Box::new(std::fs::File::open(path)?), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let fmt = FormatOptions {
        enable_gapless: true,
        ..Default::default()
    };
    let mut format = symphonia::default::get_probe()
        .format(&hint, mss, &fmt, &MetadataOptions::default())?
        .format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or("no audio track")?;
    let id = track.id;
    let mut sr = track.codec_params.sample_rate;
    let mut dec =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;
    let mut ch: Vec<Vec<f32>> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        };
        if packet.track_id() != id {
            continue;
        }
        let decoded = match dec.decode(&packet) {
            Ok(d) => d,
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(e.into()),
        };
        let spec = *decoded.spec();
        sr.get_or_insert(spec.rate);
        let nc = spec.channels.count();
        if ch.is_empty() {
            ch = vec![Vec::new(); nc];
        }
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buf.copy_interleaved_ref(decoded);
        for frame in buf.samples().chunks_exact(nc) {
            for (c, &v) in ch.iter_mut().zip(frame) {
                c.push(v);
            }
        }
    }
    Ok((ch, sr.ok_or("unknown sample rate")?))
}
