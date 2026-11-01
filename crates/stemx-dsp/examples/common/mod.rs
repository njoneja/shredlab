//! Shared helpers for the examples: WAV I/O and library song folders.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// Reads a stereo WAV (16/24-bit int or 32-bit float) as planar f32,
/// multiplied by `scale` (the library's per-stem gain).
pub fn read_wav(path: &Path, scale: f32) -> (u32, [Vec<f32>; 2]) {
    let mut r = hound::WavReader::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let spec = r.spec();
    assert_eq!(spec.channels, 2, "{}: need stereo", path.display());
    let inter: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().map(|s| s.unwrap() * scale).collect(),
        hound::SampleFormat::Int => {
            let k = scale / ((1i64 << (spec.bits_per_sample - 1)) - 1) as f32;
            r.samples::<i32>().map(|s| s.unwrap() as f32 * k).collect()
        }
    };
    let n = inter.len() / 2;
    let mut l = Vec::with_capacity(n);
    let mut rr = Vec::with_capacity(n);
    for f in inter.as_chunks::<2>().0 {
        l.push(f[0]);
        rr.push(f[1]);
    }
    (spec.sample_rate, [l, rr])
}

/// Writes a stereo 32-bit float WAV.
pub fn write_wav(path: &Path, sr: u32, ch: &[Vec<f32>; 2]) {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).unwrap();
    }
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: sr,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for (a, b) in ch[0].iter().zip(&ch[1]) {
        w.write_sample(*a).unwrap();
        w.write_sample(*b).unwrap();
    }
    w.finalize().unwrap();
}

/// A cached song in the app library (read only).
pub struct Song {
    pub dir: PathBuf,
    pub title: String,
    pub original_scale: f32,
    /// (stem name, wav path, scale) of the first stem set (or `set` if given).
    pub stems: Vec<(String, PathBuf, f32)>,
}

/// Loads `song.json` of a library song folder. `set` picks a stem-set dir
/// (e.g. "htdemucs_6s_s1"); default is the first set.
pub fn load_song(dir: &Path, set: Option<&str>) -> Song {
    let txt = std::fs::read_to_string(dir.join("song.json")).expect("song.json");
    let v: serde_json::Value = serde_json::from_str(&txt).expect("song.json parse");
    let sets = v["stem_sets"].as_array().expect("stem_sets");
    let chosen = sets
        .iter()
        .find(|s| set.is_none_or(|want| s["dir"].as_str().is_some_and(|d| d.ends_with(want))))
        .expect("stem set not found");
    let sdir = dir.join(chosen["dir"].as_str().unwrap_or("stems"));
    let stems = chosen["stems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            let name = s["name"].as_str().unwrap().to_string();
            let scale = s["scale"].as_f64().unwrap_or(1.0) as f32;
            (name.clone(), sdir.join(format!("{name}.wav")), scale)
        })
        .collect();
    Song {
        dir: dir.to_path_buf(),
        title: v["title"].as_str().unwrap_or("?").to_string(),
        original_scale: v["original"]["scale"].as_f64().unwrap_or(1.0) as f32,
        stems,
    }
}

/// Finds a library song folder by (case-insensitive) title substring under
/// `%APPDATA%\io.github.njoneja.shredlab\library\songs` or `STEMX_DATA_DIR`.
pub fn find_song(title: &str) -> Option<PathBuf> {
    let base = std::env::var_os("STEMX_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("io.github.njoneja.shredlab"))
        })?;
    let songs = base.join("library").join("songs");
    let want = title.to_lowercase();
    std::fs::read_dir(songs).ok()?.flatten().find_map(|e| {
        let txt = std::fs::read_to_string(e.path().join("song.json")).ok()?;
        let v: serde_json::Value = serde_json::from_str(&txt).ok()?;
        v["title"]
            .as_str()?
            .to_lowercase()
            .contains(&want)
            .then(|| e.path())
    })
}
