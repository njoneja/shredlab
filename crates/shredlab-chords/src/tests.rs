// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Detector tests on synthesized stems: strummed guitar chords with
//! several partials and mild inharmonicity, plucked bass lines, keys, on
//! known beat grids.

use std::time::Instant;

use crate::{detect_chords, Chord, ChordAnalysis, ChordSegment, Key, Quality, StemAudio, StemKind};

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
}

/// Harmonic amplitudes (partial 1 first) and inharmonicity `b`: partial
/// h sits at `h·f0·sqrt(1 + b·h²)`.
struct Voice {
    partials: &'static [f64],
    b: f64,
}

const BASS: Voice = Voice {
    partials: &[1.0, 0.5, 0.33, 0.25, 0.2, 0.17],
    b: 2e-5,
};
/// Weak fundamental, strongest at the 2nd and 3rd partials; the 5th
/// partial is strong enough to read as a third if it weren't filtered.
const GUITAR: Voice = Voice {
    partials: &[0.3, 1.0, 0.9, 0.6, 0.5, 0.4, 0.3, 0.25],
    b: 1e-5,
};
/// Overdriven guitar: odd partials strong, the 3rd strongest of all, so
/// each string's 3rd harmonic (an octave and a fifth up) is louder than
/// its fundamental and its octaves.
const DRIVEN: Voice = Voice {
    partials: &[0.25, 0.45, 1.0, 0.35, 0.7, 0.25, 0.5, 0.2, 0.35, 0.15],
    b: 1e-5,
};
const KEYS: Voice = Voice {
    partials: &[1.0, 0.5, 0.33, 0.25, 0.2, 0.17],
    b: 0.0,
};

/// Adds a plucked note (fast attack, exponential decay, faster for upper
/// partials) with recursive oscillators.
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
    let start = ((start_s.max(0.0) * rate) as usize).min(out.len());
    let len = ((dur_s * rate) as usize).min(out.len() - start);
    let attack = 0.005 * rate;
    let decay = 1.0 / (0.6 * dur_s.max(0.3) * rate);
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
        // A short release so a note cut off by the next one doesn't click.
        let release = (0.01 * rate) as usize;
        for (j, y) in out[start..start + len].iter_mut().enumerate() {
            let tail = ((len - j) as f64 / release as f64).min(1.0);
            let env = (j as f64 / attack).min(1.0) * env_decay * tail;
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

/// A stretch with one chord: `root` is the guitar's lowest note (MIDI).
#[derive(Debug, Clone, Copy)]
struct Part {
    start: f64,
    end: f64,
    root: i32,
    quality: Quality,
}

/// Guitar voicing: the root, each chord tone above it, and the octave.
fn voicing(root: i32, q: Quality) -> Vec<i32> {
    let mut v = vec![root, root + 12];
    for &i in q.intervals().iter().skip(1) {
        v.push(root + i32::from(i));
        v.push(root + 12 + i32::from(i));
    }
    v.sort_unstable();
    v
}

/// How the bass plays under the parts.
#[derive(Clone, Copy)]
enum BassLine {
    None,
    /// The part's root an octave under the guitar, on every beat.
    Root,
    /// One held note (MIDI) on every beat, whatever the chord.
    Pedal(i32),
}

struct Song {
    rate: u32,
    seconds: f64,
    guitar: Vec<f32>,
    bass: Vec<f32>,
    keys: Vec<f32>,
    other: Vec<f32>,
    beats: Vec<f64>,
    offset: f64,
    rng: Rng,
    /// Voice of the strummed guitar.
    voice: &'static Voice,
}

impl Song {
    fn new(seconds: f64, beats: Vec<f64>, offset: f64, rate: u32) -> Self {
        let n = (seconds * f64::from(rate)) as usize;
        Song {
            rate,
            seconds,
            guitar: vec![0.0; n],
            bass: vec![0.0; n],
            keys: vec![0.0; n],
            other: vec![0.0; n],
            beats,
            offset,
            rng: Rng(0x9E37_79B9_7F4A_7C15),
            voice: &GUITAR,
        }
    }

    /// Strums each part at its start and on every beat inside it, and
    /// plays the bass line.
    fn play(&mut self, parts: &[Part], bass: BassLine) {
        for p in parts {
            let mut hits = vec![p.start];
            hits.extend(
                self.beats
                    .iter()
                    .copied()
                    .filter(|&b| b > p.start + 0.05 && b < p.end - 0.05),
            );
            for (i, &t) in hits.iter().enumerate() {
                let end = hits.get(i + 1).copied().unwrap_or(p.end);
                self.strum(t, end - t, p.root, p.quality, 0.15);
                let note = match bass {
                    BassLine::None => None,
                    BassLine::Root => Some(p.root - 12),
                    BassLine::Pedal(m) => Some(m),
                };
                if let Some(m) = note {
                    self.bass_note(t, end - t, m);
                }
            }
        }
    }

    fn strum(&mut self, t: f64, dur: f64, root: i32, q: Quality, amp: f64) {
        let rate = f64::from(self.rate);
        for (k, m) in voicing(root, q).into_iter().enumerate() {
            let d = 0.006 * k as f64;
            add_note(
                &mut self.guitar,
                rate,
                t + d,
                dur - d,
                hz(m, self.offset),
                amp,
                self.voice,
                &mut self.rng,
            );
        }
    }

    fn bass_note(&mut self, t: f64, dur: f64, m: i32) {
        add_note(
            &mut self.bass,
            f64::from(self.rate),
            t,
            dur,
            hz(m, self.offset),
            0.4,
            &BASS,
            &mut self.rng,
        );
    }

    fn detect(&self) -> ChordAnalysis {
        self.detect_with(&self.beats)
    }

    fn detect_with(&self, beats: &[f64]) -> ChordAnalysis {
        let stems = [
            StemAudio {
                kind: StemKind::Guitar,
                samples: &self.guitar,
            },
            StemAudio {
                kind: StemKind::Bass,
                samples: &self.bass,
            },
            StemAudio {
                kind: StemKind::Keys,
                samples: &self.keys,
            },
            StemAudio {
                kind: StemKind::Other,
                samples: &self.other,
            },
        ];
        let a = detect_chords(&stems, self.rate, beats, self.offset, self.seconds);
        check_contiguous(&a, self.seconds);
        a
    }
}

fn even_beats(seconds: f64, interval: f64) -> Vec<f64> {
    (0..)
        .map(|i| 0.2 + f64::from(i) * interval)
        .take_while(|&t| t < seconds)
        .collect()
}

fn check_contiguous(a: &ChordAnalysis, seconds: f64) {
    let s = &a.segments;
    assert!(!s.is_empty());
    assert_eq!(s[0].start_s, 0.0);
    assert_eq!(s.last().expect("segments").end_s, seconds);
    for w in s.windows(2) {
        assert_eq!(w[0].end_s, w[1].start_s, "{:?}", w);
        assert!(w[0].end_s > w[0].start_s);
        assert!(w[0].chord != w[1].chord, "unmerged neighbors {:?}", w);
    }
    for seg in s {
        match (seg.chord, seg.detailed) {
            (None, None) => {}
            (Some(c), Some(d)) => {
                assert_eq!((c.root, c.bass), (d.root, d.bass), "{seg:?}");
                assert!(
                    d.quality == c.quality || Quality::EXTENDED.contains(&d.quality),
                    "{seg:?}"
                );
                assert!(Quality::ALL.contains(&c.quality), "{seg:?}");
            }
            _ => panic!("detailed without chord or the other way: {seg:?}"),
        }
    }
    for w in a.passing.windows(2) {
        assert!(w[0].end_s <= w[1].start_s, "{:?}", a.passing);
    }
    for p in &a.passing {
        assert!(p.end_s > p.start_s);
        assert!(Quality::ALL.contains(&p.chord.quality));
        assert_eq!(p.chord.bass, None);
        let inside = s
            .iter()
            .find(|g| g.start_s <= p.start_s && p.end_s <= g.end_s)
            .unwrap_or_else(|| panic!("passing chord across segments: {p:?}"));
        assert!(inside.chord.is_some(), "{p:?}");
    }
    for w in a.figures.windows(2) {
        assert!(w[0].end_s <= w[1].start_s, "{:?}", a.figures);
    }
    for f in &a.figures {
        assert!(f.end_s > f.start_s);
        assert!(
            Quality::EXTENDED.contains(&f.chord.quality) || Quality::ALL.contains(&f.chord.quality)
        );
        assert_eq!(f.chord.bass, None);
        let inside = s
            .iter()
            .find(|g| g.start_s <= f.start_s && f.end_s <= g.end_s)
            .unwrap_or_else(|| panic!("figure across segments: {f:?}"));
        let c = inside
            .chord
            .unwrap_or_else(|| panic!("figure outside a chord: {f:?}"));
        assert_eq!(c.root, f.chord.root, "{f:?}");
        assert_ne!(
            Some(f.chord.quality),
            inside.detailed.map(|d| d.quality),
            "{f:?}"
        );
        assert!(
            a.passing
                .iter()
                .all(|p| p.end_s <= f.start_s || f.end_s <= p.start_s),
            "figure over a passing chord: {f:?}"
        );
    }
}

fn figure_dump(a: &ChordAnalysis) -> String {
    a.figures
        .iter()
        .map(|p| format!("{:.3}-{:.3} {}", p.start_s, p.end_s, name(Some(p.chord))))
        .collect::<Vec<_>>()
        .join(", ")
}

const NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

fn name(c: Option<Chord>) -> String {
    match c {
        None => "N.C.".into(),
        Some(c) => {
            let mut s = format!("{}{}", NAMES[usize::from(c.root)], c.quality.suffix());
            if let Some(b) = c.bass {
                s += &format!("/{}", NAMES[usize::from(b)]);
            }
            s
        }
    }
}

fn chord_at(a: &ChordAnalysis, t: f64) -> Option<Chord> {
    a.segments
        .iter()
        .find(|s| s.start_s <= t && t < s.end_s)
        .and_then(|s| s.chord)
}

fn dump(a: &ChordAnalysis) -> String {
    a.segments
        .iter()
        .map(|s| {
            format!(
                "{:.3} {}{}",
                s.start_s,
                name(s.chord),
                if s.push { " (push)" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// A part's length in beats, guitar root (MIDI) and quality.
type Spec = (usize, i32, Quality);

/// Parts back to back from their specs, starting at `beats[first]`.
fn parts_on_beats(beats: &[f64], first: usize, spec: &[Spec]) -> Vec<Part> {
    let mut i = first;
    spec.iter()
        .map(|&(n, root, quality)| {
            let p = Part {
                start: beats[i],
                end: beats[i + n],
                root,
                quality,
            };
            i += n;
            p
        })
        .collect()
}

/// Every part reads as its chord in its middle, and a segment boundary
/// lies within `tol` seconds of every change.
fn assert_parts(a: &ChordAnalysis, parts: &[Part], tol: f64) {
    for p in parts {
        let mid = 0.5 * (p.start + p.end);
        let got = chord_at(a, mid);
        let want = Some(Chord {
            root: (p.root % 12) as u8,
            quality: p.quality,
            bass: None,
        });
        assert_eq!(name(got), name(want), "at {mid:.2}: {}", dump(a));
    }
    for w in parts.windows(2) {
        let t = w[1].start;
        assert!(
            a.segments.iter().any(|s| (s.start_s - t).abs() <= tol),
            "no boundary near {t:.3}: {}",
            dump(a)
        );
    }
}

use Quality::*;

/// E, Am, D5, G7, C, Bm7, Asus4, F#m, each for a bar.
fn progression_spec() -> Vec<Spec> {
    vec![
        (4, 40, Major),
        (4, 45, Minor),
        (4, 50, Power),
        (4, 43, Dom7),
        (4, 48, Major),
        (4, 47, Min7),
        (4, 45, Sus4),
        (4, 42, Minor),
    ]
}

#[test]
fn progression_on_beats() {
    let beats = even_beats(20.0, 0.5);
    let mut song = Song::new(20.0, beats.clone(), 0.0, RATE);
    let parts = parts_on_beats(&beats, 2, &progression_spec());
    song.play(&parts, BassLine::Root);
    let a = song.detect();
    assert_parts(&a, &parts, 0.01);
    assert!(a.passing.is_empty(), "{}", passing_dump(&a));
    // Changes on the beat are not pushes.
    for s in &a.segments {
        if s.chord.is_some() && s.start_s > beats[2] - 0.01 {
            assert!(!s.push, "{}", dump(&a));
        }
    }
}

#[test]
fn power_chords_stay_power() {
    let beats = even_beats(16.0, 0.5);
    let mut song = Song::new(16.0, beats.clone(), 0.0, RATE);
    let spec = [
        (4, 40, Power),
        (4, 45, Power),
        (4, 43, Power),
        (4, 50, Power),
        (4, 40, Major),
        (4, 40, Power),
    ];
    let parts = parts_on_beats(&beats, 2, &spec);
    song.play(&parts, BassLine::Root);
    assert_parts(&song.detect(), &parts, 0.01);
}

#[test]
fn eighth_pushes_are_kept() {
    let beats = even_beats(16.0, 0.5);
    let mut song = Song::new(16.0, beats.clone(), 0.0, RATE);
    let mut parts = parts_on_beats(
        &beats,
        2,
        &[
            (4, 40, Power),
            (4, 45, Power),
            (4, 43, Major),
            (4, 50, Power),
            (4, 40, Power),
        ],
    );
    // Every change lands an eighth before the bar.
    for i in 1..parts.len() {
        parts[i].start -= 0.25;
        parts[i - 1].end = parts[i].start;
    }
    song.play(&parts, BassLine::Root);
    let a = song.detect();
    assert_parts(&a, &parts, 0.01);
    for p in &parts[1..] {
        let s = a
            .segments
            .iter()
            .find(|s| (s.start_s - p.start).abs() <= 0.01)
            .expect("segment at the push");
        assert!(s.push, "{}", dump(&a));
    }
}

#[test]
fn noisy_cell_does_not_change_the_chord() {
    let beats = even_beats(12.0, 0.5);
    let mut song = Song::new(12.0, beats.clone(), 0.0, RATE);
    let parts = parts_on_beats(&beats, 2, &[(8, 40, Major), (8, 45, Minor)]);
    song.play(&parts, BassLine::Root);
    // A sixteenth of a loud Bb chord over the E, off the beat.
    let t = beats[5] + 0.125;
    song.strum(t, 0.12, 46, Major, 0.25);
    let a = song.detect();
    assert_parts(&a, &parts, 0.01);
    assert_eq!(
        a.segments.iter().filter(|s| s.chord.is_some()).count(),
        2,
        "{}",
        dump(&a)
    );
    assert!(a.passing.is_empty(), "{}", passing_dump(&a));
}

#[test]
fn syncopated_bass_does_not_move_changes() {
    let beats = even_beats(16.0, 0.5);
    let mut song = Song::new(16.0, beats.clone(), 0.0, RATE);
    let parts = parts_on_beats(
        &beats,
        2,
        &[
            (4, 40, Major),
            (4, 45, Major),
            (4, 47, Major),
            (4, 40, Major),
            (4, 45, Major),
        ],
    );
    song.play(&parts, BassLine::None);
    // Sparse bass: the next chord's root a sixteenth early, and a hit on
    // the "and" of beat two.
    for (i, p) in parts.iter().enumerate() {
        let early = if i == 0 { p.start } else { p.start - 0.125 };
        song.bass_note(early, 0.6, p.root - 12);
        song.bass_note(p.start + 0.75, 0.3, p.root - 12);
    }
    let a = song.detect();
    assert_parts(&a, &parts, 0.01);
}

#[test]
fn bass_pedal_gives_slash_chords() {
    let beats = even_beats(16.0, 0.5);
    let mut song = Song::new(16.0, beats.clone(), 0.0, RATE);
    // D and A over an E pedal, then E over E.
    let parts = parts_on_beats(
        &beats,
        2,
        &[
            (4, 50, Major),
            (4, 45, Major),
            (4, 40, Major),
            (4, 50, Major),
        ],
    );
    song.play(&parts, BassLine::Pedal(28));
    let a = song.detect();
    let got: Vec<String> = parts
        .iter()
        .map(|p| name(chord_at(&a, 0.5 * (p.start + p.end))))
        .collect();
    assert_eq!(got, ["D/E", "A/E", "E", "D/E"], "{}", dump(&a));
}

#[test]
fn root_bass_gives_no_slash() {
    let beats = even_beats(14.0, 0.5);
    let mut song = Song::new(14.0, beats.clone(), 0.0, RATE);
    let parts = parts_on_beats(&beats, 2, &progression_spec()[..6]);
    song.play(&parts, BassLine::Root);
    let a = song.detect();
    assert!(
        a.segments
            .iter()
            .all(|s| s.chord.is_none_or(|c| c.bass.is_none())),
        "{}",
        dump(&a)
    );
}

#[test]
fn offsets_are_removed() {
    for offset in [-30.0, 40.0] {
        let beats = even_beats(20.0, 0.5);
        let mut song = Song::new(20.0, beats.clone(), offset, RATE);
        let parts = parts_on_beats(&beats, 2, &progression_spec());
        song.play(&parts, BassLine::Root);
        assert_parts(&song.detect(), &parts, 0.01);
    }
}

#[test]
fn silence_and_noise_have_no_chord() {
    let beats = even_beats(10.0, 0.5);
    let song = Song::new(10.0, beats.clone(), 0.0, RATE);
    let a = song.detect();
    assert_eq!(dump(&a), "0.000 N.C.");
    assert_eq!(a.key, None);

    // Loud noise bursts on every beat (drum bleed) in the other stem.
    let mut song = Song::new(10.0, beats.clone(), 0.0, RATE);
    let mut rng = Rng(7);
    let rate = f64::from(RATE);
    for &b in &beats {
        let start = (b * rate) as usize;
        for j in 0..(0.2 * rate) as usize {
            if let Some(y) = song.other.get_mut(start + j) {
                *y = (0.3 * (2.0 * rng.next() - 1.0) * (-(j as f64) / (0.05 * rate)).exp()) as f32;
            }
        }
    }
    let a = song.detect();
    assert!(a.segments.iter().all(|s| s.chord.is_none()), "{}", dump(&a));

    // A quiet stretch between chords.
    let mut song = Song::new(12.0, beats_of(12.0), 0.0, RATE);
    let b = song.beats.clone();
    let parts = parts_on_beats(&b, 2, &[(6, 40, Major)]);
    song.play(&parts, BassLine::Root);
    let parts2 = parts_on_beats(&b, 14, &[(6, 45, Minor)]);
    song.play(&parts2, BassLine::Root);
    let a = song.detect();
    // The E rings into the gap but has decayed by its end.
    assert_eq!(chord_at(&a, b[13] + 0.2), None, "{}", dump(&a));
}

fn beats_of(seconds: f64) -> Vec<f64> {
    even_beats(seconds, 0.5)
}

#[test]
fn key_of_simple_progressions() {
    let cases: [(&[Spec], Key); 2] = [
        (
            &[
                (4, 48, Major),
                (4, 43, Major),
                (4, 45, Minor),
                (4, 41, Major),
                (4, 48, Major),
            ],
            Key {
                tonic: 0,
                minor: false,
            },
        ),
        (
            &[
                (4, 45, Minor),
                (4, 50, Minor),
                (4, 40, Major),
                (4, 45, Minor),
                (4, 41, Major),
                (4, 45, Minor),
            ],
            Key {
                tonic: 9,
                minor: true,
            },
        ),
    ];
    for (spec, key) in cases {
        let beats = beats_of(16.0);
        let mut song = Song::new(16.0, beats.clone(), 0.0, RATE);
        let parts = parts_on_beats(&beats, 2, spec);
        song.play(&parts, BassLine::Root);
        let a = song.detect();
        assert_eq!(a.key, Some(key), "{}", dump(&a));
    }
}

#[test]
fn tempo_drift() {
    // Beats from 0.42 s apart slowing to 0.6 s.
    let mut beats = vec![0.2];
    while *beats.last().expect("beats") < 22.0 {
        let t = *beats.last().expect("beats");
        beats.push(t + 0.42 + 0.18 * t / 22.0);
    }
    let seconds = 22.5;
    let mut song = Song::new(seconds, beats.clone(), 0.0, RATE);
    let mut parts = parts_on_beats(&beats, 2, &progression_spec());
    // One push, an eighth (half this beat) early.
    let k = 3;
    let eighth = 0.5 * (parts[k].start - beats[2 + 4 * k - 1]);
    parts[k].start -= eighth;
    parts[k - 1].end = parts[k].start;
    song.play(&parts, BassLine::Root);
    let a = song.detect();
    assert_parts(&a, &parts, 0.01);
    let s = a
        .segments
        .iter()
        .find(|s| (s.start_s - parts[k].start).abs() <= 0.01)
        .expect("push");
    assert!(s.push);
}

#[test]
fn rate_48k() {
    let beats = even_beats(20.0, 0.5);
    let mut song = Song::new(20.0, beats.clone(), -12.0, 48_000);
    let parts = parts_on_beats(&beats, 2, &progression_spec());
    song.play(&parts, BassLine::Root);
    assert_parts(&song.detect(), &parts, 0.01);
}

#[test]
fn no_beat_grid() {
    // Chords change every 2 s; without beats the grid is 0.5 s beats from
    // 0, so the changes still land on cells, and nothing is a push.
    let mut song = Song::new(16.0, Vec::new(), 0.0, RATE);
    let parts: Vec<Part> = progression_spec()
        .iter()
        .enumerate()
        .map(|(i, &(_, root, quality))| Part {
            start: 2.0 * i as f64,
            end: 2.0 * (i + 1) as f64,
            root,
            quality,
        })
        .collect();
    let grid = beats_of(16.0);
    song.beats = grid;
    song.play(&parts, BassLine::Root);
    let a = song.detect_with(&[]);
    assert_parts(&a, &parts, 0.13);
    assert!(a.segments.iter().all(|s| !s.push), "{}", dump(&a));
    // One beat is no grid either.
    let b = song.detect_with(&[3.0]);
    assert!(b.segments.iter().all(|s| !s.push));
}

#[test]
fn degenerate_inputs() {
    let x = vec![0.0f32; 1000];
    let stems = [StemAudio {
        kind: StemKind::Guitar,
        samples: &x,
    }];
    assert!(detect_chords(&stems, RATE, &[], 0.0, 0.0)
        .segments
        .is_empty());
    assert!(detect_chords(&stems, RATE, &[], 0.0, f64::NAN)
        .segments
        .is_empty());
    let a = detect_chords(&stems, 0, &[0.5, 1.0], 0.0, 2.0);
    check_contiguous(&a, 2.0);
    let a = detect_chords(&[], RATE, &[0.5, 1.0, f64::NAN, 0.7, 5.0], f64::NAN, 3.0);
    check_contiguous(&a, 3.0);
}

#[test]
fn keys_alone_and_vocals_fallback() {
    let beats = even_beats(12.0, 0.5);
    let mut song = Song::new(12.0, beats.clone(), 0.0, RATE);
    let parts = parts_on_beats(&beats, 2, &[(8, 48, Major), (8, 45, Minor)]);
    song.play(&parts, BassLine::None);
    // Move the chords to keys, and also to vocals alone.
    song.keys = std::mem::take(&mut song.guitar);
    song.guitar = vec![0.0; song.keys.len()];
    assert_parts(&song.detect(), &parts, 0.01);
    let vocals = [StemAudio {
        kind: StemKind::Vocals,
        samples: &song.keys,
    }];
    let a = detect_chords(&vocals, RATE, &beats, 0.0, 12.0);
    assert_parts(&a, &parts, 0.01);
}

/// Five minutes of four stems. Prints the time; the bound only catches
/// something pathological, so the test doesn't depend on the machine.
#[test]
fn five_minute_song_runtime() {
    let seconds = 300.0;
    let beats = even_beats(seconds, 60.0 / 132.0);
    let mut song = Song::new(seconds, beats.clone(), -14.0, RATE);
    // Triads, a power chord and a 7th: the m7 and maj7 of the keys
    // voicing sit on overtones of their thirds and can read as triads.
    let spec = [
        (4, 40, Major),
        (4, 45, Minor),
        (4, 50, Power),
        (4, 43, Dom7),
        (4, 48, Major),
        (4, 47, Minor),
        (4, 45, Sus4),
        (4, 42, Minor),
    ];
    let n = (beats.len() - 4) / 4;
    let full: Vec<Spec> = (0..n).map(|i| spec[i % spec.len()]).collect();
    let parts = parts_on_beats(&beats, 0, &full);
    song.play(&parts, BassLine::Root);
    // Keys double the chords, other plays the root high.
    let rate = f64::from(RATE);
    let mut rng = Rng(3);
    for p in &parts {
        for m in voicing(p.root + 12, p.quality) {
            add_note(
                &mut song.keys,
                rate,
                p.start,
                p.end - p.start,
                hz(m, -14.0),
                0.05,
                &KEYS,
                &mut rng,
            );
        }
        add_note(
            &mut song.other,
            rate,
            p.start,
            p.end - p.start,
            hz(p.root + 24, -14.0),
            0.1,
            &KEYS,
            &mut rng,
        );
    }
    let start = Instant::now();
    let a = song.detect();
    let elapsed = start.elapsed();
    println!(
        "5 min, 4 stems: {:.2} s, {} segments, {} passing",
        elapsed.as_secs_f64(),
        a.segments.len(),
        a.passing.len()
    );
    assert_parts(&a, &parts[..40], 0.01);
    assert!(elapsed.as_secs() < 60);
}

fn segment_at(a: &ChordAnalysis, t: f64) -> &ChordSegment {
    a.segments
        .iter()
        .find(|s| s.start_s <= t && t < s.end_s)
        .expect("segment")
}

fn passing_dump(a: &ChordAnalysis) -> String {
    a.passing
        .iter()
        .map(|p| format!("{:.3}-{:.3} {}", p.start_s, p.end_s, name(Some(p.chord))))
        .collect::<Vec<_>>()
        .join(", ")
}

#[test]
fn detailed_names() {
    let beats = even_beats(24.0, 0.5);
    let mut song = Song::new(24.0, beats.clone(), 0.0, RATE);
    // Neighbors differ in root so each stretch is its own segment. The
    // add9 decodes as sus2 (its third is weaker than the ninth here). A
    // 7sus4 is left out: its fifth is the root's 3rd harmonic, the overtone
    // filter weakens it, and the rest reads as the sus2 a tone lower.
    let spec = [
        (4, 45, Major),
        (4, 50, Major),
        (4, 45, Six),
        (4, 40, Major),
        (4, 45, Add9),
        (4, 50, Six),
        (4, 40, Dom9),
        (4, 50, Major),
    ];
    let parts = parts_on_beats(&beats, 2, &spec);
    song.play(&parts, BassLine::Root);
    let a = song.detect();
    let got: Vec<String> = parts
        .iter()
        .map(|p| {
            let s = segment_at(&a, 0.5 * (p.start + p.end));
            format!("{} {}", name(s.chord), name(s.detailed))
        })
        .collect();
    assert_eq!(
        got,
        [
            "A A",
            "D D",
            "A A6",
            "E E",
            "Asus2 Aadd9",
            "D D6",
            "E7 E9",
            "D D"
        ],
        "{}",
        dump(&a)
    );
    assert!(a.passing.is_empty(), "{}", passing_dump(&a));
}

/// D for four bars with a Bm strummed over the D bass pedal: an eighth on
/// the "and" of a beat in bar 2, and a beat in bar 4.
#[test]
fn passing_chord_inside_a_segment() {
    let beats = even_beats(12.0, 0.5);
    let mut song = Song::new(12.0, beats.clone(), 0.0, RATE);
    let d = |start, end| Part {
        start,
        end,
        root: 50,
        quality: Major,
    };
    let bm = |start, end| Part {
        start,
        end,
        root: 47,
        quality: Minor,
    };
    let eighth = (beats[7] + 0.25, beats[8]);
    let beat = (beats[14], beats[15]);
    let parts = [
        d(beats[2], eighth.0),
        bm(eighth.0, eighth.1),
        d(beats[8], beat.0),
        bm(beat.0, beat.1),
        d(beats[15], beats[18]),
    ];
    song.play(&parts, BassLine::Pedal(38));
    let a = song.detect();
    let chords: Vec<_> = a.segments.iter().filter(|s| s.chord.is_some()).collect();
    assert_eq!(chords.len(), 1, "{}", dump(&a));
    assert_eq!(name(chords[0].chord), "D", "{}", dump(&a));
    assert_eq!(a.passing.len(), 2, "{}", passing_dump(&a));
    for (p, (start, end)) in a.passing.iter().zip([eighth, beat]) {
        assert_eq!(name(Some(p.chord)), "Bm", "{}", passing_dump(&a));
        assert!((p.start_s - start).abs() <= 0.01, "{}", passing_dump(&a));
        assert!((p.end_s - end).abs() <= 0.01, "{}", passing_dump(&a));
    }
    // The Bm cells don't make the D a D6.
    assert_eq!(name(chords[0].detailed), "D", "{}", dump(&a));
}

/// A steady strummed E with drum bleed and hiss in the other stem.
#[test]
fn steady_chord_with_noise_has_no_passing_chords() {
    let seconds = 20.0;
    let beats = even_beats(seconds, 0.5);
    let mut song = Song::new(seconds, beats.clone(), 0.0, RATE);
    let parts = parts_on_beats(&beats, 1, &[(36, 40, Major)]);
    song.play(&parts, BassLine::Root);
    // Off-beat strums too, softer.
    for &b in &beats[1..37] {
        song.strum(b + 0.25, 0.25, 40, Major, 0.08);
    }
    let mut rng = Rng(11);
    let rate = f64::from(RATE);
    for (i, y) in song.other.iter_mut().enumerate() {
        *y = (0.01 * (2.0 * rng.next() - 1.0)) as f32;
        let t = i as f64 / rate;
        let k = ((t - 0.2) / 0.25).floor();
        let since = t - 0.2 - 0.25 * k;
        if k >= 0.0 && since < 0.1 {
            *y += (0.2 * (2.0 * rng.next() - 1.0) * (-since / 0.03).exp()) as f32;
        }
    }
    let a = song.detect();
    assert!(a.passing.is_empty(), "{}", passing_dump(&a));
    assert!(a.figures.is_empty(), "{}", figure_dump(&a));
    assert_eq!(name(segment_at(&a, 10.0).detailed), "E", "{}", dump(&a));
}

/// The next chord arriving a sixteenth early is not a passing chord.
#[test]
fn early_arrival_is_not_a_passing_chord() {
    let beats = even_beats(14.0, 0.5);
    let mut song = Song::new(14.0, beats.clone(), 0.0, RATE);
    let mut parts = parts_on_beats(
        &beats,
        2,
        &[
            (4, 40, Major),
            (4, 45, Major),
            (4, 50, Major),
            (4, 45, Minor),
            (4, 40, Major),
        ],
    );
    for i in 1..parts.len() {
        parts[i].start -= 0.125;
        parts[i - 1].end = parts[i].start;
    }
    song.play(&parts, BassLine::Root);
    let a = song.detect();
    assert!(a.passing.is_empty(), "{}", passing_dump(&a));
}

/// D for four bars over a D bass pedal, with one beat played as a Bm
/// arpeggio instead: B, D, F#, B, one note per sixteenth, each stopped by
/// the next, the D chord's strings stopped too. No single cell looks like
/// Bm, but the beat as a whole does, and the D's A is gone.
#[test]
fn arpeggiated_passing_chord() {
    let beats = even_beats(12.0, 0.5);
    let mut song = Song::new(12.0, beats.clone(), 0.0, RATE);
    let d = |start, end| Part {
        start,
        end,
        root: 50,
        quality: Major,
    };
    let (start, end) = (beats[10], beats[11]);
    song.play(
        &[d(beats[2], start), d(end, beats[18])],
        BassLine::Pedal(38),
    );
    song.bass_note(start, end - start, 38);
    let step = (end - start) / 4.0;
    let rate = f64::from(RATE);
    for (i, m) in [59, 62, 66, 71].into_iter().enumerate() {
        let t = start + step * i as f64;
        add_note(
            &mut song.guitar,
            rate,
            t,
            step,
            hz(m, 0.0),
            0.5,
            &GUITAR,
            &mut song.rng,
        );
    }
    let a = song.detect();
    let chords: Vec<_> = a.segments.iter().filter(|s| s.chord.is_some()).collect();
    assert_eq!(chords.len(), 1, "{}", dump(&a));
    assert_eq!(name(chords[0].chord), "D", "{}", dump(&a));
    assert_eq!(a.passing.len(), 1, "{}", passing_dump(&a));
    let p = &a.passing[0];
    assert_eq!(name(Some(p.chord)), "Bm", "{}", passing_dump(&a));
    assert!((p.start_s - start).abs() <= 0.01, "{}", passing_dump(&a));
    assert!(p.end_s >= start + 2.0 * step - 0.01, "{}", passing_dump(&a));
    assert!(p.end_s <= end + 0.01, "{}", passing_dump(&a));
}

/// An A chord struck once a bar and left ringing, with a riff of single
/// notes moving C, C# and back on the third beat, the bass on A: a line
/// over a held chord, not C and C# chords.
#[test]
fn riff_over_ringing_chord_is_not_passing() {
    let seconds = 18.0;
    let beats = even_beats(seconds, 0.5);
    let mut song = Song::new(seconds, beats.clone(), 0.0, RATE);
    let rate = f64::from(RATE);
    for bar in 0..8 {
        let t = beats[1 + 4 * bar];
        let bar_end = beats[5 + 4 * bar];
        song.strum(t, bar_end - t, 45, Major, 0.15);
        for b in 0..4 {
            song.bass_note(beats[1 + 4 * bar + b], 0.5, 33);
        }
        let riff = beats[3 + 4 * bar];
        for (i, m) in [48, 49].into_iter().enumerate() {
            add_note(
                &mut song.guitar,
                rate,
                riff + 0.25 * i as f64,
                0.25,
                hz(m, 0.0),
                0.5,
                &GUITAR,
                &mut song.rng,
            );
        }
    }
    let a = song.detect();
    assert_eq!(name(chord_at(&a, 8.0)), "A", "{}", dump(&a));
    assert!(a.passing.is_empty(), "{}", passing_dump(&a));
}

/// Each part decoded as itself with a plain detailed name, from guitar
/// with strong 3rd harmonics through a hard overdrive (a tanh clipper):
/// the 3rd harmonics would otherwise read as sevenths and ninths. The
/// clipper puts the stem at about 28 peaks per frame, the full leak share;
/// harder clipping (gain 70 and up) leaks more than that share covers and
/// the Am reads Am7 again.
#[test]
fn distorted_triads_stay_plain() {
    let beats = even_beats(26.0, 0.5);
    let mut song = Song::new(26.0, beats.clone(), 0.0, RATE);
    song.voice = &DRIVEN;
    // Neighbors differ in root.
    let spec = [
        (4, 45, Major),
        (4, 40, Power),
        (4, 45, Minor),
        (4, 50, Major),
        (4, 40, Major),
        (4, 45, Major),
        (4, 50, Minor),
        (4, 40, Minor),
        (4, 45, Major),
    ];
    let parts = parts_on_beats(&beats, 2, &spec);
    song.play(&parts, BassLine::Root);
    for y in &mut song.guitar {
        *y = 0.3 * (50.0 * *y).tanh();
    }
    let a = song.detect();
    assert_parts(&a, &parts, 0.13);
    for p in &parts {
        let s = segment_at(&a, 0.5 * (p.start + p.end));
        assert_eq!(name(s.detailed), name(s.chord), "{}", dump(&a));
    }
    assert!(a.passing.is_empty(), "{}", passing_dump(&a));
    assert!(
        a.figures
            .iter()
            .all(|f| matches!(f.chord.quality, Six | Min6)),
        "{}",
        figure_dump(&a)
    );
}

/// Sevenths and an add9 strummed on clean guitar keep their notes: the
/// overdrive handling of 3rd harmonics does not take their sevenths and
/// ninths. The maj7 is the open shape (A E G# C# E): in the stacked test
/// voicing its G# sits an octave and a fifth above the C# and next to the
/// A's octave, and reads as the triad with or without the handling.
#[test]
fn clean_sevenths_are_kept() {
    let beats = even_beats(20.0, 0.5);
    let mut song = Song::new(20.0, beats.clone(), 0.0, RATE);
    let spec = [
        (4, 45, Maj7),
        (4, 50, Major),
        (4, 45, Min7),
        (4, 40, Major),
        (4, 45, Dom7),
        (4, 50, Major),
        (4, 45, Add9),
        (4, 40, Major),
    ];
    let parts = parts_on_beats(&beats, 2, &spec);
    song.play(&parts[1..], BassLine::Root);
    let rate = f64::from(RATE);
    for &t in &beats[2..6] {
        for (k, m) in [45, 52, 56, 61, 64].into_iter().enumerate() {
            let d = 0.006 * k as f64;
            add_note(
                &mut song.guitar,
                rate,
                t + d,
                0.5 - d,
                hz(m, 0.0),
                0.15,
                &GUITAR,
                &mut song.rng,
            );
        }
        song.bass_note(t, 0.5, 33);
    }
    let a = song.detect();
    let got: Vec<String> = parts
        .iter()
        .map(|p| name(segment_at(&a, 0.5 * (p.start + p.end)).detailed))
        .collect();
    assert_eq!(
        got,
        ["Amaj7", "D", "Am7", "E", "A7", "D", "Aadd9", "E"],
        "{}",
        dump(&a)
    );
}

/// Eight bars of A over an A bass, the guitar alternating A and A6 every
/// two beats: one A segment with A6 figures on the A6 beats.
#[test]
fn sixth_figures_inside_a_segment() {
    let beats = even_beats(18.0, 0.5);
    let mut song = Song::new(18.0, beats.clone(), 0.0, RATE);
    let spec: Vec<Spec> = (0..16)
        .map(|i| (2, 45, if i % 2 == 1 { Six } else { Major }))
        .collect();
    let parts = parts_on_beats(&beats, 1, &spec);
    song.play(&parts, BassLine::Root);
    let a = song.detect();
    let chords: Vec<_> = a.segments.iter().filter(|s| s.chord.is_some()).collect();
    assert_eq!(chords.len(), 1, "{}", dump(&a));
    assert_eq!(name(chords[0].chord), "A", "{}", dump(&a));
    let sixths: Vec<&Part> = parts.iter().filter(|p| p.quality == Six).collect();
    // Most A6 stretches carry a figure, and every figure is an A6 on one.
    let hit = sixths
        .iter()
        .filter(|p| {
            a.figures
                .iter()
                .any(|f| f.start_s < p.end - 0.1 && f.end_s > p.start + 0.1)
        })
        .count();
    assert!(hit * 4 >= sixths.len() * 3, "{}", figure_dump(&a));
    for f in &a.figures {
        assert_eq!(name(Some(f.chord)), "A6", "{}", figure_dump(&a));
        let mid = 0.5 * (f.start_s + f.end_s);
        assert!(
            sixths
                .iter()
                .any(|p| p.start - 0.1 <= mid && mid <= p.end + 0.1),
            "{}",
            figure_dump(&a)
        );
    }
}

/// A for three bars with one beat of a clean open Amaj7 (A E G# C# E)
/// on the guitar: an Amaj7 figure on that beat.
#[test]
fn clean_maj7_figure() {
    let beats = even_beats(10.0, 0.5);
    let mut song = Song::new(10.0, beats.clone(), 0.0, RATE);
    let fig = 8;
    let a_parts = [
        Part {
            start: beats[1],
            end: beats[fig],
            root: 45,
            quality: Major,
        },
        Part {
            start: beats[fig + 1],
            end: beats[14],
            root: 45,
            quality: Major,
        },
    ];
    song.play(&a_parts, BassLine::Root);
    let rate = f64::from(RATE);
    for (k, m) in [45, 52, 56, 61, 64].into_iter().enumerate() {
        let d = 0.006 * k as f64;
        add_note(
            &mut song.guitar,
            rate,
            beats[fig] + d,
            0.5 - d,
            hz(m, 0.0),
            0.15,
            &GUITAR,
            &mut song.rng,
        );
    }
    song.bass_note(beats[fig], 0.5, 33);
    let a = song.detect();
    let chords: Vec<_> = a.segments.iter().filter(|s| s.chord.is_some()).collect();
    assert_eq!(chords.len(), 1, "{}", dump(&a));
    assert_eq!(a.figures.len(), 1, "{}", figure_dump(&a));
    let f = &a.figures[0];
    assert_eq!(name(Some(f.chord)), "Amaj7", "{}", figure_dump(&a));
    assert!(
        (f.start_s - beats[fig]).abs() <= 0.01,
        "{}",
        figure_dump(&a)
    );
}

/// A D struck once a bar and left ringing over a D pedal, with a G
/// strummed for a beat in bar 2: the A keeps ringing, but G and B arrive
/// together, so the G is a passing chord.
#[test]
fn new_notes_make_a_passing_chord() {
    let beats = even_beats(12.0, 0.5);
    let mut song = Song::new(12.0, beats.clone(), 0.0, RATE);
    for bar in 0..4 {
        let t = beats[1 + 4 * bar];
        song.strum(t, 2.0, 50, Major, 0.15);
        for b in 0..4 {
            song.bass_note(beats[1 + 4 * bar + b], 0.5, 38);
        }
    }
    let g = beats[7];
    song.strum(g, 0.5, 43, Major, 0.25);
    let a = song.detect();
    let chords: Vec<_> = a.segments.iter().filter(|s| s.chord.is_some()).collect();
    assert_eq!(chords.len(), 1, "{}", dump(&a));
    assert_eq!(name(chords[0].chord), "D", "{}", dump(&a));
    assert_eq!(a.passing.len(), 1, "{}", passing_dump(&a));
    let p = &a.passing[0];
    assert_eq!(name(Some(p.chord)), "G", "{}", passing_dump(&a));
    assert!((p.start_s - g).abs() <= 0.01, "{}", passing_dump(&a));
}

/// A strummed on every beat for three bars with one beat strummed `sus`
/// instead (the third left out), or with `melody` (MIDI) played over that
/// beat while the A goes on.
fn sus_song(sus: Option<Quality>, melody: Option<i32>) -> ChordAnalysis {
    let beats = even_beats(8.0, 0.5);
    let mut song = Song::new(8.0, beats.clone(), 0.0, RATE);
    let fig = 7;
    let mut parts = vec![Part {
        start: beats[1],
        end: beats[13],
        root: 45,
        quality: Major,
    }];
    if let Some(q) = sus {
        parts = vec![
            Part {
                start: beats[1],
                end: beats[fig],
                root: 45,
                quality: Major,
            },
            Part {
                start: beats[fig],
                end: beats[fig + 1],
                root: 45,
                quality: q,
            },
            Part {
                start: beats[fig + 1],
                end: beats[13],
                root: 45,
                quality: Major,
            },
        ];
    }
    song.play(&parts, BassLine::Root);
    if let Some(m) = melody {
        add_note(
            &mut song.guitar,
            f64::from(RATE),
            beats[fig],
            0.5,
            hz(m, 0.0),
            0.5,
            &GUITAR,
            &mut song.rng,
        );
    }
    song.detect()
}

/// One beat of Asus4 or Asus2 inside an A: a figure on that beat, and the
/// A stays one segment.
#[test]
fn sus_figures_inside_a_segment() {
    let beats = even_beats(8.0, 0.5);
    for (q, want) in [(Sus4, "Asus4"), (Sus2, "Asus2")] {
        let a = sus_song(Some(q), None);
        let chords: Vec<_> = a.segments.iter().filter(|s| s.chord.is_some()).collect();
        assert_eq!(chords.len(), 1, "{want}: {}", dump(&a));
        assert_eq!(name(chords[0].chord), "A", "{want}: {}", dump(&a));
        let f: Vec<_> = a
            .figures
            .iter()
            .filter(|f| name(Some(f.chord)) == want)
            .collect();
        assert_eq!(f.len(), 1, "{want}: {}", figure_dump(&a));
        assert!(
            (f[0].start_s - beats[7]).abs() <= 0.01,
            "{}",
            figure_dump(&a)
        );
    }
}

/// A melody D over an A whose C# goes on is no Asus4.
#[test]
fn melody_fourth_is_not_a_sus_figure() {
    let a = sus_song(None, Some(62));
    assert!(
        a.figures.iter().all(|f| f.chord.quality != Sus4),
        "{}",
        figure_dump(&a)
    );
}
