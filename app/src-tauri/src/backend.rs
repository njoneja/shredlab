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
//! - strength: one background thread per re-derivation (cancelled when a
//!   newer one starts or the song changes).
//! - position: a 30 Hz emitter thread (also the stereo-field events of the
//!   open Refine panel and the debounced save of refine settings).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::Serialize;
use shredlab_library::{BeatData, Library, SongMeta, StemsInfo};
use shredlab_separation::selftest::Cache;
use shredlab_separation::{
    BeatModel, DevicePref, Ep, LoadOptions, ModelEntry, ModelRegistry, ModelSource, Separator,
};

use crate::audio::{self, ClickConfig, Engine, MixMode, Track};
use crate::log;
use crate::music_start;
use crate::settings::{self, AppSettings};
use crate::types::*;
use shredlab_dsp::strength::{apply_strength, StrengthOptions};

/// Event sink: `(event name, JSON payload)`.
pub type Sink = Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>;

/// Sample rate everything is played and cached at (the model rate).
pub const RATE: u32 = shredlab_io::MODEL_SAMPLE_RATE;
const ORIGINAL: &str = "original";
const RECENT_MAX: usize = 20;
/// Mask exponent per strength level (0 = Natural: the cached stems).
pub const STRENGTH_P: [f32; 5] = [1.0, 1.5, 2.0, 3.0, 4.0];
/// Refine settings are saved this long after the last change.
const REFINE_SAVE_DELAY: Duration = Duration::from_millis(600);
const NO_DEVICE: &str =
    "No audio output device found. Connect speakers or headphones and restart ShredLab (separation and export still work).";

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
    /// original is unavailable), for the waveform and beat tracking.
    mix: Arc<[Vec<f32>; 2]>,
    /// True when `mix` is the original (decoded or cached), i.e. the song
    /// can be (re-)separated.
    mix_is_original: bool,
    /// The original as a track: played before separation, and the source
    /// of the backing track (P1) once stems are loaded. `None` when it is
    /// unavailable or not sample-aligned with the stems.
    original: Option<Track>,
    stems: Option<Vec<Track>>,
    /// The cached stem set in `stems`.
    stems_info: Option<StemsInfo>,
    beats: Option<BeatData>,
    /// Where the music starts after a quiet intro ([`music_start::detect`]).
    music_start: Option<f64>,
    /// Strength the in-memory `stems` were derived with (default: the
    /// cached set itself).
    loaded_strength: StrengthSetting,
}

impl Song {
    fn tracks(&self) -> Vec<Track> {
        match (&self.stems, &self.original) {
            (Some(s), _) => s.clone(),
            (None, Some(o)) => vec![o.clone()],
            (None, None) => vec![mix_track(&self.mix)],
        }
    }

    /// Loads the song into the engine: stems plus the original (for P1),
    /// or the original alone before separation.
    fn load_into(&self, e: &Engine) {
        match &self.stems {
            Some(s) => e.load_with_original(s.clone(), self.original.clone()),
            None => e.load(self.tracks()),
        }
    }

    fn backing_from_original(&self) -> bool {
        self.stems.is_some() && self.original.is_some()
    }

    /// Drops the original track if it is not sample-aligned with the stems.
    fn check_alignment(&mut self) {
        let (Some(o), Some(s)) = (&self.original, &self.stems) else {
            return;
        };
        let olen = o.channels[0].len();
        if let Some(t) = s.iter().find(|t| t.channels[0].len() != olen) {
            log!(
                "original ({olen} frames) and stem {} ({} frames) differ in length; backing track from stems only",
                t.name,
                t.channels[0].len()
            );
            self.original = None;
        }
    }

    fn separated_with(&self) -> Option<SeparatedWith> {
        self.stems.as_ref()?;
        self.stems_info.as_ref().map(separated_with)
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
            original_available: self.backing_from_original(),
            refine: m.refine.clone(),
            strength: m.strength,
            strength_job: 0,
        }
    }
}

fn separated_with(info: &StemsInfo) -> SeparatedWith {
    SeparatedWith {
        model_name: info.model_name.clone(),
        device_label: info.device_label.clone(),
        model_id: info.model_id.clone(),
        quality: Quality::from_shifts(info.shifts),
    }
}

fn original_track(ch: &[Vec<f32>; 2]) -> Track {
    Track {
        name: ORIGINAL.into(),
        channels: [Arc::new(ch[0].clone()), Arc::new(ch[1].clone())],
        sample_rate: RATE,
    }
}

struct Job {
    song_id: String,
    cancel: Arc<AtomicBool>,
}

struct StrengthJob {
    id: u64,
    cancel: Arc<AtomicBool>,
}

/// Same strength for playback purposes (smooth is irrelevant at Natural).
fn same_strength(a: StrengthSetting, b: StrengthSetting) -> bool {
    a.level == b.level && (a.level == 0 || a.smooth == b.smooth)
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
    /// Refine settings of the open song, by stem (only refined stems).
    refine: BTreeMap<String, RefineParams>,
    /// Unsaved refine change: (song id, time of the last change).
    refine_dirty: Option<(String, Instant)>,
    /// Stem whose Refine panel is open (stereo-field analysis).
    analysis: Option<String>,
    strength_job: Option<StrengthJob>,
    next_strength_job: u64,
}

pub struct Backend {
    cfg: Config,
    library: Library,
    engine: Option<Arc<Engine>>,
    sink: Sink,
    registry: ModelRegistry,
    app_settings: Mutex<AppSettings>,
    separator: Mutex<Option<Separator>>,
    beat_model: Mutex<Option<BeatModel>>,
    st: Mutex<State>,
    open_lock: Mutex<()>,
}

/// [`music_start::detect`] on the original mix, logged.
fn detect_music_start(song_id: &str, mix: &[Vec<f32>; 2], beats: &[f64]) -> Option<f64> {
    let t0 = Instant::now();
    let ms = music_start::detect(mix, RATE, beats);
    log!(
        "music start of {song_id}: {} ({:.0} ms)",
        ms.map_or("none (no quiet intro)".to_string(), |t| format!("{t:.2}s")),
        t0.elapsed().as_secs_f64() * 1000.0
    );
    ms
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

/// True when `stem` has a stereo exception (an enabled stereo window): the
/// window does the opposite of the row's on switch.
fn has_exception(refine: &BTreeMap<String, RefineParams>, stem: &str) -> bool {
    refine.get(stem).is_some_and(|p| p.pan.enabled)
}

/// Engine gains for `names` under the audibility rule: if any stem is
/// soloed only soloed stems play, else stems that are on, plus stems that
/// are off but have a stereo exception (they play the window, at their
/// volume). Which part of a stem is heard is set by [`engine_refine`].
pub fn gains_for(
    mix: &[StemMixState],
    refine: &BTreeMap<String, RefineParams>,
    names: &[String],
) -> Vec<(String, f32)> {
    let any_solo = mix.iter().any(|m| m.solo);
    names
        .iter()
        .map(|n| {
            if n == ORIGINAL {
                return (n.clone(), 1.0);
            }
            let g = match mix.iter().find(|m| &m.stem == n) {
                Some(m) => {
                    let audible = if any_solo {
                        m.solo
                    } else {
                        m.on || has_exception(refine, n)
                    };
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

/// Refine parameters as the engine needs them: the saved ones with the
/// stereo window inverted on rows whose switch is on (the window is the
/// exception: on = the stem minus the window, off = the window alone). A
/// stem missing from `mix` counts as on.
pub fn engine_refine(
    refine: &BTreeMap<String, RefineParams>,
    is_on: impl Fn(&str) -> bool,
) -> Vec<(String, RefineParams)> {
    refine
        .iter()
        .map(|(k, v)| {
            let mut p = *v;
            p.pan.invert = is_on(k);
            (k.clone(), p)
        })
        .collect()
}

/// The P1 mix mode for playback: the original-based backing track when the
/// original is loaded with the stems and at least half of the stems are on
/// (ignoring solo and volume), plain sum otherwise or when any stem is
/// soloed. A stem missing from `mix` counts as on (like [`gains_for`]).
pub fn mix_mode(mix: &[StemMixState], names: &[String], has_original: bool) -> MixMode {
    if !has_original {
        return MixMode::Stems;
    }
    let any_solo = mix.iter().any(|m| m.solo);
    let n_on = names
        .iter()
        .filter(|n| mix.iter().find(|m| &m.stem == *n).is_none_or(|m| m.on))
        .count();
    audio::choose_mode(names.len(), n_on, any_solo)
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
impl From<shredlab_beats::Error> for BeatErr {
    fn from(e: shredlab_beats::Error) -> Self {
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
        let (app_settings, err) = AppSettings::load(&cfg.data_dir);
        if let Some(e) = err {
            log!("settings: {e}");
        }
        log!(
            "settings: model {}, quality {}",
            app_settings.model_id.as_deref().unwrap_or("default"),
            app_settings
                .quality
                .map_or("default", |q| if q == Quality::High {
                    "high"
                } else {
                    "standard"
                })
        );
        if let Some(e) = &engine {
            e.set_master_volume(app_settings.master_volume());
        }
        let click = ClickState {
            volume: app_settings.click_volume(),
            ..ClickState::default()
        };
        let engine = engine.map(Arc::new);
        let b = Arc::new(Self {
            cfg,
            library,
            engine,
            sink,
            registry,
            app_settings: Mutex::new(app_settings),
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
                click,
                downbeats: None,
                separation: None,
                export: None,
                last_separation_error: None,
                refine: BTreeMap::new(),
                refine_dirty: None,
                analysis: None,
                strength_job: None,
                next_strength_job: 0,
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
        spawn("shredlab-position", move || {
            let mut was_active = false;
            let mut field = [0.0f32; audio::FIELD_LEN];
            loop {
                std::thread::sleep(Duration::from_millis(33));
                let Some(b) = weak.upgrade() else { break };
                b.flush_refine_save(false);
                let Some(e) = &b.engine else { break };
                let analysis = {
                    let st = lock(&b.st);
                    if st.song.is_none() {
                        was_active = false;
                        continue;
                    }
                    st.analysis.clone()
                };
                if let Some(stem) = analysis {
                    if e.take_field(&mut field) {
                        b.emit(
                            "refine-field",
                            &RefineField {
                                stem,
                                bins: field.to_vec(),
                            },
                        );
                    }
                }
                let p = e.position();
                let active = p.playing || p.count_in_beats_left > 0;
                if active || was_active {
                    b.emit_pos(p);
                }
                was_active = active;
            }
        });
        if std::env::var_os("SHREDLAB_NO_PRELOAD").is_none() {
            let b = self.clone();
            spawn("shredlab-preload", move || {
                let Some(entry) = b.current_model() else {
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

    // ------------------------------------------------------------ settings

    /// The model for the next separation: `SHREDLAB_MODEL` (dev override) if
    /// present, else the saved choice if installed, else the default.
    fn current_model(&self) -> Option<ModelEntry> {
        if let Some(e) = std::env::var("SHREDLAB_MODEL")
            .ok()
            .and_then(|id| self.registry.get(&id))
        {
            return Some(e.clone());
        }
        let choice = lock(&self.app_settings).model_id.clone();
        if let Some(e) = choice.and_then(|id| self.registry.get(&id)) {
            return Some(e.clone());
        }
        crate::models::pick_model(&self.registry).cloned()
    }

    /// (quality for the next separation, default quality) for `entry`.
    fn current_quality(&self, entry: Option<&ModelEntry>) -> (Quality, Quality) {
        let device = entry.map_or("CPU", |e| self.predict_device(e));
        let default = Quality::default_for(device);
        let choice = lock(&self.app_settings).quality;
        (choice.unwrap_or(default), default)
    }

    pub fn app_settings(&self) -> AppSettingsInfo {
        let entry = self.current_model();
        let (quality, default_quality) = self.current_quality(entry.as_ref());
        let saved = lock(&self.app_settings).clone();
        AppSettingsInfo {
            models: self
                .registry
                .models
                .iter()
                .map(|m| ModelInfo {
                    id: m.id().to_string(),
                    name: m.manifest.name.clone(),
                    personal: m.is_personal(),
                    license: m.manifest.license.spdx.clone(),
                    stems: m.manifest.stems.clone(),
                })
                .collect(),
            model_id: entry.as_ref().map(|e| e.id().to_string()),
            model_choice: saved.model_id.clone(),
            model_from_env: std::env::var("SHREDLAB_MODEL")
                .ok()
                .is_some_and(|id| self.registry.get(&id).is_some()),
            quality,
            quality_choice: saved.quality,
            default_quality,
            device_label: entry
                .as_ref()
                .map_or("CPU", |e| self.predict_device(e))
                .to_string(),
            theme: saved.theme().to_string(),
            count_in_to_music_start: saved.count_in_to_music_start(),
            click_volume: saved.click_volume(),
            master_volume: saved.master_volume(),
            stem_format: saved.stem_format(),
        }
    }

    /// Saves the settings (null = default) and returns the new effective
    /// settings. Does not re-separate anything by itself.
    pub fn set_app_settings(&self, update: AppSettingsUpdate) -> Result<AppSettingsInfo, String> {
        if let Some(id) = &update.model_id {
            if self.registry.get(id).is_none() {
                return Err(format!("Unknown model {id}"));
            }
        }
        let new = AppSettings {
            model_id: update.model_id,
            quality: update.quality,
            ..lock(&self.app_settings).clone()
        };
        new.save(&self.cfg.data_dir)
            .map_err(|e| format!("Cannot save settings: {e}"))?;
        log!("settings saved: {new:?}");
        *lock(&self.app_settings) = new;
        Ok(self.app_settings())
    }

    /// Saves the UI theme (one of [`settings::THEMES`]) with the other app
    /// settings and returns the new settings.
    pub fn set_theme(&self, theme: &str) -> Result<AppSettingsInfo, String> {
        if !settings::THEMES.contains(&theme) {
            return Err(format!("Unknown theme {theme}"));
        }
        let new = AppSettings {
            theme: Some(theme.to_string()),
            ..lock(&self.app_settings).clone()
        };
        new.save(&self.cfg.data_dir)
            .map_err(|e| format!("Cannot save settings: {e}"))?;
        log!("theme saved: {theme}");
        *lock(&self.app_settings) = new;
        Ok(self.app_settings())
    }

    /// Saves "Count in to where the music starts" with the app settings and
    /// applies it to playback at once.
    pub fn set_count_in_to_music_start(&self, on: bool) -> Result<AppSettingsInfo, String> {
        let new = AppSettings {
            count_in_to_music_start: Some(on),
            ..lock(&self.app_settings).clone()
        };
        new.save(&self.cfg.data_dir)
            .map_err(|e| format!("Cannot save settings: {e}"))?;
        log!("count in to where the music starts: {on}");
        *lock(&self.app_settings) = new;
        self.apply_click(&lock(&self.st));
        Ok(self.app_settings())
    }

    /// Saves the format of newly written stems with the app settings.
    /// Existing stems keep their format; they are still read.
    pub fn set_stem_format(&self, format: StemFormat) -> Result<AppSettingsInfo, String> {
        let new = AppSettings {
            stem_format: Some(format),
            ..lock(&self.app_settings).clone()
        };
        new.save(&self.cfg.data_dir)
            .map_err(|e| format!("Cannot save settings: {e}"))?;
        log!("stem format: {format:?}");
        *lock(&self.app_settings) = new;
        Ok(self.app_settings())
    }

    /// Saves the click and count-in volume (linear 0..1) with the app
    /// settings and applies it to playback at once.
    pub fn set_click_volume(&self, volume: f32) -> Result<AppSettingsInfo, String> {
        if !volume.is_finite() {
            return Err(format!("Invalid click volume {volume}"));
        }
        let volume = volume.clamp(0.0, 1.0);
        let new = AppSettings {
            click_volume: Some(volume),
            ..lock(&self.app_settings).clone()
        };
        new.save(&self.cfg.data_dir)
            .map_err(|e| format!("Cannot save settings: {e}"))?;
        log!("click volume: {volume:.2}");
        *lock(&self.app_settings) = new;
        let mut st = lock(&self.st);
        st.click.volume = volume;
        self.apply_click(&st);
        Ok(self.app_settings())
    }

    /// Sets the master playback volume (linear 0..1) at once; `save` also
    /// stores it with the app settings (the UI saves when a drag ends).
    pub fn set_master_volume(&self, volume: f32, save: bool) -> Result<AppSettingsInfo, String> {
        if !volume.is_finite() {
            return Err(format!("Invalid master volume {volume}"));
        }
        let volume = volume.clamp(0.0, 1.0);
        if let Some(e) = &self.engine {
            e.set_master_volume(volume);
        }
        let new = AppSettings {
            master_volume: Some(volume),
            ..lock(&self.app_settings).clone()
        };
        if save {
            new.save(&self.cfg.data_dir)
                .map_err(|e| format!("Cannot save settings: {e}"))?;
            log!("master volume: {volume:.2}");
        }
        *lock(&self.app_settings) = new;
        Ok(self.app_settings())
    }

    fn load_separator(&self, entry: &ModelEntry) -> Result<Separator, String> {
        let t0 = Instant::now();
        log!(
            "loading model {} ({:?}, ONNX Runtime {})",
            entry.id(),
            self.cfg.device,
            shredlab_separation::runtime::ort_version()
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
            &shredlab_separation::runtime::ort_version(),
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

    /// Sends gains, mix mode and the refine parameters (which follow the
    /// on switches) to the engine.
    fn apply_gains(&self, st: &State) {
        let (Some(e), Some(song)) = (&self.engine, &st.song) else {
            return;
        };
        let names: Vec<String> = song.tracks().iter().map(|t| t.name.clone()).collect();
        let mode = mix_mode(&st.mix, &names, song.backing_from_original());
        e.set_refine_all(Self::refine_list(st));
        e.set_mix(gains_for(&st.mix, &st.refine, &names), mode);
    }

    fn click_config(&self, st: &State) -> ClickConfig {
        let c = &st.click;
        let grid = st.song.as_ref().and_then(|s| s.beats.as_ref());
        let music_start = if lock(&self.app_settings).count_in_to_music_start() {
            st.song.as_ref().and_then(|s| s.music_start)
        } else {
            None
        };
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
                settings::DEFAULT_CLICK_VOLUME
            },
            music_start,
        }
    }

    fn apply_click(&self, st: &State) {
        if let Some(e) = &self.engine {
            e.set_click(self.click_config(st));
        }
    }

    /// The refine parameters for the engine (see [`engine_refine`]).
    fn refine_list(st: &State) -> Vec<(String, RefineParams)> {
        engine_refine(&st.refine, |stem| Self::switch_on(st, stem))
    }

    /// The row's on switch (a stem missing from the mix state counts as on).
    fn switch_on(st: &State, stem: &str) -> bool {
        st.mix.iter().find(|m| m.stem == stem).is_none_or(|m| m.on)
    }

    fn apply_all(&self, st: &State) {
        let Some(e) = &self.engine else { return };
        e.set_refine_all(Self::refine_list(st));
        e.set_analysis(st.analysis.as_deref());
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
        let source_exists = path_buf.is_file();
        // Song id from the content; if the file is gone, from a library
        // entry with cached stems (the cache has everything needed).
        let song_id = if source_exists {
            shredlab_library::song_id_for(&path_buf).map_err(|e| e.to_string())?
        } else {
            self.library
                .entries()
                .into_iter()
                .find(|e| e.path == path && e.stems_cached)
                .map(|e| e.song_id)
                .ok_or_else(|| format!("File not found: {path}"))?
        };
        // Stop what was going on.
        self.flush_refine_save(true);
        {
            let mut st = lock(&self.st);
            self.cancel_separation_locked(&mut st);
            self.cancel_strength_locked(&mut st);
            st.generation += 1;
            if let Some(e) = &self.engine {
                e.pause();
            }
        }
        let mut meta = match self.library.load(&song_id) {
            Ok(m) => m,
            Err(e) => {
                log!("ignoring unreadable manifest for {song_id}: {e}");
                None
            }
        };

        // Cached stems: the set for the current model and quality, else the
        // most recent readable one.
        let want = self.current_model();
        let want_shifts = self.current_quality(want.as_ref()).0.shifts();
        let mut stems: Option<Vec<Track>> = None;
        let mut stems_info: Option<StemsInfo> = None;
        let mut stems_sum: Option<[Vec<f32>; 2]> = None;
        if let Some(m) = &meta {
            let mut candidates: Vec<StemsInfo> = Vec::new();
            if let Some(s) = want.as_ref().and_then(|e| m.stem_set(e.id(), want_shifts)) {
                candidates.push(s.clone());
            }
            for s in m.stem_sets.iter().rev() {
                if !candidates.contains(s) {
                    candidates.push(s.clone());
                }
            }
            for info in candidates {
                match self.read_cached_stems(&song_id, &info) {
                    Ok((tracks, sum)) => {
                        stems = Some(tracks);
                        stems_sum = Some(sum);
                        stems_info = Some(info);
                        break;
                    }
                    Err(e) => log!("stem cache {} for {song_id} unusable ({e})", info.dir),
                }
            }
        }

        // The original: the cached copy, else decode the source.
        let mut original: Option<[Vec<f32>; 2]> = None;
        if let Some(oi) = meta.as_ref().and_then(|m| m.original.clone()) {
            let tr = Instant::now();
            match self.library.read_original(&song_id, &oi) {
                Ok(Some(ch)) => {
                    log!("read cached original in {:.2}s", tr.elapsed().as_secs_f64());
                    original = Some(ch);
                }
                Ok(None) => log!("cached original of {song_id} is missing"),
                Err(e) => log!("cached original of {song_id} unusable ({e})"),
            }
        }
        let mut native: Option<(u32, u32)> = None;
        let mut new_original_info = None;
        if original.is_none() && source_exists {
            let td = Instant::now();
            let audio = shredlab_io::decode_file(&path_buf).map_err(|e| e.to_string())?;
            if audio.is_empty() {
                return Err(format!("No audio in {path}"));
            }
            let (rate, nch) = (audio.sample_rate, audio.channels.len() as u32);
            let audio = shredlab_io::resample(&audio, RATE).map_err(|e| e.to_string())?;
            let mut ch = audio.channels.into_iter();
            let l = ch.next().unwrap_or_default();
            let r = ch.next().unwrap_or_else(|| l.clone());
            log!(
                "decoded {} ({rate} Hz, {nch} ch, {:.1}s) in {:.2}s",
                path_buf.display(),
                l.len() as f64 / RATE as f64,
                td.elapsed().as_secs_f64()
            );
            if stems.is_some() {
                // Old cache entry without the original: keep a copy now.
                log!("no cached original for {song_id}; decoded the source and caching a copy");
                match self
                    .library
                    .write_original(&song_id, RATE, [&l, &r], Some(&path_buf))
                {
                    Ok(oi) => new_original_info = Some(oi),
                    Err(e) => log!("cannot cache the original: {e}"),
                }
            }
            original = Some([l, r]);
            native = Some((rate, nch));
        }
        if original.is_none() {
            if stems.is_none() {
                return Err(format!("File not found: {path}"));
            }
            log!(
                "original of {song_id} unavailable (old cache entry and the source file is gone): backing track from the stems only"
            );
        }
        let mix_is_original = original.is_some();
        let original_tr = original.as_ref().map(original_track);
        let mix = Arc::new(original.or(stems_sum).unwrap_or_default());
        let duration = mix[0].len() as f64 / RATE as f64;

        let mut m = match meta.take() {
            Some(m) => m,
            None => SongMeta::new(
                &song_id,
                &path_buf,
                shredlab_library::read_tags(&path_buf),
                duration,
            ),
        };
        if source_exists {
            m.source_path = path.to_string();
        }
        m.duration_sec = duration;
        if let Some((rate, nch)) = native {
            m.sample_rate = rate;
            m.channels = nch;
        }
        m.last_opened_ms = shredlab_library::now_ms();
        m.hidden = false;
        if stems.is_none() {
            m.stem_sets.clear();
        }
        if let Some(oi) = new_original_info {
            m.original = Some(oi);
        }
        if let Err(e) = self.library.save(&mut m) {
            log!("cannot save library entry: {e}");
        }
        let beats = m.beats.clone();
        let need_beats = beats.is_none();
        let music_start = beats
            .as_ref()
            .and_then(|b| detect_music_start(&m.song_id, &mix, &b.beats));
        let mut song = Song {
            meta: m,
            mix: mix.clone(),
            mix_is_original,
            original: original_tr,
            stems,
            stems_info,
            beats,
            music_start,
            loaded_strength: StrengthSetting::default(),
        };
        song.check_alignment();
        let mut info = song.info();
        let generation = {
            let mut st = lock(&self.st);
            st.refine = song
                .meta
                .refine
                .iter()
                .filter(|(_, p)| !p.is_identity())
                .map(|(k, p)| (k.clone(), *p))
                .collect();
            st.analysis = None;
            if let Some(e) = &self.engine {
                e.pause();
                // Refine first, so the new track set starts with it.
                e.set_refine_all(Self::refine_list(&st));
                song.load_into(e);
                e.seek(0.0);
            }
            st.song = Some(song);
            st.loop_state = LoopState::default();
            st.downbeats = None;
            st.last_separation_error = None;
            self.apply_all(&st);
            info.strength_job = self.start_strength_locked(&mut st);
            st.generation
        };
        self.emit_position();
        log!(
            "opened \"{}\" ({song_id}) in {:.2}s: stems {}, backing track {}, beats {}",
            info.title,
            t0.elapsed().as_secs_f64(),
            match &info.separated_with {
                Some(w) => format!("cached ({}, {:?})", w.model_id, w.quality),
                None => "not separated".into(),
            },
            if info.original_available {
                "from the original"
            } else {
                "from the stems"
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
            "read {} cached stems ({}) in {:.2}s",
            tracks.len(),
            info.dir,
            t0.elapsed().as_secs_f64()
        );
        Ok((tracks, sum))
    }

    pub fn close_song(&self) {
        self.flush_refine_save(true);
        let mut st = lock(&self.st);
        self.cancel_separation_locked(&mut st);
        self.cancel_strength_locked(&mut st);
        st.analysis = None;
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

    /// Hides a song from the recent list (its data stays; opening it again
    /// brings it back).
    pub fn hide_recent(&self, song_id: &str) -> Result<(), String> {
        match self.library.set_hidden(song_id, true) {
            Ok(true) => Ok(()),
            Ok(false) => Err(format!("Unknown song {song_id}")),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Every stored song with its size on disk, largest first (walks the
    /// library; called off the UI thread).
    pub fn list_library(&self) -> LibraryListing {
        let open = lock(&self.st).song.as_ref().map(|s| s.meta.song_id.clone());
        let mut songs: Vec<StoredSongInfo> = self
            .library
            .list()
            .into_iter()
            .map(|s| StoredSongInfo {
                open: open.as_deref() == Some(s.entry.song_id.as_str()),
                song_id: s.entry.song_id,
                path: s.entry.path,
                title: s.entry.title,
                artist: s.entry.artist,
                size_bytes: s.size_bytes,
                last_opened_ms: s.entry.last_opened_ms,
                source_exists: s.source_exists,
                hidden: s.entry.hidden,
            })
            .collect();
        songs.sort_by_key(|s| std::cmp::Reverse(s.size_bytes));
        LibraryListing {
            dir: self.library.root().display().to_string(),
            total_bytes: songs.iter().map(|s| s.size_bytes).sum(),
            songs,
        }
    }

    /// Bytes stored for one song (what `delete_song` would free). Walks only
    /// that song's folder (a handful of files); called off the UI thread.
    pub fn song_size(&self, song_id: &str) -> Result<u64, String> {
        self.library.song_size(song_id).map_err(|e| e.to_string())
    }

    /// Deletes everything stored for a song and returns the bytes freed.
    /// The open song is refused (close it first): its stems are in use.
    pub fn delete_song(&self, song_id: &str) -> Result<u64, String> {
        // Serialized with open_song, so the song cannot be opened meanwhile.
        let _serial = lock(&self.open_lock);
        if lock(&self.st)
            .song
            .as_ref()
            .is_some_and(|s| s.meta.song_id == song_id)
        {
            return Err("This song is open. Close it before deleting its stems.".into());
        }
        let freed = self
            .library
            .delete_song(song_id)
            .map_err(|e| e.to_string())?;
        log!("deleted stored data of {song_id} ({freed} bytes)");
        Ok(freed)
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

    /// Separates the current song with the current model and quality. If
    /// that (model, quality) is already cached for the song, the stems are
    /// loaded from the cache instead (fast), with the same events.
    pub fn start_separation(self: &Arc<Self>) -> Result<SeparatedWith, String> {
        let entry = self.current_model().ok_or_else(|| {
            "No separation model found. Put a model folder in the models directory.".to_string()
        })?;
        let quality = self.current_quality(Some(&entry)).0;
        let shifts = quality.shifts();
        let device = self.predict_device(&entry);
        let mut st = lock(&self.st);
        let song = st.song.as_ref().ok_or("No song open")?;
        if st.separation.is_some() {
            return Err("Separation is already running".into());
        }
        let song_id = song.meta.song_id.clone();
        let cached = song
            .meta
            .stem_set(entry.id(), shifts)
            .filter(|i| self.library.stems_present(&song_id, i))
            .cloned();
        if cached.is_none() && !song.mix_is_original {
            return Err(
                "The original file is missing, so this song can't be separated again.".into(),
            );
        }
        let mix = song.mix.clone();
        let generation = st.generation;
        let cancel = Arc::new(AtomicBool::new(false));
        st.separation = Some(Job {
            song_id: song_id.clone(),
            cancel: cancel.clone(),
        });
        st.last_separation_error = None;
        drop(st);
        let started = match &cached {
            Some(info) => separated_with(info),
            None => SeparatedWith {
                model_name: entry.manifest.name.clone(),
                device_label: device.into(),
                model_id: entry.id().to_string(),
                quality,
            },
        };
        log!(
            "separation of {song_id} started: {} {quality:?} ({shifts} pass{}) on {device}{}",
            entry.id(),
            if shifts == 1 { "" } else { "es" },
            if cached.is_some() {
                " (cached)"
            } else {
                " (predicted)"
            }
        );
        let b = self.clone();
        spawn("shredlab-separation", move || {
            b.run_separation(entry, shifts, cached, song_id, mix, generation, cancel);
        });
        Ok(started)
    }

    pub fn cancel_separation(&self) {
        let mut st = lock(&self.st);
        self.cancel_separation_locked(&mut st);
    }

    #[allow(clippy::too_many_arguments)]
    fn run_separation(
        self: &Arc<Self>,
        entry: ModelEntry,
        shifts: u32,
        cached: Option<StemsInfo>,
        song_id: String,
        mix: Arc<[Vec<f32>; 2]>,
        generation: u64,
        cancel: Arc<AtomicBool>,
    ) {
        let t0 = Instant::now();
        let result = match cached {
            Some(info) => self.load_cached_set(&song_id, info),
            None => self.separate_and_cache(&entry, shifts, &song_id, &mix, &cancel),
        };
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
            Ok((tracks, info)) => {
                log!(
                    "separation of {song_id} done in {:.1}s ({}, {} pass{}, {})",
                    t0.elapsed().as_secs_f64(),
                    info.model_id,
                    info.shifts,
                    if info.shifts == 1 { "" } else { "es" },
                    info.device_label
                );
                let current = st.generation == generation
                    && st.song.as_ref().is_some_and(|s| s.meta.song_id == song_id);
                if !current || cancelled {
                    return;
                }
                let with = separated_with(&info);
                let mut original_available = false;
                if let Some(song) = st.song.as_mut() {
                    if let Ok(Some(m)) = self.library.load(&song_id) {
                        song.meta.stem_sets = m.stem_sets;
                        song.meta.original = m.original;
                    }
                    song.stems = Some(tracks);
                    song.stems_info = Some(info);
                    song.loaded_strength = StrengthSetting::default();
                    if song.original.is_none() && song.mix_is_original {
                        song.original = Some(original_track(&song.mix));
                    }
                    song.check_alignment();
                    original_available = song.backing_from_original();
                    if let Some(e) = &self.engine {
                        song.load_into(e);
                    }
                }
                self.apply_gains(&st);
                // The new set is the plain model output: apply the song's
                // strength to it (no-op at Natural).
                self.start_strength_locked(&mut st);
                drop(st);
                self.emit(
                    "separation-done",
                    &SeparationDone {
                        song_id,
                        separated_with: with,
                        original_available,
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

    /// Loads a cached stem set (switching back to a model/quality that was
    /// separated before).
    fn load_cached_set(
        &self,
        song_id: &str,
        info: StemsInfo,
    ) -> Result<(Vec<Track>, StemsInfo), String> {
        self.emit(
            "separation-progress",
            &SeparationProgress {
                song_id: song_id.to_string(),
                stage: SeparationStage::Writing,
                fraction: 0.5,
                model_name: info.model_name.clone(),
                device_label: info.device_label.clone(),
            },
        );
        let (tracks, _) = self.read_cached_stems(song_id, &info)?;
        // Mark it as the most recent set.
        let _ = self
            .library
            .update(song_id, |m| m.put_stem_set(info.clone()));
        Ok((tracks, info))
    }

    fn separate_and_cache(
        &self,
        entry: &ModelEntry,
        shifts: u32,
        song_id: &str,
        mix: &[Vec<f32>; 2],
        cancel: &AtomicBool,
    ) -> Result<(Vec<Track>, StemsInfo), String> {
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
        // Overall progress across all passes (separate_shifts maps pass k
        // to [k/shifts, (k+1)/shifts]).
        let stems = sep
            .separate_shifts(
                &[&mix[0], &mix[1]],
                shifts as usize,
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
            "separated {song_id}: {} stems, {:.1}s audio in {:.1}s on {device} ({shifts} pass{})",
            stems.names.len(),
            stems.len as f64 / RATE as f64,
            ts.elapsed().as_secs_f64(),
            if shifts == 1 { "" } else { "es" }
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
        let dir = shredlab_library::stem_set_dir(entry.id(), shifts);
        let format = lock(&self.app_settings).stem_format().cache_format();
        let files = self
            .library
            .write_stems(song_id, &dir, RATE, &views, format)
            .map_err(|e| format!("Cannot write stems: {e}"))?;
        // Keep a copy of the original next to the stems (P1 backing track
        // even if the source file moves later): the source file itself when
        // it is lossy (exact and small), else FLAC.
        let meta = self.library.load(song_id).ok().flatten();
        let have_original = meta.as_ref().is_some_and(|m| m.original.is_some());
        let source = meta.map(|m| PathBuf::from(m.source_path));
        let original_info = if have_original {
            None
        } else {
            match self.library.write_original(
                song_id,
                RATE,
                [&mix[0], &mix[1]],
                source.as_deref().filter(|p| p.is_file()),
            ) {
                Ok(oi) => Some(oi),
                Err(e) => {
                    log!("cannot cache the original: {e}");
                    None
                }
            }
        };
        let info = StemsInfo {
            model_id: entry.id().to_string(),
            model_name: model_name.clone(),
            device_label: device.clone(),
            sample_rate: RATE,
            len: stems.len,
            stems: files,
            shifts,
            dir,
            format,
        };
        self.library
            .update(song_id, |m| {
                m.put_stem_set(info.clone());
                if original_info.is_some() {
                    m.original = original_info;
                }
            })
            .map_err(|e| format!("Cannot update library: {e}"))?;
        log!(
            "wrote stems ({format:?}) in {:.2}s",
            tw.elapsed().as_secs_f64()
        );
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
        Ok((tracks, info))
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
        spawn("shredlab-beats", move || {
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
                    let music_start = detect_music_start(&song_id, &mix, &data.beats);
                    let mut st = lock(&b.st);
                    let current = st.generation == generation
                        && st.song.as_ref().is_some_and(|s| s.meta.song_id == song_id);
                    if !current {
                        return;
                    }
                    if let Some(s) = st.song.as_mut() {
                        s.beats = Some(data);
                        s.music_start = music_start;
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
            shredlab_beats::track(&[&mix[0], &mix[1]], RATE, |inp: &[f32], shape| {
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
            music_start: song.music_start,
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
        let song_duration = song.mix[0].len().min(song.mix[1].len()) as f64 / RATE as f64;
        if st.export.is_some() {
            return Err("An export is already running".into());
        }
        // Same rule and mixer as playback: all stems are passed (stems not
        // in the request have gain 0, so in original mode they are
        // subtracted from the original).
        let mut switches: Vec<(String, bool)> = Vec::new();
        let (tracks, original, mode, gains) = match &song.stems {
            Some(stems) => {
                let mut gains: Vec<(String, f32)> = Vec::new();
                for s in &req.stems {
                    if !stems.iter().any(|t| t.name == s.stem) {
                        return Err(format!("Unknown stem {}", s.stem));
                    }
                    if gains.iter().any(|(n, _)| *n == s.stem) {
                        continue;
                    }
                    let v = if s.volume.is_finite() {
                        s.volume.clamp(0.0, 2.0)
                    } else {
                        1.0
                    };
                    gains.push((s.stem.clone(), v));
                    switches.push((s.stem.clone(), s.on));
                }
                if gains.is_empty() {
                    return Err("Nothing to export: no stems selected".into());
                }
                // Soloed stems are always the plain sum (like playback);
                // like playback, only switched-on rows count as on.
                let n_on = switches.iter().filter(|(_, on)| *on).count();
                let mode = if song.backing_from_original() {
                    audio::choose_mode(stems.len(), n_on, req.solo)
                } else {
                    MixMode::Stems
                };
                (stems.clone(), song.original.clone(), mode, gains)
            }
            // Not separated yet: export the original.
            None => (
                song.tracks(),
                None,
                MixMode::Stems,
                vec![(ORIGINAL.to_string(), 1.0)],
            ),
        };
        // Click and count-in exactly as in playback (same beats, effective
        // downbeats, subdivision, bar length and volume), if requested.
        let mut click = self.click_config(&st);
        click.enabled = req.click;
        click.count_in_beats = if req.count_in {
            st.click.count_in_beats.clamp(1, 64)
        } else {
            0
        };
        let format = match req.format {
            ExportFormat::Wav => audio::ExportFormat::Wav24,
            ExportFormat::Flac => audio::ExportFormat::Flac,
            ExportFormat::Mp3 => audio::ExportFormat::Mp3 { kbps: 320 },
        };
        let render_req = audio::RenderRequest {
            tracks,
            original,
            mode,
            gains,
            speed: (req.speed_percent / 100.0).clamp(0.25, 4.0),
            semitones: req.semitones.clamp(-24.0, 24.0) + req.cents.clamp(-100.0, 100.0) / 100.0,
            click,
            start: req.start_sec.filter(|s| s.is_finite()).unwrap_or(0.0),
            end: req.end_sec.filter(|e| e.is_finite()),
            // "What you hear": the refined parts as in playback, each
            // stereo exception following the exported row's switch.
            refine: if song.stems.is_some() {
                engine_refine(&st.refine, |stem| {
                    switches
                        .iter()
                        .find(|(n, _)| n == stem)
                        .is_none_or(|(_, on)| *on)
                })
            } else {
                Vec::new()
            },
        };
        let range = audio::render_range(&render_req, song_duration)
            .ok_or("Nothing to export: the range is empty")?;
        let cancel = Arc::new(AtomicBool::new(false));
        st.export = Some(cancel.clone());
        drop(st);
        log!(
            "export started: {} ({:?}, {} of {} stems, {:?} mode, speed {}, shift {:.2} st, click {}, count-in {}, range {:.2}-{:.2} s, refined [{}])",
            path.display(),
            format,
            render_req.gains.len(),
            render_req.tracks.len(),
            render_req.mode,
            render_req.speed,
            render_req.semitones,
            render_req.click.enabled,
            render_req.click.count_in_beats,
            range.0,
            range.1,
            render_req
                .refine
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let b = self.clone();
        spawn("shredlab-export", move || {
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
        let out = audio::render(req, &mut |f| progress(0.9 * f, false), cancel)
            .map_err(|e| format!("Render failed: {e}"))?;
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        // No whole-song normalization: `encode` applies a look-ahead peak
        // limiter, so the export matches the level heard in playback.
        let peak = audio::peak(&out);
        if peak > 0.999 {
            log!("export: peak {peak:.3}, limited by the encoder");
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

    // ------------------------------------------------------------ refine

    /// The Refine panel's EQ presets.
    pub fn refine_presets(&self) -> Vec<RefinePresetInfo> {
        shredlab_dsp::refine::EQ_PRESETS
            .iter()
            .map(|p| RefinePresetInfo {
                id: p.id.into(),
                name: p.name.into(),
                eq: p.eq,
            })
            .collect()
    }

    /// Sets one stem's refine parameters: heard at once (smoothed), saved
    /// with the song shortly after the last change.
    pub fn set_refine(&self, stem: &str, params: RefineParams) -> Result<(), String> {
        if !STEMS.contains(&stem) {
            return Err(format!("Unknown stem {stem}"));
        }
        let mut st = lock(&self.st);
        let song_id = st.song.as_ref().ok_or("No song open")?.meta.song_id.clone();
        if params.is_identity() {
            st.refine.remove(stem);
        } else {
            st.refine.insert(stem.to_string(), params);
        }
        // Refine (with the exception following the switch) and gains: an
        // exception on an off row makes it audible, and back.
        self.apply_gains(&st);
        st.refine_dirty = Some((song_id, Instant::now()));
        Ok(())
    }

    /// Stereo-field analysis for the stem whose Refine panel is open, or off.
    pub fn set_refine_analysis(&self, stem: Option<String>) {
        let mut st = lock(&self.st);
        st.analysis = stem.filter(|s| STEMS.contains(&s.as_str()));
        if let Some(e) = &self.engine {
            e.set_analysis(st.analysis.as_deref());
        }
    }

    /// Writes pending refine settings to `song.json` (after
    /// [`REFINE_SAVE_DELAY`] of no changes, or now with `force`).
    fn flush_refine_save(&self, force: bool) {
        let (song_id, refine) = {
            let mut st = lock(&self.st);
            match &st.refine_dirty {
                Some((_, t)) if force || t.elapsed() >= REFINE_SAVE_DELAY => {}
                _ => return,
            }
            let Some((id, _)) = st.refine_dirty.take() else {
                return;
            };
            let refine = st.refine.clone();
            if let Some(s) = st.song.as_mut().filter(|s| s.meta.song_id == id) {
                s.meta.refine = refine.clone();
            }
            (id, refine)
        };
        if let Err(e) = self.library.update(&song_id, |m| m.refine = refine) {
            log!("cannot save refine settings of {song_id}: {e}");
        }
    }

    // ------------------------------------------------------------ strength

    /// Sets the song's separation strength (saved at once) and re-derives
    /// the stems in the background if needed. Returns the job id (0 when
    /// nothing has to be computed, e.g. not separated yet).
    pub fn set_strength(self: &Arc<Self>, strength: StrengthSetting) -> Result<u64, String> {
        let s = StrengthSetting {
            level: strength.level.min(STRENGTH_P.len() as u8 - 1),
            smooth: strength.smooth,
        };
        let mut st = lock(&self.st);
        let song = st.song.as_mut().ok_or("No song open")?;
        song.meta.strength = s;
        let song_id = song.meta.song_id.clone();
        let job = self.start_strength_locked(&mut st);
        drop(st);
        self.library
            .update(&song_id, |m| m.strength = s)
            .map_err(|e| e.to_string())?;
        Ok(job)
    }

    fn cancel_strength_locked(&self, st: &mut State) {
        if let Some(j) = st.strength_job.take() {
            j.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Starts deriving the open song's stems for its saved strength, unless
    /// the loaded set already has it. Cancels a running derivation. Returns
    /// the job id, or 0 when there is nothing to do.
    fn start_strength_locked(self: &Arc<Self>, st: &mut State) -> u64 {
        self.cancel_strength_locked(st);
        let Some(song) = st.song.as_ref() else {
            return 0;
        };
        let want = song.meta.strength;
        let (Some(info), Some(_)) = (song.stems_info.clone(), song.stems.as_ref()) else {
            return 0;
        };
        if same_strength(want, song.loaded_strength) {
            return 0;
        }
        st.next_strength_job += 1;
        let id = st.next_strength_job;
        let cancel = Arc::new(AtomicBool::new(false));
        st.strength_job = Some(StrengthJob {
            id,
            cancel: cancel.clone(),
        });
        let song_id = song.meta.song_id.clone();
        let original = song.original.clone();
        let generation = st.generation;
        log!(
            "strength of {song_id}: level {} (p {}{}) started, job {id}",
            want.level,
            STRENGTH_P[want.level as usize],
            if want.smooth && want.level > 0 {
                ", smooth"
            } else {
                ""
            }
        );
        let b = self.clone();
        spawn("shredlab-strength", move || {
            b.run_strength(id, song_id, info, original, want, generation, cancel);
        });
        id
    }

    #[allow(clippy::too_many_arguments)]
    fn run_strength(
        self: &Arc<Self>,
        job: u64,
        song_id: String,
        info: StemsInfo,
        original: Option<Track>,
        want: StrengthSetting,
        generation: u64,
        cancel: Arc<AtomicBool>,
    ) {
        let t0 = Instant::now();
        let mut th = Throttle::new();
        let progress = |th: &mut Throttle, f: f32, force: bool| {
            if let Some(f) = th.next(f, force) {
                self.emit(
                    "strength-progress",
                    &StrengthProgress {
                        song_id: song_id.clone(),
                        job,
                        fraction: f,
                    },
                );
            }
        };
        progress(&mut th, 0.0, true);
        let result: Result<Vec<Track>, String> = (|| {
            // The base set from disk (the derived set replaces it in memory).
            let (base, _) = self.read_cached_stems(&song_id, &info)?;
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            progress(&mut th, 0.3, true);
            if want.level == 0 {
                return Ok(base);
            }
            let o = original.ok_or(
                "Separation strength needs the original mix, which isn't available for this song.",
            )?;
            let views: Vec<[&[f32]; 2]> = base
                .iter()
                .map(|t| [&t.channels[0][..], &t.channels[1][..]])
                .collect();
            let len = o.channels[0].len();
            if views.iter().any(|v| v[0].len() != len || v[1].len() != len) {
                return Err("stems and original differ in length".into());
            }
            let out = apply_strength(
                [&o.channels[0][..], &o.channels[1][..]],
                &views,
                STRENGTH_P[want.level as usize],
                &StrengthOptions {
                    smooth: want.smooth,
                    ..Default::default()
                },
                &mut |f| progress(&mut th, 0.3 + 0.7 * f, false),
                &cancel,
            )
            .map_err(|_| "cancelled".to_string())?;
            Ok(base
                .iter()
                .zip(out)
                .map(|(t, [l, r])| Track {
                    name: t.name.clone(),
                    channels: [Arc::new(l), Arc::new(r)],
                    sample_rate: t.sample_rate,
                })
                .collect())
        })();
        let mut st = lock(&self.st);
        let current = st.strength_job.as_ref().is_some_and(|j| j.id == job)
            && st.generation == generation
            && !cancel.load(Ordering::Relaxed);
        if !current {
            log!("strength job {job} dropped (superseded or cancelled)");
            drop(st);
            self.emit(
                "strength-error",
                &StrengthError {
                    song_id,
                    job,
                    message: "Cancelled".into(),
                    cancelled: true,
                },
            );
            return;
        }
        st.strength_job = None;
        match result {
            Ok(tracks) => {
                if let Some(song) = st.song.as_mut() {
                    song.stems = Some(tracks);
                    song.loaded_strength = want;
                    if let Some(e) = &self.engine {
                        song.load_into(e);
                    }
                }
                self.apply_gains(&st);
                drop(st);
                log!(
                    "strength job {job} done in {:.2}s",
                    t0.elapsed().as_secs_f64()
                );
                progress(&mut th, 1.0, true);
                self.emit(
                    "strength-done",
                    &StrengthDone {
                        song_id,
                        job,
                        strength: want,
                    },
                );
            }
            Err(msg) => {
                drop(st);
                log!("strength job {job} failed: {msg}");
                self.emit(
                    "strength-error",
                    &StrengthError {
                        song_id,
                        job,
                        message: msg,
                        cancelled: false,
                    },
                );
            }
        }
    }

    /// True while a strength derivation runs.
    pub fn strength_running(&self) -> bool {
        lock(&self.st).strength_job.is_some()
    }

    /// Autotest helper, unstretched, for checking an export of the stems
    /// `on`: the P1 backing track (original minus the other stems) and the
    /// plain sum of `on`. `None` without stems or original.
    #[cfg(debug_assertions)]
    pub fn debug_mix_references(&self, on: &[&str]) -> Option<[[Vec<f32>; 2]; 2]> {
        let st = lock(&self.st);
        let song = st.song.as_ref()?;
        let o = song.original.as_ref()?;
        let stems = song.stems.as_ref()?;
        let p1 = [0, 1].map(|c| {
            let mut v: Vec<f32> = o.channels[c].to_vec();
            for t in stems.iter().filter(|t| !on.contains(&t.name.as_str())) {
                for (s, y) in v.iter_mut().zip(t.channels[c].iter()) {
                    *s -= y;
                }
            }
            v
        });
        let sum = [0, 1].map(|c| {
            let mut v = vec![0.0f32; o.channels[c].len()];
            for t in stems.iter().filter(|t| on.contains(&t.name.as_str())) {
                for (s, y) in v.iter_mut().zip(t.channels[c].iter()) {
                    *s += y;
                }
            }
            v
        });
        Some([p1, sum])
    }
}

impl Backend {
    /// Autotest helper: the engine's diagnostics.
    #[cfg(debug_assertions)]
    pub fn debug_engine_stats(&self) -> Option<audio::EngineStats> {
        self.engine.as_ref().map(|e| e.stats())
    }

    /// Autotest helper, unstretched: the refined backing mix in P1 mode,
    /// computed offline. `rows` are the heard rows (gain 1) with their on
    /// switch; every other stem is silent:
    /// `original + Σ_rows refine(stem, switch) − Σ_all stem`.
    #[cfg(debug_assertions)]
    pub fn debug_refine_reference(&self, rows: &[(&str, bool)]) -> Option<[Vec<f32>; 2]> {
        use shredlab_dsp::refine::refine_offline;
        let st = lock(&self.st);
        let song = st.song.as_ref()?;
        let o = song.original.as_ref()?;
        let mut out = [o.channels[0].to_vec(), o.channels[1].to_vec()];
        for t in song.stems.as_ref()? {
            let row = rows.iter().find(|(n, _)| *n == t.name);
            let r = row.map(|&(_, on)| {
                let mut p = st.refine.get(&t.name).copied().unwrap_or_default();
                p.pan.invert = on;
                refine_offline(&p, RATE, &t.channels[0], &t.channels[1])
            });
            for c in 0..2 {
                for (i, v) in out[c].iter_mut().enumerate() {
                    let sv = t.channels[c].get(i).copied().unwrap_or(0.0);
                    let rv = r
                        .as_ref()
                        .map_or(0.0, |r| r[c].get(i).copied().unwrap_or(0.0));
                    *v += rv - sv;
                }
            }
        }
        Some(out)
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
        let none = BTreeMap::new();
        let g = gains_for(&mix, &none, &names);
        assert_eq!(g[0], ("vocals".into(), 0.5));
        assert_eq!(g[1].1, 0.0);
        assert_eq!(g[2].1, 0.8);
        // An off row with a stereo exception plays (the window) at its
        // volume; an EQ alone doesn't make it audible.
        let mut refine = BTreeMap::new();
        let mut p = RefineParams::default();
        p.eq.low_cut.enabled = true;
        refine.insert("bass".to_string(), p);
        assert_eq!(gains_for(&mix, &refine, &names)[1].1, 0.0);
        p.pan = shredlab_dsp::refine::PanParams::left();
        refine.insert("bass".to_string(), p);
        assert_eq!(gains_for(&mix, &refine, &names)[1].1, 1.0);
        // The engine gets the window inverted on rows that are on.
        let er = engine_refine(&refine, |s| s != "bass");
        assert!(!er[0].1.pan.invert);
        let er = engine_refine(&refine, |_| true);
        assert!(er[0].1.pan.invert && er[0].1.pan.center == -1.0);
        // Solo wins over on/off, even for a stem that is off.
        let mix = vec![
            ms("vocals", true, 0.5, false),
            ms("bass", false, 0.7, true),
            ms("drums", true, 0.8, false),
        ];
        let g = gains_for(&mix, &none, &names);
        assert_eq!(g.iter().map(|x| x.1).collect::<Vec<_>>(), [0.0, 0.7, 0.0]);
        // With a solo, an exception doesn't make an unsoloed row audible.
        let g = gains_for(&mix, &refine, &["vocals".into(), "drums".into()]);
        assert_eq!(g.iter().map(|x| x.1).collect::<Vec<_>>(), [0.0, 0.0]);
        // Original track is always unity.
        assert_eq!(gains_for(&mix, &none, &["original".into()])[0].1, 1.0);
    }

    #[test]
    fn p1_mix_mode_rule() {
        let names: Vec<String> = STEMS.iter().map(|s| s.to_string()).collect();
        let all_on: Vec<StemMixState> = STEMS.iter().map(|s| ms(s, true, 1.0, false)).collect();
        assert_eq!(mix_mode(&all_on, &names, true), MixMode::Original);
        // No original loaded: always the plain sum.
        assert_eq!(mix_mode(&all_on, &names, false), MixMode::Stems);
        // "All but guitar": 5 of 6 on.
        let mut m = all_on.clone();
        m[1].on = false;
        assert_eq!(mix_mode(&m, &names, true), MixMode::Original);
        // An on stem at volume 0 still counts as on.
        m[0].volume = 0.0;
        assert_eq!(mix_mode(&m, &names, true), MixMode::Original);
        // 3 of 6 on: still original; 2 of 6: sum.
        for x in m.iter_mut().take(3) {
            x.on = false;
        }
        assert_eq!(mix_mode(&m, &names, true), MixMode::Original);
        m[3].on = false;
        assert_eq!(mix_mode(&m, &names, true), MixMode::Stems);
        // Solo always gives the sum, even with everything on.
        let mut m = all_on.clone();
        m[2].solo = true;
        assert_eq!(mix_mode(&m, &names, true), MixMode::Stems);
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
        let dir =
            std::env::temp_dir().join(format!("shredlab-backend-{}-{name}", std::process::id()));
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
    fn click_volume_is_saved_and_applied() {
        let (b, _) = test_backend("click-volume");
        assert_eq!(
            b.app_settings().click_volume,
            settings::DEFAULT_CLICK_VOLUME
        );
        assert_eq!(b.set_click_volume(0.35).unwrap().click_volume, 0.35);
        assert_eq!(AppSettings::load(b.data_dir()).0.click_volume, Some(0.35));
        assert_eq!(b.click_config(&lock(&b.st)).volume, 0.35);
        // Clamped; non-finite is refused and leaves the setting alone.
        assert_eq!(b.set_click_volume(3.0).unwrap().click_volume, 1.0);
        assert!(b.set_click_volume(f32::NAN).is_err());
        assert_eq!(b.app_settings().click_volume, 1.0);
    }

    #[test]
    fn master_volume_is_saved_on_request() {
        let (b, _) = test_backend("master-volume");
        assert_eq!(
            b.app_settings().master_volume,
            settings::DEFAULT_MASTER_VOLUME
        );
        // While dragging: effective at once, not written.
        assert_eq!(b.set_master_volume(0.4, false).unwrap().master_volume, 0.4);
        assert_eq!(AppSettings::load(b.data_dir()).0.master_volume, None);
        // On release: saved.
        assert_eq!(b.set_master_volume(0.6, true).unwrap().master_volume, 0.6);
        assert_eq!(AppSettings::load(b.data_dir()).0.master_volume, Some(0.6));
        assert_eq!(b.set_master_volume(-1.0, true).unwrap().master_volume, 0.0);
        assert!(b.set_master_volume(f32::INFINITY, true).is_err());
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
        shredlab_io::write_wav(&wav, RATE, &[&tone, &tone], shredlab_io::WavFormat::F32).unwrap();
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
            solo: false,
            speed_percent: 100.0,
            semitones: 0.0,
            cents: 0.0,
            click: false,
            count_in: false,
            start_sec: None,
            end_sec: None,
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

    fn wait_export(b: &Arc<Backend>) {
        let t0 = Instant::now();
        while b.export_running() && t0.elapsed() < Duration::from_secs(30) {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn export_req(
        path: &Path,
        count_in: bool,
        start: Option<f64>,
        end: Option<f64>,
    ) -> ExportRequest {
        ExportRequest {
            path: path.display().to_string(),
            format: ExportFormat::Wav,
            stems: vec![],
            solo: false,
            speed_percent: 100.0,
            semitones: 0.0,
            cents: 0.0,
            click: false,
            count_in,
            start_sec: start,
            end_sec: end,
        }
    }

    #[test]
    fn music_start_setting_and_export_range() {
        let (b, _) = test_backend("music-start");
        std::fs::create_dir_all(b.data_dir()).unwrap();
        // 4 s of quiet talk, then 8 s of loud tone; 0.5 s beats from 0.25 s.
        let wav = b.data_dir().join("talk.wav");
        let n = RATE as usize * 12;
        let x: Vec<f32> = (0..n)
            .map(|i| {
                let a = if i < 4 * RATE as usize { 0.001 } else { 0.5 };
                a * (i as f32 * 0.05).sin()
            })
            .collect();
        shredlab_io::write_wav(&wav, RATE, &[&x, &x], shredlab_io::WavFormat::F32).unwrap();
        b.open_song(wav.to_str().unwrap()).unwrap();
        let beats: Vec<f64> = (0..24).map(|i| 0.25 + i as f64 * 0.5).collect();
        {
            let mut st = lock(&b.st);
            let song = st.song.as_mut().unwrap();
            song.music_start = detect_music_start("talk", &song.mix, &beats);
            song.beats = Some(BeatData {
                beats: beats.clone(),
                downbeats: vec![],
                bpm: Some(120.0),
                beats_per_bar: Some(4),
            });
        }
        let ms = b.get_beats().unwrap().unwrap().music_start.unwrap();
        assert!((ms - (4.0 - music_start::PRE_ROLL)).abs() < 0.011, "{ms}");
        b.set_click(ClickState {
            enabled: false,
            count_in_enabled: true,
            count_in_beats: 4,
            subdivision: 1,
            beats_per_bar: 4,
            volume: 0.5,
        });
        // On by default: the playback click config carries it.
        assert!(b.app_settings().count_in_to_music_start);
        assert_eq!(b.click_config(&lock(&b.st)).music_start, Some(ms));

        // Whole-song export with count-in starts at the music start: the
        // count-in leads into the beat at 4.25 s.
        let out = b.data_dir().join("whole.wav");
        b.export_mix(export_req(&out, true, None, None)).unwrap();
        wait_export(&b);
        let len = |p: &Path| shredlab_io::decode_file(p).unwrap().channels[0].len();
        let lead = 4.0 * 0.5 - (4.25 - ms);
        let want =
            ((12.0 - ms) * RATE as f64).round() as usize + (lead * RATE as f64).round() as usize;
        assert!(len(&out).abs_diff(want) <= 1, "{} vs {want}", len(&out));

        // A range from the middle: exactly its length (no count-in).
        let out = b.data_dir().join("range.wav");
        b.export_mix(export_req(&out, false, Some(5.0), Some(7.5)))
            .unwrap();
        wait_export(&b);
        assert_eq!(len(&out), (2.5 * RATE as f64) as usize);

        // Setting off: persisted, and the whole song with count-in starts at 0.
        assert!(
            !b.set_count_in_to_music_start(false)
                .unwrap()
                .count_in_to_music_start
        );
        assert_eq!(
            AppSettings::load(b.data_dir()).0.count_in_to_music_start,
            Some(false)
        );
        assert_eq!(b.click_config(&lock(&b.st)).music_start, None);
        let out = b.data_dir().join("off.wav");
        b.export_mix(export_req(&out, true, None, None)).unwrap();
        wait_export(&b);
        let lead = 4.0 * 0.5 - 0.25;
        let want = 12 * RATE as usize + (lead * RATE as f64).round() as usize;
        assert!(len(&out).abs_diff(want) <= 1, "{} vs {want}", len(&out));
        // Empty range: refused.
        assert!(b
            .export_mix(export_req(&out, false, Some(3.0), Some(3.0)))
            .is_err());
    }

    /// A song with three cached stems (guitar hard-left 440 Hz + hard-right
    /// 660 Hz, centred bass and drums) and a cached original with a leftover.
    fn song_with_stems(b: &Arc<Backend>) -> (String, PathBuf) {
        std::fs::create_dir_all(b.data_dir()).unwrap();
        let n = RATE as usize * 3;
        let t = |f: f32, a: f32| -> Vec<f32> {
            (0..n)
                .map(|i| a * (i as f32 * f * std::f32::consts::TAU / RATE as f32).sin())
                .collect()
        };
        let (gl, gr, ba, dr, res) = (
            t(440.0, 0.3),
            t(660.0, 0.25),
            t(82.0, 0.4),
            t(2000.0, 0.05),
            t(5100.0, 0.02),
        );
        let ol: Vec<f32> = (0..n).map(|i| gl[i] + ba[i] + dr[i] + res[i]).collect();
        let or: Vec<f32> = (0..n).map(|i| gr[i] + ba[i] + dr[i] + res[i]).collect();
        let wav = b.data_dir().join("song.wav");
        shredlab_io::write_wav(&wav, RATE, &[&ol, &or], shredlab_io::WavFormat::F32).unwrap();
        let info = b.open_song(wav.to_str().unwrap()).unwrap();
        let id = info.song_id.clone();
        let dir = shredlab_library::stem_set_dir("test", 1);
        let files = b
            .library
            .write_stems(
                &id,
                &dir,
                RATE,
                &[
                    ("guitar", [&gl[..], &gr[..]]),
                    ("bass", [&ba[..], &ba[..]]),
                    ("drums", [&dr[..], &dr[..]]),
                ],
                shredlab_library::CacheFormat::Wav,
            )
            .unwrap();
        let oi = b
            .library
            .write_original(&id, RATE, [&ol, &or], Some(&wav))
            .unwrap();
        b.library
            .update(&id, |m| {
                m.original = Some(oi);
                m.put_stem_set(StemsInfo {
                    model_id: "test".into(),
                    model_name: "Test".into(),
                    device_label: "CPU".into(),
                    sample_rate: RATE,
                    len: n,
                    stems: files,
                    shifts: 1,
                    dir,
                    format: shredlab_library::CacheFormat::Wav,
                });
            })
            .unwrap();
        (id, wav)
    }

    fn wait_strength(b: &Arc<Backend>) {
        let t0 = Instant::now();
        while b.strength_running() && t0.elapsed() < Duration::from_secs(30) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    #[allow(clippy::needless_range_loop)]
    fn refine_and_strength_persist_and_export_what_you_hear() {
        use shredlab_dsp::refine::{pan_offline, PanParams};
        let (b, events) = test_backend("refine");
        let (id, wav) = song_with_stems(&b);
        let info = b.open_song(wav.to_str().unwrap()).unwrap();
        assert!(info.stems_cached && info.original_available);
        assert!(info.refine.is_empty() && info.strength_job == 0);

        // Refine: unknown stem refused; the guitar's exception "left" is
        // saved with the song (without any switch-derived inversion).
        let left = RefineParams {
            pan: PanParams::left(),
            ..RefineParams::default()
        };
        assert!(b.set_refine("banjo", left).is_err());
        b.set_refine("guitar", left).unwrap();
        b.set_refine("bass", RefineParams::default()).unwrap();
        b.flush_refine_save(true);
        let again = b.open_song(wav.to_str().unwrap()).unwrap();
        assert_eq!(again.refine.len(), 1);
        assert_eq!(again.refine["guitar"], left);

        // Export "All but guitar": the guitar row is off with the exception
        // "left", so only the left guitar plays: original minus the RIGHT
        // guitar.
        let (stems, orig) = {
            let st = lock(&b.st);
            let s = st.song.as_ref().unwrap();
            (s.stems.clone().unwrap(), s.original.clone().unwrap())
        };
        let g = stems.iter().find(|t| t.name == "guitar").unwrap();
        let sel = pan_offline(&left.pan, RATE, &g.channels[0], &g.channels[1]);
        let row = |stem: &str, on: bool| ExportStem {
            stem: stem.into(),
            volume: 1.0,
            on,
        };
        let export = |name: &str, stems: Vec<ExportStem>| {
            let out = b.data_dir().join(name);
            let mut req = export_req(&out, false, None, None);
            req.stems = stems;
            b.export_mix(req).unwrap();
            wait_export(&b);
            shredlab_io::decode_file(&out).unwrap()
        };
        let got = export(
            "allbut.wav",
            vec![row("guitar", false), row("bass", true), row("drums", true)],
        );
        for c in 0..2 {
            let err = (0..got.channels[c].len())
                .map(|i| {
                    let want = orig.channels[c][i] - g.channels[c][i] + sel[c][i];
                    (got.channels[c][i] - want).abs()
                })
                .fold(0.0f32, f32::max);
            // 24-bit file.
            assert!(err < 1e-4, "ch {c}: {err}");
        }
        let ms_err = |x: &[f32], y: &dyn Fn(usize) -> f32| {
            x.iter()
                .enumerate()
                .map(|(i, a)| (a - y(i)).powi(2))
                .sum::<f32>()
                / x.len() as f32
        };
        // The left guitar is still there (left channel ~ the original, the
        // mask leaks a little: within -36 dB), the right guitar is gone.
        let e = ms_err(&got.channels[0], &|i| orig.channels[0][i]);
        assert!(e < 1e-5, "{e}");
        let e = ms_err(&got.channels[1], &|i| {
            orig.channels[1][i] - g.channels[1][i]
        });
        assert!(e < 1e-5, "{e}");
        // All on (guitar ON except left): the original minus the left guitar.
        let got = export(
            "allon.wav",
            vec![row("guitar", true), row("bass", true), row("drums", true)],
        );
        let e = ms_err(&got.channels[0], &|i| {
            orig.channels[0][i] - g.channels[0][i]
        });
        assert!(e < 1e-5, "{e}");
        let e = ms_err(&got.channels[1], &|i| orig.channels[1][i]);
        assert!(e < 1e-5, "{e}");

        // Strength: derived stems sum to the original, saved, re-applied on open.
        let job = b
            .set_strength(StrengthSetting {
                level: 2,
                smooth: false,
            })
            .unwrap();
        assert!(job > 0);
        wait_strength(&b);
        // The job clears its running flag before it emits strength-done.
        let t0 = Instant::now();
        while !lock(&events).iter().any(|e| e == "strength-done")
            && t0.elapsed() < Duration::from_secs(5)
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(lock(&events).iter().any(|e| e == "strength-done"));
        let sum_err = |b: &Arc<Backend>| {
            let st = lock(&b.st);
            let s = st.song.as_ref().unwrap();
            let stems = s.stems.as_ref().unwrap();
            let o = s.original.as_ref().unwrap();
            (0..o.channels[0].len())
                .map(|i| {
                    let sum: f32 = stems.iter().map(|t| t.channels[0][i]).sum();
                    (sum - o.channels[0][i]).abs()
                })
                .fold(0.0f32, f32::max)
        };
        assert!(sum_err(&b) < 1e-4, "{}", sum_err(&b));
        // Smooth: no longer sums to the original (the leftover is not shared out).
        b.set_strength(StrengthSetting {
            level: 2,
            smooth: true,
        })
        .unwrap();
        wait_strength(&b);
        assert!(sum_err(&b) > 1e-3);
        // Same setting again: nothing to do.
        assert_eq!(
            b.set_strength(StrengthSetting {
                level: 2,
                smooth: true,
            })
            .unwrap(),
            0
        );
        let reopened = b.open_song(wav.to_str().unwrap()).unwrap();
        assert_eq!(
            reopened.strength,
            StrengthSetting {
                level: 2,
                smooth: true
            }
        );
        assert!(reopened.strength_job > 0);
        wait_strength(&b);
        assert_eq!(
            lock(&b.st).song.as_ref().unwrap().loaded_strength,
            reopened.strength
        );
        // Back to Natural: the cached stems again.
        b.set_strength(StrengthSetting::default()).unwrap();
        wait_strength(&b);
        let st = lock(&b.st);
        let s = st.song.as_ref().unwrap();
        assert_eq!(
            s.stems.as_ref().unwrap()[0].channels[0][..],
            stems[0].channels[0][..]
        );
        assert_eq!(s.meta.song_id, id);
    }
}
