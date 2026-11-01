//! App state and jobs, independent of Tauri (events go through a [`Sink`]),
//! so the same code runs in the app, the debug autotest and unit tests.
//!
//! Threads:
//! - `open_song` runs on a blocking worker (decode + resample, or reading
//!   the cached stems).
//! - separation: one background thread per run, cancelled through an
//!   `AtomicBool`. The loaded `Separator` is kept between runs.
//! - beat tracking: one background thread per song (Beat This! on ORT CPU).
//! - export: one background thread per export.
//! - position: a 30 Hz emitter thread.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::Serialize;
use stemx_library::{BeatData, Library, SongMeta, StemsInfo};
use stemx_separation::selftest::Cache;
use stemx_separation::{
    BeatModel, DevicePref, Ep, LoadOptions, ModelEntry, ModelRegistry, ModelSource, Separator,
};

use crate::audio::{self, ClickConfig, Engine, Track};
use crate::log;
use crate::types::*;

/// Event sink: `(event name, JSON payload)`.
pub type Sink = Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>;

/// Sample rate everything is played and cached at (the model rate).
pub const RATE: u32 = stemx_io::MODEL_SAMPLE_RATE;
const ORIGINAL: &str = "original";
const RECENT_MAX: usize = 20;
const NO_DEVICE: &str =
    "No audio output device found. Connect speakers or headphones and restart Stem Extractor (separation and export still work).";

pub struct Config {
    pub data_dir: PathBuf,
    pub model_dirs: Vec<(PathBuf, ModelSource)>,
    pub beat_model: Option<PathBuf>,
    pub device: DevicePref,
    /// Beat model device (CPU by default: no self-test for that model).
    pub beat_device: DevicePref,
}

impl Config {
    pub fn cache_dir(&self) -> PathBuf {
        self.data_dir.join("cache")
    }
}

struct Song {
    meta: SongMeta,
    /// Original mix at [`RATE`] (or the sum of the cached stems when the
    /// song was opened from cache), for the waveform and beat tracking.
    mix: Arc<[Vec<f32>; 2]>,
    original: Option<Track>,
    stems: Option<Vec<Track>>,
    beats: Option<BeatData>,
}

impl Song {
    fn tracks(&self) -> Vec<Track> {
        match (&self.stems, &self.original) {
            (Some(s), _) => s.clone(),
            (None, Some(o)) => vec![o.clone()],
            (None, None) => vec![mix_track(&self.mix)],
        }
    }

    fn separated_with(&self) -> Option<SeparatedWith> {
        self.stems.as_ref()?;
        self.meta.stems.as_ref().map(|s| SeparatedWith {
            model_name: s.model_name.clone(),
            device_label: s.device_label.clone(),
        })
    }

    fn info(&self) -> SongInfo {
        let m = &self.meta;
        SongInfo {
            song_id: m.song_id.clone(),
            path: m.source_path.clone(),
            title: m.title.clone(),
            artist: m.artist.clone(),
            duration_sec: m.duration_sec,
            sample_rate: m.sample_rate,
            channels: m.channels,
            stems_cached: self.stems.is_some(),
            separated_with: self.separated_with(),
            beats_ready: self.beats.is_some(),
            bpm: self.beats.as_ref().and_then(|b| b.bpm),
            settings: m.settings.clone(),
        }
    }
}

struct Job {
    song_id: String,
    cancel: Arc<AtomicBool>,
}

struct State {
    song: Option<Song>,
    /// Bumped on every open/close; background results for an older
    /// generation are dropped.
    generation: u64,
    mix: Vec<StemMixState>,
    speed_percent: u32,
    semitones: i32,
    cents: i32,
    loop_state: LoopState,
    click: ClickState,
    /// Effective downbeats from the UI (`set_meter_overrides`).
    downbeats: Option<Vec<f64>>,
    separation: Option<Job>,
    export: Option<Arc<AtomicBool>>,
    last_separation_error: Option<String>,
}

pub struct Backend {
    cfg: Config,
    library: Library,
    engine: Option<Arc<Engine>>,
    sink: Sink,
    registry: ModelRegistry,
    separator: Mutex<Option<Separator>>,
    beat_model: Mutex<Option<BeatModel>>,
    st: Mutex<State>,
    open_lock: Mutex<()>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn mix_track(mix: &Arc<[Vec<f32>; 2]>) -> Track {
    Track {
        name: ORIGINAL.into(),
        channels: [Arc::new(mix[0].clone()), Arc::new(mix[1].clone())],
        sample_rate: RATE,
    }
}

fn device_label(ep: Ep) -> &'static str {
    if ep.is_gpu() {
        "GPU"
    } else {
        "CPU"
    }
}

/// Engine gains for `names` under the audibility rule: if any stem is
/// soloed only soloed stems play, else stems that are on.
pub fn gains_for(mix: &[StemMixState], names: &[String]) -> Vec<(String, f32)> {
    let any_solo = mix.iter().any(|m| m.solo);
    names
        .iter()
        .map(|n| {
            if n == ORIGINAL {
                return (n.clone(), 1.0);
            }
            let g = match mix.iter().find(|m| &m.stem == n) {
                Some(m) => {
                    let audible = if any_solo { m.solo } else { m.on };
                    if audible && m.volume.is_finite() {
                        m.volume.clamp(0.0, 2.0)
                    } else {
                        0.0
                    }
                }
                None if any_solo => 0.0,
                None => 1.0,
            };
            (n.clone(), g)
        })
        .collect()
}

/// Snaps UI downbeats to grid beats within 1 ms so they compare equal.
pub fn snap_downbeats(downbeats: &[f64], beats: &[f64]) -> Vec<f64> {
    let mut out: Vec<f64> = downbeats
        .iter()
        .filter(|d| d.is_finite())
        .map(|&d| {
            let i = beats.partition_point(|&b| b < d);
            [i.checked_sub(1), Some(i)]
                .into_iter()
                .flatten()
                .filter_map(|j| beats.get(j))
                .copied()
                .find(|b| (b - d).abs() <= 0.001)
                .unwrap_or(d)
        })
        .collect();
    out.sort_by(f64::total_cmp);
    out.dedup();
    out
}

/// Peak envelope: `n` equal slices, max |sample| over both channels.
pub fn peaks(mix: &[Vec<f32>; 2], n: usize) -> Vec<f32> {
    let len = mix[0].len().min(mix[1].len());
    let n = n.max(1);
    (0..n)
        .map(|i| {
            let a = i * len / n;
            let b = ((i + 1) * len / n).max(a + 1).min(len);
            if a >= len {
                return 0.0;
            }
            let mut m = 0.0f32;
            for c in mix {
                for &v in &c[a..b] {
                    let v = v.abs();
                    if v > m {
                        m = v;
                    }
                }
            }
            m.min(1.0)
        })
        .collect()
}

/// Rate-limits progress events and keeps the fraction monotonic.
struct Throttle {
    last: Option<Instant>,
    max: f32,
}

impl Throttle {
    fn new() -> Self {
        Self {
            last: None,
            max: 0.0,
        }
    }
    /// Returns the fraction to emit, or `None` to skip.
    fn next(&mut self, f: f32, force: bool) -> Option<f32> {
        let f = f.clamp(0.0, 1.0).max(self.max);
        self.max = f;
        let due = self
            .last
            .is_none_or(|t| t.elapsed() >= Duration::from_millis(100));
        if force || due {
            self.last = Some(Instant::now());
            Some(f)
        } else {
            None
        }
    }
}

#[derive(Debug)]
struct BeatErr(String);
impl From<stemx_beats::Error> for BeatErr {
    fn from(e: stemx_beats::Error) -> Self {
        Self(e.to_string())
    }
}

impl Backend {
    pub fn new(cfg: Config, engine: Option<Engine>, sink: Sink) -> Result<Arc<Self>, String> {
        let library = Library::open(&cfg.data_dir.join("library")).map_err(|e| e.to_string())?;
        let registry = ModelRegistry::scan(&cfg.model_dirs);
        log!("data dir: {}", cfg.data_dir.display());
        for (d, s) in &cfg.model_dirs {
            log!("model dir ({}): {}", s.as_str(), d.display());
        }
        for m in &registry.models {
            log!(
                "model found: {} \"{}\" [{}] in {}",
                m.id(),
                m.manifest.name,
                m.source.as_str(),
                m.dir.display()
            );
        }
        for p in &registry.problems {
            log!("model problem: {}: {}", p.path.display(), p.message);
        }
        if registry.models.is_empty() {
            log!("WARNING: no separation model found");
        }
        match &cfg.beat_model {
            Some(p) => log!("beat model: {}", p.display()),
            None => log!(
                "WARNING: beat model {} not found",
                crate::models::BEAT_MODEL_FILE
            ),
        }
        let engine = engine.map(Arc::new);
        let b = Arc::new(Self {
            cfg,
            library,
            engine,
            sink,
            registry,
            separator: Mutex::new(None),
            beat_model: Mutex::new(None),
            st: Mutex::new(State {
                song: None,
                generation: 0,
                mix: STEMS
                    .iter()
                    .map(|s| StemMixState {
                        stem: s.to_string(),
                        on: true,
                        volume: 1.0,
                        solo: false,
                    })
                    .collect(),
                speed_percent: 100,
                semitones: 0,
                cents: 0,
                loop_state: LoopState::default(),
                click: ClickState::default(),
                downbeats: None,
                separation: None,
                export: None,
                last_separation_error: None,
            }),
            open_lock: Mutex::new(()),
        });
        if let Some(e) = &b.engine {
            let weak = Arc::downgrade(&b);
            e.set_on_end(Box::new(move || {
                if let Some(b) = weak.upgrade() {
                    b.emit_position();
                }
            }));
            let weak = Arc::downgrade(&b);
            e.set_on_loop_wrap(Box::new(move || {
                if let Some(b) = weak.upgrade() {
                    b.emit_position();
                }
            }));
        }
        Ok(b)
    }

    fn emit<T: Serialize>(&self, name: &str, payload: &T) {
        match serde_json::to_value(payload) {
            Ok(v) => (self.sink)(name, v),
            Err(e) => log!("cannot serialize {name}: {e}"),
        }
    }

    pub fn has_engine(&self) -> bool {
        self.engine.is_some()
    }

    /// Starts the position emitter and preloads the separation model (runs
    /// or reuses the device self-test, so the first separation starts
    /// fast).
    pub fn start_background(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        spawn("stemx-position", move || {
            let mut was_active = false;
            loop {
                std::thread::sleep(Duration::from_millis(33));
                let Some(b) = weak.upgrade() else { break };
                let Some(e) = &b.engine else { break };
                if lock(&b.st).song.is_none() {
                    was_active = false;
                    continue;
                }
                let p = e.position();
                let active = p.playing || p.count_in_beats_left > 0;
                if active || was_active {
                    b.emit_pos(p);
                }
                was_active = active;
            }
        });
        if std::env::var_os("STEMX_NO_PRELOAD").is_none() {
            let b = self.clone();
            spawn("stemx-preload", move || {
                let Some(entry) = crate::models::pick_model(&b.registry).cloned() else {
                    return;
                };
                let mut sep = lock(&b.separator);
                if sep.is_none() {
                    match b.load_separator(&entry) {
                        Ok(s) => *sep = Some(s),
                        Err(e) => log!("preload failed: {e}"),
                    }
                }
            });
        }
    }

    fn load_separator(&self, entry: &ModelEntry) -> Result<Separator, String> {
        let t0 = Instant::now();
        log!(
            "loading model {} ({:?}, ONNX Runtime {})",
            entry.id(),
            self.cfg.device,
            stemx_separation::runtime::ort_version()
        );
        let opts = LoadOptions {
            device: self.cfg.device,
            cache_dir: Some(self.cfg.cache_dir()),
            ..Default::default()
        };
        let sep =
            Separator::load(entry, opts, &mut |m| log!("  {m}")).map_err(|e| e.to_string())?;
        for r in sep.rejected() {
            log!("  rejected {}: {}", r.ep.label(), r.reason);
        }
        log!(
            "model {} ready on {} in {:.1}s, self-test {}",
            entry.id(),
            sep.ep().label(),
            t0.elapsed().as_secs_f64(),
            sep.selftest_status()
        );
        Ok(sep)
    }

    /// Best guess of the separation device before loading: the loaded
    /// separator's, else GPU unless the cached self-test failed.
    fn predict_device(&self, entry: &ModelEntry) -> &'static str {
        if let Ok(g) = self.separator.try_lock() {
            if let Some(s) = g.as_ref().filter(|s| s.manifest().id == entry.id()) {
                return device_label(s.ep());
            }
        }
        if self.cfg.device == DevicePref::Cpu {
            return "CPU";
        }
        let Some(ep) = Ep::gpu_candidates().into_iter().next() else {
            return "CPU";
        };
        let cache = Cache::open(Some(&self.cfg.cache_dir()));
        let key = Cache::selftest_key(
            &entry.manifest.sha256,
            ep.key(),
            &stemx_separation::runtime::ort_version(),
            "",
        );
        match cache.get_selftest(&key) {
            Some(c) if !c.passed => "CPU",
            _ => "GPU",
        }
    }

    // ------------------------------------------------------------ engine

    fn emit_pos(&self, p: audio::Position) {
        self.emit(
            "playback-position",
            &PlaybackPosition {
                position_sec: p.seconds,
                playing: p.playing,
                count_in_beats_left: p.count_in_beats_left,
            },
        );
    }

    pub fn emit_position(&self) {
        if let Some(e) = &self.engine {
            self.emit_pos(e.position());
        }
    }

    fn apply_gains(&self, st: &State) {
        let (Some(e), Some(song)) = (&self.engine, &st.song) else {
            return;
        };
        let names: Vec<String> = song.tracks().iter().map(|t| t.name.clone()).collect();
        e.set_gains(gains_for(&st.mix, &names));
    }

    fn click_config(st: &State) -> ClickConfig {
        let c = &st.click;
        let grid = st.song.as_ref().and_then(|s| s.beats.as_ref());
        let beats = grid.map(|g| g.beats.clone()).unwrap_or_default();
        let downbeats = match (&st.downbeats, grid) {
            (Some(d), _) => snap_downbeats(d, &beats),
            (None, Some(g)) => g.downbeats.clone(),
            (None, None) => Vec::new(),
        };
        ClickConfig {
            enabled: c.enabled,
            beats,
            downbeats,
            subdivision: c.subdivision.clamp(1, 3),
            count_in_beats: if c.count_in_enabled {
                c.count_in_beats.clamp(1, 64)
            } else {
                0
            },
            beats_per_bar: c.beats_per_bar.clamp(1, 64),
            volume: if c.volume.is_finite() {
                c.volume.clamp(0.0, 1.0)
            } else {
                0.8
            },
        }
    }

    fn apply_click(&self, st: &State) {
        if let Some(e) = &self.engine {
            e.set_click(Self::click_config(st));
        }
    }

    fn apply_all(&self, st: &State) {
        let Some(e) = &self.engine else { return };
        self.apply_gains(st);
        e.set_speed(st.speed_percent as f64 / 100.0);
        e.set_pitch(st.semitones as f64 + st.cents as f64 / 100.0);
        e.set_loop(st.loop_state.range());
        self.apply_click(st);
    }

    // ------------------------------------------------------------ songs

    fn cancel_separation_locked(&self, st: &mut State) {
        if let Some(job) = st.separation.take() {
            job.cancel.store(true, Ordering::Relaxed);
            log!("separation of {} cancelled", job.song_id);
            self.emit(
                "separation-error",
                &SeparationError {
                    song_id: job.song_id,
                    message: "Cancelled".into(),
                    cancelled: true,
                },
            );
        }
    }

    pub fn open_song(self: &Arc<Self>, path: &str) -> Result<SongInfo, String> {
        let _serial = lock(&self.open_lock);
        let t0 = Instant::now();
        let path_buf = PathBuf::from(path);
        if !path_buf.is_file() {
            return Err(format!("File not found: {path}"));
        }
        // Stop what was going on.
        {
            let mut st = lock(&self.st);
            self.cancel_separation_locked(&mut st);
            st.generation += 1;
            if let Some(e) = &self.engine {
                e.pause();
            }
        }
        let song_id = stemx_library::song_id_for(&path_buf).map_err(|e| e.to_string())?;
        let mut meta = match self.library.load(&song_id) {
            Ok(m) => m,
            Err(e) => {
                log!("ignoring unreadable manifest for {song_id}: {e}");
                None
            }
        };

        // Cached stems?
        let mut stems: Option<Vec<Track>> = None;
        let mut mix: Option<[Vec<f32>; 2]> = None;
        if let Some(info) = meta.as_ref().and_then(|m| m.stems.clone()) {
            match self.read_cached_stems(&song_id, &info) {
                Ok((tracks, sum)) => {
                    stems = Some(tracks);
                    mix = Some(sum);
                }
                Err(e) => log!("stem cache for {song_id} unusable ({e}); will re-separate"),
            }
        }
        let mut original = None;
        let (native_rate, native_channels) = match (&mix, &meta) {
            (Some(_), Some(m)) => (m.sample_rate, m.channels),
            _ => {
                let td = Instant::now();
                let audio = stemx_io::decode_file(&path_buf).map_err(|e| e.to_string())?;
                if audio.is_empty() {
                    return Err(format!("No audio in {path}"));
                }
                let (rate, nch) = (audio.sample_rate, audio.channels.len() as u32);
                let audio = stemx_io::resample(&audio, RATE).map_err(|e| e.to_string())?;
                let mut ch = audio.channels.into_iter();
                let l = ch.next().unwrap_or_default();
                let r = ch.next().unwrap_or_else(|| l.clone());
                log!(
                    "decoded {} ({rate} Hz, {nch} ch, {:.1}s) in {:.2}s",
                    path_buf.display(),
                    l.len() as f64 / RATE as f64,
                    td.elapsed().as_secs_f64()
                );
                mix = Some([l, r]);
                (rate, nch)
            }
        };
        let mix = Arc::new(mix.unwrap_or_default());
        if stems.is_none() {
            original = Some(mix_track(&mix));
        }
        let duration = mix[0].len() as f64 / RATE as f64;

        let mut m = match meta.take() {
            Some(m) => m,
            None => SongMeta::new(
                &song_id,
                &path_buf,
                stemx_library::read_tags(&path_buf),
                duration,
            ),
        };
        m.source_path = path.to_string();
        m.duration_sec = duration;
        m.sample_rate = native_rate;
        m.channels = native_channels;
        m.last_opened_ms = stemx_library::now_ms();
        if stems.is_none() {
            m.stems = None;
        }
        if let Err(e) = self.library.save(&mut m) {
            log!("cannot save library entry: {e}");
        }
        let beats = m.beats.clone();
        let need_beats = beats.is_none();
        let song = Song {
            meta: m,
            mix: mix.clone(),
            original,
            stems,
            beats,
        };
        let info = song.info();
        let generation = {
            let mut st = lock(&self.st);
            if let Some(e) = &self.engine {
                e.pause();
                e.load(song.tracks());
                e.seek(0.0);
            }
            st.song = Some(song);
            st.loop_state = LoopState::default();
            st.downbeats = None;
            st.last_separation_error = None;
            self.apply_all(&st);
            st.generation
        };
        self.emit_position();
        log!(
            "opened \"{}\" ({song_id}) in {:.2}s: stems {}, beats {}",
            info.title,
            t0.elapsed().as_secs_f64(),
            if info.stems_cached {
                "cached"
            } else {
                "not separated"
            },
            if info.beats_ready {
                "cached"
            } else {
                "pending"
            },
        );
        if need_beats {
            self.spawn_beats(song_id, mix, generation);
        }
        Ok(info)
    }

    fn read_cached_stems(
        &self,
        song_id: &str,
        info: &StemsInfo,
    ) -> Result<(Vec<Track>, [Vec<f32>; 2]), String> {
        if info.sample_rate != RATE {
            return Err(format!("unexpected sample rate {}", info.sample_rate));
        }
        if !self.library.stems_present(song_id, info) {
            return Err("stem files missing".into());
        }
        let t0 = Instant::now();
        let data = self
            .library
            .read_stems(song_id, info)
            .map_err(|e| e.to_string())?;
        let len = data.iter().map(|(_, c)| c[0].len()).max().unwrap_or(0);
        let mut sum = [vec![0.0f32; len], vec![0.0f32; len]];
        let mut tracks = Vec::with_capacity(data.len());
        for (name, [l, r]) in data {
            for (s, x) in sum[0].iter_mut().zip(&l) {
                *s += x;
            }
            for (s, x) in sum[1].iter_mut().zip(&r) {
                *s += x;
            }
            tracks.push(Track {
                name,
                channels: [Arc::new(l), Arc::new(r)],
                sample_rate: RATE,
            });
        }
        log!(
            "read {} cached stems in {:.2}s",
            tracks.len(),
            t0.elapsed().as_secs_f64()
        );
        Ok((tracks, sum))
    }

    pub fn close_song(&self) {
        let mut st = lock(&self.st);
        self.cancel_separation_locked(&mut st);
        st.generation += 1;
        if let Some(e) = &self.engine {
            e.pause();
            e.load(Vec::new());
        }
        st.song = None;
    }

    pub fn library(&self) -> Vec<LibraryEntry> {
        self.library
            .recent(RECENT_MAX)
            .into_iter()
            .map(|e| LibraryEntry {
                song_id: e.song_id,
                path: e.path,
                title: e.title,
                artist: e.artist,
                duration_sec: e.duration_sec,
                stems_cached: e.stems_cached,
                last_opened_ms: e.last_opened_ms,
            })
            .collect()
    }

    pub fn save_song_settings(
        &self,
        song_id: &str,
        settings: serde_json::Value,
    ) -> Result<(), String> {
        {
            let mut st = lock(&self.st);
            if let Some(s) = st.song.as_mut().filter(|s| s.meta.song_id == song_id) {
                s.meta.settings = Some(settings.clone());
            }
        }
        match self
            .library
            .update(song_id, |m| m.settings = Some(settings))
        {
            Ok(true) => Ok(()),
            Ok(false) => Err(format!("Unknown song {song_id}")),
            Err(e) => Err(e.to_string()),
        }
    }

    // ------------------------------------------------------------ separation

    pub fn start_separation(self: &Arc<Self>) -> Result<SeparatedWith, String> {
        let entry = crate::models::pick_model(&self.registry)
            .cloned()
            .ok_or_else(|| {
                "No separation model found. Put a model folder in the models directory.".to_string()
            })?;
        let device = self.predict_device(&entry);
        let mut st = lock(&self.st);
        let song = st.song.as_ref().ok_or("No song open")?;
        if st.separation.is_some() {
            return Err("Separation is already running".into());
        }
        let song_id = song.meta.song_id.clone();
        let mix = song.mix.clone();
        let generation = st.generation;
        let cancel = Arc::new(AtomicBool::new(false));
        st.separation = Some(Job {
            song_id: song_id.clone(),
            cancel: cancel.clone(),
        });
        st.last_separation_error = None;
        drop(st);
        let started = SeparatedWith {
            model_name: entry.manifest.name.clone(),
            device_label: device.into(),
        };
        log!(
            "separation of {song_id} started: {} on {device} (predicted)",
            entry.id()
        );
        let b = self.clone();
        spawn("stemx-separation", move || {
            b.run_separation(entry, song_id, mix, generation, cancel);
        });
        Ok(started)
    }

    pub fn cancel_separation(&self) {
        let mut st = lock(&self.st);
        self.cancel_separation_locked(&mut st);
    }

    fn run_separation(
        self: &Arc<Self>,
        entry: ModelEntry,
        song_id: String,
        mix: Arc<[Vec<f32>; 2]>,
        generation: u64,
        cancel: Arc<AtomicBool>,
    ) {
        let t0 = Instant::now();
        let result = self.separate_and_cache(&entry, &song_id, &mix, &cancel);
        let cancelled = cancel.load(Ordering::Relaxed);
        let mut st = lock(&self.st);
        // Clear our job record (only if it is still ours).
        if st
            .separation
            .as_ref()
            .is_some_and(|j| Arc::ptr_eq(&j.cancel, &cancel))
        {
            st.separation = None;
        }
        match result {
            Ok((tracks, with)) => {
                log!(
                    "separation of {song_id} done in {:.1}s on {}",
                    t0.elapsed().as_secs_f64(),
                    with.device_label
                );
                let current = st.generation == generation
                    && st.song.as_ref().is_some_and(|s| s.meta.song_id == song_id);
                if !current || cancelled {
                    return;
                }
                if let Some(song) = st.song.as_mut() {
                    if let Ok(Some(m)) = self.library.load(&song_id) {
                        song.meta.stems = m.stems;
                    }
                    song.stems = Some(tracks);
                    song.original = None;
                    if let Some(e) = &self.engine {
                        e.load(song.tracks());
                    }
                }
                self.apply_gains(&st);
                drop(st);
                self.emit(
                    "separation-done",
                    &SeparationDone {
                        song_id,
                        separated_with: with,
                    },
                );
            }
            Err(_) if cancelled => {
                // The cancel path already emitted separation-error.
            }
            Err(msg) => {
                log!("separation of {song_id} failed: {msg}");
                st.last_separation_error = Some(msg.clone());
                drop(st);
                self.emit(
                    "separation-error",
                    &SeparationError {
                        song_id,
                        message: msg,
                        cancelled: false,
                    },
                );
            }
        }
    }

    fn separate_and_cache(
        &self,
        entry: &ModelEntry,
        song_id: &str,
        mix: &[Vec<f32>; 2],
        cancel: &AtomicBool,
    ) -> Result<(Vec<Track>, SeparatedWith), String> {
        let model_name = entry.manifest.name.clone();
        let mut device = self.predict_device(entry).to_string();
        let mut th = Throttle::new();
        let progress = |th: &mut Throttle, stage, f: f32, device: &str, force| {
            if let Some(f) = th.next(f, force) {
                self.emit(
                    "separation-progress",
                    &SeparationProgress {
                        song_id: song_id.to_string(),
                        stage,
                        fraction: f,
                        model_name: model_name.clone(),
                        device_label: device.to_string(),
                    },
                );
            }
        };
        progress(&mut th, SeparationStage::LoadingModel, 0.0, &device, true);

        let mut guard = lock(&self.separator);
        if guard.as_ref().is_none_or(|s| s.manifest().id != entry.id()) {
            *guard = None;
            *guard = Some(self.load_separator(entry)?);
        }
        let sep = guard.as_mut().ok_or("model not loaded")?;
        device = device_label(sep.ep()).to_string();
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        progress(&mut th, SeparationStage::Decoding, 0.05, &device, true);
        let ts = Instant::now();
        progress(&mut th, SeparationStage::Separating, 0.06, &device, true);
        let stems = sep
            .separate(
                &[&mix[0], &mix[1]],
                &mut |f| {
                    progress(
                        &mut th,
                        SeparationStage::Separating,
                        0.06 + 0.86 * f,
                        &device,
                        false,
                    )
                },
                cancel,
            )
            .map_err(|e| e.to_string())?;
        // GPU may have failed over to CPU mid-run.
        device = device_label(sep.ep()).to_string();
        drop(guard);
        log!(
            "separated {song_id}: {} stems, {:.1}s audio in {:.1}s on {device}",
            stems.names.len(),
            stems.len as f64 / RATE as f64,
            ts.elapsed().as_secs_f64()
        );
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        if stems.channels != 2 || stems.sample_rate != RATE {
            return Err(format!(
                "model output {} ch at {} Hz is not supported",
                stems.channels, stems.sample_rate
            ));
        }

        progress(&mut th, SeparationStage::Writing, 0.93, &device, true);
        let tw = Instant::now();
        let views: Vec<(&str, [&[f32]; 2])> = stems
            .names
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let ch = stems.stem(i);
                (n.as_str(), [ch[0], ch[1]])
            })
            .collect();
        let files = self
            .library
            .write_stems(song_id, RATE, &views)
            .map_err(|e| format!("Cannot write stems: {e}"))?;
        let info = StemsInfo {
            model_id: entry.id().to_string(),
            model_name: model_name.clone(),
            device_label: device.clone(),
            sample_rate: RATE,
            len: stems.len,
            stems: files,
        };
        self.library
            .update(song_id, |m| m.stems = Some(info))
            .map_err(|e| format!("Cannot update library: {e}"))?;
        log!("wrote stems in {:.2}s", tw.elapsed().as_secs_f64());
        let tracks = (0..stems.names.len())
            .map(|i| {
                let ch = stems.stem(i);
                Track {
                    name: stems.names[i].clone(),
                    channels: [Arc::new(ch[0].to_vec()), Arc::new(ch[1].to_vec())],
                    sample_rate: RATE,
                }
            })
            .collect();
        progress(&mut th, SeparationStage::Writing, 1.0, &device, true);
        Ok((
            tracks,
            SeparatedWith {
                model_name,
                device_label: device,
            },
        ))
    }

    pub fn separation_running(&self) -> bool {
        lock(&self.st).separation.is_some()
    }

    pub fn last_separation_error(&self) -> Option<String> {
        lock(&self.st).last_separation_error.clone()
    }

    // ------------------------------------------------------------ beats

    fn spawn_beats(self: &Arc<Self>, song_id: String, mix: Arc<[Vec<f32>; 2]>, generation: u64) {
        let Some(model_path) = self.cfg.beat_model.clone() else {
            log!("no beat model; click track and count-in use defaults");
            return;
        };
        let b = self.clone();
        spawn("stemx-beats", move || {
            let t0 = Instant::now();
            match b.track_beats(&model_path, &mix) {
                Ok(data) => {
                    log!(
                        "beats for {song_id}: {} beats, {} downbeats, {:.1} BPM, {:?}/bar in {:.1}s",
                        data.beats.len(),
                        data.downbeats.len(),
                        data.bpm.unwrap_or(0.0),
                        data.beats_per_bar,
                        t0.elapsed().as_secs_f64()
                    );
                    if let Err(e) = b.library.update(&song_id, |m| m.beats = Some(data.clone())) {
                        log!("cannot save beats: {e}");
                    }
                    let mut st = lock(&b.st);
                    let current = st.generation == generation
                        && st.song.as_ref().is_some_and(|s| s.meta.song_id == song_id);
                    if !current {
                        return;
                    }
                    if let Some(s) = st.song.as_mut() {
                        s.beats = Some(data);
                    }
                    b.apply_click(&st);
                    drop(st);
                    b.emit("beats-ready", &SongIdPayload { song_id });
                }
                Err(e) => log!("beat tracking for {song_id} failed: {e}"),
            }
        });
    }

    fn track_beats(&self, model_path: &Path, mix: &[Vec<f32>; 2]) -> Result<BeatData, String> {
        let mut guard = lock(&self.beat_model);
        if guard.is_none() {
            let t0 = Instant::now();
            let m = BeatModel::load(model_path, self.cfg.beat_device, Some(8), &mut |m| {
                log!("{m}")
            })
            .map_err(|e| e.to_string())?;
            log!(
                "beat model loaded on {} in {:.1}s",
                m.ep().label(),
                t0.elapsed().as_secs_f64()
            );
            *guard = Some(m);
        }
        let model = guard.as_mut().ok_or("beat model not loaded")?;
        let run = |model: &mut BeatModel| {
            stemx_beats::track(&[&mix[0], &mix[1]], RATE, |inp: &[f32], shape| {
                model.run(inp, shape).map_err(|e| BeatErr(e.to_string()))
            })
        };
        let grid = match run(model) {
            Ok(g) => g,
            Err(e) if model.ep().is_gpu() => {
                log!("beat model failed on GPU ({}); retrying on CPU", e.0);
                let m = BeatModel::load(model_path, DevicePref::Cpu, Some(8), &mut |m| log!("{m}"))
                    .map_err(|e| e.to_string())?;
                *guard = Some(m);
                let model = guard.as_mut().ok_or("beat model not loaded")?;
                run(model).map_err(|e| e.0)?
            }
            Err(e) => return Err(e.0),
        };
        Ok(BeatData {
            bpm: grid.tempo_bpm(),
            beats_per_bar: grid.most_common_beats_per_bar().map(|n| n as u32),
            beats: grid.beats,
            downbeats: grid.downbeats,
        })
    }

    pub fn get_beats(&self) -> Result<Option<BeatGrid>, String> {
        let st = lock(&self.st);
        let song = st.song.as_ref().ok_or("No song open")?;
        Ok(song.beats.as_ref().map(|b| BeatGrid {
            beats: b.beats.clone(),
            downbeats: b.downbeats.clone(),
            bpm: b.bpm,
        }))
    }

    // ------------------------------------------------------------ transport

    /// `Err` only when a song is open but there is no audio output device.
    pub fn play(&self) -> Result<(), String> {
        let st = lock(&self.st);
        if st.song.is_none() {
            return Ok(());
        }
        let Some(e) = &self.engine else {
            return Err(NO_DEVICE.into());
        };
        let pos = e.position();
        if !pos.playing {
            let dur = e.duration();
            if let Some((a, b)) = st.loop_state.range() {
                if pos.seconds < a || pos.seconds >= b {
                    e.seek(a.min(dur));
                }
            } else if pos.seconds >= dur - 1e-3 {
                e.seek(0.0);
            }
            e.play();
        }
        drop(st);
        self.emit_position();
        Ok(())
    }

    pub fn pause(&self) {
        if lock(&self.st).song.is_none() {
            return;
        }
        if let Some(e) = &self.engine {
            e.pause();
        }
        self.emit_position();
    }

    pub fn seek(&self, position_sec: f64) {
        if lock(&self.st).song.is_none() || !position_sec.is_finite() {
            return;
        }
        if let Some(e) = &self.engine {
            e.seek(position_sec.clamp(0.0, e.duration()));
        }
        self.emit_position();
    }

    pub fn set_mix(&self, stems: Vec<StemMixState>) {
        let mut st = lock(&self.st);
        st.mix = stems;
        self.apply_gains(&st);
    }

    pub fn set_speed(&self, percent: u32) {
        let mut st = lock(&self.st);
        st.speed_percent = percent.clamp(25, 400);
        if let Some(e) = &self.engine {
            e.set_speed(st.speed_percent as f64 / 100.0);
        }
    }

    pub fn set_pitch(&self, semitones: i32, cents: i32) {
        let mut st = lock(&self.st);
        st.semitones = semitones.clamp(-24, 24);
        st.cents = cents.clamp(-100, 100);
        if let Some(e) = &self.engine {
            e.set_pitch(st.semitones as f64 + st.cents as f64 / 100.0);
        }
    }

    pub fn set_loop(&self, loop_state: LoopState) {
        let mut st = lock(&self.st);
        st.loop_state = loop_state;
        if let Some(e) = &self.engine {
            e.set_loop(loop_state.range());
        }
    }

    pub fn set_click(&self, click: ClickState) {
        let mut st = lock(&self.st);
        st.click = click;
        self.apply_click(&st);
    }

    pub fn set_meter_overrides(&self, payload: MeterOverridesPayload) {
        let mut st = lock(&self.st);
        st.downbeats = Some(payload.downbeats);
        self.apply_click(&st);
    }

    pub fn get_waveform(&self, n_points: u32) -> Result<Waveform, String> {
        let mix = {
            let st = lock(&self.st);
            st.song.as_ref().ok_or("No song open")?.mix.clone()
        };
        Ok(Waveform {
            duration_sec: mix[0].len() as f64 / RATE as f64,
            peaks: peaks(&mix, n_points.clamp(1, 100_000) as usize),
        })
    }

    pub fn position(&self) -> Option<audio::Position> {
        self.engine.as_ref().map(|e| e.position())
    }

    // ------------------------------------------------------------ export

    pub fn export_mix(self: &Arc<Self>, req: ExportRequest) -> Result<(), String> {
        let path = PathBuf::from(&req.path);
        if req.path.trim().is_empty() {
            return Err("No export path".into());
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            if !parent.is_dir() {
                return Err(format!("Folder does not exist: {}", parent.display()));
            }
        }
        let mut st = lock(&self.st);
        let song = st.song.as_ref().ok_or("No song open")?;
        if st.export.is_some() {
            return Err("An export is already running".into());
        }
        let (tracks, gains): (Vec<Track>, Vec<(String, f32)>) = match &song.stems {
            Some(stems) => {
                let mut tracks = Vec::new();
                let mut gains = Vec::new();
                for s in &req.stems {
                    let t = stems
                        .iter()
                        .find(|t| t.name == s.stem)
                        .ok_or_else(|| format!("Unknown stem {}", s.stem))?;
                    tracks.push(t.clone());
                    let v = if s.volume.is_finite() {
                        s.volume.clamp(0.0, 2.0)
                    } else {
                        1.0
                    };
                    gains.push((s.stem.clone(), v));
                }
                (tracks, gains)
            }
            // Not separated yet: export the original.
            None => (song.tracks(), vec![(ORIGINAL.to_string(), 1.0)]),
        };
        if tracks.is_empty() {
            return Err("Nothing to export: no stems selected".into());
        }
        let cancel = Arc::new(AtomicBool::new(false));
        st.export = Some(cancel.clone());
        drop(st);
        let format = match req.format {
            ExportFormat::Wav => audio::ExportFormat::Wav24,
            ExportFormat::Flac => audio::ExportFormat::Flac,
            ExportFormat::Mp3 => audio::ExportFormat::Mp3 { kbps: 320 },
        };
        let render_req = audio::RenderRequest {
            tracks,
            gains,
            speed: (req.speed_percent / 100.0).clamp(0.25, 4.0),
            semitones: req.semitones.clamp(-24.0, 24.0) + req.cents.clamp(-100.0, 100.0) / 100.0,
        };
        log!(
            "export started: {} ({:?}, {} stems, speed {}, shift {:.2} st)",
            path.display(),
            format,
            render_req.tracks.len(),
            render_req.speed,
            render_req.semitones
        );
        let b = self.clone();
        spawn("stemx-export", move || {
            let t0 = Instant::now();
            let res = b.run_export(&render_req, &path, format, &cancel);
            let cancelled = cancel.load(Ordering::Relaxed);
            {
                let mut st = lock(&b.st);
                if st.export.as_ref().is_some_and(|c| Arc::ptr_eq(c, &cancel)) {
                    st.export = None;
                }
            }
            if cancelled {
                let _ = std::fs::remove_file(&path);
                log!("export cancelled, partial file removed");
                return;
            }
            match res {
                Ok(()) => {
                    log!(
                        "export done in {:.1}s: {}",
                        t0.elapsed().as_secs_f64(),
                        path.display()
                    );
                    b.emit(
                        "export-done",
                        &ExportDone {
                            path: path.display().to_string(),
                        },
                    );
                }
                Err(msg) => {
                    let _ = std::fs::remove_file(&path);
                    log!("export failed: {msg}");
                    b.emit(
                        "export-error",
                        &ExportError {
                            message: msg,
                            cancelled: false,
                        },
                    );
                }
            }
        });
        Ok(())
    }

    fn run_export(
        &self,
        req: &audio::RenderRequest,
        path: &Path,
        format: audio::ExportFormat,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        let mut th = Throttle::new();
        let mut progress = |f: f32, force: bool| {
            if let Some(f) = th.next(f, force) {
                self.emit("export-progress", &ExportProgress { fraction: f });
            }
        };
        progress(0.0, true);
        let mut out = audio::render(req, &mut |f| progress(0.9 * f, false), cancel)
            .map_err(|e| format!("Render failed: {e}"))?;
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        // Avoid clipping: scale the whole mix down if it peaks above 0 dBFS.
        let peak = out.iter().flat_map(|c| c.iter()).fold(0.0f32, |m, v| {
            if v.is_finite() {
                m.max(v.abs())
            } else {
                m
            }
        });
        if peak > 0.999 {
            let k = 0.999 / peak;
            for c in out.iter_mut() {
                for v in c.iter_mut() {
                    *v *= k;
                }
            }
            log!("export: peak {peak:.3}, scaled by {k:.3}");
        }
        progress(0.9, true);
        let rate = req.tracks.first().map_or(RATE, |t| t.sample_rate);
        audio::encode(path, &out, rate, format).map_err(|e| format!("Encoding failed: {e}"))?;
        progress(1.0, true);
        Ok(())
    }

    pub fn cancel_export(&self) {
        let job = lock(&self.st).export.take();
        if let Some(c) = job {
            c.store(true, Ordering::Relaxed);
            self.emit(
                "export-error",
                &ExportError {
                    message: "Cancelled".into(),
                    cancelled: true,
                },
            );
        }
    }

    pub fn export_running(&self) -> bool {
        lock(&self.st).export.is_some()
    }

    pub fn data_dir(&self) -> &Path {
        &self.cfg.data_dir
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    if let Err(e) = std::thread::Builder::new().name(name.into()).spawn(f) {
        log!("cannot start thread {name}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(stem: &str, on: bool, volume: f32, solo: bool) -> StemMixState {
        StemMixState {
            stem: stem.into(),
            on,
            volume,
            solo,
        }
    }

    #[test]
    fn audibility_rule() {
        let names: Vec<String> = ["vocals", "bass", "drums"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mix = vec![
            ms("vocals", true, 0.5, false),
            ms("bass", false, 1.0, false),
            ms("drums", true, 0.8, false),
        ];
        let g = gains_for(&mix, &names);
        assert_eq!(g[0], ("vocals".into(), 0.5));
        assert_eq!(g[1].1, 0.0);
        assert_eq!(g[2].1, 0.8);
        // Solo wins over on/off, even for a stem that is off.
        let mix = vec![
            ms("vocals", true, 0.5, false),
            ms("bass", false, 0.7, true),
            ms("drums", true, 0.8, false),
        ];
        let g = gains_for(&mix, &names);
        assert_eq!(g.iter().map(|x| x.1).collect::<Vec<_>>(), [0.0, 0.7, 0.0]);
        // Original track is always unity.
        assert_eq!(gains_for(&mix, &["original".into()])[0].1, 1.0);
    }

    #[test]
    fn downbeats_snap_within_1ms() {
        let beats = [0.5, 1.0, 1.5, 2.0];
        let d = snap_downbeats(&[1.0004, 2.0, 3.0, 0.4999], &beats);
        assert_eq!(d, vec![0.5, 1.0, 2.0, 3.0]);
    }

    #[test]
    fn peaks_shape() {
        let l: Vec<f32> = (0..1000)
            .map(|i| if i == 10 { -0.9 } else { 0.1 })
            .collect();
        let r = vec![0.2f32; 1000];
        let p = peaks(&[l, r], 10);
        assert_eq!(p.len(), 10);
        assert!((p[0] - 0.9).abs() < 1e-6);
        assert!((p[5] - 0.2).abs() < 1e-6);
        // More points than samples still works.
        assert_eq!(peaks(&[vec![0.5; 3], vec![0.5; 3]], 7).len(), 7);
        assert_eq!(peaks(&[vec![], vec![]], 4), vec![0.0; 4]);
    }

    #[test]
    fn throttle_is_monotonic() {
        let mut t = Throttle::new();
        assert_eq!(t.next(0.5, true), Some(0.5));
        assert_eq!(t.next(0.2, true), Some(0.5));
        assert_eq!(t.next(0.6, false), None);
        assert_eq!(t.next(0.7, true), Some(0.7));
    }

    fn test_backend(name: &str) -> (Arc<Backend>, Arc<Mutex<Vec<String>>>) {
        let dir = std::env::temp_dir().join(format!("stemx-backend-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let events = Arc::new(Mutex::new(Vec::new()));
        let ev = events.clone();
        let sink: Sink = Arc::new(move |n, _| lock(&ev).push(n.to_string()));
        let cfg = Config {
            data_dir: dir,
            model_dirs: vec![],
            beat_model: None,
            device: DevicePref::Cpu,
            beat_device: DevicePref::Cpu,
        };
        (Backend::new(cfg, None, sink).unwrap(), events)
    }

    #[test]
    fn open_wav_play_export_without_models() {
        let (b, events) = test_backend("open");
        // Commands that need a song fail cleanly; transport is ignored.
        assert_eq!(b.get_waveform(10).unwrap_err(), "No song open");
        b.play().unwrap();
        b.seek(3.0);
        assert!(b.start_separation().is_err());

        let wav = b.data_dir().join("tone.wav");
        std::fs::create_dir_all(b.data_dir()).unwrap();
        let tone: Vec<f32> = (0..RATE * 2)
            .map(|i| (i as f32 * 0.05).sin() * 0.5)
            .collect();
        stemx_io::write_wav(&wav, RATE, &[&tone, &tone], stemx_io::WavFormat::F32).unwrap();
        let info = b.open_song(wav.to_str().unwrap()).unwrap();
        assert_eq!(info.title, "tone");
        assert!((info.duration_sec - 2.0).abs() < 1e-3);
        assert!(!info.stems_cached && !info.beats_ready);
        assert_eq!(b.library().len(), 1);
        let w = b.get_waveform(100).unwrap();
        assert_eq!(w.peaks.len(), 100);
        assert!(w.peaks[50] > 0.4);
        assert_eq!(b.get_beats().unwrap(), None);
        // No model -> start_separation errors.
        assert!(b
            .start_separation()
            .unwrap_err()
            .contains("No separation model"));

        b.save_song_settings(&info.song_id, serde_json::json!({"version": 1}))
            .unwrap();
        let again = b.open_song(wav.to_str().unwrap()).unwrap();
        assert_eq!(again.settings, Some(serde_json::json!({"version": 1})));
        assert_eq!(again.song_id, info.song_id);

        b.seek(1.0);
        assert!(b.play().unwrap_err().contains("audio output"));
        b.pause();

        let out = b.data_dir().join("out.wav");
        b.export_mix(ExportRequest {
            path: out.display().to_string(),
            format: ExportFormat::Wav,
            stems: vec![],
            speed_percent: 100.0,
            semitones: 0.0,
            cents: 0.0,
        })
        .unwrap();
        let t0 = Instant::now();
        while b.export_running() && t0.elapsed() < Duration::from_secs(30) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(out.is_file());
        let ev = lock(&events).clone();
        assert!(ev.iter().any(|e| e == "export-done"), "{ev:?}");
        b.close_song();
        assert!(b.get_beats().is_err());
    }
}
