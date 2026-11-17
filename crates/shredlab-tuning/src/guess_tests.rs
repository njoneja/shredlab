// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Tuning guess tests on synthesized bass and guitar lines.

use crate::{guess_song_tuning, StemAudio, StemKind, TuningGuess};

const RATE: u32 = 44_100;

/// A line of plucked notes, 0.8 s each, cycling through `pattern` (MIDI
/// note, count) until `seconds` are filled, `offset` cents off A440.
fn line(pattern: &[(i32, usize)], seconds: f64, offset: f64) -> Vec<f32> {
    let rate = f64::from(RATE);
    let note_len = (0.8 * rate) as usize;
    let notes: Vec<i32> = pattern
        .iter()
        .flat_map(|&(m, n)| std::iter::repeat_n(m, n))
        .collect();
    let total = (seconds * rate) as usize;
    let mut out = Vec::with_capacity(total);
    let mut k = 0;
    while out.len() < total {
        let m = notes[k % notes.len()];
        k += 1;
        let f = 440.0 * 2f64.powf((f64::from(m - 69) * 100.0 + offset) / 1200.0);
        for j in 0..note_len {
            let t = j as f64 / rate;
            let env = (t / 0.005).min(1.0) * (-t / 1.0).exp();
            let y: f64 = [1.0, 0.5, 0.3, 0.2]
                .iter()
                .enumerate()
                .map(|(h, a)| a * (std::f64::consts::TAU * f * (h + 1) as f64 * t).sin())
                .sum();
            out.push((0.2 * env * y) as f32);
        }
    }
    out
}

fn guess(stems: &[(StemKind, &[f32])], offset: f32) -> Option<TuningGuess> {
    let input: Vec<StemAudio> = stems
        .iter()
        .map(|&(kind, samples)| StemAudio { kind, samples })
        .collect();
    guess_song_tuning(&input, RATE, offset)
}

fn bass(g: Option<TuningGuess>) -> (i32, bool) {
    let g = g.expect("a guess");
    assert_eq!(g.below_standard, g.lowest_midi - 28);
    (g.below_standard, g.likely)
}

#[test]
fn eb_standard_bass_is_likely_eb() {
    // Eb1 Ab1 Bb1 Eb2, 20 cents sharp of A440.
    let b = line(&[(27, 4), (32, 2), (34, 2), (39, 2)], 40.0, 20.0);
    assert_eq!(bass(guess(&[(StemKind::Bass, &b)], 20.0)), (-1, true));
}

#[test]
fn standard_bass_is_likely_standard() {
    let b = line(&[(28, 4), (33, 2), (35, 2), (40, 2)], 40.0, -15.0);
    assert_eq!(bass(guess(&[(StemKind::Bass, &b)], -15.0)), (0, true));
    // A few D1s under a lot of E1 (a passing note on a five-string) don't
    // make it drop D.
    let b = line(&[(28, 8), (26, 1), (33, 4), (40, 3)], 40.0, 0.0);
    assert_eq!(bass(guess(&[(StemKind::Bass, &b)], 0.0)), (0, true));
}

#[test]
fn notes_below_eb_are_only_possible() {
    // Drop D (or D standard): D1 A1 D2.
    let b = line(&[(26, 4), (33, 3), (38, 3)], 40.0, 0.0);
    assert_eq!(bass(guess(&[(StemKind::Bass, &b)], 0.0)), (-2, false));
    // Drop C# (Eb standard with the low string down): C#1 Ab1 C#2, some E1.
    let b = line(&[(25, 3), (28, 1), (32, 3), (37, 3)], 40.0, 0.0);
    assert_eq!(bass(guess(&[(StemKind::Bass, &b)], 0.0)), (-3, false));
}

#[test]
fn no_low_notes_no_guess() {
    // The bass never goes under A1.
    let b = line(&[(33, 3), (36, 3), (38, 3), (40, 1)], 40.0, 0.0);
    assert_eq!(guess(&[(StemKind::Bass, &b)], 0.0), None);
    // Too little bass to judge.
    let b = line(&[(27, 4), (32, 2)], 12.0, 0.0);
    assert_eq!(guess(&[(StemKind::Bass, &b)], 0.0), None);
}

#[test]
fn a_low_note_much_rarer_than_its_octave_does_not_count() {
    // D1 at a sixth of D2: octave errors, not a tuning.
    let b = line(&[(26, 1), (38, 6), (33, 3), (31, 3)], 40.0, 0.0);
    assert_eq!(guess(&[(StemKind::Bass, &b)], 0.0), None);
}

#[test]
fn the_guitar_backs_a_weak_bass_guess() {
    // Eb1 only about 8 % of the bass time: weak alone.
    let b = line(&[(27, 2), (32, 10), (34, 10), (39, 4)], 60.0, 0.0);
    assert_eq!(bass(guess(&[(StemKind::Bass, &b)], 0.0)), (-1, false));
    // With a guitar playing its low Eb and no low E: strong.
    let g = line(&[(39, 3), (46, 3), (51, 3)], 40.0, 0.0);
    let both = guess(&[(StemKind::Bass, &b), (StemKind::Guitar, &g)], 0.0);
    assert_eq!(bass(both), (-1, true));
}

#[test]
fn guitar_alone_gives_no_guess() {
    // Its stem often holds bass bleed, so the guitar never guesses alone.
    let g = line(&[(39, 3), (46, 3), (51, 3)], 40.0, 10.0);
    assert_eq!(guess(&[(StemKind::Guitar, &g)], 10.0), None);
}
