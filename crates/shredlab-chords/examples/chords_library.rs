// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Runs the chord detector on every song in a ShredLab library and prints
//! the key, segment, push and passing-chord counts, the runtime and the
//! first segments per song, for checking it on real stems.
//!
//! cargo run --release -p shredlab-chords --example chords_library -- <library root> [title filter] [segments] [from-to]
//!
//! With `from-to` (seconds, e.g. `60-75`) it lists the segments in that
//! stretch instead of the first ones, then every grid cell in it: position,
//! level, onset, change cost, bass note, the decoded and final chord, the
//! segment's detailed name, its score in the cell and the passing chord
//! the cell lies in, the five best-scoring chords, the two best of the
//! extensions the detector doesn't decode (6, m6, add9, 9, 7sus4), and the
//! chroma. Segments list their detailed name in brackets when it differs,
//! and passing chords (in brackets) and figures (in braces) appear between
//! them.

use shredlab_chords::{detect_chords, diag, Chord, Key, Quality, StemAudio, StemKind};

const SHARPS: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];
const FLATS: [&str; 12] = [
    "C", "Db", "D", "Eb", "E", "F", "Gb", "G", "Ab", "A", "Bb", "B",
];

fn main() {
    let mut args = std::env::args().skip(1);
    let root = args
        .next()
        .expect("usage: chords_library <library root> [title filter] [segments]");
    let filter = args.next().unwrap_or_default().to_lowercase();
    let shown: usize = args.next().and_then(|n| n.parse().ok()).unwrap_or(40);
    let range: Option<(f64, f64)> = args.next().and_then(|r| {
        let (a, b) = r.split_once('-')?;
        Some((a.parse().ok()?, b.parse().ok()?))
    });
    let lib = shredlab_library::Library::open(std::path::Path::new(&root)).expect("open library");
    let songs = std::fs::read_dir(std::path::Path::new(&root).join("songs")).expect("songs");
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
        let beats = meta
            .beats
            .as_ref()
            .map(|b| b.beats.clone())
            .unwrap_or_default();
        let offset = match &meta.tuning {
            Some(t) => f64::from(t.offset_cents),
            None => shredlab_tuning::estimate_song_tuning(&input, info.sample_rate)
                .map_or(0.0, |e| f64::from(e.offset_cents)),
        };
        let t0 = std::time::Instant::now();
        let a = detect_chords(&input, info.sample_rate, &beats, offset, meta.duration_sec);
        let elapsed = t0.elapsed().as_secs_f64();
        let names = match a.key {
            Some(k) if uses_flats(k) => &FLATS,
            _ => &SHARPS,
        };
        let key = a.key.map_or("?".into(), |k| {
            format!(
                "{}{}",
                names[usize::from(k.tonic)],
                if k.minor { "m" } else { "" }
            )
        });
        let chords = a.segments.iter().filter(|s| s.chord.is_some()).count();
        let pushes = a.segments.iter().filter(|s| s.push).count();
        let detailed = a.segments.iter().filter(|s| s.detailed != s.chord).count();
        println!(
            "{} - {}  key {key}  offset {offset:+.1}  beats {}  segments {} ({chords} chords, {pushes} pushes, {detailed} detailed, {} passing, {} figures)  {elapsed:.2}s",
            meta.artist.as_deref().unwrap_or("?"),
            meta.title,
            beats.len(),
            a.segments.len(),
            a.passing.len(),
            a.figures.len(),
        );
        // Share of the chord time per quality.
        let mut by_quality = [0.0f64; 9];
        let mut slash = 0.0;
        for s in &a.segments {
            if let Some(c) = s.chord {
                let i = Quality::ALL
                    .iter()
                    .position(|&q| q == c.quality)
                    .unwrap_or(0);
                by_quality[i] += s.end_s - s.start_s;
                if c.bass.is_some() {
                    slash += s.end_s - s.start_s;
                }
            }
        }
        let total: f64 = by_quality.iter().sum::<f64>().max(1e-9);
        let shares: Vec<String> = Quality::ALL
            .iter()
            .zip(&by_quality)
            .filter(|(_, &t)| t > 0.0)
            .map(|(q, t)| {
                format!(
                    "{}:{:.0}%",
                    if q.suffix().is_empty() {
                        "maj"
                    } else {
                        q.suffix()
                    },
                    100.0 * t / total
                )
            })
            .collect();
        // Share of the chord time per detailed name that differs.
        let mut ext = [0.0f64; 5];
        for s in &a.segments {
            if let Some(d) = s.detailed {
                if let Some(i) = Quality::EXTENDED.iter().position(|&q| q == d.quality) {
                    ext[i] += s.end_s - s.start_s;
                }
            }
        }
        let ext_shares: Vec<String> = Quality::EXTENDED
            .iter()
            .zip(&ext)
            .filter(|(_, &t)| t > 0.0)
            .map(|(q, t)| format!("{}:{:.0}%", q.suffix(), 100.0 * t / total))
            .collect();
        // Where the pushes sit relative to the nearest beat, in sixteenths.
        let mut push_at = [0usize; 5];
        for s in a.segments.iter().filter(|s| s.push) {
            let i = beats.partition_point(|&b| b < s.start_s);
            let near = [i.checked_sub(1), Some(i)]
                .into_iter()
                .flatten()
                .filter_map(|j| beats.get(j).map(|&b| (j, b)))
                .min_by(|a, b| (a.1 - s.start_s).abs().total_cmp(&(b.1 - s.start_s).abs()));
            if let Some((j, b)) = near {
                let len = if j + 1 < beats.len() {
                    beats[j + 1] - b
                } else {
                    b - beats[j.saturating_sub(1)]
                };
                let x = ((s.start_s - b) / len * 4.0).round() as i64;
                push_at[(x.clamp(-2, 2) + 2) as usize] += 1;
            }
        }
        println!(
            "    {}  slash:{:.0}%  detailed {}  pushes -8th:{} -16th:{} +16th:{} +8th:{}",
            shares.join(" "),
            100.0 * slash / total,
            if ext_shares.is_empty() {
                "-".to_string()
            } else {
                ext_shares.join(" ")
            },
            push_at[0],
            push_at[1],
            push_at[3],
            push_at[4]
        );
        let listed: Vec<_> = match range {
            Some((from, to)) => a
                .segments
                .iter()
                .filter(|s| s.end_s > from && s.start_s < to)
                .collect(),
            None => a.segments.iter().take(shown).collect(),
        };
        for s in listed {
            let detail = if s.detailed != s.chord {
                format!("{:10}", format!("({})", name(s.detailed, names)))
            } else {
                format!("{:10}", "")
            };
            println!(
                "    {:7.2}  {:8}{detail}{}",
                s.start_s,
                name(s.chord, names),
                if s.push { "  (push)" } else { "" }
            );
            let mut inner: Vec<(f64, String)> = a
                .passing
                .iter()
                .filter(|p| p.start_s >= s.start_s && p.start_s < s.end_s)
                .map(|p| {
                    (
                        p.start_s,
                        format!(
                            "      {:7.2}  [{}]  (passing, {:.2} s)",
                            p.start_s,
                            name(Some(p.chord), names),
                            p.end_s - p.start_s
                        ),
                    )
                })
                .collect();
            inner.extend(
                a.figures
                    .iter()
                    .filter(|f| f.start_s >= s.start_s && f.start_s < s.end_s)
                    .map(|f| {
                        (
                            f.start_s,
                            format!(
                                "      {:7.2}  {{{}}}  (figure, {:.2} s)",
                                f.start_s,
                                name(Some(f.chord), names),
                                f.end_s - f.start_s
                            ),
                        )
                    }),
            );
            inner.sort_by(|x, y| x.0.total_cmp(&y.0));
            for (_, line) in inner {
                println!("{line}");
            }
        }
        if let Some((from, to)) = range {
            print_cells(
                &diag::cell_report(
                    &input,
                    info.sample_rate,
                    &beats,
                    offset,
                    meta.duration_sec,
                    from,
                    to,
                ),
                names,
            );
        }
    }
}

fn print_cells(cells: &[diag::CellReport], names: &[&str; 12]) {
    let short = |c: Option<(u8, &str)>| match c {
        Some((r, q)) => format!("{}{q}", names[usize::from(r)]),
        None => "N.C.".into(),
    };
    println!(
        "
    {:>7} {:4} {:>4} {:>4} {:>4} {:4} {:8} {:8} {:8} {:>4} {:6} | top chords (score) | extra | chroma C..B",
        "time", "pos", "lvl", "ons", "cost", "bass", "decoded", "final", "detailed", "fit", "pass"
    );
    let full = |c: Option<(u8, &str, Option<u8>)>| match c {
        Some((r, q, b)) => {
            let mut n = format!("{}{q}", names[usize::from(r)]);
            if let Some(b) = b {
                n += "/";
                n += names[usize::from(b)];
            }
            n
        }
        None => "N.C.".into(),
    };
    for c in cells {
        let fin = full(c.segment);
        let detailed = full(c.detailed);
        let fit = c.segment_score.map_or("-".into(), |v| format!("{v:.2}"));
        let pass = match (c.passing, c.figure) {
            (Some(p), _) => short(Some(p)),
            (None, Some(f)) => format!("{{{}}}", short(Some(f))),
            (None, None) => String::new(),
        };
        let top: Vec<String> = c
            .top
            .iter()
            .map(|(ch, v)| format!("{} {v:.2}", short(*ch)))
            .collect();
        let extra: Vec<String> = c
            .extra
            .iter()
            .map(|((r, q), v)| format!("{}{q} {v:.2}", names[usize::from(*r)]))
            .collect();
        let chroma: String = c
            .chroma
            .iter()
            .map(|&x| match x {
                x if x >= 0.75 => '#',
                x if x >= 0.45 => '+',
                x if x >= 0.2 => '.',
                _ => ' ',
            })
            .collect();
        println!(
            "    {:7.2} {:4} {:4.2} {:4.2} {:4.2} {:4} {:8} {:8} {:8} {:>4} {:6} | {} | {} | [{chroma}]",
            c.start_s,
            c.pos,
            c.level,
            c.onset,
            c.penalty,
            c.bass.map_or("-", |b| names[usize::from(b)]),
            short(c.decoded.map(|(r, q)| (r, q.suffix()))),
            fin,
            detailed,
            fit,
            pass,
            top.join(", "),
            extra.join(", "),
        );
    }
}

fn uses_flats(k: Key) -> bool {
    // The relative major's tonic decides: F, Bb, Eb, Ab, Db, Gb.
    let major = if k.minor { (k.tonic + 3) % 12 } else { k.tonic };
    matches!(major, 5 | 10 | 3 | 8 | 1 | 6)
}

fn name(c: Option<Chord>, names: &[&str; 12]) -> String {
    match c {
        None => "N.C.".into(),
        Some(c) => {
            let mut s = format!("{}{}", names[usize::from(c.root)], c.quality.suffix());
            if let Some(b) = c.bass {
                s += "/";
                s += names[usize::from(b)];
            }
            s
        }
    }
}
