// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Live detector tests on synthesized plucks: decaying partials with mild
//! inharmonicity and a little noise, at known frequencies.

use std::time::Instant;

use crate::{LiveDetector, LiveReading, LEVEL_FLOOR_DB};

/// Deterministic xorshift generator.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Roughly Gaussian, unit variance.
    fn noise(&mut self) -> f64 {
        (0..4).map(|_| self.next() - 0.5).sum::<f64>() * 3f64.sqrt()
    }
}

/// Partial amplitudes (fundamental first) and inharmonicity `b`: partial
/// h sits at `h·f0·sqrt(1 + b·h²)`.
struct Voice {
    partials: &'static [f64],
    b: f64,
}

/// Bass: strong fundamental, slightly stiff strings.
const BASS: Voice = Voice {
    partials: &[1.0, 0.5, 0.33, 0.25, 0.2, 0.17],
    b: 2e-5,
};
/// Guitar: weak fundamental, strongest at the 2nd and 3rd partials.
const GUITAR: Voice = Voice {
    partials: &[0.3, 1.0, 0.9, 0.6, 0.5, 0.4, 0.3, 0.25],
    b: 1e-5,
};
/// High guitar notes: few partials.
const HIGH: Voice = Voice {
    partials: &[1.0, 0.4, 0.25, 0.1],
    b: 1e-5,
};

/// Silence before the pluck, in seconds.
const LEAD_S: f64 = 0.05;

/// A plucked note after [`LEAD_S`] of silence: 5 ms attack, exponential
/// decay (faster for upper partials), white noise 40 dB below the note.
fn pluck(rate: f64, f0: f64, voice: &Voice, dur_s: f64, seed: u64) -> Vec<f32> {
    let n = ((LEAD_S + dur_s) * rate) as usize;
    let start = (LEAD_S * rate) as usize;
    let mut out = vec![0.0f32; n];
    let mut rng = Rng(seed);
    let amp = 0.3;
    for (i, &a) in voice.partials.iter().enumerate() {
        let h = (i + 1) as f64;
        let f = h * f0 * (1.0 + voice.b * h * h).sqrt();
        if f > 0.45 * rate {
            break;
        }
        let w = std::f64::consts::TAU * f / rate;
        let phase = std::f64::consts::TAU * rng.next();
        let tau = 1.5 / (1.0 + 0.4 * (h - 1.0));
        for (j, y) in out[start..].iter_mut().enumerate() {
            let t = j as f64 / rate;
            let env = (t / 0.005).min(1.0) * (-t / tau).exp();
            *y += (amp * a * env * (w * j as f64 + phase).sin()) as f32;
        }
    }
    let noise = amp * 0.01;
    for y in &mut out {
        *y += (noise * rng.noise()) as f32;
    }
    out
}

fn hz(midi: i32, cents: f64) -> f64 {
    440.0 * 2f64.powf((f64::from(midi - 69) * 100.0 + cents) / 1200.0)
}

fn cents(measured: f32, expected: f64) -> f64 {
    1200.0 * (f64::from(measured) / expected).log2()
}

/// Runs the detector over `samples` in device-sized chunks and returns the
/// readings with the time (seconds) at the end of each reading's window.
fn run(det: &mut LiveDetector, samples: &[f32], rate: f64) -> Vec<(f64, LiveReading)> {
    let mut readings = Vec::new();
    for chunk in samples.chunks(441) {
        det.push(chunk, &mut readings);
    }
    // The first reading ends at one window, the rest a hop apart.
    let (w, hop) = (det.window_len(), det.hop_len());
    readings
        .into_iter()
        .enumerate()
        .map(|(k, r)| ((w + k * hop) as f64 / rate, r))
        .collect()
}

/// Readings within the note's first `until_s` seconds (after [`LEAD_S`])
/// once the detector's hold after the attack is over: the attack is seen
/// at the first reading at the latest, and the hold lasts a window, the
/// settling time and a few hops for the attack span and the median.
fn steady(
    readings: &[(f64, LiveReading)],
    det: &LiveDetector,
    rate: f64,
    until_s: f64,
) -> Vec<LiveReading> {
    let (w, hop) = (det.window_len() as f64, det.hop_len() as f64);
    let seen = (w / rate).max(LEAD_S);
    let start = seen + (w + 0.06 * rate + 10.0 * hop) / rate;
    readings
        .iter()
        .filter(|(t, _)| *t >= start && *t <= LEAD_S + until_s)
        .map(|(_, r)| *r)
        .collect()
}

/// Checks every reported pitch in the steady part of the note is within
/// `tol` cents of `f0` and most readings report one. Returns the worst
/// error.
fn check_note(rate: u32, f0: f64, low_hz: f32, voice: &Voice, tol: f64, seed: u64) -> f64 {
    let r = f64::from(rate);
    let mut det = LiveDetector::new(rate, low_hz);
    let audio = pluck(r, f0, voice, 1.5, seed);
    let readings = run(&mut det, &audio, r);
    let steady = steady(&readings, &det, r, 1.3);
    assert!(!steady.is_empty());
    let reported: Vec<f32> = steady.iter().filter_map(|r| r.hz).collect();
    assert!(
        reported.len() * 10 >= steady.len() * 9,
        "{f0:.2} Hz at {rate}: {} of {} readings reported",
        reported.len(),
        steady.len()
    );
    let worst =
        reported
            .iter()
            .map(|&h| cents(h, f0))
            .fold(0.0f64, |w, c| if c.abs() > w.abs() { c } else { w });
    assert!(
        worst.abs() <= tol,
        "{f0:.2} Hz at {rate}: worst {worst:.2} cents"
    );
    worst
}

/// MIDI notes from B0 (30.87 Hz) to E6 (1318.5 Hz).
const NOTES: [i32; 15] = [23, 28, 33, 38, 40, 43, 45, 50, 55, 59, 64, 69, 76, 81, 88];

#[test]
fn plucks_within_one_cent_across_the_range() {
    let offsets = [-41.0, -17.0, 0.0, 9.0, 23.0, 46.0];
    for rate in [44_100u32, 48_000] {
        for (i, &midi) in NOTES.iter().enumerate() {
            let mut line = String::new();
            for k in 0..3 {
                let off = offsets[(i + 2 * k + rate as usize % 7) % offsets.len()];
                let f0 = hz(midi, off);
                let voice = if f0 < 70.0 {
                    &BASS
                } else if f0 < 700.0 {
                    &GUITAR
                } else {
                    &HIGH
                };
                let low_hz = (0.9 * f0 as f32).min(75.0);
                let worst = check_note(rate, f0, low_hz, voice, 1.0, (i * 3 + k) as u64 + 1);
                line += &format!(" {off:+.0}c:{worst:+.3}");
            }
            println!("{rate} midi {midi}:{line}");
        }
    }
}

#[test]
fn bass_voice_on_guitar_window_and_guitar_voice_on_bass_window() {
    // A bass E1 under the long window and a weak-fundamental A1 under the
    // short one.
    check_note(48_000, hz(28, -7.0), 30.0, &GUITAR, 1.0, 11);
    check_note(48_000, hz(33, 13.0), 50.0, &GUITAR, 1.0, 12);
    check_note(44_100, hz(40, 31.0), 75.0, &BASS, 1.0, 13);
}

/// Weak fundamentals under a strong 2nd harmonic: the period must not be
/// read as half.
#[test]
fn weak_fundamental_gives_the_right_octave() {
    const VOICES: [Voice; 5] = [
        // Fundamental 20 dB under the 2nd, nothing else.
        Voice {
            partials: &[0.1, 1.0],
            b: 1e-5,
        },
        Voice {
            partials: &[0.2, 1.0],
            b: 1e-5,
        },
        // Strong even partials, weak odd ones.
        Voice {
            partials: &[0.1, 1.0, 0.05, 0.6, 0.05, 0.3],
            b: 1e-5,
        },
        // No fundamental at all.
        Voice {
            partials: &[0.0, 1.0, 0.7, 0.5, 0.3],
            b: 1e-5,
        },
        GUITAR,
    ];
    for (v, voice) in VOICES.iter().enumerate() {
        for (k, &midi) in [28, 40, 45, 50, 55, 59, 64, 69].iter().enumerate() {
            let f0 = hz(midi, 12.0);
            let low_hz = (0.9 * f0 as f32).min(75.0);
            let worst = check_note(48_000, f0, low_hz, voice, 1.0, (v * 10 + k) as u64 + 100);
            println!("voice {v} midi {midi}: {worst:+.3}");
        }
    }
}

/// With much more noise (about 20 dB under the note) readings may drop out
/// or wobble, but none lands on another octave.
#[test]
fn noisy_plucks_keep_the_octave() {
    let rate = 48_000u32;
    for (k, &midi) in NOTES.iter().enumerate() {
        let f0 = hz(midi, -6.0);
        let voice = if f0 < 70.0 { &BASS } else { &GUITAR };
        let mut audio = pluck(f64::from(rate), f0, voice, 1.5, k as u64 + 40);
        let mut rng = Rng(k as u64 + 90);
        for y in &mut audio {
            *y += (0.03 * rng.noise()) as f32;
        }
        let mut det = LiveDetector::new(rate, (0.9 * f0 as f32).min(75.0));
        let readings = run(&mut det, &audio, f64::from(rate));
        let steady = steady(&readings, &det, f64::from(rate), 1.3);
        let reported: Vec<f64> = steady
            .iter()
            .filter_map(|r| r.hz)
            .map(|h| cents(h, f0))
            .collect();
        let worst = reported.iter().fold(0.0f64, |w, c| w.max(c.abs()));
        println!(
            "midi {midi}: {} of {} reported, worst {worst:.2} cents",
            reported.len(),
            steady.len()
        );
        assert!(worst < 50.0, "midi {midi}: {worst} cents");
    }
}

#[test]
fn silence_and_noise_give_no_pitch() {
    let rate = 48_000u32;
    let mut det = LiveDetector::new(rate, 75.0);
    let mut readings = Vec::new();
    det.push(&vec![0.0; 48_000], &mut readings);
    assert!(!readings.is_empty());
    for r in &readings {
        assert_eq!(r.hz, None);
        assert_eq!(r.level_db, LEVEL_FLOOR_DB);
    }

    // White noise at about −20 dBFS: loud enough, but not periodic.
    for (seed, low_hz) in [(1u64, 75.0f32), (2, 30.0)] {
        let mut det = LiveDetector::new(rate, low_hz);
        let mut rng = Rng(seed);
        let noise: Vec<f32> = (0..96_000).map(|_| (0.1 * rng.noise()) as f32).collect();
        readings.clear();
        det.push(&noise, &mut readings);
        let level = readings[readings.len() / 2].level_db;
        assert!((level + 20.0).abs() < 1.0, "noise level {level}");
        let pitched = readings.iter().filter(|r| r.hz.is_some()).count();
        let max_clarity = readings.iter().map(|r| r.clarity).fold(0.0, f32::max);
        println!("noise: {pitched} pitched, highest clarity {max_clarity:.3}");
        assert_eq!(pitched, 0);
    }

    // A clean tone under the quiet gate.
    let mut det = LiveDetector::new(rate, 75.0);
    let tone: Vec<f32> = (0..48_000)
        .map(|i| (3e-4 * (std::f64::consts::TAU * 110.0 * i as f64 / 48_000.0).sin()) as f32)
        .collect();
    readings.clear();
    det.push(&tone, &mut readings);
    let last = readings.last().unwrap();
    assert!(last.clarity > 0.95 && last.level_db < -70.0, "{last:?}");
    assert!(readings.iter().all(|r| r.hz.is_none()));
}

#[test]
fn window_and_hop_scale_with_rate() {
    let d = LiveDetector::new(48_000, 75.0);
    assert_eq!((d.window_len(), d.hop_len()), (4096, 512));
    let d = LiveDetector::new(48_000, 30.0);
    assert_eq!((d.window_len(), d.hop_len()), (8192, 512));
    let d = LiveDetector::new(44_100, 75.0);
    assert_eq!((d.window_len(), d.hop_len()), (3763, 470));
    let d = LiveDetector::new(96_000, 75.0);
    assert_eq!((d.window_len(), d.hop_len()), (8192, 1024));
}

#[test]
fn chunking_does_not_change_readings() {
    let audio = pluck(48_000.0, hz(45, 5.0), &GUITAR, 1.0, 7);
    let mut a = LiveDetector::new(48_000, 75.0);
    let mut whole = Vec::new();
    a.push(&audio, &mut whole);
    let mut b = LiveDetector::new(48_000, 75.0);
    let mut parts = Vec::new();
    for chunk in audio.chunks(97) {
        b.push(chunk, &mut parts);
    }
    assert_eq!(whole, parts);
    // One reading per hop once the first window is full.
    assert_eq!(whole.len(), 1 + (audio.len() - 4096) / 512);
}

#[test]
fn median_starts_over_after_a_dropout() {
    let rate = 48_000.0;
    let mut audio = pluck(rate, hz(40, 0.0), &GUITAR, 0.6, 3);
    audio.extend(std::iter::repeat_n(0.0, 24_000));
    audio.extend(pluck(rate, hz(64, 0.0), &GUITAR, 0.6, 4));
    let mut det = LiveDetector::new(48_000, 75.0);
    let readings = run(&mut det, &audio, rate);
    // After the gap the first reported pitch is already the new note.
    let gap_end = (0.65 + 0.5) + LEAD_S;
    let first = readings
        .iter()
        .find(|(t, r)| *t > gap_end && r.hz.is_some())
        .unwrap();
    assert!(
        cents(first.1.hz.unwrap(), hz(64, 0.0)).abs() < 5.0,
        "{first:?}"
    );
}

/// Prints the cost per hop. Run in release for a meaningful number:
/// `cargo test -p shredlab-tuning --release -- --nocapture per_hop`.
#[test]
fn per_hop_cost() {
    let rate = 48_000u32;
    let mut audio = Vec::new();
    for (k, midi) in [23, 28, 40, 45, 64, 76].iter().enumerate() {
        audio.extend(pluck(48_000.0, hz(*midi, 3.0), &GUITAR, 2.0, k as u64));
    }
    for low_hz in [75.0, 30.0] {
        let mut det = LiveDetector::new(rate, low_hz);
        let mut readings = Vec::with_capacity(1024);
        let start = Instant::now();
        let mut hops = 0;
        for chunk in audio.chunks(480) {
            det.push(chunk, &mut readings);
            hops += readings.len();
            readings.clear();
        }
        let per_hop = start.elapsed().as_secs_f64() / hops as f64;
        let budget = det.hop_len() as f64 / f64::from(rate);
        println!(
            "window {}: {:.1} us per hop, {:.2}% of real time",
            det.window_len(),
            per_hop * 1e6,
            100.0 * per_hop / budget
        );
        if !cfg!(debug_assertions) {
            assert!(per_hop < 0.1 * budget, "{per_hop} s per hop");
        }
    }
}

/// A pluck with a real attack: a noise burst and the pitch starting
/// `sharp` cents high, settling with a 20 ms time constant.
fn sharp_attack(rate: f64, f0: f64, sharp: f64, dur_s: f64, seed: u64) -> Vec<f32> {
    let n = (dur_s * rate) as usize;
    let mut rng = Rng(seed);
    let mut phase = [0.0f64; 4];
    (0..n)
        .map(|j| {
            let t = j as f64 / rate;
            let f = f0 * 2f64.powf(sharp * (-t / 0.02).exp() / 1200.0);
            let env = (t / 0.003).min(1.0) * (-t / 1.5).exp();
            let mut y = 0.0;
            for (h, (p, a)) in phase.iter_mut().zip([0.5, 1.0, 0.7, 0.4]).enumerate() {
                *p += std::f64::consts::TAU * f * (h + 1) as f64 / rate;
                y += a * p.sin();
            }
            let burst = if t < 0.015 { 0.5 * rng.noise() } else { 0.0 };
            (0.15 * env * y + 0.15 * burst + 0.002 * rng.noise()) as f32
        })
        .collect()
}

#[test]
fn attacks_are_held_back_until_the_note_settles() {
    let rate = 48_000.0;
    for (k, (midi, low_hz)) in [(40, 75.0), (28, 30.0), (64, 75.0)].into_iter().enumerate() {
        let f0 = hz(midi, 0.0);
        // Silence, a pluck, then the same string plucked again while it
        // still rings.
        let mut audio = vec![0.0f32; 24_000];
        audio.extend(sharp_attack(rate, f0, 40.0, 1.0, k as u64));
        audio.extend(sharp_attack(rate, f0, 40.0, 1.0, k as u64 + 10));
        let mut det = LiveDetector::new(48_000, low_hz);
        let readings = run(&mut det, &audio, rate);
        let reported: Vec<(f64, f64)> = readings
            .iter()
            .filter_map(|(t, r)| r.hz.map(|h| (*t, cents(h, f0))))
            .collect();
        let worst = reported.iter().fold(0.0f64, |w, c| w.max(c.1.abs()));
        assert!(worst < 2.0, "midi {midi}: {worst:.2} cents");
        // Each note shows within 0.4 s of its attack.
        for start in [0.5, 1.5] {
            assert!(
                reported.iter().any(|(t, _)| *t > start && *t < start + 0.4),
                "midi {midi}: nothing within 0.4 s of {start}"
            );
        }
    }
}
