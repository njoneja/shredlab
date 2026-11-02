//! Comparison against the NumPy reference (`spikes/onnx_beat_this.py`).
//!
//! Fixtures are generated locally (gitignored) with
//! `.venv/Scripts/python.exe tools/beat_this_fixtures.py`; tests skip with a
//! message when they are missing.

use std::path::PathBuf;

use shredlab_beats::{log_mel, postprocess, preprocess_interleaved, resample_mono, Logits, N_MELS};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/local/beat_this")
}

fn have_fixtures() -> bool {
    let ok = dir().join("meta.json").exists();
    if !ok {
        eprintln!("SKIP: no fixtures; run tools/beat_this_fixtures.py");
    }
    ok
}

fn f32s(name: &str) -> Vec<f32> {
    let b = std::fs::read(dir().join(format!("{name}.float32"))).unwrap();
    b.as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect()
}

fn f64s(name: &str) -> Vec<f64> {
    let b = std::fs::read(dir().join(format!("{name}.float64"))).unwrap();
    b.as_chunks::<8>()
        .0
        .iter()
        .map(|c| f64::from_le_bytes(*c))
        .collect()
}

fn snr_db(r: &[f32], x: &[f32]) -> f64 {
    let s: f64 = r.iter().map(|&v| (v as f64).powi(2)).sum();
    let e: f64 = r
        .iter()
        .zip(x)
        .map(|(&a, &b)| (a as f64 - b as f64).powi(2))
        .sum();
    10.0 * (s / (e + 1e-30)).log10()
}

fn max_abs(r: &[f32], x: &[f32]) -> f32 {
    r.iter()
        .zip(x)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f32::max)
}

const SYNTH: [(&str, u32, usize); 3] = [
    ("synth_44k_stereo", 44100, 2),
    ("synth_48k_mono", 48000, 1),
    ("synth_22k_mono", 22050, 1),
];

/// log-mel alone, from the identical 22050 Hz signal: must match numpy closely.
#[test]
fn log_mel_matches_numpy() {
    if !have_fixtures() {
        return;
    }
    for (name, _, _) in SYNTH {
        let mono = f32s(&format!("{name}_mono22k"));
        let want = f32s(&format!("{name}_spect"));
        let got = log_mel(&mono);
        assert_eq!(got.frames * N_MELS, want.len(), "{name}: frame count");
        let (d, snr) = (max_abs(&want, &got.data), snr_db(&want, &got.data));
        eprintln!("{name}: log-mel max abs diff {d:.2e}, SNR {snr:.1} dB");
        assert!(d < 1e-3, "{name}: max abs diff {d}");
    }
}

/// Full preprocessing (rubato instead of soxr) from the original samples.
#[test]
fn preprocess_close_to_soxr_reference() {
    if !have_fixtures() {
        return;
    }
    for (name, sr, ch) in SYNTH {
        let audio = f32s(&format!("{name}_audio"));
        let want_mono = f32s(&format!("{name}_mono22k"));
        let want = f32s(&format!("{name}_spect"));
        let got = preprocess_interleaved(&audio, ch, sr).unwrap();
        assert_eq!(got.frames * N_MELS, want.len(), "{name}: frame count");
        let mono64: Vec<f64> = (0..audio.len() / ch)
            .map(|i| (0..ch).map(|c| audio[i * ch + c] as f64).sum::<f64>() / ch as f64)
            .collect();
        let mono: Vec<f32> = resample_mono(&mono64, sr)
            .unwrap()
            .iter()
            .map(|&v| v as f32)
            .collect();
        assert_eq!(mono.len(), want_mono.len(), "{name}: resampled length");
        // Ignore 2048 samples at each edge for the waveform SNR (filter edge effects).
        let e = 2048.min(mono.len() / 4);
        let snr_wave = snr_db(&want_mono[e..mono.len() - e], &mono[e..mono.len() - e]);
        let snr = snr_db(&want, &got.data);
        eprintln!(
            "{name}: rubato vs soxr waveform SNR {snr_wave:.1} dB | spect max abs {:.2e}, SNR {snr:.1} dB",
            max_abs(&want, &got.data)
        );
        assert!(snr > 35.0, "{name}: spectrogram SNR {snr:.1} dB");
    }
}

fn songs() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir())
        .unwrap()
        .filter_map(|e| {
            let n = e.unwrap().file_name().into_string().unwrap();
            n.strip_suffix("_beat_logits.float32").map(str::to_owned)
        })
        .collect();
    v.sort();
    v
}

/// Postprocessing on the torch logits must reproduce upstream exactly.
#[test]
fn postprocess_matches_upstream() {
    if !have_fixtures() {
        return;
    }
    let songs = songs();
    assert!(!songs.is_empty());
    for key in songs {
        let logits = Logits {
            beat: f32s(&format!("{key}_beat_logits")),
            downbeat: f32s(&format!("{key}_down_logits")),
        };
        let grid = postprocess(&logits);
        let (rb, rd) = (f64s(&format!("{key}_beats")), f64s(&format!("{key}_downs")));
        assert_eq!(grid.beats.len(), rb.len(), "{key}: beat count");
        assert_eq!(grid.downbeats.len(), rd.len(), "{key}: downbeat count");
        for (a, b) in grid
            .beats
            .iter()
            .zip(&rb)
            .chain(grid.downbeats.iter().zip(&rd))
        {
            assert!((a - b).abs() < 1e-9, "{key}: {a} vs {b}");
        }
        eprintln!(
            "{key}: {} beats, {} downbeats identical; ~{:.1} BPM, {:?} beats/bar",
            rb.len(),
            rd.len(),
            grid.tempo_bpm().unwrap_or(0.0),
            grid.most_common_beats_per_bar()
        );
    }
}
