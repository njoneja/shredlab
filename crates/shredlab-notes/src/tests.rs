// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Tests on synthesized stems: plucked and sustained tones with harmonics,
//! pitch contours, and chords.

use std::f64::consts::TAU;

use crate::{detect_notes, Attack, NoteAnalysis, NoteInput, StemKind};

const SR: u32 = 44_100;

fn hz(midi: f64) -> f64 {
    440.0 * 2f64.powf((midi - 69.0) / 12.0)
}

/// Deterministic noise in −1..1.
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    }
}

/// Adds a tone to `buf`: `pitch(t)` is the MIDI note `t` seconds after the
/// start, `harm` the harmonic amplitudes (the first is the fundamental),
/// `decay` the amplitude decay rate per second; `pick` adds a short noise
/// burst at the start.
#[allow(clippy::too_many_arguments)]
fn tone(
    buf: &mut [f32],
    start_s: f64,
    dur_s: f64,
    pitch: impl Fn(f64) -> f64,
    amp: f64,
    harm: &[f64],
    decay: f64,
    pick: bool,
) {
    let rate = f64::from(SR);
    let s0 = (start_s * rate) as usize;
    let len = (dur_s * rate) as usize;
    let mut phase = vec![0.0f64; harm.len()];
    let mut noise = Noise(s0 as u64 + 7);
    for i in 0..len {
        let k = s0 + i;
        if k >= buf.len() {
            break;
        }
        let t = i as f64 / rate;
        let f = hz(pitch(t));
        let env =
            amp * (-decay * t).exp() * (t / 0.003).min(1.0) * ((dur_s - t) / 0.008).clamp(0.0, 1.0);
        let mut y = 0.0;
        for (h, (a, ph)) in harm.iter().zip(phase.iter_mut()).enumerate() {
            let fh = f * (h + 1) as f64;
            if fh < 0.45 * rate {
                y += a * ph.sin();
            }
            *ph = (*ph + TAU * fh / rate) % TAU;
        }
        let mut v = env * y;
        if pick && t < 0.006 {
            v += amp * 0.4 * noise.next() * (1.0 - t / 0.006);
        }
        buf[k] += v as f32;
    }
}

const GUITAR: [f64; 8] = [1.0, 0.7, 0.5, 0.35, 0.25, 0.15, 0.1, 0.06];
const BASS: [f64; 6] = [1.0, 0.6, 0.35, 0.2, 0.12, 0.06];

fn buffer(secs: f64) -> Vec<f32> {
    vec![0.0; (secs * f64::from(SR)) as usize]
}

fn clip(buf: &mut [f32], drive: f32) {
    let norm = drive.tanh();
    for x in buf {
        *x = (drive * *x).tanh() / norm * 0.5;
    }
}

fn detect(kind: StemKind, x: &[f32], offset_cents: f64, beats: &[f64]) -> NoteAnalysis {
    let a = detect_notes(&NoteInput {
        samples: x,
        sample_rate: SR,
        kind,
        offset_cents,
        beats,
    });
    for w in a.notes.windows(2) {
        assert!(w[0].start_s <= w[1].start_s, "notes sorted\n{}", show(&a));
        assert!(
            w[0].end_s <= w[1].start_s + 1e-9,
            "notes overlap\n{}",
            show(&a)
        );
    }
    for n in &a.notes {
        assert!(n.end_s > n.start_s);
        assert!((0.0..=1.0).contains(&n.confidence));
        assert!((-50.0..=50.0).contains(&n.cents));
        for &(s, e) in &a.chordal {
            assert!(
                n.end_s <= s + 1e-9 || n.start_s >= e - 1e-9,
                "note in a chordal stretch\n{}",
                show(&a)
            );
        }
    }
    a
}

fn show(a: &NoteAnalysis) -> String {
    let mut s = String::new();
    for n in &a.notes {
        s += &format!(
            "{:6.3}-{:6.3} midi {:3} {:+5.1}c conf {:.2} {:?}{}{}\n",
            n.start_s,
            n.end_s,
            n.midi,
            n.cents,
            n.confidence,
            n.attack,
            n.bend.map_or(String::new(), |b| format!(
                " bend {:+.2} {:.3}->{:.3} rel {:?}",
                b.semitones, b.start_s, b.peak_s, b.release_s
            )),
            if n.vibrato { " vib" } else { "" }
        );
    }
    for (a, b) in &a.chordal {
        s += &format!("chordal {a:.3}-{b:.3}\n");
    }
    s
}

fn midis(a: &NoteAnalysis) -> Vec<u8> {
    a.notes.iter().map(|n| n.midi).collect()
}

/// Plucked notes back to back, each `len` long.
fn melody(notes: &[u8], len: f64, harm: &[f64], decay: f64) -> Vec<f32> {
    let mut x = buffer(0.3 + notes.len() as f64 * len + 0.3);
    for (i, &m) in notes.iter().enumerate() {
        tone(
            &mut x,
            0.3 + i as f64 * len,
            len,
            |_| f64::from(m),
            0.3,
            harm,
            decay,
            true,
        );
    }
    x
}

#[test]
fn clean_plucked_notes() {
    let notes = [57, 60, 62, 64, 67, 69, 72, 76, 81, 88];
    let x = melody(&notes, 0.3, &GUITAR, 3.0);
    let a = detect(StemKind::Guitar, &x, 0.0, &[]);
    assert_eq!(midis(&a), notes, "\n{}", show(&a));
    for (i, n) in a.notes.iter().enumerate() {
        assert_eq!(n.attack, Attack::Picked, "\n{}", show(&a));
        assert!(
            (n.start_s - (0.3 + i as f64 * 0.3)).abs() < 0.03,
            "\n{}",
            show(&a)
        );
        assert!(n.bend.is_none() && !n.vibrato, "\n{}", show(&a));
        assert!(n.cents.abs() < 8.0 && n.confidence > 0.5, "\n{}", show(&a));
    }
    assert!(a.chordal.is_empty(), "\n{}", show(&a));
}

#[test]
fn overdriven_notes_stay_single() {
    let notes = [40, 45, 52, 57, 64, 69];
    let mut x = melody(&notes, 0.4, &GUITAR, 2.0);
    clip(&mut x, 6.0);
    let a = detect(StemKind::Guitar, &x, 0.0, &[]);
    assert_eq!(midis(&a), notes, "\n{}", show(&a));
    assert!(a.chordal.is_empty(), "\n{}", show(&a));
}

#[test]
fn bass_line_with_low_b() {
    let notes = [23, 28, 33, 35, 40, 47];
    let x = melody(&notes, 0.5, &BASS, 1.5);
    let a = detect(StemKind::Bass, &x, 0.0, &[]);
    assert_eq!(midis(&a), notes, "\n{}", show(&a));
}

#[test]
fn bass_missing_fundamental() {
    let notes = [28, 31, 33, 36, 26];
    let harm = [0.0, 1.0, 0.7, 0.5, 0.3, 0.2];
    let x = melody(&notes, 0.5, &harm, 1.5);
    let a = detect(StemKind::Bass, &x, 0.0, &[]);
    assert_eq!(midis(&a), notes, "\n{}", show(&a));
}

#[test]
fn octave_traps() {
    // Weak fundamental under a strong 2nd harmonic, clean and overdriven.
    let notes = [45, 50, 55, 59, 64];
    let harm = [0.15, 1.0, 0.25, 0.4, 0.1, 0.1];
    let x = melody(&notes, 0.4, &harm, 2.0);
    let a = detect(StemKind::Guitar, &x, 0.0, &[]);
    assert_eq!(midis(&a), notes, "clean\n{}", show(&a));
    let mut x = melody(&notes, 0.4, &[0.3, 1.0, 0.2, 0.3], 2.0);
    clip(&mut x, 4.0);
    let a = detect(StemKind::Guitar, &x, 0.0, &[]);
    assert_eq!(midis(&a), notes, "overdriven\n{}", show(&a));
    assert!(a.chordal.is_empty(), "\n{}", show(&a));
}

/// Raised-cosine move from 0 to 1 over `len` starting at `at`.
fn ramp(t: f64, at: f64, len: f64) -> f64 {
    let u = ((t - at) / len).clamp(0.0, 1.0);
    0.5 - 0.5 * (std::f64::consts::PI * u).cos()
}

#[test]
fn full_step_bend_and_release() {
    let mut x = buffer(2.0);
    let pitch = |t: f64| 67.0 + 2.0 * ramp(t, 0.3, 0.12) - 2.0 * ramp(t, 0.75, 0.12);
    tone(&mut x, 0.2, 1.4, pitch, 0.3, &GUITAR, 0.8, true);
    let a = detect(StemKind::Guitar, &x, 0.0, &[]);
    assert_eq!(midis(&a), [67], "\n{}", show(&a));
    let b = a.notes[0].bend.expect("bend");
    assert!((b.semitones - 2.0).abs() < 0.25, "\n{}", show(&a));
    assert!(
        (b.start_s - 0.5).abs() < 0.06 && (b.peak_s - 0.62).abs() < 0.06,
        "\n{}",
        show(&a)
    );
    let r = b.release_s.expect("release");
    assert!((r - 1.07).abs() < 0.08, "\n{}", show(&a));
}

#[test]
fn half_step_bend_held() {
    let mut x = buffer(1.6);
    tone(
        &mut x,
        0.2,
        1.2,
        |t| 64.0 + ramp(t, 0.35, 0.1),
        0.3,
        &GUITAR,
        0.8,
        true,
    );
    let a = detect(StemKind::Guitar, &x, 0.0, &[]);
    assert_eq!(midis(&a), [64], "\n{}", show(&a));
    let b = a.notes[0].bend.expect("bend");
    assert!(
        (b.semitones - 1.0).abs() < 0.2 && b.release_s.is_none(),
        "\n{}",
        show(&a)
    );
}

#[test]
fn slide_over_three_semitones() {
    // Fret by fret: a semitone every 25 ms.
    let mut x = buffer(1.5);
    let pitch = |t: f64| 57.0 + ((t - 0.4) / 0.025).ceil().clamp(0.0, 3.0);
    tone(&mut x, 0.2, 1.1, pitch, 0.3, &GUITAR, 1.0, true);
    let a = detect(StemKind::Guitar, &x, 0.0, &[]);
    assert_eq!(midis(&a), [57, 60], "\n{}", show(&a));
    assert_eq!(a.notes[0].attack, Attack::Picked);
    assert_eq!(a.notes[1].attack, Attack::Slide, "\n{}", show(&a));
    assert!(a.notes.iter().all(|n| n.bend.is_none()), "\n{}", show(&a));
}

#[test]
fn hammer_on_and_pull_off() {
    let mut x = buffer(1.4);
    let pitch = |t: f64| if (0.3..0.6).contains(&t) { 59.0 } else { 57.0 };
    tone(&mut x, 0.2, 0.9, pitch, 0.3, &GUITAR, 1.5, true);
    let a = detect(StemKind::Guitar, &x, 0.0, &[]);
    assert_eq!(midis(&a), [57, 59, 57], "\n{}", show(&a));
    let attacks: Vec<Attack> = a.notes.iter().map(|n| n.attack).collect();
    assert_eq!(
        attacks,
        [Attack::Picked, Attack::Legato, Attack::Legato],
        "\n{}",
        show(&a)
    );
    assert!((a.notes[1].start_s - 0.5).abs() < 0.03, "\n{}", show(&a));
}

#[test]
fn vibrato_is_flagged() {
    let mut x = buffer(1.8);
    let pitch = |t: f64| 64.0 + 0.35 * (TAU * 6.0 * t).sin() * ramp(t, 0.1, 0.1);
    tone(&mut x, 0.2, 1.3, pitch, 0.3, &GUITAR, 0.5, true);
    let a = detect(StemKind::Guitar, &x, 0.0, &[]);
    assert_eq!(midis(&a), [64], "\n{}", show(&a));
    assert!(
        a.notes[0].vibrato && a.notes[0].bend.is_none(),
        "\n{}",
        show(&a)
    );
}

#[test]
fn vocal_glide_is_a_slide() {
    let mut x = buffer(1.6);
    let pitch = |t: f64| 60.0 + 5.0 * ramp(t, 0.5, 0.2);
    tone(
        &mut x,
        0.2,
        1.2,
        pitch,
        0.3,
        &[1.0, 0.5, 0.3, 0.2, 0.1],
        0.0,
        false,
    );
    let a = detect(StemKind::Vocals, &x, 0.0, &[]);
    assert_eq!(midis(&a), [60, 65], "\n{}", show(&a));
    assert_eq!(a.notes[1].attack, Attack::Slide, "\n{}", show(&a));
}

#[test]
fn double_stop_is_chordal() {
    let mut x = buffer(1.6);
    for m in [55.0, 59.0] {
        tone(&mut x, 0.3, 1.0, |_| m, 0.25, &GUITAR, 1.0, true);
    }
    let a = detect(StemKind::Guitar, &x, 0.0, &[]);
    assert_eq!(a.chordal.len(), 1, "\n{}", show(&a));
    let (s, e) = a.chordal[0];
    assert!(s < 0.45 && e > 1.0, "\n{}", show(&a));
}

#[test]
fn strummed_chord_is_chordal_and_a_single_note_is_not() {
    let mut x = buffer(3.4);
    // A single note first, then an open E chord strummed (weak low
    // fundamentals).
    tone(&mut x, 0.2, 0.8, |_| 64.0, 0.3, &GUITAR, 1.5, true);
    let harm = [0.4, 1.0, 0.6, 0.4, 0.25, 0.15, 0.1];
    for (i, m) in [40.0, 47.0, 52.0, 56.0, 59.0, 64.0].into_iter().enumerate() {
        tone(
            &mut x,
            1.2 + 0.012 * i as f64,
            2.0,
            |_| m,
            0.15,
            &harm,
            0.8,
            true,
        );
    }
    let a = detect(StemKind::Guitar, &x, 0.0, &[]);
    assert_eq!(midis(&a), [64], "\n{}", show(&a));
    assert_eq!(a.chordal.len(), 1, "\n{}", show(&a));
    let (s, e) = a.chordal[0];
    assert!((1.0..1.4).contains(&s) && e > 2.8, "\n{}", show(&a));
}

#[test]
fn silence_and_hiss_give_nothing() {
    let x = buffer(2.0);
    let a = detect(StemKind::Guitar, &x, 0.0, &[]);
    assert!(a.notes.is_empty() && a.chordal.is_empty());
    let mut noise = Noise(3);
    let x: Vec<f32> = (0..SR * 2).map(|_| (0.01 * noise.next()) as f32).collect();
    let a = detect(StemKind::Bass, &x, 0.0, &[]);
    assert!(a.notes.is_empty(), "\n{}", show(&a));
    let a = detect(StemKind::Other, &[], 0.0, &[]);
    assert!(a.notes.is_empty());
}

#[test]
fn offset_names_notes() {
    // A song 30 cents flat: notes are named from its own reference.
    let notes = [57.0, 62.0, 66.0];
    let mut x = buffer(1.8);
    for (i, m) in notes.iter().enumerate() {
        tone(
            &mut x,
            0.3 + i as f64 * 0.4,
            0.4,
            |_| m - 0.3,
            0.3,
            &GUITAR,
            2.0,
            true,
        );
    }
    let a = detect(StemKind::Guitar, &x, -30.0, &[]);
    assert_eq!(midis(&a), [57, 62, 66], "\n{}", show(&a));
    assert!(
        a.notes.iter().all(|n| n.cents.abs() < 8.0),
        "\n{}",
        show(&a)
    );
    // Without the offset the same notes read 30 cents flat.
    let a = detect(StemKind::Guitar, &x, 0.0, &[]);
    assert_eq!(midis(&a), [57, 62, 66], "\n{}", show(&a));
    assert!(
        a.notes.iter().all(|n| (n.cents + 30.0).abs() < 8.0),
        "\n{}",
        show(&a)
    );
}

#[test]
fn minimum_length_follows_the_beats() {
    let mut x = buffer(1.6);
    tone(&mut x, 0.3, 0.4, |_| 60.0, 0.3, &GUITAR, 2.0, true);
    tone(&mut x, 0.7, 0.06, |_| 67.0, 0.3, &GUITAR, 2.0, true);
    tone(&mut x, 0.76, 0.4, |_| 64.0, 0.3, &GUITAR, 2.0, true);
    let a = detect(StemKind::Guitar, &x, 0.0, &[]);
    assert_eq!(midis(&a), [60, 67, 64], "no beats\n{}", show(&a));
    // At 80 bpm a 32nd is 94 ms.
    let beats: Vec<f64> = (0..4).map(|i| i as f64 * 0.75).collect();
    let a = detect(StemKind::Guitar, &x, 0.0, &beats);
    assert_eq!(midis(&a), [60, 64], "80 bpm\n{}", show(&a));
}

#[test]
fn keys_at_48k() {
    let notes = [36, 48, 60, 72, 84, 91];
    let rate = 48_000u32;
    let mut x = vec![0.0f32; (rate as f64 * 3.0) as usize];
    for (i, &m) in notes.iter().enumerate() {
        let f = hz(f64::from(m));
        let s0 = ((0.2 + i as f64 * 0.4) * f64::from(rate)) as usize;
        for j in 0..(0.4 * f64::from(rate)) as usize {
            let t = j as f64 / f64::from(rate);
            let env =
                0.3 * (-2.0 * t).exp() * (t / 0.003).min(1.0) * ((0.4 - t) / 0.008).clamp(0.0, 1.0);
            let y: f64 = (1..=4)
                .map(|h| (TAU * f * h as f64 * t).sin() / h as f64)
                .filter(|_| true)
                .sum();
            x[s0 + j] += (env * y) as f32;
        }
    }
    let a = detect_notes(&NoteInput {
        samples: &x,
        sample_rate: rate,
        kind: StemKind::Keys,
        offset_cents: 0.0,
        beats: &[],
    });
    assert_eq!(midis(&a), notes, "\n{}", show(&a));
}

#[test]
fn four_minute_stem() {
    let mut noise = Noise(11);
    let len = 0.25;
    let count = (240.0 / len) as usize;
    let notes: Vec<u8> = (0..count)
        .map(|_| 45 + ((noise.next() + 1.0) * 12.0) as u8)
        .collect();
    let x = melody(&notes, len, &GUITAR, 3.0);
    let t0 = std::time::Instant::now();
    let a = detect(StemKind::Guitar, &x, 0.0, &[]);
    let elapsed = t0.elapsed().as_secs_f64();
    println!(
        "four-minute guitar stem: {elapsed:.2} s, {} notes",
        a.notes.len()
    );
    let right = a
        .notes
        .iter()
        .filter(|n| {
            let i = ((n.start_s - 0.3) / len).round() as usize;
            notes.get(i) == Some(&n.midi)
        })
        .count();
    assert!(right as f64 >= 0.95 * count as f64, "{right} of {count}");
    assert!(elapsed < 30.0);
}

/// A fake Basic Pitch: every window holds A4 over its frames 30 to 120,
/// attacked at frame 30.
fn fake_model(x: &[f32]) -> Result<crate::WindowOutput, String> {
    use crate::basic_pitch::{FRAMES_PER_WINDOW, N_CONTOUR, N_NOTES, WINDOW};
    assert_eq!(x.len(), WINDOW);
    let mut w = crate::WindowOutput {
        note: vec![0.0; FRAMES_PER_WINDOW * N_NOTES],
        onset: vec![0.0; FRAMES_PER_WINDOW * N_NOTES],
        contour: vec![0.0; FRAMES_PER_WINDOW * N_CONTOUR],
    };
    let k = 69 - 21;
    w.onset[30 * N_NOTES + k] = 0.9;
    for t in 30..120 {
        w.note[t * N_NOTES + k] = 0.8;
        w.contour[t * N_CONTOUR + 3 * k + 1] = 0.9;
    }
    Ok(w)
}

#[test]
fn the_model_path_runs_windows_through_the_closure() {
    // Four seconds of a quiet tone (the level gate needs some signal).
    let mut x = buffer(4.0);
    tone(&mut x, 0.0, 4.0, |_| 69.0, 0.3, &GUITAR, 0.0, false);
    let mut calls = 0;
    let input = NoteInput {
        samples: &x,
        sample_rate: SR,
        kind: StemKind::Guitar,
        offset_cents: 0.0,
        beats: &[],
    };
    let a = crate::detect_notes_with_model(&input, &mut |w: &[f32]| {
        calls += 1;
        fake_model(w)
    })
    .unwrap();
    assert_eq!(calls, 3);
    assert!(!a.notes.is_empty());
    assert!(a
        .notes
        .iter()
        .all(|n| n.midi == 69 && n.attack == Attack::Picked));
    assert!(a.notes.windows(2).all(|w| w[0].end_s <= w[1].start_s));
    // Errors come back from the closure.
    let err = crate::detect_notes_with_model(&input, &mut |_: &[f32]| Err("boom".into()));
    assert_eq!(err.unwrap_err(), "boom");
}

#[test]
fn the_bass_never_uses_the_model() {
    let x = melody(&[40, 43, 45], 0.4, &BASS, 3.0);
    let input = NoteInput {
        samples: &x,
        sample_rate: SR,
        kind: StemKind::Bass,
        offset_cents: 0.0,
        beats: &[],
    };
    let a = crate::detect_notes_with_model(&input, &mut |_: &[f32]| {
        panic!("the bass must not call the model")
    })
    .unwrap();
    assert_eq!(midis(&a), midis(&detect_notes(&input)));
    assert_eq!(midis(&a), vec![40, 43, 45]);
    // The tracker hears no other notes.
    assert!(a.others.is_empty());
}
