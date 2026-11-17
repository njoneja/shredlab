// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Runs the tuning estimate on every song in a ShredLab library and prints
//! the result per song, for checking the estimator on real stems.
//!
//! cargo run --release -p shredlab-tuning --example measure_library -- <library root>

use shredlab_tuning::{estimate_song_tuning, StemAudio, StemKind};

fn main() {
    let root = std::env::args()
        .nth(1)
        .expect("usage: measure_library <library root>");
    let lib = shredlab_library::Library::open(std::path::Path::new(&root)).expect("open library");
    let songs = std::fs::read_dir(std::path::Path::new(&root).join("songs")).expect("songs");
    for entry in songs.flatten() {
        let id = entry.file_name().to_string_lossy().to_string();
        let Ok(Some(meta)) = lib.load(&id) else {
            continue;
        };
        let Some(info) = meta.stem_sets.last() else {
            continue;
        };
        let Ok(stems) = lib.read_stems(&id, info) else {
            println!("{:40} cannot read stems", meta.title);
            continue;
        };
        let mono: Vec<(StemKind, Vec<f32>)> = stems
            .iter()
            .filter_map(|(name, [l, r])| {
                let kind = match name.as_str() {
                    "bass" => StemKind::Bass,
                    "guitar" => StemKind::Guitar,
                    "piano" | "keys" => StemKind::Keys,
                    "other" => StemKind::Other,
                    "vocals" => StemKind::Vocals,
                    _ => return None,
                };
                Some((kind, l.iter().zip(r).map(|(a, b)| 0.5 * (a + b)).collect()))
            })
            .collect();
        // With SHIFT_CENTS set, measure every stem resampled by those amounts
        // and print the measured change, and its error, next to each.
        if let Ok(list) = std::env::var("SHIFT_CENTS") {
            let base = estimate(&mono, info.sample_rate);
            let mut line = format!("{:32} base {:+6.1}", meta.title, base);
            for c in list.split(',').filter_map(|c| c.trim().parse::<f64>().ok()) {
                let shifted: Vec<(StemKind, Vec<f32>)> =
                    mono.iter().map(|(k, s)| (*k, resample(s, c))).collect();
                let m = estimate(&shifted, info.sample_rate);
                let diff = m - base;
                let diff = diff - 100.0 * (diff / 100.0).round();
                line += &format!("  {c:+}:{:+5.1}", diff - c as f32);
            }
            println!("{line}");
            continue;
        }
        let input: Vec<StemAudio> = mono
            .iter()
            .map(|(kind, s)| StemAudio {
                kind: *kind,
                samples: s,
            })
            .collect();
        let t0 = std::time::Instant::now();
        match estimate_song_tuning(&input, info.sample_rate) {
            Some(e) => println!(
                "{:40} {:+6.1}  conf {:.2}  peaks {:7}  segs {:2}{}  {}  {:.2}s",
                meta.title,
                e.offset_cents,
                e.confidence,
                e.peak_count,
                e.segments.len(),
                if e.drifts { " drifts" } else { "" },
                if e.is_reliable() { "ok " } else { "LOW" },
                t0.elapsed().as_secs_f64()
            ),
            None => println!("{:40} no estimate", meta.title),
        }
        if let Some(e) = estimate_song_tuning(&input, info.sample_rate) {
            let v: Vec<String> = e
                .segments
                .iter()
                .map(|s| format!("{:.0}", s.offset_cents))
                .collect();
            println!("    {}", v.join(" "));
        }
    }
}

fn estimate(mono: &[(StemKind, Vec<f32>)], rate: u32) -> f32 {
    let input: Vec<StemAudio> = mono
        .iter()
        .map(|(kind, s)| StemAudio {
            kind: *kind,
            samples: s,
        })
        .collect();
    estimate_song_tuning(&input, rate).map_or(f32::NAN, |e| e.offset_cents)
}

/// Raises the pitch by `cents` (reads faster), with linear interpolation.
fn resample(s: &[f32], cents: f64) -> Vec<f32> {
    let r = 2f64.powf(cents / 1200.0);
    let n = ((s.len() - 1) as f64 / r) as usize;
    (0..n)
        .map(|i| {
            let x = i as f64 * r;
            let j = x as usize;
            let f = (x - j as f64) as f32;
            s[j] * (1.0 - f) + s[(j + 1).min(s.len() - 1)] * f
        })
        .collect()
}
