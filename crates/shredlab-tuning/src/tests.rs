// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Estimator tests on synthesized stems: equal-tempered notes with several
//! harmonics and mild string inharmonicity, at known offsets.

use std::time::Instant;

use crate::{estimate_song_tuning, StemAudio, StemKind, TuningEstimate};

const RATE: u32 = 44_100;

/// Deterministic xorshift generator.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + (self.next() * f64::from(hi - lo + 1)) as i32
    }
}

/// Harmonic amplitudes (partial 1 first) and inharmonicity `b`: partial
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
/// Keys and pads: harmonic.
const KEYS: Voice = Voice {
    partials: &[1.0, 0.5, 0.33, 0.25, 0.2, 0.17],
    b: 0.0,
};
const PAD: Voice = Voice {
    partials: &[1.0, 0.4, 0.2, 0.1],
    b: 0.0,
};

/// Adds a plucked note (fast attack, exponential decay, faster for upper
/// partials) starting at `start_s`. Uses recursive oscillators to keep
/// long songs cheap.
#[allow(clippy::too_many_arguments)]
fn add_note(
    out: &mut [f32],
    rate: f64,
    start_s: f64,
    dur_s: f64,
    f0: f64,
    amp: f64,
    voice: &Voice,
    rng: &mut Rng,
) {
    let start = ((start_s * rate) as usize).min(out.len());
    let len = ((dur_s * rate) as usize).min(out.len() - start);
    let attack = 0.005 * rate;
    let decay = 1.0 / (0.4 * dur_s * rate);
    for (i, &partial_amp) in voice.partials.iter().enumerate() {
        let h = (i + 1) as f64;
        let f = h * f0 * (1.0 + voice.b * h * h).sqrt();
        if f > 0.45 * rate {
            break;
        }
        let w = std::f64::consts::TAU * f / rate;
        let (step_im, step_re) = w.sin_cos();
        let phase = std::f64::consts::TAU * rng.next();
        let (mut im, mut re) = phase.sin_cos();
        let a = amp * partial_amp;
        let mut env_decay = 1.0;
        let decay_step = (-decay * (1.0 + 0.3 * h)).exp();
        for (j, y) in out[start..start + len].iter_mut().enumerate() {
            let env = (j as f64 / attack).min(1.0) * env_decay;
            *y += (a * env * im) as f32;
            env_decay *= decay_step;
            let r = re * step_re - im * step_im;
            im = re * step_im + im * step_re;
            re = r;
        }
    }
}

fn hz(midi: i32, cents: f64) -> f64 {
    440.0 * 2f64.powf((f64::from(midi - 69) * 100.0 + cents) / 1200.0)
}

/// A song with bass, guitar (single notes and dyads) and keys (triads).
/// `cents_at(t)` gives the tuning offset at time `t`.
struct Song {
    bass: Vec<f32>,
    guitar: Vec<f32>,
    keys: Vec<f32>,
    other: Vec<f32>,
}

impl Song {
    fn new(seconds: f64, seed: u64, cents_at: impl Fn(f64) -> f64) -> Self {
        let rate = f64::from(RATE);
        let n = (seconds * rate) as usize;
        let mut rng = Rng(seed);
        let mut song = Song {
            bass: vec![0.0; n],
            guitar: vec![0.0; n],
            keys: vec![0.0; n],
            other: vec![0.0; n],
        };
        // Bass: E1..G2, eighth-ish notes.
        let mut t = 0.0;
        while t < seconds {
            let d = 0.25 + 0.25 * rng.next();
            let note = rng.range(28, 43);
            add_note(
                &mut song.bass,
                rate,
                t,
                d,
                hz(note, cents_at(t)),
                0.5,
                &BASS,
                &mut rng,
            );
            t += d;
        }
        // Guitar: E2..E5, single notes and fifths.
        let mut t = 0.0;
        while t < seconds {
            let d = 0.2 + 0.4 * rng.next();
            let note = rng.range(40, 76);
            let c = cents_at(t);
            add_note(
                &mut song.guitar,
                rate,
                t,
                d,
                hz(note, c),
                0.3,
                &GUITAR,
                &mut rng,
            );
            if rng.next() < 0.4 {
                let f = hz(note + 7, c);
                add_note(&mut song.guitar, rate, t, d, f, 0.25, &GUITAR, &mut rng);
            }
            t += d;
        }
        // Keys: major and minor triads, C3..C5.
        let mut t = 0.0;
        while t < seconds {
            let d = 0.8 + 0.8 * rng.next();
            let root = rng.range(48, 72);
            let third = if rng.next() < 0.5 { 3 } else { 4 };
            let c = cents_at(t);
            for interval in [0, third, 7] {
                let f = hz(root + interval, c);
                add_note(&mut song.keys, rate, t, d, f, 0.2, &KEYS, &mut rng);
            }
            t += d;
        }
        // Other: a quiet pad an octave up.
        let mut t = 0.0;
        while t < seconds {
            let d = 1.0 + rng.next();
            let note = rng.range(60, 84);
            add_note(
                &mut song.other,
                rate,
                t,
                d,
                hz(note, cents_at(t)),
                0.1,
                &PAD,
                &mut rng,
            );
            t += d;
        }
        song
    }

    fn stems(&self) -> Vec<StemAudio<'_>> {
        vec![
            StemAudio {
                kind: StemKind::Bass,
                samples: &self.bass,
            },
            StemAudio {
                kind: StemKind::Guitar,
                samples: &self.guitar,
            },
            StemAudio {
                kind: StemKind::Keys,
                samples: &self.keys,
            },
            StemAudio {
                kind: StemKind::Other,
                samples: &self.other,
            },
        ]
    }
}

/// Distance between two offsets in cents, across the ±50 wrap.
fn wrapped_error(measured: f32, expected: f64) -> f64 {
    let d = f64::from(measured) - expected;
    d - 100.0 * (d / 100.0).round()
}

fn estimate(stems: &[StemAudio]) -> TuningEstimate {
    estimate_song_tuning(stems, RATE).expect("an estimate")
}

#[test]
fn offsets_within_one_cent() {
    let mut worst: f64 = 0.0;
    for (i, cents) in (-45..=45).step_by(5).enumerate() {
        let cents = f64::from(cents);
        let song = Song::new(30.0, 0x9E37_79B9 + i as u64, |_| cents);
        let e = estimate(&song.stems());
        let err = f64::from(e.offset_cents) - cents;
        worst = worst.max(err.abs());
        assert!(err.abs() <= 1.0, "{cents}: measured {}", e.offset_cents);
        assert!(e.is_reliable(), "{cents}: {e:?}");
        assert!(!e.drifts, "{cents}: {:?}", e.segments);
        let reference = 440.0 * 2f64.powf(f64::from(e.offset_cents) / 1200.0);
        assert!((f64::from(e.reference_hz) - reference).abs() < 1e-3);
    }
    println!("worst error over -45..+45 cents: {worst:.3}");
}

#[test]
fn offsets_near_the_wrap() {
    for (i, cents) in [-50.0, -49.0, -47.5, 47.5, 49.0, 50.0]
        .into_iter()
        .enumerate()
    {
        let song = Song::new(30.0, 0x1234_5678 + i as u64, |_| cents);
        let e = estimate(&song.stems());
        let err = wrapped_error(e.offset_cents, cents);
        assert!(err.abs() <= 1.0, "{cents}: measured {}", e.offset_cents);
        assert!((-50.0..=50.0).contains(&e.offset_cents));
        assert!(e.is_reliable());
    }
}

#[test]
fn linear_drift_is_detected() {
    let (from, to, seconds) = (-22.0, -9.0, 60.0);
    let cents_at = |t: f64| from + (to - from) * t / seconds;
    let song = Song::new(seconds, 42, cents_at);
    let e = estimate(&song.stems());
    assert!(e.drifts, "{e:?}");
    assert_eq!(e.segments.len(), 6, "{:?}", e.segments);
    for s in &e.segments {
        let mid = cents_at(f64::from(s.start_s) + 5.0);
        assert!(
            (f64::from(s.offset_cents) - mid).abs() <= 1.5,
            "segment at {}: {} vs {mid}",
            s.start_s,
            s.offset_cents
        );
    }
    assert!(e
        .segments
        .windows(2)
        .all(|w| w[1].offset_cents > w[0].offset_cents));
    let mean = (from + to) / 2.0;
    assert!((f64::from(e.offset_cents) - mean).abs() <= 1.5, "{e:?}");
}

#[test]
fn silence_gives_none() {
    let silent = vec![0.0f32; RATE as usize * 20];
    let stems = [
        StemAudio {
            kind: StemKind::Bass,
            samples: &silent,
        },
        StemAudio {
            kind: StemKind::Other,
            samples: &silent,
        },
        StemAudio {
            kind: StemKind::Vocals,
            samples: &silent,
        },
    ];
    assert_eq!(estimate_song_tuning(&stems, RATE), None);
    assert_eq!(estimate_song_tuning(&[], RATE), None);
    let empty = StemAudio {
        kind: StemKind::Guitar,
        samples: &[],
    };
    assert_eq!(estimate_song_tuning(&[empty], RATE), None);
    let one = Song::new(5.0, 7, |_| 0.0);
    let bass = StemAudio {
        kind: StemKind::Bass,
        samples: &one.bass,
    };
    assert_eq!(estimate_song_tuning(&[bass], 0), None);
}

#[test]
fn bass_only() {
    for cents in [-31.0, 12.0] {
        let song = Song::new(30.0, 99, |_| cents);
        let bass = StemAudio {
            kind: StemKind::Bass,
            samples: &song.bass,
        };
        let e = estimate(&[bass]);
        assert!(
            (f64::from(e.offset_cents) - cents).abs() <= 1.0,
            "{cents}: {e:?}"
        );
        assert!(e.is_reliable());
    }
}

/// Separated stems carry bleed and artifacts; broadband noise adds peaks at
/// random offsets, which lowers the confidence but must not bias the mean.
#[test]
fn noise_lowers_confidence_only() {
    let mut song = Song::new(30.0, 77, |_| -27.0);
    let clean = estimate(&song.stems()).confidence;
    let mut rng = Rng(1234);
    for stem in [
        &mut song.bass,
        &mut song.guitar,
        &mut song.keys,
        &mut song.other,
    ] {
        for y in stem.iter_mut() {
            *y += (0.05 * (rng.next() - 0.5)) as f32;
        }
    }
    let e = estimate(&song.stems());
    println!("confidence clean {clean:.3}, noisy {:.3}", e.confidence);
    assert!(e.confidence < clean);
    assert!((e.offset_cents + 27.0).abs() <= 1.0, "{e:?}");
}

/// A bass holding one low note: no averaging over notes, so any
/// frequency-dependent bias of the peak interpolation would show.
#[test]
fn held_low_note() {
    let rate = f64::from(RATE);
    let mut rng = Rng(8);
    for (note, cents) in [(28, 23.0), (33, -8.0), (31, 41.0)] {
        let mut bass = vec![0.0f32; RATE as usize * 20];
        for i in 0..40 {
            add_note(
                &mut bass,
                rate,
                f64::from(i) * 0.5,
                0.5,
                hz(note, cents),
                0.5,
                &BASS,
                &mut rng,
            );
        }
        let e = estimate(&[StemAudio {
            kind: StemKind::Bass,
            samples: &bass,
        }]);
        let err = f64::from(e.offset_cents) - cents;
        assert!(err.abs() <= 1.0, "note {note} at {cents}: {e:?}");
    }
}

#[test]
fn vocals_are_a_fallback_only() {
    let song = Song::new(20.0, 5, |_| 20.0);
    let other = Song::new(20.0, 6, |_| -20.0);
    let silent = vec![0.0f32; song.bass.len()];
    // Vocals (here a synthetic stand-in) are ignored when another stem plays...
    let stems = [
        StemAudio {
            kind: StemKind::Guitar,
            samples: &song.guitar,
        },
        StemAudio {
            kind: StemKind::Vocals,
            samples: &other.guitar,
        },
    ];
    assert!((estimate(&stems).offset_cents - 20.0).abs() <= 1.0);
    // ...and used when every other stem is silent.
    let stems = [
        StemAudio {
            kind: StemKind::Guitar,
            samples: &silent,
        },
        StemAudio {
            kind: StemKind::Vocals,
            samples: &other.guitar,
        },
    ];
    assert!((estimate(&stems).offset_cents + 20.0).abs() <= 1.0);
}

#[test]
fn other_sample_rates() {
    // The window scales with the rate; synthesize at 48 kHz.
    let rate = 48_000u32;
    let mut samples = vec![0.0f32; rate as usize * 20];
    let mut rng = Rng(3);
    let mut t = 0.0;
    while t < 19.5 {
        let note = rng.range(40, 70);
        add_note(
            &mut samples,
            f64::from(rate),
            t,
            0.4,
            hz(note, 17.0),
            0.3,
            &GUITAR,
            &mut rng,
        );
        t += 0.4;
    }
    let stems = [StemAudio {
        kind: StemKind::Guitar,
        samples: &samples,
    }];
    let e = estimate_song_tuning(&stems, rate).expect("an estimate");
    assert!((e.offset_cents - 17.0).abs() <= 1.0, "{e:?}");
}

/// Five minutes of four stems. Prints the time; the bound only catches
/// something pathological, so the test doesn't depend on the machine.
#[test]
fn five_minute_song_runtime() {
    let song = Song::new(300.0, 11, |_| -14.0);
    let start = Instant::now();
    let e = estimate(&song.stems());
    let elapsed = start.elapsed();
    println!(
        "5 min, 4 stems: {:.2} s, {} peaks",
        elapsed.as_secs_f64(),
        e.peak_count
    );
    assert!((e.offset_cents + 14.0).abs() <= 1.0);
    assert!(elapsed.as_secs() < 60);
}
