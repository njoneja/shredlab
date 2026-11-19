// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! App state and jobs, independent of Tauri (events go through a [`Sink`]),
//! so the same code runs in the app, the debug autotest and unit tests.
//!
//! Threads:
//! - `open_song` runs on a blocking worker (decode + resample, or reading
//!   the cached stems).
//! - separation: one background thread per run, canceled through an
//!   `AtomicBool`. The loaded `Separator` is kept between runs.
//! - beat tracking: one background thread per song (Beat This! on ORT CPU).
//! - export: one background thread per export.
//! - strength: one background thread per re-derivation (canceled when a
//!   newer one starts or the song changes).
//! - chords: one background thread per (song, stem set).
//! - notes: one background thread at a time, one stem per run; each run
//!   starts the next stem when it ends. Stems other than the bass run the
//!   Basic Pitch note model (ORT CPU, loaded on first use and kept).
//! - position: a 30 Hz emitter thread (also the stereo-field events of the
//!   open Refine panel, the debounced save of refine settings, the debounced
//!   switch of the notes to a changed stereo mask and the check for a
//!   failed tuner input).
//! - live tuner: `shredlab-audio`'s input worker runs the pitch detector
//!   and emits `tuner-reading` itself (see `tuner.rs`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::Serialize;
use shredlab_library::{BeatData, Library, NotesData, NotesFast, SongMeta, StemNotes, StemsInfo};
use shredlab_separation::selftest::Cache;
use shredlab_separation::{
    BeatModel, DevicePref, Ep, LoadOptions, ModelEntry, ModelRegistry, ModelSource, NoteModel,
    Separator,
};

use crate::audio::{self, ClickConfig, Engine, MixMode, Track};
use crate::log;
use crate::music_start;
use crate::settings::{self, AppSettings};
use crate::tuner;
use crate::types::*;
use shredlab_dsp::refine::PanParams;
use shredlab_dsp::strength::{apply_strength, StrengthOptions};

/// Event sink: `(event name, JSON payload)`.
pub type Sink = Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>;

/// Sample rate everything is played and cached at (the model rate).
pub const RATE: u32 = shredlab_io::MODEL_SAMPLE_RATE;
const ORIGINAL: &str = "original";
const RECENT_MAX: usize = 20;
/// Mask exponent per strength level (0 = Natural: the cached stems).
pub const STRENGTH_P: [f32; 5] = [1.0, 1.5, 2.0, 3.0, 4.0];
/// Refine and per-song UI settings are saved this long after the last change.
const SONG_SAVE_DELAY: Duration = Duration::from_millis(600);
/// Stems with notes, in the order the notes job takes them.
const NOTES_STEMS: [&str; 5] = ["guitar", "bass", "vocals", "piano", "other"];
/// Sources kept per stem in `song.json` (oldest dropped first).
const NOTES_KEEP: usize = 4;
/// The notes follow a changed Refine stereo mask this long after the last
/// change (dragging the window does not start a job per step).
const NOTES_DELAY: Duration = Duration::from_secs(1);
/// Loop results kept per song in `loop_notes.json` (oldest dropped first).
const LOOP_NOTES_KEEP: usize = 20;
/// `detect_loop_notes` error of a request a newer one replaced before it
/// started detecting.
pub const LOOP_SUPERSEDED: &str = "Superseded by a newer loop request";
const NO_DEVICE: &str =
    "No audio output device found. Connect speakers or headphones and restart ShredLab (separation and export still work).";

pub struct Config {
    pub data_dir: PathBuf,
    pub model_dirs: Vec<(PathBuf, ModelSource)>,
    pub beat_model: Option<PathBuf>,
    /// Basic Pitch (`shredlab-notes` falls back to its pitch tracker
    /// without it).
    pub note_model: Option<PathBuf>,
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
    /// original is unavailable), for the waveform and beat tracking. Shared
    /// with `original`, not copied.
    mix: Mix,
    /// True when `mix` is the original (decoded or cached), i.e. the song
    /// can be (re-)separated.
    mix_is_original: bool,
    /// The original as a track: played before separation, and the source
    /// of the backing track built from it once stems are loaded. `None`
    /// when it is unavailable or not sample-aligned with the stems.
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
            (None, None) => vec![original_track(&self.mix)],
        }
    }

    /// Loads the song into the engine: stems plus the original (for the
    /// backing track), or the original alone before separation.
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
            notes_fast: m.notes_fast,
            notes_quiet: m.notes_quiet,
            tuning: m.tuning.clone(),
            chords: self.current_chords().cloned(),
            // Filled in by the caller, which holds the notes' stereo masks.
            notes: Vec::new(),
            notes_pending: Vec::new(),
            notes_running: None,
        }
    }

    /// Key of the loaded stem set for [`ChordsData::stems`].
    fn chords_stems_key(&self) -> Option<String> {
        self.stems.as_ref()?;
        self.stems_info
            .as_ref()
            .map(|i| format!("{}_s{}", i.model_id, i.shifts))
    }

    /// The saved chords, if they are current for the loaded stem set.
    fn current_chords(&self) -> Option<&ChordsData> {
        let key = self.chords_stems_key()?;
        self.meta
            .chords
            .as_ref()
            .filter(|c| c.version >= shredlab_chords::VERSION && c.stems == key)
    }

    /// The pitched stems of the loaded set with the source their notes are
    /// computed from now, in job order. Empty before separation.
    fn notes_sources(&self, pans: &BTreeMap<String, PanParams>) -> Vec<(String, String)> {
        let (Some(key), Some(stems)) = (self.chords_stems_key(), self.stems.as_ref()) else {
            return Vec::new();
        };
        NOTES_STEMS
            .iter()
            .filter(|n| stems.iter().any(|t| t.name == **n))
            .map(|n| {
                let detail = notes_detail_for(n, &self.meta);
                let src = notes_source(&key, self.loaded_strength, pans.get(*n), detail);
                (n.to_string(), src)
            })
            .collect()
    }

    /// The saved notes of `stem` for `source`, if current.
    fn saved_notes(&self, stem: &str, source: &str) -> Option<&StemNotes> {
        self.meta
            .notes
            .as_ref()
            .filter(|n| n.version == shredlab_notes::VERSION)?
            .entries
            .iter()
            .rev()
            .find(|e| e.stem == stem && e.source == source)
    }

    /// The saved notes of each pitched stem for its current source.
    fn current_notes(&self, pans: &BTreeMap<String, PanParams>) -> Vec<StemNotes> {
        self.notes_sources(pans)
            .iter()
            .filter_map(|(stem, src)| self.saved_notes(stem, src).cloned())
            .collect()
    }

    /// Pitched stems (with their source) whose current notes are missing.
    fn notes_missing(&self, pans: &BTreeMap<String, PanParams>) -> Vec<(String, String)> {
        self.notes_sources(pans)
            .into_iter()
            .filter(|(stem, src)| self.saved_notes(stem, src).is_none())
            .collect()
    }
}

/// The notes crate's stem kind for an app stem name (`None`: no notes).
fn notes_kind(stem: &str) -> Option<shredlab_notes::StemKind> {
    use shredlab_notes::StemKind;
    Some(match stem {
        "bass" => StemKind::Bass,
        "guitar" => StemKind::Guitar,
        "vocals" => StemKind::Vocals,
        "piano" | "keys" => StemKind::Keys,
        "other" => StemKind::Other,
        _ => return None,
    })
}

/// The note detection settings that apply to `stem`: the song's for the
/// stems on the note model, the defaults for the bass (the pitch tracker
/// ignores them).
fn notes_detail_for(stem: &str, meta: &SongMeta) -> shredlab_notes::Detail {
    notes_detail(stem, meta.notes_fast, meta.notes_quiet)
}

/// The note detection settings `fast` and `quiet` as they apply to `stem`
/// (the defaults for the bass).
fn notes_detail(stem: &str, fast: NotesFast, quiet: bool) -> shredlab_notes::Detail {
    use shredlab_notes::{Detail, Fast};
    match notes_kind(stem) {
        Some(k) if k.uses_model() => Detail {
            fast: match fast {
                NotesFast::Fewer => Fast::Fewer,
                NotesFast::Normal => Fast::Normal,
                NotesFast::More => Fast::More,
            },
            quiet,
        },
        _ => Detail::default(),
    }
}

/// The offset the notes are named relative to: the measured one, or A440
/// when the measurement is unreliable (it is noise then, as for chords).
fn notes_offset(tuning: &shredlab_library::TuningData) -> f64 {
    if tuning.confidence >= shredlab_tuning::MIN_CONFIDENCE
        && tuning.peaks >= shredlab_tuning::MIN_PEAKS
    {
        tuning.offset_cents as f64
    } else {
        0.0
    }
}

/// What a stem's notes are computed from (`StemNotes::source`): the stem
/// set (`<model id>_s<shifts>`), the separation strength of the loaded
/// stems (`|st<level>`, `s` added for smooth), Refine's stereo mask when
/// it is on (`|pan<center>:<width>:<softness>`, two decimals), the Fast
/// runs setting when it is not Normal (`|ffewer`, `|fmore`) and `|q` when
/// quiet notes are included, so keys from before these settings stay
/// valid. Refine's EQ is not part of it: the notes ignore it.
pub fn notes_source(
    stems_key: &str,
    strength: StrengthSetting,
    pan: Option<&PanParams>,
    detail: shredlab_notes::Detail,
) -> String {
    let mut s = format!("{stems_key}|st{}", strength.level);
    if strength.level > 0 && strength.smooth {
        s.push('s');
    }
    if let Some(p) = pan.filter(|p| p.enabled) {
        // `+ 0.0` turns -0.0 into 0.0, so the key does not depend on the sign of zero.
        s += &format!(
            "|pan{:.2}:{:.2}:{:.2}",
            p.center + 0.0,
            p.width + 0.0,
            p.softness + 0.0
        );
    }
    match detail.fast {
        shredlab_notes::Fast::Normal => {}
        shredlab_notes::Fast::Fewer => s += "|ffewer",
        shredlab_notes::Fast::More => s += "|fmore",
    }
    if detail.quiet {
        s += "|q";
    }
    s
}

/// The notes job's input: the stem as loaded (its strength applied), the
/// stereo window of Refine's mask when one is set (the part the mask
/// selects, whichever way the row's switch flips it for playback), mono.
fn notes_input(t: &Track, pan: Option<&PanParams>) -> Vec<f32> {
    if !pan.is_some_and(|p| p.enabled) {
        return mono_of(t);
    }
    notes_input_range(t, pan, 0..t.len())
}

/// [`notes_input`] for the samples `range` of the stem only (a loop's
/// excerpt; the stereo mask runs on the excerpt alone).
fn notes_input_range(
    t: &Track,
    pan: Option<&PanParams>,
    range: std::ops::Range<usize>,
) -> Vec<f32> {
    let n = range.len();
    let mut l = vec![0.0f32; n];
    let mut r = vec![0.0f32; n];
    t.channels[0].read(range.start, &mut l);
    t.channels[1].read(range.start, &mut r);
    let Some(p) = pan.filter(|p| p.enabled) else {
        return l.iter().zip(&r).map(|(a, b)| 0.5 * (a + b)).collect();
    };
    let mut window = *p;
    window.invert = false;
    let [l, r] = shredlab_dsp::refine::pan_offline(&window, t.sample_rate, &l, &r);
    l.iter().zip(&r).map(|(a, b)| 0.5 * (a + b)).collect()
}

/// Source key of a loop's notes (`loop_notes.json`): the stem's source for
/// the loop's settings ([`notes_source`]) and `|loop<start>-<end>` in
/// whole milliseconds.
pub fn loop_notes_source(source: &str, start_s: f64, end_s: f64) -> String {
    let ms = |t: f64| (t * 1000.0).round() as i64;
    format!("{source}|loop{}-{}", ms(start_s), ms(end_s))
}

/// The saved loop notes of `stem` for `source`, if current.
fn find_loop_notes(d: &NotesData, stem: &str, source: &str) -> Option<StemNotes> {
    if d.version != shredlab_notes::VERSION {
        return None;
    }
    d.entries
        .iter()
        .rev()
        .find(|e| e.stem == stem && e.source == source)
        .cloned()
}

/// Adds a loop result to `loop_notes.json`: replaces the same (stem,
/// source), keeps the newest [`LOOP_NOTES_KEEP`], and starts over when the
/// saved entries are from another algorithm version.
fn add_loop_notes_entry(d: &mut NotesData, entry: StemNotes) {
    if d.version != shredlab_notes::VERSION {
        *d = NotesData {
            version: shredlab_notes::VERSION,
            entries: Vec::new(),
        };
    }
    d.entries
        .retain(|e| !(e.stem == entry.stem && e.source == entry.source));
    d.entries.push(entry);
    let extra = d.entries.len().saturating_sub(LOOP_NOTES_KEEP);
    d.entries.drain(..extra);
}

/// [`shredlab_notes::NoteAnalysis`] as stored.
fn stem_notes(stem: String, source: String, a: &shredlab_notes::NoteAnalysis) -> StemNotes {
    use shredlab_notes::Attack;
    StemNotes {
        stem,
        source,
        notes: a
            .notes
            .iter()
            .map(|n| shredlab_library::NoteSpan {
                start_s: n.start_s,
                end_s: n.end_s,
                midi: n.midi,
                cents: n.cents,
                conf: n.confidence,
                attack: match n.attack {
                    Attack::Picked => None,
                    Attack::Legato => Some("legato".into()),
                    Attack::Slide => Some("slide".into()),
                },
                bend: n.bend.map(|b| shredlab_library::BendSpan {
                    start_s: b.start_s,
                    peak_s: b.peak_s,
                    release_s: b.release_s,
                    semitones: b.semitones,
                }),
                vibrato: n.vibrato,
            })
            .collect(),
        chordal: a.chordal.iter().map(|&(s, e)| [s, e]).collect(),
        others: a
            .others
            .iter()
            .map(|h| shredlab_library::HeardSpan {
                start_s: h.start_s,
                end_s: h.end_s,
                midi: h.midi,
                conf: h.confidence,
            })
            .collect(),
    }
}

/// Adds `entry` to the saved notes: replaces the same (stem, source), drops
/// the oldest sources of the stem beyond [`NOTES_KEEP`], and starts over
/// when the saved entries are from another algorithm version.
fn add_notes_entry(data: &mut Option<NotesData>, entry: StemNotes) {
    let d = data.get_or_insert_with(NotesData::default);
    if d.version != shredlab_notes::VERSION {
        *d = NotesData {
            version: shredlab_notes::VERSION,
            entries: Vec::new(),
        };
    }
    d.entries
        .retain(|e| !(e.stem == entry.stem && e.source == entry.source));
    let stem = entry.stem.clone();
    d.entries.push(entry);
    while d.entries.iter().filter(|e| e.stem == stem).count() > NOTES_KEEP {
        if let Some(i) = d.entries.iter().position(|e| e.stem == stem) {
            d.entries.remove(i);
        }
    }
}

/// The stereo masks the notes follow: the enabled ones of `refine`.
fn notes_pans(refine: &BTreeMap<String, RefineParams>) -> BTreeMap<String, PanParams> {
    refine
        .iter()
        .filter(|(_, p)| p.pan.enabled)
        .map(|(k, p)| (k.clone(), p.pan))
        .collect()
}

fn separated_with(info: &StemsInfo) -> SeparatedWith {
    SeparatedWith {
        model_name: info.model_name.clone(),
        device_label: info.device_label.clone(),
        model_id: info.model_id.clone(),
        quality: Quality::from_shifts(info.shifts),
    }
}

/// Planar stereo at [`RATE`], shared between the song and its tracks.
type Mix = [Arc<Vec<f32>>; 2];

fn chans(mix: &Mix) -> [&[f32]; 2] {
    [&mix[0], &mix[1]]
}

/// Decoded stems as `(name, [L, R])`.
type RawStems = Vec<(String, [Vec<f32>; 2])>;

/// Holds stems as 16-bit integers (half the memory of f32, see
/// [`Track::quantized`]), freeing each f32 stem once converted.
fn quantize_stems(stems: RawStems) -> Vec<Track> {
    stems
        .into_iter()
        .map(|(name, [l, r])| Track::quantized(name, [&l, &r], RATE))
        .collect()
}

/// Sum of the stems: the mix when the original is unavailable. Only built
/// in that case (rare: an old cache entry whose source file is gone); it
/// walks every sample of every stem.
fn stem_sum(stems: &[Track]) -> [Vec<f32>; 2] {
    let len = stems.iter().map(Track::len).max().unwrap_or(0);
    let mut sum = [vec![0.0f32; len], vec![0.0f32; len]];
    let mut buf = vec![0.0f32; len];
    for t in stems {
        for (s, c) in sum.iter_mut().zip(&t.channels) {
            let n = c.len().min(len);
            c.read(0, &mut buf[..n]);
            for (s, x) in s.iter_mut().zip(&buf[..n]) {
                *s += x;
            }
        }
    }
    sum
}

/// The mix as the original track (shares the samples).
fn original_track(mix: &Mix) -> Track {
    Track {
        name: ORIGINAL.into(),
        channels: mix.clone().map(Into::into),
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
    /// Tuning's playback-rate correction in cents; reset on every open.
    rate_cents: i32,
    loop_state: LoopState,
    click: ClickState,
    /// Effective downbeats from the UI (`set_meter_overrides`).
    downbeats: Option<Vec<f64>>,
    separation: Option<Job>,
    export: Option<Arc<AtomicBool>>,
    last_separation_error: Option<String>,
    /// Refine settings of the open song, by stem (only refined stems).
    refine: BTreeMap<String, RefineParams>,
    /// Unsaved refine or per-song settings change of the open song: (song
    /// id, time of the last change).
    song_dirty: Option<(String, Instant)>,
    /// Stem whose Refine panel is open (stereo-field analysis).
    analysis: Option<String>,
    strength_job: Option<StrengthJob>,
    next_strength_job: u64,
    /// Chords job running: (song id, stem set key).
    chords_job: Option<(String, String)>,
    /// Refine stereo masks the notes follow, by stem: the enabled ones of
    /// `refine` as they were [`NOTES_DELAY`] after their last change.
    notes_pans: BTreeMap<String, PanParams>,
    /// A stereo mask changed at this time and the notes don't follow it yet.
    notes_pans_changed: Option<Instant>,
    /// Notes job running: (song id, stem, source).
    notes_job: Option<(String, String, String)>,
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
    /// Loaded by the first notes run that needs it; only the notes job
    /// and loop requests lock it.
    note_model: Mutex<Option<NoteModel>>,
    /// Number of the newest `detect_loop_notes` request.
    loop_notes_seq: AtomicU64,
    /// Held while a loop request detects (one at a time, so a newer one
    /// can supersede those still waiting).
    loop_notes_lock: Mutex<()>,
    st: Mutex<State>,
    open_lock: Mutex<()>,
    /// The live tuner's input capture while it listens. Never locked
    /// together with `st` or `app_settings`.
    tuner: Mutex<Option<audio::InputCapture>>,
}

/// [`music_start::detect`] on the original mix, logged.
fn detect_music_start(song_id: &str, mix: &Mix, beats: &[f64]) -> Option<f64> {
    let t0 = Instant::now();
    let ms = music_start::detect(chans(mix), RATE, beats);
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

/// The mix mode for playback: the original-based backing track when the
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

/// Runs the tuning estimate on the pitched stems (mono, at the stem rate).
fn measure_tuning(stems: &[Track]) -> shredlab_library::TuningData {
    use shredlab_tuning::{StemAudio, StemKind};
    let kind = |name: &str| match name {
        "bass" => Some(StemKind::Bass),
        "guitar" => Some(StemKind::Guitar),
        "piano" | "keys" => Some(StemKind::Keys),
        "other" => Some(StemKind::Other),
        "vocals" => Some(StemKind::Vocals),
        _ => None,
    };
    let rate = stems.first().map_or(RATE, |t| t.sample_rate);
    let mono: Vec<(StemKind, Vec<f32>)> = stems
        .iter()
        .filter_map(|t| Some((kind(&t.name)?, mono_of(t))))
        .collect();
    let input: Vec<StemAudio> = mono
        .iter()
        .map(|(kind, s)| StemAudio {
            kind: *kind,
            samples: s,
        })
        .collect();
    match shredlab_tuning::estimate_song_tuning(&input, rate) {
        Some(e) => shredlab_library::TuningData {
            guess: if e.is_reliable() {
                shredlab_tuning::guess_song_tuning(&input, rate, e.offset_cents).map(|g| {
                    shredlab_library::TuningGuessData {
                        lowest_midi: g.lowest_midi,
                        below_standard: g.below_standard,
                        likely: g.likely,
                    }
                })
            } else {
                None
            },
            version: shredlab_tuning::VERSION,
            offset_cents: e.offset_cents,
            reference_hz: e.reference_hz,
            confidence: e.confidence,
            peaks: e.peak_count,
            drifts: e.drifts,
            segments: e
                .segments
                .iter()
                .map(|s| shredlab_library::TuningSegment {
                    start_s: s.start_s,
                    offset_cents: s.offset_cents,
                    weight: s.weight,
                })
                .collect(),
        },
        None => shredlab_library::TuningData {
            version: shredlab_tuning::VERSION,
            reference_hz: 440.0,
            ..Default::default()
        },
    }
}

/// Runs chord detection on the pitched stems (mono, at the stem rate).
fn detect_chords(
    stems: &[Track],
    beats: &[f64],
    offset_cents: f64,
    duration: f64,
    stems_key: String,
) -> shredlab_library::ChordsData {
    use shredlab_chords::{StemAudio, StemKind};
    let kind = |name: &str| match name {
        "bass" => Some(StemKind::Bass),
        "guitar" => Some(StemKind::Guitar),
        "piano" | "keys" => Some(StemKind::Keys),
        "other" => Some(StemKind::Other),
        "vocals" => Some(StemKind::Vocals),
        _ => None,
    };
    let rate = stems.first().map_or(RATE, |t| t.sample_rate);
    let mono: Vec<(StemKind, Vec<f32>)> = stems
        .iter()
        .filter_map(|t| Some((kind(&t.name)?, mono_of(t))))
        .collect();
    let input: Vec<StemAudio> = mono
        .iter()
        .map(|(kind, s)| StemAudio {
            kind: *kind,
            samples: s,
        })
        .collect();
    let a = shredlab_chords::detect_chords(&input, rate, beats, offset_cents, duration);
    shredlab_library::ChordsData {
        version: shredlab_chords::VERSION,
        stems: stems_key,
        key: a.key.map(|k| shredlab_library::ChordKey {
            tonic: k.tonic,
            minor: k.minor,
        }),
        segments: a
            .segments
            .iter()
            .map(|s| shredlab_library::ChordSpan {
                start_s: s.start_s,
                end_s: s.end_s,
                root: s.chord.map(|c| c.root),
                quality: s
                    .chord
                    .map_or(String::new(), |c| c.quality.suffix().to_string()),
                bass: s.chord.and_then(|c| c.bass),
                detail: match (s.chord, s.detailed) {
                    (Some(c), Some(d)) if d.quality != c.quality => {
                        Some(d.quality.suffix().to_string())
                    }
                    _ => None,
                },
                push: s.push,
            })
            .collect(),
        passing: a.passing.iter().map(passing_span).collect(),
        figures: a.figures.iter().map(passing_span).collect(),
    }
}

fn passing_span(p: &shredlab_chords::PassingChord) -> shredlab_library::PassingSpan {
    shredlab_library::PassingSpan {
        start_s: p.start_s,
        end_s: p.end_s,
        root: p.chord.root,
        quality: p.chord.quality.suffix().to_string(),
    }
}

/// Mono downmix of a stereo track.
fn mono_of(t: &Track) -> Vec<f32> {
    let [l, r] = &t.channels;
    let n = l.len().min(r.len());
    let mut a = vec![0.0f32; n];
    let mut b = vec![0.0f32; n];
    l.read(0, &mut a);
    r.read(0, &mut b);
    for (x, y) in a.iter_mut().zip(&b) {
        *x = 0.5 * (*x + *y);
    }
    a
}

/// Cached beat data from a decoded grid and its downbeat scores.
fn beat_data(t: shredlab_beats::Tracked) -> BeatData {
    BeatData {
        bpm: t.grid.tempo_bpm(),
        beats_per_bar: t.grid.most_common_beats_per_bar().map(|n| n as u32),
        downbeat_version: shredlab_beats::DOWNBEAT_DECODER_VERSION,
        downbeat_scores: t.downbeat_scores,
        beats: t.grid.beats,
        downbeats: t.grid.downbeats,
    }
}

/// Decodes the downbeats of a cached grid again from its stored scores,
/// keeping the beats. `false` when it has no scores (the model must run).
fn redecode_downbeats(b: &mut BeatData) -> bool {
    if b.downbeat_scores.len() != b.beats.len() {
        return false;
    }
    let beats = std::mem::take(&mut b.beats);
    let downbeat_scores = std::mem::take(&mut b.downbeat_scores);
    let downbeats = shredlab_beats::downbeats_from_scores(&beats, &downbeat_scores);
    *b = beat_data(shredlab_beats::Tracked {
        grid: shredlab_beats::BeatGrid { beats, downbeats },
        downbeat_scores,
    });
    true
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
pub fn peaks(mix: [&[f32]; 2], n: usize) -> Vec<f32> {
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
        match &cfg.note_model {
            Some(p) => log!("note model: {}", p.display()),
            None => log!(
                "WARNING: note model {} not found; notes use the pitch tracker",
                crate::models::NOTE_MODEL_FILE
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
            note_model: Mutex::new(None),
            loop_notes_seq: AtomicU64::new(0),
            loop_notes_lock: Mutex::new(()),
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
                rate_cents: 0,
                loop_state: LoopState::default(),
                click,
                downbeats: None,
                separation: None,
                export: None,
                last_separation_error: None,
                refine: BTreeMap::new(),
                song_dirty: None,
                analysis: None,
                strength_job: None,
                next_strength_job: 0,
                chords_job: None,
                notes_pans: BTreeMap::new(),
                notes_pans_changed: None,
                notes_job: None,
            }),
            open_lock: Mutex::new(()),
            tuner: Mutex::new(None),
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

    /// Starts the position emitter (which also flushes the debounced song
    /// save: Refine and per-song settings) and preloads the separation model
    /// (runs or reuses the device self-test, so the first separation starts
    /// fast).
    pub fn start_background(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        spawn("shredlab-position", move || {
            let mut was_active = false;
            let mut field = [0.0f32; audio::FIELD_LEN];
            loop {
                std::thread::sleep(Duration::from_millis(33));
                let Some(b) = weak.upgrade() else { break };
                b.flush_song_save(false);
                b.flush_notes_pans(false);
                b.poll_tuner();
                // Without an engine the thread stays alive for the flush.
                let Some(e) = &b.engine else { continue };
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
            tuner_device_id: saved.tuner_device_id.clone(),
            tuner_channel: saved.tuner_channel.unwrap_or(0),
        }
    }

    /// Changes the saved settings with `f` and writes the file. The lock is
    /// held across the change and the (small, atomic) write, so concurrent
    /// setters cannot lose each other's change. With `save` false only
    /// memory changes.
    fn update_settings(
        &self,
        save: bool,
        f: impl FnOnce(&mut AppSettings),
    ) -> Result<AppSettings, String> {
        let mut cur = lock(&self.app_settings);
        let mut new = cur.clone();
        f(&mut new);
        if save {
            new.save(&self.cfg.data_dir)
                .map_err(|e| format!("Cannot save settings: {e}"))?;
        }
        *cur = new.clone();
        Ok(new)
    }

    /// Saves the settings (null = default) and returns the new effective
    /// settings. Does not re-separate anything by itself.
    pub fn set_app_settings(&self, update: AppSettingsUpdate) -> Result<AppSettingsInfo, String> {
        if let Some(id) = &update.model_id {
            if self.registry.get(id).is_none() {
                return Err(format!("Unknown model {id}"));
            }
        }
        let new = self.update_settings(true, |s| {
            s.model_id = update.model_id;
            s.quality = update.quality;
        })?;
        log!("settings saved: {new:?}");
        Ok(self.app_settings())
    }

    /// Saves the UI theme (one of [`settings::THEMES`]) with the other app
    /// settings and returns the new settings.
    pub fn set_theme(&self, theme: &str) -> Result<AppSettingsInfo, String> {
        if !settings::THEMES.contains(&theme) {
            return Err(format!("Unknown theme {theme}"));
        }
        self.update_settings(true, |s| s.theme = Some(theme.to_string()))?;
        log!("theme saved: {theme}");
        Ok(self.app_settings())
    }

    /// Saves "Count in to where the music starts" with the app settings and
    /// applies it to playback at once.
    pub fn set_count_in_to_music_start(&self, on: bool) -> Result<AppSettingsInfo, String> {
        self.update_settings(true, |s| s.count_in_to_music_start = Some(on))?;
        log!("count in to where the music starts: {on}");
        self.apply_click(&lock(&self.st));
        Ok(self.app_settings())
    }

    /// Saves the format of newly written stems with the app settings.
    /// Existing stems keep their format; they are still read.
    pub fn set_stem_format(&self, format: StemFormat) -> Result<AppSettingsInfo, String> {
        self.update_settings(true, |s| s.stem_format = Some(format))?;
        log!("stem format: {format:?}");
        Ok(self.app_settings())
    }

    /// Saves the click and count-in volume (linear 0..1) with the app
    /// settings and applies it to playback at once.
    pub fn set_click_volume(&self, volume: f32) -> Result<AppSettingsInfo, String> {
        if !volume.is_finite() {
            return Err(format!("Invalid click volume {volume}"));
        }
        let volume = volume.clamp(0.0, 1.0);
        self.update_settings(true, |s| s.click_volume = Some(volume))?;
        log!("click volume: {volume:.2}");
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
        self.update_settings(save, |s| s.master_volume = Some(volume))?;
        if save {
            log!("master volume: {volume:.2}");
        }
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
            count_in_each_repeat: c.count_in_each_repeat,
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
        e.set_rate_correction(st.rate_cents);
        e.set_loop(st.loop_state.range());
        self.apply_click(st);
    }

    // ------------------------------------------------------------ songs

    fn cancel_separation_locked(&self, st: &mut State) {
        if let Some(job) = st.separation.take() {
            job.cancel.store(true, Ordering::Relaxed);
            log!("separation of {} canceled", job.song_id);
            self.emit(
                "separation-error",
                &SeparationError {
                    song_id: job.song_id,
                    message: "Canceled".into(),
                    canceled: true,
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
        self.flush_song_save(true);
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
                    Ok(data) => {
                        stems = Some(quantize_stems(data));
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
        let mix: Mix = original
            .or_else(|| stems.as_deref().map(stem_sum))
            .unwrap_or_default()
            .map(Arc::new);
        let original_tr = mix_is_original.then(|| original_track(&mix));
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
        // Downbeats from an older decoder: decode them again from the stored
        // scores, or else rerun the model below while keeping the beats
        // (meter overrides are beat indices).
        let mut keep_beats = None;
        if let Some(b) = m
            .beats
            .as_mut()
            .filter(|b| b.downbeat_version < shredlab_beats::DOWNBEAT_DECODER_VERSION)
        {
            if !redecode_downbeats(b) {
                keep_beats = Some(b.beats.clone());
            }
        }
        if let Err(e) = self.library.save(&mut m) {
            log!("cannot save library entry: {e}");
        }
        let beats = m.beats.clone();
        let need_beats = beats.is_none() || keep_beats.is_some();
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
        let need_tuning = song
            .meta
            .tuning
            .as_ref()
            .is_none_or(|t| t.version < shredlab_tuning::VERSION);
        let tuning_stems = if need_tuning {
            song.stems.clone()
        } else {
            None
        };
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
            st.notes_pans = notes_pans(&st.refine);
            st.notes_pans_changed = None;
            info.notes = song.current_notes(&st.notes_pans);
            info.notes_pending = song
                .notes_missing(&st.notes_pans)
                .into_iter()
                .map(|(stem, _)| stem)
                .collect();
            info.notes_running = st
                .notes_job
                .as_ref()
                .filter(|j| j.0 == song.meta.song_id)
                .map(|j| j.1.clone());
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
            st.rate_cents = 0;
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
        if let Some(stems) = tuning_stems {
            self.spawn_tuning(song_id.clone(), stems, generation);
        }
        self.maybe_spawn_chords();
        self.maybe_spawn_notes();
        if need_beats {
            self.spawn_beats(song_id, mix, generation, keep_beats);
        }
        Ok(info)
    }

    fn read_cached_stems(&self, song_id: &str, info: &StemsInfo) -> Result<RawStems, String> {
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
        log!(
            "read {} cached stems ({}) in {:.2}s",
            data.len(),
            info.dir,
            t0.elapsed().as_secs_f64()
        );
        Ok(data)
    }

    pub fn close_song(&self) {
        self.stop_tuner_because("song_closed");
        self.flush_song_save(true);
        let mut st = lock(&self.st);
        self.cancel_separation_locked(&mut st);
        self.cancel_strength_locked(&mut st);
        st.analysis = None;
        st.notes_pans.clear();
        st.notes_pans_changed = None;
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

    /// Stores the per-song UI settings. For the open song they are kept in
    /// memory and written like Refine (shortly after the last change, or on
    /// song switch, close and exit), so the UI can send every change.
    pub fn save_song_settings(
        &self,
        song_id: &str,
        settings: serde_json::Value,
    ) -> Result<(), String> {
        {
            let mut st = lock(&self.st);
            if let Some(s) = st.song.as_mut().filter(|s| s.meta.song_id == song_id) {
                s.meta.settings = Some(settings);
                st.song_dirty = Some((song_id.to_string(), Instant::now()));
                return Ok(());
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
        mix: Mix,
        generation: u64,
        cancel: Arc<AtomicBool>,
    ) {
        let t0 = Instant::now();
        let result = match cached {
            Some(info) => self.load_cached_set(&song_id, info),
            None => self.separate_and_cache(&entry, shifts, &song_id, &mix, &cancel),
        };
        let canceled = cancel.load(Ordering::Relaxed);
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
                if !current || canceled {
                    return;
                }
                let with = separated_with(&info);
                let mut original_available = false;
                if let Some(song) = st.song.as_mut() {
                    if let Ok(Some(m)) = self.library.load(&song_id) {
                        song.meta.stem_sets = m.stem_sets;
                        song.meta.original = m.original;
                    }
                    song.stems = Some(tracks.clone());
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
                // New stems: measure the tuning again on them.
                self.spawn_tuning(song_id.clone(), tracks, generation);
                self.emit(
                    "separation-done",
                    &SeparationDone {
                        song_id,
                        separated_with: with,
                        original_available,
                    },
                );
                // The new stems' notes wait for their tuning, like the chords.
                self.emit_notes_status();
            }
            Err(_) if canceled => {
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
                        canceled: false,
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
        let tracks = quantize_stems(self.read_cached_stems(song_id, &info)?);
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
        mix: &Mix,
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
            return Err("canceled".into());
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
            return Err("canceled".into());
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
        // Keep a copy of the original next to the stems (the backing track works
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
                Track::quantized(stems.names[i].clone(), [ch[0], ch[1]], RATE)
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

    // ------------------------------------------------------------ tuning

    /// Measures the song's offset from A440 on its stems, saves it and
    /// emits `tuning-ready`. A song that can't be measured still gets a
    /// result (zero confidence), so the job doesn't rerun on every open.
    fn spawn_tuning(self: &Arc<Self>, song_id: String, stems: Vec<Track>, generation: u64) {
        let b = self.clone();
        spawn("shredlab-tuning", move || {
            let t0 = Instant::now();
            let data = measure_tuning(&stems);
            log!(
                "tuning for {song_id}: {:+.1} cents (A{:.1}), confidence {:.2}, {} peaks{}{} in {:.2}s",
                data.offset_cents,
                data.reference_hz,
                data.confidence,
                data.peaks,
                if data.drifts { ", drifts" } else { "" },
                data.guess.map_or(String::new(), |g| format!(
                    ", guess {:+} (lowest {}{})",
                    g.below_standard,
                    g.lowest_midi,
                    if g.likely { ", likely" } else { "" }
                )),
                t0.elapsed().as_secs_f64()
            );
            // In memory first, so a settings save of the open song between
            // here and the file update can't drop the new result.
            let current = {
                let mut st = lock(&b.st);
                let current = st.generation == generation
                    && st.song.as_ref().is_some_and(|s| s.meta.song_id == song_id);
                if current {
                    if let Some(s) = st.song.as_mut() {
                        s.meta.tuning = Some(data.clone());
                    }
                }
                current
            };
            if let Err(e) = b
                .library
                .update(&song_id, |m| m.tuning = Some(data.clone()))
            {
                log!("cannot save tuning: {e}");
            }
            if !current {
                return;
            }
            b.emit(
                "tuning-ready",
                &TuningReady {
                    song_id,
                    tuning: data,
                },
            );
            b.maybe_spawn_chords();
            b.maybe_spawn_notes();
        });
    }

    // ------------------------------------------------------------ chords

    /// Starts the chords job for the open song when its stems, beats and a
    /// current tuning are in and its saved chords are missing, older than
    /// the algorithm or from another stem set. Called whenever one of those
    /// arrives; does nothing while a job for the same set runs.
    fn maybe_spawn_chords(self: &Arc<Self>) {
        let mut st = lock(&self.st);
        let generation = st.generation;
        let Some(song) = st.song.as_ref() else {
            return;
        };
        let Some(key) = song.chords_stems_key() else {
            return;
        };
        if song.current_chords().is_some() {
            return;
        }
        let (Some(stems), Some(beats)) = (song.stems.clone(), song.beats.as_ref()) else {
            return;
        };
        let Some(tuning) = song
            .meta
            .tuning
            .as_ref()
            .filter(|t| t.version >= shredlab_tuning::VERSION)
        else {
            return;
        };
        let song_id = song.meta.song_id.clone();
        let job = (song_id.clone(), key.clone());
        if st.chords_job.as_ref() == Some(&job) {
            return;
        }
        let beats = beats.beats.clone();
        // An unreliable offset is noise; center on A440 then.
        let offset = if tuning.confidence >= shredlab_tuning::MIN_CONFIDENCE
            && tuning.peaks >= shredlab_tuning::MIN_PEAKS
        {
            tuning.offset_cents as f64
        } else {
            0.0
        };
        let duration = song.meta.duration_sec;
        st.chords_job = Some(job.clone());
        drop(st);
        let b = self.clone();
        spawn("shredlab-chords", move || {
            let t0 = Instant::now();
            let data = detect_chords(&stems, &beats, offset, duration, key);
            log!(
                "chords for {song_id}: {} segments, {} pushes, key {:?} in {:.2}s",
                data.segments.len(),
                data.segments.iter().filter(|s| s.push).count(),
                data.key,
                t0.elapsed().as_secs_f64()
            );
            let current = {
                let mut st = lock(&b.st);
                if st.chords_job.as_ref() == Some(&job) {
                    st.chords_job = None;
                }
                let current = st.generation == generation
                    && st.song.as_ref().is_some_and(|s| {
                        s.meta.song_id == song_id
                            && s.chords_stems_key().as_deref() == Some(data.stems.as_str())
                    });
                if current {
                    if let Some(s) = st.song.as_mut() {
                        s.meta.chords = Some(data.clone());
                    }
                }
                current
            };
            if let Err(e) = b
                .library
                .update(&song_id, |m| m.chords = Some(data.clone()))
            {
                log!("cannot save chords: {e}");
            }
            if current {
                b.emit(
                    "chords-ready",
                    &ChordsReady {
                        song_id,
                        chords: data,
                    },
                );
            } else {
                // The song or its stems changed meanwhile.
                b.maybe_spawn_chords();
            }
        });
    }

    // ------------------------------------------------------------ notes

    /// The `notes-status` payload of the open song.
    fn notes_status_locked(st: &State) -> Option<NotesStatus> {
        let song = st.song.as_ref()?;
        Some(NotesStatus {
            song_id: song.meta.song_id.clone(),
            pending: song
                .notes_missing(&st.notes_pans)
                .into_iter()
                .map(|(stem, _)| stem)
                .collect(),
            running: st
                .notes_job
                .as_ref()
                .filter(|j| j.0 == song.meta.song_id)
                .map(|j| j.1.clone()),
        })
    }

    fn emit_notes_status(&self) {
        let status = Self::notes_status_locked(&lock(&self.st));
        if let Some(s) = status {
            self.emit("notes-status", &s);
        }
    }

    /// Starts the notes job on the next pitched stem of the open song whose
    /// notes for its current source (stem set, strength, stereo mask, note
    /// detail) are
    /// not saved, once the stems, beats and a current tuning are in. One
    /// stem at a time: a running job starts the next one when it ends.
    /// Emits `notes-status` either way.
    fn maybe_spawn_notes(self: &Arc<Self>) {
        let mut st = lock(&self.st);
        let generation = st.generation;
        let Some(song) = st.song.as_ref() else {
            return;
        };
        let status = Self::notes_status_locked(&st);
        let start = (|| {
            if st.notes_job.is_some() {
                return None;
            }
            let (stems, beats) = (song.stems.as_ref()?, song.beats.as_ref()?);
            let tuning = song
                .meta
                .tuning
                .as_ref()
                .filter(|t| t.version >= shredlab_tuning::VERSION)?;
            let (stem, source) = song.notes_missing(&st.notes_pans).into_iter().next()?;
            let track = stems.iter().find(|t| t.name == stem)?.clone();
            let offset = notes_offset(tuning);
            let pan = st.notes_pans.get(&stem).copied();
            let detail = notes_detail_for(&stem, &song.meta);
            Some((
                stem,
                source,
                track,
                beats.beats.clone(),
                offset,
                pan,
                detail,
            ))
        })();
        let Some((stem, source, track, beats, offset, pan, detail)) = start else {
            drop(st);
            if let Some(s) = status {
                self.emit("notes-status", &s);
            }
            return;
        };
        let song_id = song.meta.song_id.clone();
        let job = (song_id.clone(), stem.clone(), source.clone());
        st.notes_job = Some(job.clone());
        let status = Self::notes_status_locked(&st);
        drop(st);
        if let Some(s) = status {
            self.emit("notes-status", &s);
        }
        let b = self.clone();
        spawn("shredlab-notes", move || {
            let t0 = Instant::now();
            let Some(kind) = notes_kind(&stem) else {
                return;
            };
            let samples = notes_input(&track, pan.as_ref());
            drop(track);
            let input = shredlab_notes::NoteInput {
                samples: &samples,
                sample_rate: RATE,
                kind,
                offset_cents: offset,
                beats: &beats,
                detail,
            };
            let a = b.detect_notes(&input);
            drop(samples);
            let entry = stem_notes(stem.clone(), source.clone(), &a);
            log!(
                "notes for {song_id} {stem} ({source}): {} notes, {} chordal stretches, {} others heard in {:.2}s",
                entry.notes.len(),
                entry.chordal.len(),
                entry.others.len(),
                t0.elapsed().as_secs_f64()
            );
            let shown = {
                let mut st = lock(&b.st);
                if st.notes_job.as_ref() == Some(&job) {
                    st.notes_job = None;
                }
                let pans = st.notes_pans.clone();
                let same_song = st.generation == generation
                    && st.song.as_ref().is_some_and(|s| s.meta.song_id == song_id);
                let mut shown = false;
                if same_song {
                    if let Some(s) = st.song.as_mut() {
                        add_notes_entry(&mut s.meta.notes, entry.clone());
                        shown = s
                            .notes_sources(&pans)
                            .iter()
                            .any(|(n, src)| *n == stem && *src == source);
                    }
                }
                shown
            };
            if let Err(e) = b
                .library
                .update(&song_id, |m| add_notes_entry(&mut m.notes, entry.clone()))
            {
                log!("cannot save notes: {e}");
            }
            if shown {
                b.emit(
                    "notes-ready",
                    &NotesReady {
                        song_id,
                        stem,
                        source,
                        notes: entry,
                    },
                );
            }
            b.maybe_spawn_notes();
        });
    }

    /// One stem's notes: with the note model for the stems that use it
    /// (loaded here on first use), else, or when the model fails, with the
    /// pitch tracker.
    fn detect_notes(&self, input: &shredlab_notes::NoteInput) -> shredlab_notes::NoteAnalysis {
        let path = match &self.cfg.note_model {
            Some(p) if input.kind.uses_model() => p,
            _ => return shredlab_notes::detect_notes(input),
        };
        let mut guard = lock(&self.note_model);
        if guard.is_none() {
            let t0 = Instant::now();
            match NoteModel::load(path, Some(4)) {
                Ok(m) => {
                    log!("note model loaded in {:.2}s", t0.elapsed().as_secs_f64());
                    *guard = Some(m);
                }
                Err(e) => log!("note model: {e}"),
            }
        }
        let Some(model) = guard.as_mut() else {
            return shredlab_notes::detect_notes(input);
        };
        let run = &mut |w: &[f32]| {
            let (note, onset, contour) = model.run(w).map_err(|e| e.to_string())?;
            Ok(shredlab_notes::WindowOutput {
                note,
                onset,
                contour,
            })
        };
        match shredlab_notes::detect_notes_with_model(input, run) {
            Ok(a) => a,
            Err(e) => {
                log!("note model failed ({e}); using the pitch tracker");
                shredlab_notes::detect_notes(input)
            }
        }
    }

    /// The notes of `stem` in the loop `start_s`..`end_s` of the open song,
    /// found with the loop's own settings (`detect_loop_notes`): from
    /// `loop_notes.json` when found before, else detected on the loop plus
    /// [`shredlab_notes::EXCERPT_PAD_S`] on each side (the stem as the notes
    /// job sees it, the song's beats and offset), clipped to the loop and
    /// saved there. Only for stems on the note model. Requests detect one
    /// at a time; one that a newer request replaced before it started
    /// returns [`LOOP_SUPERSEDED`].
    pub fn detect_loop_notes(
        self: &Arc<Self>,
        start_s: f64,
        end_s: f64,
        fast: NotesFast,
        quiet: bool,
        stem: &str,
    ) -> Result<StemNotes, String> {
        let seq = self.loop_notes_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let kind = notes_kind(stem)
            .filter(|k| k.uses_model())
            .ok_or_else(|| format!("No loop notes for the {stem} stem"))?;
        let not_ready = || "The song's notes are not ready yet".to_string();
        let (song_id, source, track, beats, offset, pan, detail, excerpt) = {
            let st = lock(&self.st);
            let song = st.song.as_ref().ok_or("No song open")?;
            let (Some(stems), Some(key)) = (song.stems.as_ref(), song.chords_stems_key()) else {
                return Err(not_ready());
            };
            let beats = song.beats.as_ref().ok_or_else(not_ready)?;
            let tuning = song
                .meta
                .tuning
                .as_ref()
                .filter(|t| t.version >= shredlab_tuning::VERSION)
                .ok_or_else(not_ready)?;
            let track = stems
                .iter()
                .find(|t| t.name == stem)
                .ok_or_else(|| format!("No {stem} stem"))?
                .clone();
            let duration = track.len() as f64 / f64::from(RATE);
            let excerpt = shredlab_notes::Excerpt::new(start_s, end_s, duration)
                .ok_or("The loop is too short or outside the song")?;
            let pan = st.notes_pans.get(stem).copied();
            let detail = notes_detail(stem, fast, quiet);
            let source = loop_notes_source(
                &notes_source(&key, song.loaded_strength, pan.as_ref(), detail),
                excerpt.start_s,
                excerpt.end_s,
            );
            (
                song.meta.song_id.clone(),
                source,
                track,
                beats.beats.clone(),
                notes_offset(tuning),
                pan,
                detail,
                excerpt,
            )
        };
        let saved = || find_loop_notes(&self.library.load_loop_notes(&song_id), stem, &source);
        if let Some(hit) = saved() {
            return Ok(hit);
        }
        let _one = lock(&self.loop_notes_lock);
        if self.loop_notes_seq.load(Ordering::SeqCst) != seq {
            return Err(LOOP_SUPERSEDED.into());
        }
        // An identical request may have finished while this one waited.
        if let Some(hit) = saved() {
            return Ok(hit);
        }
        let t0 = Instant::now();
        let samples = notes_input_range(&track, pan.as_ref(), excerpt.samples(RATE, track.len()));
        drop(track);
        let beats = excerpt.beats(&beats);
        let input = shredlab_notes::NoteInput {
            samples: &samples,
            sample_rate: RATE,
            kind,
            offset_cents: offset,
            beats: &beats,
            detail,
        };
        let a = excerpt.finish(self.detect_notes(&input));
        let entry = stem_notes(stem.to_string(), source, &a);
        log!(
            "loop notes for {song_id} {stem} ({}): {} notes, {} chordal stretches, {} others heard in {:.2}s",
            entry.source,
            entry.notes.len(),
            entry.chordal.len(),
            entry.others.len(),
            t0.elapsed().as_secs_f64()
        );
        if let Err(e) = self
            .library
            .update_loop_notes(&song_id, |d| add_loop_notes_entry(d, entry.clone()))
        {
            log!("cannot save loop notes: {e}");
        }
        Ok(entry)
    }

    /// Lets the notes follow changed Refine stereo masks, [`NOTES_DELAY`]
    /// after the last change (now with `force`): stems whose new source is
    /// saved get it at once (`notes-ready`), the others are queued.
    pub fn flush_notes_pans(self: &Arc<Self>, force: bool) {
        let ready = {
            let mut st = lock(&self.st);
            match st.notes_pans_changed {
                Some(t) if force || t.elapsed() >= NOTES_DELAY => {}
                _ => return,
            }
            st.notes_pans_changed = None;
            let new = notes_pans(&st.refine);
            if new == st.notes_pans {
                return;
            }
            let old = std::mem::replace(&mut st.notes_pans, new);
            let Some(song) = st.song.as_ref() else {
                return;
            };
            let changed = |stem: &str| old.get(stem) != st.notes_pans.get(stem);
            song.notes_sources(&st.notes_pans)
                .into_iter()
                .filter(|(stem, _)| changed(stem))
                .filter_map(|(stem, source)| {
                    let notes = song.saved_notes(&stem, &source)?.clone();
                    Some(NotesReady {
                        song_id: song.meta.song_id.clone(),
                        stem,
                        source,
                        notes,
                    })
                })
                .collect::<Vec<_>>()
        };
        for r in ready {
            self.emit("notes-ready", &r);
        }
        self.maybe_spawn_notes();
    }

    /// Sets the open song's note detection settings, Fast runs and Quiet
    /// notes together (saved at once): stems whose notes for the new
    /// settings are saved get them at once (`notes-ready`), the others are
    /// queued (`notes-status`).
    pub fn set_notes_options(self: &Arc<Self>, fast: NotesFast, quiet: bool) -> Result<(), String> {
        let (song_id, ready) = {
            let mut st = lock(&self.st);
            let pans = st.notes_pans.clone();
            let song = st.song.as_mut().ok_or("No song open")?;
            if song.meta.notes_fast == fast && song.meta.notes_quiet == quiet {
                return Ok(());
            }
            let old = song.notes_sources(&pans);
            song.meta.notes_fast = fast;
            song.meta.notes_quiet = quiet;
            let ready = song
                .notes_sources(&pans)
                .into_iter()
                .filter(|s| !old.contains(s))
                .filter_map(|(stem, source)| {
                    let notes = song.saved_notes(&stem, &source)?.clone();
                    Some(NotesReady {
                        song_id: song.meta.song_id.clone(),
                        stem,
                        source,
                        notes,
                    })
                })
                .collect::<Vec<_>>();
            (song.meta.song_id.clone(), ready)
        };
        let saved = self.library.update(&song_id, |m| {
            m.notes_fast = fast;
            m.notes_quiet = quiet;
        });
        for r in ready {
            self.emit("notes-ready", &r);
        }
        self.maybe_spawn_notes();
        saved.map(|_| ()).map_err(|e| e.to_string())
    }

    // ------------------------------------------------------------ beats

    /// Tracks beats and downbeats; with `keep_beats` (a cached grid from an
    /// older downbeat decoder) only the downbeats are replaced.
    fn spawn_beats(
        self: &Arc<Self>,
        song_id: String,
        mix: Mix,
        generation: u64,
        keep_beats: Option<Vec<f64>>,
    ) {
        let Some(model_path) = self.cfg.beat_model.clone() else {
            log!("no beat model; click track and count-in use defaults");
            return;
        };
        let b = self.clone();
        spawn("shredlab-beats", move || {
            let t0 = Instant::now();
            match b.track_beats(&model_path, &mix, keep_beats) {
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
                    b.maybe_spawn_chords();
                    b.maybe_spawn_notes();
                }
                Err(e) => log!("beat tracking for {song_id} failed: {e}"),
            }
        });
    }

    fn track_beats(
        &self,
        model_path: &Path,
        mix: &Mix,
        keep_beats: Option<Vec<f64>>,
    ) -> Result<BeatData, String> {
        let spec =
            shredlab_beats::preprocess(&[&mix[0], &mix[1]], RATE).map_err(|e| e.to_string())?;
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
            shredlab_beats::infer(&spec, |inp: &[f32], shape| {
                model.run(inp, shape).map_err(|e| BeatErr(e.to_string()))
            })
        };
        let logits = match run(model) {
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
        let tracked = match keep_beats {
            Some(beats) => shredlab_beats::decode_bars(&logits, beats),
            None => shredlab_beats::postprocess_bars(&logits),
        };
        Ok(beat_data(tracked))
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
        // The speakers must not feed the tuner's microphone.
        if lock(&self.st).song.is_some() {
            self.stop_tuner_because("playback");
        }
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

    pub fn set_rate_correction(&self, cents: i32) {
        let mut st = lock(&self.st);
        st.rate_cents = cents.clamp(-audio::MAX_RATE_CENTS, audio::MAX_RATE_CENTS);
        if let Some(e) = &self.engine {
            e.set_rate_correction(st.rate_cents);
        }
    }

    /// Plays a reference tone at `hz` (absolute pitch; never exported).
    pub fn play_tone(&self, hz: f32, sound: audio::ToneSound) {
        if let Some(e) = &self.engine {
            e.play_tone(hz, sound);
        }
    }

    pub fn stop_tone(&self) {
        if let Some(e) = &self.engine {
            e.stop_tone();
        }
    }

    // ------------------------------------------------------------ live tuner

    pub fn list_input_devices(&self) -> Vec<InputDeviceInfo> {
        audio::list_input_devices()
            .into_iter()
            .map(|d| InputDeviceInfo {
                id: d.id,
                name: d.name,
                channels: d.channels,
                default: d.default,
            })
            .collect()
    }

    /// Starts listening on `channel` (0-based) of `device_id` (`None` = the
    /// system default) for notes down to `low_hz`, replacing a running
    /// tuner. Pauses playback and stops the reference tone first. A device
    /// that is gone falls back to the default (`fell_back`); otherwise the
    /// device and channel are saved with the app settings.
    pub fn start_tuner(
        &self,
        device_id: Option<String>,
        channel: u16,
        low_hz: f32,
    ) -> Result<TunerStarted, String> {
        if !(low_hz.is_finite() && low_hz > 0.0) {
            return Err(format!("Invalid lowest note {low_hz} Hz"));
        }
        let low_hz = low_hz.clamp(20.0, 2000.0);
        self.stop_tuner();
        self.pause();
        self.stop_tone();
        let devices = audio::list_input_devices();
        let (id, fell_back) = tuner::resolve_device(device_id.as_deref(), &devices)?;
        let channels = devices
            .iter()
            .find(|d| d.id == id)
            .map_or(1, |d| d.channels);
        let channel = if channel < channels && !fell_back {
            channel
        } else {
            0
        };
        let sink = self.sink.clone();
        let capture = tuner::start(&id, channel, low_hz, move |r| {
            if let Ok(v) = serde_json::to_value(r) {
                sink("tuner-reading", v);
            }
        })?;
        let info = capture.info().clone();
        log!(
            "tuner listening: {} (input {} of {}, {} Hz), lowest note {low_hz:.1} Hz{}",
            info.device_name,
            info.channel + 1,
            info.channels,
            info.sample_rate,
            if fell_back {
                " (saved device not found; default used)"
            } else {
                ""
            }
        );
        // Two starts at once: the later one wins, the other is closed.
        let old = lock(&self.tuner).replace(capture);
        drop(old);
        if !fell_back {
            let saved = lock(&self.app_settings).clone();
            if saved.tuner_device_id != device_id || saved.tuner_channel.unwrap_or(0) != channel {
                if let Err(e) = self.update_settings(true, |s| {
                    s.tuner_device_id = device_id;
                    s.tuner_channel = Some(channel);
                }) {
                    log!("tuner input not saved: {e}");
                }
            }
        }
        Ok(TunerStarted {
            device_id: id,
            device_name: info.device_name,
            channels: info.channels,
            channel: info.channel,
            sample_rate: info.sample_rate,
            fell_back,
        })
    }

    /// Stops the tuner's input stream; true if it was listening.
    pub fn stop_tuner(&self) -> bool {
        let capture = lock(&self.tuner).take();
        // Dropping joins the capture thread; done outside the lock.
        match capture {
            Some(c) => {
                c.stop();
                log!("tuner stopped");
                true
            }
            None => false,
        }
    }

    /// Stops the tuner on the backend's own initiative and tells the UI.
    fn stop_tuner_because(&self, reason: &'static str) {
        if self.stop_tuner() {
            self.emit("tuner-stopped", &TunerStopped { reason });
        }
    }

    /// Stops the tuner if its input stream failed (device unplugged) and
    /// emits `tuner-error`. Called by the position thread.
    pub fn poll_tuner(&self) {
        let failed = {
            let mut g = lock(&self.tuner);
            match g.as_ref().and_then(|c| c.error()) {
                Some(e) => g.take().map(|c| (c, e)),
                None => None,
            }
        };
        if let Some((c, e)) = failed {
            c.stop();
            log!("tuner input failed: {e}");
            self.emit(
                "tuner-error",
                &TunerError {
                    message: format!("The input stopped: {e}"),
                },
            );
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
            peaks: peaks(chans(&mix), n_points.clamp(1, 100_000) as usize),
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
        // An export plays its range once.
        click.count_in_each_repeat = false;
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
            rate_cents: req
                .rate_cents
                .clamp(-audio::MAX_RATE_CENTS, audio::MAX_RATE_CENTS),
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
            let canceled = cancel.load(Ordering::Relaxed);
            {
                let mut st = lock(&b.st);
                if st.export.as_ref().is_some_and(|c| Arc::ptr_eq(c, &cancel)) {
                    st.export = None;
                }
            }
            if canceled {
                let _ = std::fs::remove_file(&path);
                log!("export canceled, partial file removed");
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
                            canceled: false,
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
            return Err("canceled".into());
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
                    message: "Canceled".into(),
                    canceled: true,
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
        if notes_pans(&st.refine) != st.notes_pans {
            st.notes_pans_changed = Some(Instant::now());
        }
        // Refine (with the exception following the switch) and gains: an
        // exception on an off row makes it audible, and back.
        self.apply_gains(&st);
        st.song_dirty = Some((song_id, Instant::now()));
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

    /// Writes pending refine and per-song settings to `song.json` (after
    /// [`SONG_SAVE_DELAY`] of no changes, or now with `force`: song switch,
    /// close and app exit).
    pub fn flush_song_save(&self, force: bool) {
        let (song_id, refine, settings) = {
            let mut st = lock(&self.st);
            match &st.song_dirty {
                Some((_, t)) if force || t.elapsed() >= SONG_SAVE_DELAY => {}
                _ => return,
            }
            let Some((id, _)) = st.song_dirty.take() else {
                return;
            };
            let refine = st.refine.clone();
            let mut settings = None;
            if let Some(s) = st.song.as_mut().filter(|s| s.meta.song_id == id) {
                s.meta.refine = refine.clone();
                settings = s.meta.settings.clone();
            }
            (id, refine, settings)
        };
        let saved = self.library.update(&song_id, |m| {
            m.refine = refine;
            if settings.is_some() {
                m.settings = settings;
            }
        });
        if let Err(e) = saved {
            log!("cannot save settings of {song_id}: {e}");
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
            let base = self.read_cached_stems(&song_id, &info)?;
            if cancel.load(Ordering::Relaxed) {
                return Err("canceled".into());
            }
            progress(&mut th, 0.3, true);
            if want.level == 0 {
                return Ok(quantize_stems(base));
            }
            let o = original.ok_or(
                "Separation strength needs the original mix, which isn't available for this song.",
            )?;
            let (Some(ol), Some(or)) = (o.channels[0].as_f32(), o.channels[1].as_f32()) else {
                return Err("the original is not held as f32".into());
            };
            let views: Vec<[&[f32]; 2]> = base.iter().map(|(_, [l, r])| [&l[..], &r[..]]).collect();
            let len = ol.len();
            if views.iter().any(|v| v[0].len() != len || v[1].len() != len) {
                return Err("stems and original differ in length".into());
            }
            let out = apply_strength(
                [ol, or],
                &views,
                STRENGTH_P[want.level as usize],
                &StrengthOptions {
                    smooth: want.smooth,
                    ..Default::default()
                },
                &mut |f| progress(&mut th, 0.3 + 0.7 * f, false),
                &cancel,
            )
            .map_err(|_| "canceled".to_string())?;
            drop(views);
            Ok(quantize_stems(
                base.into_iter().map(|(name, _)| name).zip(out).collect(),
            ))
        })();
        let mut st = lock(&self.st);
        let current = st.strength_job.as_ref().is_some_and(|j| j.id == job)
            && st.generation == generation
            && !cancel.load(Ordering::Relaxed);
        if !current {
            log!("strength job {job} dropped (superseded or canceled)");
            drop(st);
            self.emit(
                "strength-error",
                &StrengthError {
                    song_id,
                    job,
                    message: "Canceled".into(),
                    canceled: true,
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
                // The notes follow the loaded stems' strength.
                self.maybe_spawn_notes();
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
                        canceled: false,
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
    /// `on`: the backing track built from the original (original minus the
    /// other stems) and the plain sum of `on`. `None` without stems or
    /// original.
    #[cfg(debug_assertions)]
    pub fn debug_mix_references(&self, on: &[&str]) -> Option<[[Vec<f32>; 2]; 2]> {
        let st = lock(&self.st);
        let song = st.song.as_ref()?;
        let o = song.original.as_ref()?;
        let stems = song.stems.as_ref()?;
        let p1 = [0, 1].map(|c| {
            let mut v: Vec<f32> = o.channels[c].to_vec();
            for t in stems.iter().filter(|t| !on.contains(&t.name.as_str())) {
                for (s, y) in v.iter_mut().zip(t.channels[c].to_vec()) {
                    *s -= y;
                }
            }
            v
        });
        let sum = [0, 1].map(|c| {
            let mut v = vec![0.0f32; o.channels[c].len()];
            for t in stems.iter().filter(|t| on.contains(&t.name.as_str())) {
                for (s, y) in v.iter_mut().zip(t.channels[c].to_vec()) {
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

    /// Autotest helper, unstretched: the refined backing mix built from the
    /// original, computed offline. `rows` are the heard rows (gain 1) with their on
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
            let s = t.channels.clone().map(|c| c.to_vec());
            let row = rows.iter().find(|(n, _)| *n == t.name);
            let r = row.map(|&(_, on)| {
                let mut p = st.refine.get(&t.name).copied().unwrap_or_default();
                p.pan.invert = on;
                refine_offline(&p, RATE, &s[0], &s[1])
            });
            for c in 0..2 {
                for (i, v) in out[c].iter_mut().enumerate() {
                    let sv = s[c].get(i).copied().unwrap_or(0.0);
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
    fn old_downbeats_are_decoded_again_from_scores() {
        let beats: Vec<f64> = (0..12).map(|i| 0.5 * i as f64).collect();
        let scores: Vec<f32> = (0..12)
            .map(|i| if i % 4 == 1 { 4.0 } else { -4.0 })
            .collect();
        let mut b = BeatData {
            beats: beats.clone(),
            downbeats: vec![0.5, 1.0, 2.5, 4.5],
            downbeat_scores: scores,
            ..Default::default()
        };
        assert!(redecode_downbeats(&mut b));
        assert_eq!(b.beats, beats);
        assert_eq!(b.downbeats, [0.5, 2.5, 4.5]);
        assert_eq!(b.beats_per_bar, Some(4));
        assert_eq!(b.downbeat_version, shredlab_beats::DOWNBEAT_DECODER_VERSION);
        // Without scores the model has to run again.
        let mut old = BeatData {
            beats,
            ..Default::default()
        };
        assert!(!redecode_downbeats(&mut old));
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
        let p = peaks([&l, &r], 10);
        assert_eq!(p.len(), 10);
        assert!((p[0] - 0.9).abs() < 1e-6);
        assert!((p[5] - 0.2).abs() < 1e-6);
        // More points than samples still works.
        assert_eq!(peaks([&[0.5; 3], &[0.5; 3]], 7).len(), 7);
        assert_eq!(peaks([&[], &[]], 4), vec![0.0; 4]);
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
            note_model: None,
            device: DevicePref::Cpu,
            beat_device: DevicePref::Cpu,
        };
        (Backend::new(cfg, None, sink).unwrap(), events)
    }

    #[test]
    fn concurrent_setters_keep_every_change() {
        let (b, _) = test_backend("settings-race");
        std::fs::create_dir_all(b.data_dir()).unwrap();
        let go = Arc::new(std::sync::Barrier::new(5));
        let setters: [fn(&Backend); 5] = [
            |b| drop(b.set_theme("valve").unwrap()),
            |b| drop(b.set_click_volume(0.25).unwrap()),
            |b| drop(b.set_stem_format(StemFormat::Flac).unwrap()),
            |b| drop(b.set_count_in_to_music_start(false).unwrap()),
            |b| drop(b.set_master_volume(0.5, true).unwrap()),
        ];
        let threads: Vec<_> = setters
            .into_iter()
            .map(|f| {
                let (b, go) = (b.clone(), go.clone());
                std::thread::spawn(move || {
                    go.wait();
                    f(&b);
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let saved = AppSettings::load(b.data_dir()).0;
        assert_eq!(saved.theme.as_deref(), Some("valve"));
        assert_eq!(saved.click_volume, Some(0.25));
        assert_eq!(saved.stem_format, Some(StemFormat::Flac));
        assert_eq!(saved.count_in_to_music_start, Some(false));
        assert_eq!(saved.master_volume, Some(0.5));
    }

    #[test]
    fn song_settings_are_saved_later_without_an_engine() {
        let (b, _) = test_backend("song-save");
        let (id, wav) = song_with_stems(&b);
        b.open_song(wav.to_str().unwrap()).unwrap();
        b.start_background();
        let saved = |b: &Backend| b.library.load(&id).unwrap().unwrap().settings;
        let v = serde_json::json!({"version": 1, "count_in_beats": 4});
        b.save_song_settings(&id, v.clone()).unwrap();
        assert_ne!(saved(&b), Some(v.clone()));
        // The position thread writes it after the delay, with no audio engine.
        let t0 = Instant::now();
        while saved(&b).as_ref() != Some(&v) && t0.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(saved(&b), Some(v));
        // A forced flush (song switch, close, app exit) writes at once.
        let w = serde_json::json!({"version": 1, "count_in_beats": 2});
        b.save_song_settings(&id, w.clone()).unwrap();
        b.flush_song_save(true);
        assert_eq!(saved(&b), Some(w));
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
            rate_cents: 0,
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
            rate_cents: 0,
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
                ..Default::default()
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
            count_in_each_repeat: false,
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

    #[test]
    fn flac_cached_stems_reload_to_the_same_integers() {
        // Holding stems as 16-bit integers loses nothing on a FLAC cache:
        // the reloaded stem quantizes to the integers of the fresh stem.
        let (b, _) = test_backend("flac-int16");
        std::fs::create_dir_all(b.data_dir()).unwrap();
        let n = 30_000;
        let loud: Vec<f32> = (0..n).map(|i| 1.37 * (i as f32 * 0.021).sin()).collect();
        let quiet: Vec<f32> = (0..n).map(|i| 0.01 * (i as f32 * 0.37).cos()).collect();
        let stems = [
            ("drums", [&loud[..], &quiet[..]]),
            ("piano", [&quiet[..], &quiet[..]]),
        ];
        let dir = shredlab_library::stem_set_dir("test", 1);
        let format = shredlab_library::CacheFormat::Flac;
        let files = b
            .library
            .write_stems("q1", &dir, RATE, &stems, format)
            .unwrap();
        let info = StemsInfo {
            model_id: "test".into(),
            model_name: "Test".into(),
            device_label: "CPU".into(),
            sample_rate: RATE,
            len: n,
            stems: files,
            shifts: 1,
            dir,
            format,
        };
        let reloaded = quantize_stems(b.read_cached_stems("q1", &info).unwrap());
        let ints = |t: &Track| {
            t.channels.clone().map(|c| match c {
                audio::Channel::I16 { data, step } => (data.to_vec(), step),
                audio::Channel::F32(_) => panic!("not quantized"),
            })
        };
        for ((name, ch), r) in stems.iter().zip(&reloaded) {
            let fresh = Track::quantized(*name, *ch, RATE);
            let (f, r) = (ints(&fresh), ints(r));
            for c in 0..2 {
                assert_eq!(f[c].0, r[c].0, "{name} channel {c}");
                assert!((f[c].1 - r[c].1).abs() <= f[c].1 * 1e-6);
            }
        }
    }

    /// A song with three cached stems (guitar hard-left 440 Hz + hard-right
    /// 660 Hz, centered bass and drums) and a cached original with a leftover.
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
        b.flush_song_save(true);
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
        let sel = pan_offline(
            &left.pan,
            RATE,
            &g.channels[0].to_vec(),
            &g.channels[1].to_vec(),
        );
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
                    let want = orig.channels[c].get(i) - g.channels[c].get(i) + sel[c][i];
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
        let e = ms_err(&got.channels[0], &|i| orig.channels[0].get(i));
        assert!(e < 1e-5, "{e}");
        let e = ms_err(&got.channels[1], &|i| {
            orig.channels[1].get(i) - g.channels[1].get(i)
        });
        assert!(e < 1e-5, "{e}");
        // All on (guitar ON except left): the original minus the left guitar.
        let got = export(
            "allon.wav",
            vec![row("guitar", true), row("bass", true), row("drums", true)],
        );
        let e = ms_err(&got.channels[0], &|i| {
            orig.channels[0].get(i) - g.channels[0].get(i)
        });
        assert!(e < 1e-5, "{e}");
        let e = ms_err(&got.channels[1], &|i| orig.channels[1].get(i));
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
                    let sum: f32 = stems.iter().map(|t| t.channels[0].get(i)).sum();
                    (sum - o.channels[0].get(i)).abs()
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
            s.stems.as_ref().unwrap()[0].channels[0].to_vec(),
            stems[0].channels[0].to_vec()
        );
        assert_eq!(s.meta.song_id, id);
    }

    #[test]
    fn notes_source_keys() {
        use shredlab_dsp::refine::PanParams;
        let natural = StrengthSetting::default();
        let normal = shredlab_notes::Detail::default();
        assert_eq!(
            notes_source("htdemucs_6s_s2", natural, None, normal),
            "htdemucs_6s_s2|st0"
        );
        let tight = StrengthSetting {
            level: 4,
            smooth: true,
        };
        let left = PanParams::left();
        assert_eq!(
            notes_source("m_s1", tight, Some(&left), normal),
            "m_s1|st4s|pan-1.00:1.34:0.20"
        );
        // The note settings are added when they are not the defaults.
        use shredlab_notes::{Detail, Fast};
        let more = Detail {
            fast: Fast::More,
            quiet: true,
        };
        assert_eq!(
            notes_source("m_s1", tight, Some(&left), more),
            "m_s1|st4s|pan-1.00:1.34:0.20|fmore|q"
        );
        let fewer = Detail {
            fast: Fast::Fewer,
            quiet: false,
        };
        assert_eq!(
            notes_source("m_s1", natural, None, fewer),
            "m_s1|st0|ffewer"
        );
        let quiet = Detail {
            fast: Fast::Normal,
            quiet: true,
        };
        assert_eq!(notes_source("m_s1", natural, None, quiet), "m_s1|st0|q");
        // The bass ignores them.
        let mut meta = SongMeta::new("x", std::path::Path::new("x.wav"), Default::default(), 1.0);
        meta.notes_fast = NotesFast::More;
        meta.notes_quiet = true;
        assert_eq!(notes_detail_for("bass", &meta), normal);
        assert_eq!(notes_detail_for("guitar", &meta), more);
        // Smooth means nothing at Natural; a disabled mask is no mask.
        let smooth0 = StrengthSetting {
            level: 0,
            smooth: true,
        };
        assert_eq!(
            notes_source("m_s1", smooth0, Some(&PanParams::default()), normal),
            "m_s1|st0"
        );
        let center = PanParams {
            center: -0.0,
            ..PanParams::center()
        };
        assert_eq!(
            notes_source("m_s1", natural, Some(&center), normal),
            "m_s1|st0|pan0.00:0.66:0.20"
        );
    }

    #[test]
    fn notes_cache_keeps_four_sources_per_stem() {
        let entry = |stem: &str, src: &str| StemNotes {
            stem: stem.into(),
            source: src.into(),
            ..Default::default()
        };
        let mut d = Some(NotesData {
            version: shredlab_notes::VERSION + 1,
            entries: vec![entry("guitar", "old")],
        });
        // Another algorithm version: dropped.
        add_notes_entry(&mut d, entry("bass", "a"));
        for src in ["a", "b", "c", "d", "e"] {
            add_notes_entry(&mut d, entry("guitar", src));
        }
        add_notes_entry(&mut d, entry("guitar", "c"));
        let d = d.unwrap();
        assert_eq!(d.version, shredlab_notes::VERSION);
        let keys: Vec<String> = d
            .entries
            .iter()
            .map(|e| format!("{}:{}", e.stem, e.source))
            .collect();
        assert_eq!(
            keys,
            ["bass:a", "guitar:b", "guitar:d", "guitar:e", "guitar:c"]
        );
    }

    #[test]
    fn notes_follow_strength_and_stereo_mask() {
        let (b, events) = test_backend("notes");
        let (id, wav) = song_with_stems(&b);
        // Beats and a current tuning, so the notes job can run.
        let beats: Vec<f64> = (0..6).map(|k| 0.25 + 0.5 * k as f64).collect();
        b.library
            .update(&id, |m| {
                m.beats = Some(BeatData {
                    downbeats: vec![beats[0], beats[4]],
                    downbeat_scores: vec![0.0; beats.len()],
                    downbeat_version: shredlab_beats::DOWNBEAT_DECODER_VERSION,
                    bpm: Some(120.0),
                    beats_per_bar: Some(4),
                    beats,
                });
                m.tuning = Some(shredlab_library::TuningData {
                    version: shredlab_tuning::VERSION,
                    reference_hz: 440.0,
                    ..Default::default()
                });
            })
            .unwrap();
        let wait = |b: &Arc<Backend>, stem: &str, src: &str| {
            let t0 = Instant::now();
            loop {
                let saved = b.library.load(&id).unwrap().unwrap().notes;
                let found = saved
                    .is_some_and(|n| n.entries.iter().any(|e| e.stem == stem && e.source == src));
                if found && lock(&b.st).notes_job.is_none() {
                    return;
                }
                assert!(
                    t0.elapsed() < Duration::from_secs(20),
                    "no notes for {stem} {src}"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        let info = b.open_song(wav.to_str().unwrap()).unwrap();
        assert!(info.notes.is_empty());
        // Drums have no notes; guitar comes first.
        assert_eq!(info.notes_pending, ["guitar", "bass"]);
        wait(&b, "guitar", "test_s1|st0");
        wait(&b, "bass", "test_s1|st0");
        assert!(lock(&events).iter().any(|e| e == "notes-ready"));
        assert!(lock(&events).iter().any(|e| e == "notes-status"));

        // Reopened: the saved notes come with the song, nothing pending.
        let info = b.open_song(wav.to_str().unwrap()).unwrap();
        let mut stems: Vec<&str> = info.notes.iter().map(|n| n.stem.as_str()).collect();
        stems.sort_unstable();
        assert_eq!(stems, ["bass", "guitar"]);
        assert!(info.notes_pending.is_empty());

        // A stereo mask on the guitar: followed after the delay (forced
        // here), computed for the new source; the bass keeps its notes.
        let left = RefineParams {
            pan: shredlab_dsp::refine::PanParams::left(),
            ..RefineParams::default()
        };
        b.set_refine("guitar", left).unwrap();
        b.flush_notes_pans(false);
        assert!(
            lock(&b.st).notes_pans.is_empty(),
            "followed before the delay"
        );
        b.flush_notes_pans(true);
        let masked = "test_s1|st0|pan-1.00:1.34:0.20";
        wait(&b, "guitar", masked);
        {
            let st = lock(&b.st);
            let song = st.song.as_ref().unwrap();
            assert!(song.notes_missing(&st.notes_pans).is_empty());
            let cur = song.current_notes(&st.notes_pans);
            assert!(cur.iter().any(|n| n.stem == "guitar" && n.source == masked));
            assert!(cur
                .iter()
                .any(|n| n.stem == "bass" && n.source == "test_s1|st0"));
        }
        // Back to no mask: the saved notes, no new job.
        lock(&events).clear();
        b.set_refine("guitar", RefineParams::default()).unwrap();
        b.flush_notes_pans(true);
        assert!(lock(&b.st).notes_job.is_none());
        assert!(lock(&events).iter().any(|e| e == "notes-ready"));

        // More fast runs and quiet notes, set together: saved with the
        // song, the guitar computed for them once; the bass (pitch tracker)
        // keeps its notes.
        b.set_notes_options(NotesFast::More, true).unwrap();
        let m = b.library.load(&id).unwrap().unwrap();
        assert_eq!((m.notes_fast, m.notes_quiet), (NotesFast::More, true));
        wait(&b, "guitar", "test_s1|st0|fmore|q");
        {
            let st = lock(&b.st);
            let song = st.song.as_ref().unwrap();
            assert!(song.notes_missing(&st.notes_pans).is_empty());
            let cur = song.current_notes(&st.notes_pans);
            assert!(cur
                .iter()
                .any(|n| n.stem == "bass" && n.source == "test_s1|st0"));
        }
        // Back to the defaults: the saved notes, no new job.
        lock(&events).clear();
        b.set_notes_options(NotesFast::Normal, false).unwrap();
        assert!(lock(&b.st).notes_job.is_none());
        assert!(lock(&events).iter().any(|e| e == "notes-ready"));
        assert!(b
            .open_song(wav.to_str().unwrap())
            .unwrap()
            .notes_pending
            .is_empty());

        // Another strength: the loaded stems change, so do the sources.
        b.set_strength(StrengthSetting {
            level: 2,
            smooth: false,
        })
        .unwrap();
        wait_strength(&b);
        wait(&b, "guitar", "test_s1|st2");
        wait(&b, "bass", "test_s1|st2");
    }

    #[test]
    fn loop_notes_keys_and_cache() {
        assert_eq!(
            loop_notes_source("m_s1|st0|fmore|q", 209.0004, 232.4996),
            "m_s1|st0|fmore|q|loop209000-232500"
        );
        assert_eq!(
            loop_notes_source("m_s1|st0", 1.2345, 2.0),
            "m_s1|st0|loop1235-2000"
        );
        let entry = |stem: &str, src: &str| StemNotes {
            stem: stem.into(),
            source: src.into(),
            ..Default::default()
        };
        let mut d = NotesData {
            version: shredlab_notes::VERSION + 1,
            entries: vec![entry("guitar", "old")],
        };
        // Another algorithm version: dropped, and never found.
        assert!(find_loop_notes(&d, "guitar", "old").is_none());
        for i in 0..LOOP_NOTES_KEEP + 5 {
            add_loop_notes_entry(&mut d, entry("guitar", &format!("l{i}")));
        }
        // The same key again moves to the end instead of repeating.
        add_loop_notes_entry(&mut d, entry("guitar", "l10"));
        add_loop_notes_entry(&mut d, entry("vocals", "l10"));
        assert_eq!(d.version, shredlab_notes::VERSION);
        assert_eq!(d.entries.len(), LOOP_NOTES_KEEP);
        let keys: Vec<String> = d
            .entries
            .iter()
            .map(|e| format!("{}:{}", e.stem, e.source))
            .collect();
        assert_eq!(keys[0], "guitar:l6");
        assert_eq!(keys[LOOP_NOTES_KEEP - 2..], ["guitar:l10", "vocals:l10"]);
        assert!(find_loop_notes(&d, "guitar", "l5").is_none());
        assert!(find_loop_notes(&d, "guitar", "l6").is_some());
        assert!(find_loop_notes(&d, "bass", "l10").is_none());
    }

    #[test]
    fn loop_notes_detect_the_loop_alone_and_are_cached() {
        let (b, _) = test_backend("loop-notes");
        let (id, wav) = song_with_stems(&b);
        let beats: Vec<f64> = (0..6).map(|k| 0.25 + 0.5 * k as f64).collect();
        b.library
            .update(&id, |m| {
                m.beats = Some(BeatData {
                    downbeats: vec![beats[0], beats[4]],
                    downbeat_scores: vec![0.0; beats.len()],
                    downbeat_version: shredlab_beats::DOWNBEAT_DECODER_VERSION,
                    bpm: Some(120.0),
                    beats_per_bar: Some(4),
                    beats,
                });
                m.tuning = Some(shredlab_library::TuningData {
                    version: shredlab_tuning::VERSION,
                    reference_hz: 440.0,
                    ..Default::default()
                });
            })
            .unwrap();
        // Before a song is open, and for the bass: errors.
        assert!(b
            .detect_loop_notes(0.5, 2.0, NotesFast::More, true, "guitar")
            .is_err());
        b.open_song(wav.to_str().unwrap()).unwrap();
        assert!(b
            .detect_loop_notes(0.5, 2.0, NotesFast::More, true, "bass")
            .is_err());
        assert!(b
            .detect_loop_notes(2.99, 9.0, NotesFast::More, true, "guitar")
            .is_err());
        let n = b
            .detect_loop_notes(0.5, 2.0, NotesFast::More, true, "guitar")
            .unwrap();
        assert_eq!(n.stem, "guitar");
        assert_eq!(n.source, "test_s1|st0|fmore|q|loop500-2000");
        assert!(n
            .notes
            .iter()
            .all(|x| x.start_s >= 0.5 && x.end_s <= 2.0 && x.start_s < x.end_s));
        assert!(n.chordal.iter().all(|c| c[0] >= 0.5 && c[1] <= 2.0));
        assert!(!n.notes.is_empty() || !n.chordal.is_empty());
        // Saved beside song.json, not in it, and returned from there.
        let saved = b.library.load_loop_notes(&id);
        assert_eq!(saved.entries, vec![n.clone()]);
        let again = b
            .detect_loop_notes(0.5, 2.0, NotesFast::More, true, "guitar")
            .unwrap();
        assert_eq!(again, n);
        assert_eq!(b.library.load_loop_notes(&id).entries.len(), 1);
        let meta = b.library.load(&id).unwrap().unwrap();
        assert_eq!(meta.notes_fast, NotesFast::Normal);
        assert!(!meta.notes_quiet);
        assert!(meta
            .notes
            .is_none_or(|d| d.entries.iter().all(|e| !e.source.contains("loop"))));
        // Other settings or another range: another entry.
        b.detect_loop_notes(0.5, 2.0, NotesFast::Normal, false, "guitar")
            .unwrap();
        b.detect_loop_notes(0.0, 1.0, NotesFast::More, true, "guitar")
            .unwrap();
        let keys: Vec<String> = b
            .library
            .load_loop_notes(&id)
            .entries
            .iter()
            .map(|e| e.source.clone())
            .collect();
        assert_eq!(
            keys,
            [
                "test_s1|st0|fmore|q|loop500-2000",
                "test_s1|st0|loop500-2000",
                "test_s1|st0|fmore|q|loop0-1000"
            ]
        );
    }
}
