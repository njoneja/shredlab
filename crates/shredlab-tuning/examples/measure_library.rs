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
