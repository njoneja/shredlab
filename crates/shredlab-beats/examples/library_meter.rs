// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Compares the upstream "minimal" downbeats (`postprocess`) with the bar
//! decoder (`postprocess_bars`) on every song of a ShredLab library: bar
//! lengths in beats and the times where the two disagree.
//!
//! ```text
//! cargo run --release -p shredlab-beats --example library_meter -- \
//!     [--library <dir>] [--model <onnx>] [--threads 8] [--cache <dir>] \
//!     [--song <title substring>] [--dump <t0> <t1>]
//! ```
//!
//! The library defaults to `%APPDATA%/io.github.njoneja.shredlab/library`
//! and is only read. Each song's mix is its original file, or the sum of its
//! stems when there is none. With `--cache`, the network logits are stored
//! there per song and reused on the next run, so changing the decoder does
//! not rerun the model. `--dump` prints every beat between `t0` and `t1`
//! seconds with its downbeat score and both decisions.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use ort::session::Session;
use ort::value::TensorRef;
use shredlab_beats::{infer, postprocess, postprocess_bars, preprocess, Logits};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Debug)]
struct E(String);
impl std::fmt::Display for E {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for E {}
impl From<shredlab_beats::Error> for E {
    fn from(e: shredlab_beats::Error) -> Self {
        E(e.to_string())
    }
}
impl From<ort::Error> for E {
    fn from(e: ort::Error) -> Self {
        E(e.to_string())
    }
}

struct Args {
    library: PathBuf,
    model: PathBuf,
    threads: usize,
    cache: Option<PathBuf>,
    song: Option<String>,
    dump: Option<(f64, f64)>,
}

fn args() -> Res<Args> {
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    let mut a = Args {
        library: Path::new(&appdata).join("io.github.njoneja.shredlab/library"),
        model: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../models/onnx/beat_this_final0.onnx"),
        threads: 8,
        cache: None,
        song: None,
        dump: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("{x} needs a value"));
        match x.as_str() {
            "--library" => a.library = val()?.into(),
            "--model" => a.model = val()?.into(),
            "--threads" => a.threads = val()?.parse()?,
            "--cache" => a.cache = Some(val()?.into()),
            "--song" => a.song = Some(val()?.to_lowercase()),
            "--dump" => {
                let t0 = val()?.parse()?;
                let t1 = it.next().ok_or("--dump needs t0 t1")?.parse()?;
                a.dump = Some((t0, t1));
            }
            _ => return Err(format!("unknown argument {x}").into()),
        }
    }
    Ok(a)
}

fn main() -> Res<()> {
    let a = args()?;
    let mut session: Option<Session> = None;
    let mut rows = Vec::new();
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(a.library.join("songs"))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    dirs.sort();
    for dir in dirs {
        let Ok(text) = std::fs::read_to_string(dir.join("song.json")) else {
            continue;
        };
        let meta: serde_json::Value = serde_json::from_str(&text)?;
        let id = meta["song_id"].as_str().unwrap_or("?").to_string();
        let title = meta["title"].as_str().unwrap_or("?").to_string();
        if let Some(s) = &a.song {
            if !title.to_lowercase().contains(s) {
                continue;
            }
        }
        let cached = a
            .cache
            .as_ref()
            .map(|c| c.join(format!("{id}.logits")))
            .and_then(|p| read_logits(&p));
        let logits = match cached {
            Some(l) => l,
            None => {
                let Some((channels, sr)) = mix(&dir, &meta)? else {
                    println!("== {title}: no audio, skipped");
                    continue;
                };
                if session.is_none() {
                    session = Some(open_session(&a.model, a.threads)?);
                }
                let t0 = Instant::now();
                let refs: Vec<&[f32]> = channels.iter().map(Vec::as_slice).collect();
                let spec = preprocess(&refs, sr)?;
                let s = session.as_mut().ok_or("no session")?;
                let l = infer(&spec, |input: &[f32], shape: [usize; 3]| -> Result<_, E> {
                    let t = TensorRef::from_array_view((shape, input))?;
                    let out = s.run(ort::inputs!["spect" => t])?;
                    let (_, b) = out["beat"].try_extract_tensor::<f32>()?;
                    let (_, d) = out["downbeat"].try_extract_tensor::<f32>()?;
                    Ok((b.to_vec(), d.to_vec()))
                })?;
                println!(
                    "== {title}: {:.0}s of audio, model in {:.1}s",
                    channels[0].len() as f64 / sr as f64,
                    t0.elapsed().as_secs_f64()
                );
                if let Some(c) = &a.cache {
                    std::fs::create_dir_all(c)?;
                    write_logits(&c.join(format!("{id}.logits")), &l)?;
                }
                l
            }
        };
        let old = postprocess(&logits);
        let new = postprocess_bars(&logits);
        let beats = &new.grid.beats;
        let stored: Vec<f64> = meta["beats"]["beats"]
            .as_array()
            .map(|v| v.iter().filter_map(|x| x.as_f64()).collect())
            .unwrap_or_default();
        let same_beats = stored.len() == beats.len()
            && stored.iter().zip(beats).all(|(a, b)| (a - b).abs() < 1e-6);
        let idx = |d: &[f64]| -> Vec<usize> {
            d.iter()
                .map(|&t| beats.partition_point(|&b| b < t - 1e-9))
                .collect()
        };
        let (io, inew) = (idx(&old.downbeats), idx(&new.grid.downbeats));
        let only_old: Vec<f64> = io
            .iter()
            .filter(|i| !inew.contains(i))
            .map(|&i| beats[i])
            .collect();
        let only_new: Vec<f64> = inew
            .iter()
            .filter(|i| !io.contains(i))
            .map(|&i| beats[i])
            .collect();
        println!(
            "== {title} ({} beats; stored beats {})",
            beats.len(),
            if stored.is_empty() {
                "none".to_string()
            } else if same_beats {
                "identical".to_string()
            } else {
                format!("differ ({} stored)", stored.len())
            }
        );
        println!(
            "  old bars {} | first downbeat {}",
            hist(&old.beats_per_bar()),
            fmt_t(old.downbeats.first())
        );
        println!(
            "  new bars {} | first downbeat {}",
            hist(&new.grid.beats_per_bar()),
            fmt_t(new.grid.downbeats.first())
        );
        for (what, d) in [("old", &io), ("new", &inew)] {
            let bars: Vec<usize> = d.windows(2).map(|w| w[1] - w[0]).collect();
            let common = most_common(&bars);
            let odd: Vec<String> = d
                .windows(2)
                .filter(|w| Some(w[1] - w[0]) != common)
                .map(|w| format!("{:.2}({})", beats[w[0]], w[1] - w[0]))
                .collect();
            println!("  {what} odd bars: {}", odd.join(" "));
        }
        println!("  only old: {}", times(&only_old));
        println!("  only new: {}", times(&only_new));
        if let Some((t0, t1)) = a.dump {
            for (i, &t) in beats.iter().enumerate() {
                if t < t0 || t > t1 {
                    continue;
                }
                println!(
                    "  {t:7.2}  {:6.2}  {}  {}",
                    new.downbeat_scores[i],
                    if io.contains(&i) { "OLD" } else { "   " },
                    if inew.contains(&i) { "NEW" } else { "   " },
                );
            }
        }
        rows.push((
            title,
            hist(&old.beats_per_bar()),
            hist(&new.grid.beats_per_bar()),
            only_old.len(),
            only_new.len(),
        ));
    }
    println!("\n| song | old bars | new bars | only old | only new |");
    println!("|---|---|---|---|---|");
    for (t, o, n, a, b) in rows {
        println!("| {t} | {o} | {n} | {a} | {b} |");
    }
    Ok(())
}

fn open_session(model: &Path, threads: usize) -> Res<Session> {
    let env = ort::init()
        .with_name("shredlab-beats")
        .build()
        .map_err(|e| E(e.to_string()))?;
    Ok(Session::builder(&env)?
        .with_intra_threads(threads)
        .map_err(|e| E(e.to_string()))?
        .commit_from_file(model)?)
}

/// `beats-per-bar:count`, most common first.
fn hist(bars: &[usize]) -> String {
    let mut h: BTreeMap<usize, usize> = BTreeMap::new();
    for &b in bars {
        *h.entry(b).or_default() += 1;
    }
    let mut v: Vec<_> = h.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v.iter()
        .map(|(b, c)| format!("{b}:{c}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn most_common(bars: &[usize]) -> Option<usize> {
    let mut h: BTreeMap<usize, usize> = BTreeMap::new();
    for &b in bars {
        *h.entry(b).or_default() += 1;
    }
    h.into_iter().max_by_key(|&(_, c)| c).map(|(b, _)| b)
}

fn fmt_t(t: Option<&f64>) -> String {
    t.map_or("-".into(), |t| format!("{t:.2}s"))
}

fn times(t: &[f64]) -> String {
    if t.is_empty() {
        return "-".into();
    }
    let shown: Vec<String> = t.iter().take(24).map(|t| format!("{t:.2}")).collect();
    let more = if t.len() > 24 {
        format!(" (+{} more)", t.len() - 24)
    } else {
        String::new()
    };
    format!("{}{more}", shown.join(" "))
}

fn write_logits(p: &Path, l: &Logits) -> Res<()> {
    let mut b = Vec::with_capacity(8 + 8 * l.beat.len());
    b.extend_from_slice(&(l.beat.len() as u64).to_le_bytes());
    for v in l.beat.iter().chain(&l.downbeat) {
        b.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(p, b)?;
    Ok(())
}

fn read_logits(p: &Path) -> Option<Logits> {
    let b = std::fs::read(p).ok()?;
    let n = u64::from_le_bytes(b.get(..8)?.try_into().ok()?) as usize;
    let v: Vec<f32> = b[8..]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect();
    (v.len() == 2 * n).then(|| Logits {
        beat: v[..n].to_vec(),
        downbeat: v[n..].to_vec(),
    })
}

/// The song's mix: its original file, else the sum of its stems (each times
/// its scale). `None` when neither is there.
fn mix(dir: &Path, meta: &serde_json::Value) -> Res<Option<(Vec<Vec<f32>>, u32)>> {
    if let Some(o) = meta["original"].as_object() {
        let file = o
            .get("file")
            .and_then(|f| f.as_str())
            .unwrap_or("original.wav");
        let scale = o.get("scale").and_then(|s| s.as_f64()).unwrap_or(1.0) as f32;
        let p = dir.join(file);
        if p.exists() {
            let (mut ch, sr) = decode(&p)?;
            ch.iter_mut().flatten().for_each(|v| *v *= scale);
            return Ok(Some((ch, sr)));
        }
    }
    let Some(stems) = meta["stems"]["stems"].as_array() else {
        return Ok(None);
    };
    let mut sum: Vec<Vec<f32>> = Vec::new();
    let mut rate = 0;
    for s in stems {
        let name = s["name"].as_str().unwrap_or("");
        let scale = s["scale"].as_f64().unwrap_or(1.0) as f32;
        let Some(p) = ["wav", "flac"]
            .iter()
            .map(|e| dir.join("stems").join(format!("{name}.{e}")))
            .find(|p| p.exists())
        else {
            return Ok(None);
        };
        let (ch, sr) = decode(&p)?;
        rate = sr;
        if sum.is_empty() {
            sum = vec![Vec::new(); ch.len()];
        }
        for (acc, c) in sum.iter_mut().zip(&ch) {
            if acc.len() < c.len() {
                acc.resize(c.len(), 0.0);
            }
            for (a, &v) in acc.iter_mut().zip(c) {
                *a += v * scale;
            }
        }
    }
    Ok((!sum.is_empty()).then_some((sum, rate)))
}

/// Minimal symphonia decode to planar f32 at the native rate.
fn decode(path: &Path) -> Res<(Vec<Vec<f32>>, u32)> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
    use symphonia::core::errors::Error as SymError;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let mss = MediaSourceStream::new(Box::new(std::fs::File::open(path)?), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let fmt = FormatOptions {
        enable_gapless: true,
        ..Default::default()
    };
    let mut format = symphonia::default::get_probe()
        .format(&hint, mss, &fmt, &MetadataOptions::default())?
        .format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or("no audio track")?;
    let id = track.id;
    let mut sr = track.codec_params.sample_rate;
    let mut dec =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;
    let mut ch: Vec<Vec<f32>> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        };
        if packet.track_id() != id {
            continue;
        }
        let decoded = match dec.decode(&packet) {
            Ok(d) => d,
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(e.into()),
        };
        let spec = *decoded.spec();
        sr.get_or_insert(spec.rate);
        let nc = spec.channels.count();
        if ch.is_empty() {
            ch = vec![Vec::new(); nc];
        }
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buf.copy_interleaved_ref(decoded);
        for frame in buf.samples().chunks_exact(nc) {
            for (c, &v) in ch.iter_mut().zip(frame) {
                c.push(v);
            }
        }
    }
    Ok((ch, sr.ok_or("unknown sample rate")?))
}
