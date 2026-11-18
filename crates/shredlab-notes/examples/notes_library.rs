// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Runs the note detector on every pitched stem of every song in a
//! ShredLab library and prints per stem the note count, the chordal share,
//! the bend, slide, legato and vibrato counts, the count of other notes
//! heard (model path) and the bytes they take in `song.json`, and the
//! runtime, for checking it on real stems.
//!
//! cargo run --release -p shredlab-notes --example notes_library -- <library root> [title filter] [from-to] [stem] [--model <nmp.onnx>] [--pan <center>,<width>]
//!
//! With `from-to` (seconds, e.g. `60-75`) it also lists the notes in that
//! stretch: start, end, name, cents, confidence, attack, bend and vibrato,
//! and the chordal stretches, and sums them up (count, notes from C4 up,
//! bends, the best-fitting major key and its share, other notes heard).
//! `stem` limits it to
//! one stem (bass, guitar, piano, other, vocals). `--model` runs Basic
//! Pitch (ONNX Runtime on the CPU) for the stems that use it, as the app
//! does; `--pan` applies Refine's stereo mask (softness 0.2) first.

use ort::session::Session;
use ort::value::TensorRef;
use shredlab_dsp::refine::pan::{pan_offline, PanParams};
use shredlab_notes::{
    detect_notes, detect_notes_with_model, Attack, Note, NoteAnalysis, NoteInput, StemKind,
    WindowOutput,
};

const NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

fn name(midi: u8) -> String {
    format!(
        "{}{}",
        NAMES[usize::from(midi % 12)],
        i32::from(midi) / 12 - 1
    )
}

const MAJOR: [u8; 7] = [0, 2, 4, 5, 7, 9, 11];

/// The major key whose scale holds the most notes, and that share.
fn best_key(notes: &[&Note]) -> (u8, f64) {
    (0..12u8)
        .map(|t| {
            let k = notes
                .iter()
                .filter(|n| MAJOR.contains(&((n.midi + 12 - t) % 12)))
                .count();
            (t, k as f64 / notes.len().max(1) as f64)
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap_or((0, 0.0))
}

/// Basic Pitch through ONNX Runtime.
struct Model {
    session: Session,
    input: String,
}

impl Model {
    fn load(path: &str) -> Model {
        let env = ort::init().with_name("notes_library").build().expect("ort");
        let session = Session::builder(&env)
            .expect("session builder")
            .with_intra_threads(4)
            .expect("threads")
            .commit_from_file(path)
            .expect("load model");
        let input = session.inputs()[0].name().to_string();
        Model { session, input }
    }

    fn run(&mut self, x: &[f32]) -> Result<WindowOutput, String> {
        let e = |e: ort::Error| e.to_string();
        let t = TensorRef::from_array_view(([1usize, x.len(), 1], x)).map_err(e)?;
        let out = self
            .session
            .run(ort::inputs![self.input.as_str() => t])
            .map_err(e)?;
        let get = |k: &str| -> Result<Vec<f32>, String> {
            let (_, v) = out[k].try_extract_tensor::<f32>().map_err(e)?;
            Ok(v.to_vec())
        };
        Ok(WindowOutput {
            note: get("StatefulPartitionedCall:1")?,
            onset: get("StatefulPartitionedCall:2")?,
            contour: get("StatefulPartitionedCall:0")?,
        })
    }
}

fn main() {
    let usage = "usage: notes_library <library root> [title filter] [from-to] [stem] [--model <nmp.onnx>] [--pan <center>,<width>]";
    let mut model: Option<Model> = None;
    let mut pan: Option<PanParams> = None;
    let mut rest = Vec::new();
    let mut all = std::env::args().skip(1);
    while let Some(a) = all.next() {
        match a.as_str() {
            "--model" => model = Some(Model::load(&all.next().expect(usage))),
            "--pan" => {
                let v = all.next().expect(usage);
                let (c, w) = v.split_once(',').expect(usage);
                pan = Some(PanParams {
                    enabled: true,
                    center: c.parse().expect(usage),
                    width: w.parse().expect(usage),
                    softness: 0.2,
                    invert: false,
                });
            }
            _ => rest.push(a),
        }
    }
    let mut args = rest.into_iter();
    let root = args.next().expect(usage);
    let filter = args.next().unwrap_or_default().to_lowercase();
    let range: Option<(f64, f64)> = args.next().and_then(|r| {
        let (a, b) = r.split_once('-')?;
        Some((a.parse().ok()?, b.parse().ok()?))
    });
    let only = args.next();
    let lib = shredlab_library::Library::open(std::path::Path::new(&root)).expect("open library");
    let songs = std::fs::read_dir(std::path::Path::new(&root).join("songs")).expect("songs");
    println!(
        "{:34} {:7} {:>6} {:>8} {:>5} {:>6} {:>6} {:>4} {:>5} {:>6} {:>7} {:>6}",
        "song",
        "stem",
        "notes",
        "chordal",
        "bends",
        "slides",
        "legato",
        "vib",
        "conf",
        "others",
        "o.bytes",
        "time"
    );
    for entry in songs.flatten() {
        let id = entry.file_name().to_string_lossy().to_string();
        let Ok(Some(meta)) = lib.load(&id) else {
            continue;
        };
        if !meta.title.to_lowercase().contains(&filter) {
            continue;
        }
        let Some(info) = meta.stem_sets.last() else {
            continue;
        };
        let Ok(stems) = lib.read_stems(&id, info) else {
            println!("{:34} cannot read stems", meta.title);
            continue;
        };
        let beats = meta
            .beats
            .as_ref()
            .map(|b| b.beats.clone())
            .unwrap_or_default();
        let offset = meta
            .tuning
            .as_ref()
            .map_or(0.0, |t| f64::from(t.offset_cents));
        let title: String = meta.title.chars().take(34).collect();
        for (stem, [l, r]) in &stems {
            let kind = match stem.as_str() {
                "bass" => StemKind::Bass,
                "guitar" => StemKind::Guitar,
                "piano" | "keys" => StemKind::Keys,
                "other" => StemKind::Other,
                "vocals" => StemKind::Vocals,
                _ => continue,
            };
            if only.as_deref().is_some_and(|o| o != stem) {
                continue;
            }
            let [l, r] = match &pan {
                Some(p) => pan_offline(p, info.sample_rate, l, r),
                None => [l.clone(), r.clone()],
            };
            let mono: Vec<f32> = l.iter().zip(&r).map(|(a, b)| 0.5 * (a + b)).collect();
            let input = NoteInput {
                samples: &mono,
                sample_rate: info.sample_rate,
                kind,
                offset_cents: offset,
                beats: &beats,
            };
            let t0 = std::time::Instant::now();
            let a = match model.as_mut() {
                Some(m) => {
                    detect_notes_with_model(&input, &mut |x: &[f32]| m.run(x)).expect("note model")
                }
                None => detect_notes(&input),
            };
            let elapsed = t0.elapsed().as_secs_f64();
            let duration = mono.len() as f64 / f64::from(info.sample_rate);
            print_summary(&title, stem, &a, duration, elapsed);
            if let Some((from, to)) = range {
                print_range(&a, from, to);
            }
        }
    }
}

fn print_summary(title: &str, stem: &str, a: &NoteAnalysis, duration: f64, elapsed: f64) {
    let chordal: f64 = a.chordal.iter().map(|(s, e)| e - s).sum::<f64>().max(0.0);
    let count = |at: Attack| a.notes.iter().filter(|n| n.attack == at).count();
    let conf = if a.notes.is_empty() {
        0.0
    } else {
        a.notes.iter().map(|n| n.confidence).sum::<f32>() / a.notes.len() as f32
    };
    println!(
        "{:34} {:7} {:>6} {:>7.0}% {:>5} {:>6} {:>6} {:>4} {:>5.2} {:>6} {:>7} {:>5.2}s",
        title,
        stem,
        a.notes.len(),
        100.0 * chordal / duration.max(1e-9),
        a.notes.iter().filter(|n| n.bend.is_some()).count(),
        count(Attack::Slide),
        count(Attack::Legato),
        a.notes.iter().filter(|n| n.vibrato).count(),
        conf,
        a.others.len(),
        others_bytes(a),
        elapsed
    );
}

/// Bytes the other notes heard add to a stem's entry in `song.json`.
fn others_bytes(a: &NoteAnalysis) -> usize {
    let others = a
        .others
        .iter()
        .map(|h| shredlab_library::HeardSpan {
            start_s: h.start_s,
            end_s: h.end_s,
            midi: h.midi,
            conf: h.confidence,
        })
        .collect();
    let entry = |others| shredlab_library::StemNotes {
        others,
        ..Default::default()
    };
    let len = |e| serde_json::to_string_pretty(&e).map_or(0, |s| s.len());
    len(entry(others)) - len(entry(Vec::new()))
}

fn print_range(a: &NoteAnalysis, from: f64, to: f64) {
    let inside: Vec<&Note> = a
        .notes
        .iter()
        .filter(|n| n.end_s > from && n.start_s < to)
        .collect();
    let (key, share) = best_key(&inside);
    let chordal: f64 = a
        .chordal
        .iter()
        .map(|&(s, e)| (e.min(to) - s.max(from)).max(0.0))
        .sum::<f64>()
        + 0.0;
    let heard: Vec<_> = a
        .others
        .iter()
        .filter(|h| h.end_s > from && h.start_s < to)
        .collect();
    println!(
        "    {from}-{to}: {} notes, {} from C4 up, {} bends, chordal {chordal:.1}s, best major key {} {:.0}%, {} others heard",
        inside.len(),
        inside.iter().filter(|n| n.midi >= 60).count(),
        inside.iter().filter(|n| n.bend.is_some()).count(),
        NAMES[usize::from(key)],
        100.0 * share,
        heard.len()
    );
    for &(s, e) in a.chordal.iter().filter(|(s, e)| *e > from && *s < to) {
        println!("    chordal {s:7.2}-{e:7.2}");
    }
    for n in a.notes.iter().filter(|n| n.end_s > from && n.start_s < to) {
        let bend = n.bend.map_or(String::new(), |b| {
            format!(
                "  bend {:+.2} {:.2}->{:.2}{}",
                b.semitones,
                b.start_s,
                b.peak_s,
                b.release_s
                    .map_or(String::new(), |r| format!(" rel {r:.2}"))
            )
        });
        println!(
            "    {:7.2}-{:7.2}  {:4} {:+4.0}c  conf {:.2}  {:7}{}{}",
            n.start_s,
            n.end_s,
            name(n.midi),
            n.cents,
            n.confidence,
            match n.attack {
                Attack::Picked => "picked",
                Attack::Legato => "legato",
                Attack::Slide => "slide",
            },
            bend,
            if n.vibrato { "  vibrato" } else { "" }
        );
    }
    for h in heard {
        println!(
            "    {:7.2}-{:7.2}  {:4} heard  conf {:.2}",
            h.start_s,
            h.end_s,
            name(h.midi),
            h.confidence
        );
    }
}
