// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Separation strength on a cached library song: timing, p = 1 deviation
//! from the cached stems, and the sum check. Reads the library, never writes
//! to it.
//!
//! cargo run --release -p shredlab-dsp --example strength_song -- "<title or song dir>" [set] [--write]
//!
//! `set` is a stem-set dir suffix such as `htdemucs_6s_s1` (default: first).
//! `--write` saves the derived stems as float WAVs in out/strength/<song>/p<p>/.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use shredlab_dsp::strength::{apply_strength, StrengthOptions};

#[path = "common/mod.rs"]
mod common;

fn snr_db(reference: &[f32], x: &[f32]) -> f64 {
    let (mut p, mut e) = (0.0f64, 0.0f64);
    for (a, b) in reference.iter().zip(x) {
        p += (*a as f64).powi(2);
        e += (*a as f64 - *b as f64).powi(2);
    }
    10.0 * (p / e.max(1e-30)).log10()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let write = args.iter().any(|a| a == "--write");
    let pos: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let which = pos.first().map(|s| s.as_str()).unwrap_or("Mr. Crowley");
    let set = pos.get(1).map(|s| s.as_str());
    let dir = if Path::new(which).join("song.json").exists() {
        PathBuf::from(which)
    } else {
        common::find_song(which)
            .unwrap_or_else(|| panic!("song {which:?} not found in the library"))
    };
    let song = common::load_song(&dir, set);
    let t = Instant::now();
    let (sr, orig) = common::read_wav(&dir.join("original.wav"), song.original_scale);
    let stems: Vec<(String, [Vec<f32>; 2])> = song
        .stems
        .iter()
        .map(|(name, path, scale)| (name.clone(), common::read_wav(path, *scale).1))
        .collect();
    let len = orig[0].len();
    println!(
        "{}: {:.1} s, {} stems, loaded in {:.2} s",
        song.title,
        len as f64 / sr as f64,
        stems.len(),
        t.elapsed().as_secs_f64()
    );
    // How far the cached stems are from summing to the original.
    let mut sum = [vec![0.0f32; len], vec![0.0f32; len]];
    for (_, s) in &stems {
        for ch in 0..2 {
            for (a, b) in sum[ch].iter_mut().zip(&s[ch]) {
                *a += b;
            }
        }
    }
    println!(
        "cached stems vs original (sum): {:.1} / {:.1} dB",
        snr_db(&orig[0], &sum[0]),
        snr_db(&orig[1], &sum[1])
    );
    let views: Vec<[&[f32]; 2]> = stems.iter().map(|(_, s)| [&s[0][..], &s[1][..]]).collect();
    let cancel = AtomicBool::new(false);
    // STRENGTH_THREADS=1 to time a single core.
    let opts = StrengthOptions {
        threads: std::env::var("STRENGTH_THREADS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        ..Default::default()
    };
    // STRENGTH_NFFT=2048 etc. to compare STFT sizes (hop = n_fft / 4).
    let opts = match std::env::var("STRENGTH_NFFT")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        Some(n) => StrengthOptions {
            n_fft: n,
            hop: n / 4,
            ..opts
        },
        None => opts,
    };
    // Diagnostic: p = 1 applied to the stems' own sum instead of the
    // original isolates the cost of the shared-bin phase/ratio approximation
    // from the redistributed leftover.
    {
        let out =
            apply_strength([&sum[0], &sum[1]], &views, 1.0, &opts, &mut |_| {}, &cancel).unwrap();
        print!("p 1 on the stems' sum (no leftover) vs cached stems:");
        for ((name, s), o) in stems.iter().zip(&out) {
            let d = (snr_db(&s[0], &o[0]) + snr_db(&s[1], &o[1])) / 2.0;
            print!(" {name} {d:.1}");
        }
        println!(" dB");
    }
    for p in [1.0f32, 2.0, 4.0] {
        let t = Instant::now();
        let out =
            apply_strength([&orig[0], &orig[1]], &views, p, &opts, &mut |_| {}, &cancel).unwrap();
        let el = t.elapsed().as_secs_f64();
        let mut sum = [vec![0.0f32; len], vec![0.0f32; len]];
        for s in &out {
            for ch in 0..2 {
                for (a, b) in sum[ch].iter_mut().zip(&s[ch]) {
                    *a += b;
                }
            }
        }
        print!(
            "p {p}: {el:.2} s ({:.1}x real time); sum vs original {:.0} dB; vs cached stems:",
            len as f64 / sr as f64 / el,
            snr_db(&orig[0], &sum[0]).min(snr_db(&orig[1], &sum[1]))
        );
        for ((name, s), o) in stems.iter().zip(&out) {
            let d = (snr_db(&s[0], &o[0]) + snr_db(&s[1], &o[1])) / 2.0;
            print!(" {name} {d:.1}");
        }
        println!(" dB");
        if write {
            let base = PathBuf::from("out/strength")
                .join(song.title.replace(['/', '\\', ':'], "_"))
                .join(format!("p{p}"));
            for ((name, _), o) in stems.iter().zip(&out) {
                common::write_wav(&base.join(format!("{name}.wav")), sr, o);
            }
            println!("  wrote {}", base.display());
        }
    }
}
