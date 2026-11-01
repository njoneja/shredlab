//! Library: one folder per song (keyed by a content hash) holding a JSON
//! manifest (`song.json`) and the cached stems.
//!
//! Layout under the library root:
//!
//! ```text
//! <root>/songs/<song_id>/song.json
//! <root>/songs/<song_id>/original.wav               copy of the original mix
//! <root>/songs/<song_id>/stems/<model_id>_s<shifts>/<stem>.wav
//! <root>/songs/<song_id>/stems/<stem>.wav           (schema 1 entries)
//! ```
//!
//! Stems are cached per (model, shifts), so switching model or separation
//! quality back is instant. The original mix is cached too (44.1 kHz, same
//! format as the stems), so the backing track can be built from it even
//! when the source file has moved.
//!
//! Stems are stored as **16-bit PCM WAV with a per-stem gain** (`scale` in
//! `song.json`): samples are divided by `scale` (= max(peak, 1)) before
//! quantization, so loud stems never clip, and multiplied back on load.
//! That halves the disk footprint of float WAV (a 5 min song is ~300 MB for
//! 6 stems instead of ~600 MB); 16-bit noise (-96 dBFS) is inaudible for
//! practice playback and export.
//!
//! This crate has no Tauri dependency; the app passes the root directory.

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Version of this crate, from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Crate name, from `Cargo.toml`.
pub const NAME: &str = env!("CARGO_PKG_NAME");

/// `song.json` schema version. 2: `stem_sets` (per model and shifts) and
/// `original`; schema 1 `stems` is migrated on load.
pub const SCHEMA: u32 = 2;

/// Bytes hashed from the start and from the end of the file for the song id.
const HASH_SPAN: u64 = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("{path}: invalid song manifest: {msg}")]
    Json { path: String, msg: String },
    #[error("{path}: invalid stem file: {msg}")]
    Wav { path: String, msg: String },
    #[error("invalid input: {0}")]
    Input(String),
}

impl Error {
    fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.display().to_string(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Model and device used to make the cached stems.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StemsInfo {
    /// Registry id, e.g. `htdemucs_6s`.
    pub model_id: String,
    /// Display name shown in the UI, e.g. "Demucs 6-stem".
    pub model_name: String,
    /// Short device label, "GPU" or "CPU".
    pub device_label: String,
    pub sample_rate: u32,
    /// Frames per channel.
    pub len: usize,
    /// One entry per stem file.
    pub stems: Vec<StemFile>,
    /// Separation passes (1 = Standard, 2 = High). Schema 1 entries: 1.
    #[serde(default = "one")]
    pub shifts: u32,
    /// Folder of the stem files, relative to the song folder. Schema 1
    /// entries: `stems`.
    #[serde(default = "legacy_dir")]
    pub dir: String,
}

fn one() -> u32 {
    1
}

fn legacy_dir() -> String {
    "stems".into()
}

impl StemsInfo {
    /// True if this set was made by `model_id` with `shifts` passes.
    pub fn is(&self, model_id: &str, shifts: u32) -> bool {
        self.model_id == model_id && self.shifts == shifts
    }
}

/// Folder (relative to the song folder) for the stems of one model and
/// pass count: `stems/<model_id>_s<shifts>` (id sanitized).
pub fn stem_set_dir(model_id: &str, shifts: u32) -> String {
    let id: String = model_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("stems/{id}_s{shifts}")
}

/// The cached copy of the original mix (`original.wav`, 16-bit stereo,
/// multiply by `scale`), at `sample_rate`, `len` frames.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OriginalInfo {
    pub sample_rate: u32,
    pub len: usize,
    pub scale: f32,
}

/// One cached stem: `stems/<name>.wav`, 16-bit stereo, multiply by `scale`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StemFile {
    pub name: String,
    pub scale: f32,
}

/// Cached beat grid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct BeatData {
    pub beats: Vec<f64>,
    pub downbeats: Vec<f64>,
    pub bpm: Option<f64>,
    pub beats_per_bar: Option<u32>,
}

/// Contents of `song.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SongMeta {
    pub schema: u32,
    pub song_id: String,
    /// Path the song was last opened from.
    pub source_path: String,
    pub title: String,
    pub artist: Option<String>,
    pub duration_sec: f64,
    /// Of the decoded original.
    pub sample_rate: u32,
    pub channels: u32,
    /// Schema 1 only (a single stem set); moved into `stem_sets` on load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stems: Option<StemsInfo>,
    /// Cached stem sets, one per (model, shifts), most recent last.
    #[serde(default)]
    pub stem_sets: Vec<StemsInfo>,
    /// Cached copy of the original mix.
    #[serde(default)]
    pub original: Option<OriginalInfo>,
    #[serde(default)]
    pub beats: Option<BeatData>,
    /// Opaque UI settings, stored and returned verbatim.
    #[serde(default)]
    pub settings: Option<serde_json::Value>,
    pub created_ms: u64,
    pub last_opened_ms: u64,
    pub updated_ms: u64,
}

impl SongMeta {
    /// A new manifest with timestamps set to now.
    pub fn new(song_id: &str, source_path: &Path, tags: Tags, duration_sec: f64) -> Self {
        let now = now_ms();
        let title = tags.title.unwrap_or_else(|| file_title(source_path));
        Self {
            schema: SCHEMA,
            song_id: song_id.to_string(),
            source_path: source_path.display().to_string(),
            title,
            artist: tags.artist,
            duration_sec,
            sample_rate: 0,
            channels: 0,
            stems: None,
            stem_sets: Vec::new(),
            original: None,
            beats: None,
            settings: None,
            created_ms: now,
            last_opened_ms: now,
            updated_ms: now,
        }
    }

    /// The cached set for (`model_id`, `shifts`), if any.
    pub fn stem_set(&self, model_id: &str, shifts: u32) -> Option<&StemsInfo> {
        self.stem_sets.iter().find(|s| s.is(model_id, shifts))
    }

    /// Adds or replaces the set for its (model, shifts); it becomes the
    /// most recent.
    pub fn put_stem_set(&mut self, info: StemsInfo) {
        self.stem_sets
            .retain(|s| !s.is(&info.model_id, info.shifts));
        self.stem_sets.push(info);
    }

    /// Moves a schema 1 `stems` entry into `stem_sets`.
    fn migrate(&mut self) {
        if let Some(s) = self.stems.take() {
            if self.stem_set(&s.model_id, s.shifts).is_none() {
                self.stem_sets.insert(0, s);
            }
        }
        self.schema = SCHEMA;
    }
}

/// Title/artist read from the file's tags.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tags {
    pub title: Option<String>,
    pub artist: Option<String>,
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// File name without extension, used when there is no title tag.
pub fn file_title(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Stable id for an audio file: sha256 over the size and the first and last
/// 4 MB, as 32 hex chars. Independent of path and modification time, so a
/// moved or copied file keeps its cache.
pub fn song_id_for(path: &Path) -> Result<String> {
    let mut f = fs::File::open(path).map_err(|e| Error::io(path, e))?;
    let size = f.metadata().map_err(|e| Error::io(path, e))?.len();
    let mut h = Sha256::new();
    h.update(size.to_le_bytes());
    let mut buf = vec![0u8; HASH_SPAN.min(size) as usize];
    f.read_exact(&mut buf).map_err(|e| Error::io(path, e))?;
    h.update(&buf);
    if size > HASH_SPAN {
        let start = size.saturating_sub(HASH_SPAN).max(HASH_SPAN);
        let n = (size - start) as usize;
        buf.resize(n, 0);
        f.seek(SeekFrom::Start(start))
            .and_then(|_| f.read_exact(&mut buf))
            .map_err(|e| Error::io(path, e))?;
        h.update(&buf);
    }
    let digest = h.finalize();
    Ok(digest[..16].iter().map(|b| format!("{b:02x}")).collect())
}

/// Reads title and artist tags (ID3, Vorbis comments, MP4 atoms) without
/// decoding audio. Missing or unreadable tags give `None`s.
pub fn read_tags(path: &Path) -> Tags {
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::{MetadataOptions, StandardTagKey};
    use symphonia::core::probe::Hint;

    let mut tags = Tags::default();
    let Ok(file) = fs::File::open(path) else {
        return tags;
    };
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let Ok(mut probed) = symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    ) else {
        return tags;
    };
    let mut visit = |rev: &symphonia::core::meta::MetadataRevision| {
        for t in rev.tags() {
            let v = t.value.to_string().trim().to_string();
            if v.is_empty() {
                continue;
            }
            match t.std_key {
                Some(StandardTagKey::TrackTitle) if tags.title.is_none() => tags.title = Some(v),
                Some(StandardTagKey::Artist) if tags.artist.is_none() => tags.artist = Some(v),
                Some(StandardTagKey::AlbumArtist) if tags.artist.is_none() => tags.artist = Some(v),
                _ => {}
            }
        }
    };
    if let Some(m) = probed.metadata.get() {
        if let Some(rev) = m.current() {
            visit(rev);
        }
    }
    if let Some(rev) = probed.format.metadata().current() {
        visit(rev);
    }
    tags
}

/// Summary for the recent-songs list.
#[derive(Debug, Clone, PartialEq)]
pub struct RecentEntry {
    pub song_id: String,
    pub path: String,
    pub title: String,
    pub artist: Option<String>,
    pub duration_sec: f64,
    pub stems_cached: bool,
    pub last_opened_ms: u64,
}

/// A song library rooted at a directory.
#[derive(Debug, Clone)]
pub struct Library {
    root: PathBuf,
}

impl Library {
    /// Opens (creating if needed) a library at `root`.
    pub fn open(root: &Path) -> Result<Self> {
        let songs = root.join("songs");
        fs::create_dir_all(&songs).map_err(|e| Error::io(&songs, e))?;
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn song_dir(&self, song_id: &str) -> PathBuf {
        self.root.join("songs").join(song_id)
    }

    fn manifest_path(&self, song_id: &str) -> PathBuf {
        self.song_dir(song_id).join("song.json")
    }

    fn stem_path(&self, song_id: &str, dir: &str, stem: &str) -> PathBuf {
        let mut p = self.song_dir(song_id);
        for part in dir.split('/').filter(|s| !s.is_empty() && *s != "..") {
            p.push(part);
        }
        p.join(format!("{stem}.wav"))
    }

    fn original_path(&self, song_id: &str) -> PathBuf {
        self.song_dir(song_id).join("original.wav")
    }

    /// Loads `song.json`, `Ok(None)` if the song is not in the library.
    pub fn load(&self, song_id: &str) -> Result<Option<SongMeta>> {
        let path = self.manifest_path(song_id);
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Error::io(&path, e)),
        };
        let mut meta: SongMeta = serde_json::from_str(&text).map_err(|e| Error::Json {
            path: path.display().to_string(),
            msg: e.to_string(),
        })?;
        meta.migrate();
        Ok(Some(meta))
    }

    /// Writes `song.json` atomically (temp file + rename) and bumps
    /// `updated_ms`.
    pub fn save(&self, meta: &mut SongMeta) -> Result<()> {
        if meta.song_id.is_empty() || meta.song_id.contains(['/', '\\', '.']) {
            return Err(Error::Input(format!("bad song id {:?}", meta.song_id)));
        }
        meta.updated_ms = now_ms();
        let dir = self.song_dir(&meta.song_id);
        fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        let path = self.manifest_path(&meta.song_id);
        let tmp = dir.join("song.json.tmp");
        let text = serde_json::to_string_pretty(meta).map_err(|e| Error::Json {
            path: path.display().to_string(),
            msg: e.to_string(),
        })?;
        fs::write(&tmp, text).map_err(|e| Error::io(&tmp, e))?;
        fs::rename(&tmp, &path).map_err(|e| Error::io(&path, e))
    }

    /// Loads, modifies and saves a manifest. `Ok(false)` if the song is not
    /// in the library.
    pub fn update(&self, song_id: &str, f: impl FnOnce(&mut SongMeta)) -> Result<bool> {
        let Some(mut meta) = self.load(song_id)? else {
            return Ok(false);
        };
        f(&mut meta);
        self.save(&mut meta)?;
        Ok(true)
    }

    /// Songs with a readable manifest, most recently opened first.
    pub fn recent(&self, limit: usize) -> Vec<RecentEntry> {
        let Ok(rd) = fs::read_dir(self.root.join("songs")) else {
            return Vec::new();
        };
        let mut out: Vec<RecentEntry> = rd
            .filter_map(|e| e.ok())
            .filter_map(|e| self.load(&e.file_name().to_string_lossy()).ok().flatten())
            .map(|m| RecentEntry {
                stems_cached: !m.stem_sets.is_empty(),
                song_id: m.song_id,
                path: m.source_path,
                title: m.title,
                artist: m.artist,
                duration_sec: m.duration_sec,
                last_opened_ms: m.last_opened_ms,
            })
            .collect();
        out.sort_by_key(|e| std::cmp::Reverse(e.last_opened_ms));
        out.truncate(limit);
        out
    }

    /// Writes stereo stems to the cache folder `dir` (relative to the song
    /// folder, see [`stem_set_dir`]) and returns their [`StemFile`] entries
    /// (in the given order). Does not touch `song.json`; add the set with
    /// [`SongMeta::put_stem_set`] and [`save`](Self::save) afterwards, so a
    /// half-written cache is never referenced.
    pub fn write_stems(
        &self,
        song_id: &str,
        dir: &str,
        sample_rate: u32,
        stems: &[(&str, [&[f32]; 2])],
    ) -> Result<Vec<StemFile>> {
        let folder = self.stem_path(song_id, dir, "x");
        let folder = folder.parent().unwrap_or(&folder).to_path_buf();
        fs::create_dir_all(&folder).map_err(|e| Error::io(&folder, e))?;
        let results: Vec<Result<StemFile>> = std::thread::scope(|s| {
            let handles: Vec<_> = stems
                .iter()
                .map(|(name, ch)| {
                    let path = self.stem_path(song_id, dir, name);
                    s.spawn(move || {
                        let scale = write_stem_wav(&path, sample_rate, *ch)?;
                        Ok(StemFile {
                            name: name.to_string(),
                            scale,
                        })
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err(Error::Input("stem writer panicked".into())))
                })
                .collect()
        });
        results.into_iter().collect()
    }

    /// Reads all cached stems of `info` (in parallel), as `(name, [L, R])`.
    pub fn read_stems(
        &self,
        song_id: &str,
        info: &StemsInfo,
    ) -> Result<Vec<(String, [Vec<f32>; 2])>> {
        let results: Vec<Result<(String, [Vec<f32>; 2])>> = std::thread::scope(|s| {
            let handles: Vec<_> = info
                .stems
                .iter()
                .map(|sf| {
                    let path = self.stem_path(song_id, &info.dir, &sf.name);
                    s.spawn(move || {
                        let (rate, ch) = read_stem_wav(&path, sf.scale)?;
                        if rate != info.sample_rate {
                            return Err(Error::Wav {
                                path: path.display().to_string(),
                                msg: format!("sample rate {rate}, expected {}", info.sample_rate),
                            });
                        }
                        Ok((sf.name.clone(), ch))
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err(Error::Input("stem reader panicked".into())))
                })
                .collect()
        });
        results.into_iter().collect()
    }

    /// True if every stem file of `info` exists.
    pub fn stems_present(&self, song_id: &str, info: &StemsInfo) -> bool {
        info.stems
            .iter()
            .all(|s| self.stem_path(song_id, &info.dir, &s.name).is_file())
    }

    /// Writes the cached copy of the original mix (`original.wav`, same
    /// 16-bit + scale format as the stems). Set `meta.original` and save
    /// afterwards.
    pub fn write_original(
        &self,
        song_id: &str,
        sample_rate: u32,
        ch: [&[f32]; 2],
    ) -> Result<OriginalInfo> {
        let dir = self.song_dir(song_id);
        fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        let scale = write_stem_wav(&self.original_path(song_id), sample_rate, ch)?;
        Ok(OriginalInfo {
            sample_rate,
            len: ch[0].len(),
            scale,
        })
    }

    /// Reads the cached original, `None` if the file is missing.
    pub fn read_original(
        &self,
        song_id: &str,
        info: &OriginalInfo,
    ) -> Result<Option<[Vec<f32>; 2]>> {
        let path = self.original_path(song_id);
        if !path.is_file() {
            return Ok(None);
        }
        let (rate, ch) = read_stem_wav(&path, info.scale)?;
        if rate != info.sample_rate {
            return Err(Error::Wav {
                path: path.display().to_string(),
                msg: format!("sample rate {rate}, expected {}", info.sample_rate),
            });
        }
        Ok(Some(ch))
    }

    /// Removes a song folder entirely.
    pub fn remove(&self, song_id: &str) -> Result<()> {
        let dir = self.song_dir(song_id);
        match fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::io(&dir, e)),
        }
    }
}

// ------------------------------------------------------------------ WAV

/// Writes 16-bit stereo PCM; returns the scale (>= 1) the samples were
/// divided by. Written to a temp file and renamed into place.
fn write_stem_wav(path: &Path, sample_rate: u32, ch: [&[f32]; 2]) -> Result<f32> {
    let len = ch[0].len();
    if ch[1].len() != len {
        return Err(Error::Input("stem channels differ in length".into()));
    }
    let peak = ch.iter().flat_map(|c| c.iter()).fold(0.0f32, |m, &v| {
        if v.is_finite() {
            m.max(v.abs())
        } else {
            m
        }
    });
    let scale = peak.max(1.0);
    let inv = 32767.0 / scale;
    let data_bytes = len as u64 * 4;
    if data_bytes > u32::MAX as u64 - 36 {
        return Err(Error::Input("stem too long for WAV".into()));
    }
    let mut buf = Vec::with_capacity(44 + data_bytes as usize);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_bytes as u32).to_le_bytes());
    buf.extend_from_slice(b"WAVEfmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
    buf.extend_from_slice(&2u16.to_le_bytes()); // channels
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&(sample_rate * 4).to_le_bytes());
    buf.extend_from_slice(&4u16.to_le_bytes()); // block align
    buf.extend_from_slice(&16u16.to_le_bytes()); // bits
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&(data_bytes as u32).to_le_bytes());
    for i in 0..len {
        for c in ch {
            let v = c[i];
            let v = if v.is_finite() { v } else { 0.0 };
            let q = (v * inv).round().clamp(-32768.0, 32767.0) as i16;
            buf.extend_from_slice(&q.to_le_bytes());
        }
    }
    let tmp = path.with_extension("wav.tmp");
    let mut f = fs::File::create(&tmp).map_err(|e| Error::io(&tmp, e))?;
    f.write_all(&buf)
        .and_then(|_| f.sync_all())
        .map_err(|e| Error::io(&tmp, e))?;
    drop(f);
    fs::rename(&tmp, path).map_err(|e| Error::io(path, e))?;
    Ok(scale)
}

/// Reads a 16-bit stereo PCM WAV written by [`write_stem_wav`] (any chunk
/// order), multiplying by `scale`.
fn read_stem_wav(path: &Path, scale: f32) -> Result<(u32, [Vec<f32>; 2])> {
    let bad = |msg: &str| Error::Wav {
        path: path.display().to_string(),
        msg: msg.to_string(),
    };
    let bytes = fs::read(path).map_err(|e| Error::io(path, e))?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(bad("not a RIFF/WAVE file"));
    }
    let mut pos = 12;
    let mut rate = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        let body = pos + 8;
        let end = body.saturating_add(size).min(bytes.len());
        if id == b"fmt " {
            if end - body < 16 {
                return Err(bad("short fmt chunk"));
            }
            let f = &bytes[body..end];
            let fmt = u16::from_le_bytes([f[0], f[1]]);
            let nch = u16::from_le_bytes([f[2], f[3]]);
            let bits = u16::from_le_bytes([f[14], f[15]]);
            if fmt != 1 || nch != 2 || bits != 16 {
                return Err(bad("expected 16-bit stereo PCM"));
            }
            rate = Some(u32::from_le_bytes([f[4], f[5], f[6], f[7]]));
        } else if id == b"data" {
            let rate = rate.ok_or_else(|| bad("data before fmt"))?;
            let data = &bytes[body..end];
            let n = data.len() / 4;
            let k = scale / 32767.0;
            let mut l = Vec::with_capacity(n);
            let mut r = Vec::with_capacity(n);
            for fr in data.as_chunks::<4>().0 {
                l.push(i16::from_le_bytes([fr[0], fr[1]]) as f32 * k);
                r.push(i16::from_le_bytes([fr[2], fr[3]]) as f32 * k);
            }
            return Ok((rate, [l, r]));
        }
        pos = body + size + (size & 1);
    }
    Err(bad("no data chunk"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("stemx-lib-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn meta(lib_id: &str, opened: u64) -> SongMeta {
        let mut m = SongMeta::new(
            lib_id,
            Path::new("C:/music/My Song.mp3"),
            Tags::default(),
            12.5,
        );
        m.last_opened_ms = opened;
        m
    }

    #[test]
    fn metadata_is_set() {
        assert_eq!(NAME, "stemx-library");
        assert!(!VERSION.is_empty());
    }

    #[test]
    fn song_id_depends_on_content_not_path() {
        let d = tmpdir("id");
        let a = d.join("a.bin");
        let b = d.join("b.bin");
        let c = d.join("c.bin");
        let big: Vec<u8> = (0..(9 * 1024 * 1024)).map(|i| (i % 251) as u8).collect();
        fs::write(&a, &big).unwrap();
        fs::write(&b, &big).unwrap();
        let mut changed = big.clone();
        *changed.last_mut().unwrap() ^= 1;
        fs::write(&c, &changed).unwrap();
        let ia = song_id_for(&a).unwrap();
        assert_eq!(ia.len(), 32);
        assert_eq!(ia, song_id_for(&b).unwrap());
        assert_ne!(ia, song_id_for(&c).unwrap());
        // Small file path.
        let s = d.join("small.bin");
        fs::write(&s, b"hello").unwrap();
        assert_eq!(song_id_for(&s).unwrap().len(), 32);
        assert!(song_id_for(&d.join("missing")).is_err());
    }

    #[test]
    fn manifest_roundtrip_and_update() {
        let lib = Library::open(&tmpdir("manifest")).unwrap();
        assert!(lib.load("abc").unwrap().is_none());
        let mut m = meta("abc", 5);
        assert_eq!(m.title, "My Song");
        m.settings = Some(serde_json::json!({"version": 1, "loop": {"enabled": false}}));
        m.beats = Some(BeatData {
            beats: vec![0.5, 1.0, 1.5],
            downbeats: vec![0.5],
            bpm: Some(120.0),
            beats_per_bar: Some(4),
        });
        lib.save(&mut m).unwrap();
        let back = lib.load("abc").unwrap().unwrap();
        assert_eq!(back, m);
        assert!(lib.update("abc", |m| m.title = "Other".into()).unwrap());
        assert_eq!(lib.load("abc").unwrap().unwrap().title, "Other");
        assert!(!lib.update("nope", |_| {}).unwrap());
        let mut bad = meta("../x", 1);
        assert!(lib.save(&mut bad).is_err());
    }

    #[test]
    fn recent_sorted_and_limited() {
        let lib = Library::open(&tmpdir("recent")).unwrap();
        for (id, t) in [("a1", 10), ("b2", 30), ("c3", 20)] {
            lib.save(&mut meta(id, t)).unwrap();
        }
        // A junk folder is ignored.
        fs::create_dir_all(lib.song_dir("junk")).unwrap();
        fs::write(lib.song_dir("junk").join("song.json"), "{").unwrap();
        let r = lib.recent(2);
        assert_eq!(
            r.iter().map(|e| e.song_id.as_str()).collect::<Vec<_>>(),
            ["b2", "c3"]
        );
        assert!(!r[0].stems_cached);
    }

    #[test]
    fn stems_roundtrip_with_scale() {
        let lib = Library::open(&tmpdir("stems")).unwrap();
        let n = 1000;
        let quiet: Vec<f32> = (0..n).map(|i| ((i as f32) * 0.01).sin() * 0.5).collect();
        let loud: Vec<f32> = (0..n).map(|i| ((i as f32) * 0.02).sin() * 1.8).collect();
        let dir = stem_set_dir("htdemucs_6s", 2);
        assert_eq!(dir, "stems/htdemucs_6s_s2");
        let files = lib
            .write_stems(
                "s1",
                &dir,
                44100,
                &[("vocals", [&quiet, &loud]), ("bass", [&loud, &quiet])],
            )
            .unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].name, "vocals");
        assert!((files[0].scale - 1.8).abs() < 1e-3);
        let info = StemsInfo {
            model_id: "htdemucs_6s".into(),
            model_name: "Demucs 6-stem".into(),
            device_label: "CPU".into(),
            sample_rate: 44100,
            len: n,
            stems: files,
            shifts: 2,
            dir: dir.clone(),
        };
        assert!(lib.stems_present("s1", &info));
        let back = lib.read_stems("s1", &info).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[1].0, "bass");
        let err = |a: &[f32], b: &[f32]| {
            a.iter()
                .zip(b)
                .map(|(x, y)| (x - y).abs())
                .fold(0.0f32, f32::max)
        };
        assert_eq!(back[0].1[0].len(), n);
        assert!(err(&back[0].1[0], &quiet) < 2.0 / 32767.0 * 1.8);
        assert!(err(&back[0].1[1], &loud) < 2.0 / 32767.0 * 1.8);
        assert!(err(&back[1].1[0], &loud) < 2.0 / 32767.0 * 1.8);
        // Missing file -> not present, read fails.
        fs::remove_file(lib.stem_path("s1", &dir, "bass")).unwrap();
        assert!(!lib.stems_present("s1", &info));
        assert!(lib.read_stems("s1", &info).is_err());
        lib.remove("s1").unwrap();
        assert!(!lib.song_dir("s1").exists());
    }

    #[test]
    fn stem_sets_keyed_by_model_and_shifts() {
        let lib = Library::open(&tmpdir("sets")).unwrap();
        let mut m = meta("k1", 1);
        let set = |model: &str, shifts: u32| StemsInfo {
            model_id: model.into(),
            model_name: model.into(),
            device_label: "GPU".into(),
            sample_rate: 44100,
            len: 10,
            stems: vec![],
            shifts,
            dir: stem_set_dir(model, shifts),
        };
        m.put_stem_set(set("htdemucs_6s", 2));
        m.put_stem_set(set("htdemucs_6s", 1));
        m.put_stem_set(set("bs-roformer-sw", 1));
        let mut again = set("htdemucs_6s", 2);
        again.device_label = "CPU".into();
        m.put_stem_set(again);
        assert_eq!(m.stem_sets.len(), 3);
        // Replaced entry moved to the end (most recent).
        assert_eq!(m.stem_sets.last().unwrap().device_label, "CPU");
        assert!(m.stem_set("htdemucs_6s", 1).is_some());
        assert!(m.stem_set("bs-roformer-sw", 2).is_none());
        assert_ne!(
            m.stem_set("htdemucs_6s", 1).unwrap().dir,
            m.stem_set("htdemucs_6s", 2).unwrap().dir
        );
        lib.save(&mut m).unwrap();
        assert_eq!(lib.load("k1").unwrap().unwrap().stem_sets.len(), 3);
        assert!(lib.recent(5)[0].stems_cached);
    }

    #[test]
    fn schema1_stems_migrate_to_stem_sets() {
        let lib = Library::open(&tmpdir("migrate")).unwrap();
        fs::create_dir_all(lib.song_dir("old1")).unwrap();
        let v1 = serde_json::json!({
            "schema": 1, "song_id": "old1", "source_path": "C:/a.mp3",
            "title": "A", "artist": null, "duration_sec": 1.0,
            "sample_rate": 44100, "channels": 2,
            "stems": {"model_id": "htdemucs_6s", "model_name": "Demucs 6-stem",
                      "device_label": "GPU", "sample_rate": 44100, "len": 5,
                      "stems": [{"name": "bass", "scale": 1.0}]},
            "created_ms": 1, "last_opened_ms": 1, "updated_ms": 1
        });
        fs::write(lib.manifest_path("old1"), v1.to_string()).unwrap();
        let m = lib.load("old1").unwrap().unwrap();
        assert!(m.stems.is_none());
        assert_eq!(m.schema, SCHEMA);
        let s = m.stem_set("htdemucs_6s", 1).unwrap();
        assert_eq!(s.dir, "stems");
        assert_eq!(
            lib.stem_path("old1", &s.dir, "bass"),
            lib.song_dir("old1").join("stems").join("bass.wav")
        );
        assert!(m.original.is_none());
        // Saving writes the new layout only.
        let mut m = m;
        lib.save(&mut m).unwrap();
        let text = fs::read_to_string(lib.manifest_path("old1")).unwrap();
        assert!(text.contains("stem_sets") && !text.contains("\"stems\": {"));
    }

    #[test]
    fn original_roundtrip() {
        let lib = Library::open(&tmpdir("orig")).unwrap();
        let a: Vec<f32> = (0..500).map(|i| (i as f32 * 0.1).sin() * 1.2).collect();
        let b: Vec<f32> = a.iter().map(|x| -x * 0.5).collect();
        let info = lib.write_original("o1", 44100, [&a, &b]).unwrap();
        assert_eq!(info.len, 500);
        assert!((info.scale - 1.2).abs() < 1e-3);
        let back = lib.read_original("o1", &info).unwrap().unwrap();
        assert!(back[0]
            .iter()
            .zip(&a)
            .all(|(x, y)| (x - y).abs() < 2.0 * 1.2 / 32767.0));
        fs::remove_file(lib.original_path("o1")).unwrap();
        assert!(lib.read_original("o1", &info).unwrap().is_none());
    }

    #[test]
    fn tags_fallback_for_non_audio() {
        let d = tmpdir("tags");
        let p = d.join("x.mp3");
        fs::write(&p, b"not audio").unwrap();
        assert_eq!(read_tags(&p), Tags::default());
        assert_eq!(file_title(&p), "x");
    }
}
