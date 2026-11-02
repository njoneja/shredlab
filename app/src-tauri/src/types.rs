// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Serde types of the UI <-> backend contract (`app/src/lib/API.md`).
//! Field names are the exact snake_case JSON names.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use crate::settings::{Quality, StemFormat};
pub use shredlab_dsp::refine::{EqParams, RefineParams};
pub use shredlab_library::StrengthSetting;

pub const STEMS: [&str; 6] = ["vocals", "guitar", "bass", "drums", "piano", "other"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeparatedWith {
    pub model_name: String,
    pub device_label: String,
    /// Registry id of the model.
    pub model_id: String,
    /// "standard" (1 pass) or "high" (2 passes).
    pub quality: Quality,
}

#[derive(Debug, Clone, Serialize)]
pub struct SongInfo {
    pub song_id: String,
    pub path: String,
    pub title: String,
    pub artist: Option<String>,
    pub duration_sec: f64,
    pub sample_rate: u32,
    pub channels: u32,
    pub stems_cached: bool,
    pub separated_with: Option<SeparatedWith>,
    pub beats_ready: bool,
    pub bpm: Option<f64>,
    pub settings: Option<serde_json::Value>,
    /// The original mix is loaded next to the stems, so the backing track
    /// can be built from it (P1). False before separation or when an old
    /// cache entry has no original and the source file is gone.
    pub original_available: bool,
    /// Refine panel settings of the refined stems (others are unrefined).
    pub refine: BTreeMap<String, RefineParams>,
    /// Saved separation strength of this song.
    pub strength: StrengthSetting,
    /// Id of a strength job the backend started while opening (applying a
    /// saved strength), 0 when none; its `strength-done`/`-error` event
    /// carries the same id.
    pub strength_job: u64,
}

/// One separation model from the registry.
#[derive(Debug, Clone, Serialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    /// Personal model (manifest `personal` flag or found in the personal
    /// models folder): shown as "personal, license unknown" when the
    /// license is unknown.
    pub personal: bool,
    /// SPDX id from the manifest, e.g. "MIT", or "unknown".
    pub license: String,
    pub stems: Vec<String>,
}

/// App settings as the UI shows them.
#[derive(Debug, Clone, Serialize)]
pub struct AppSettingsInfo {
    pub models: Vec<ModelInfo>,
    /// Model used for the next separation (null if no model is installed).
    pub model_id: Option<String>,
    /// The user's saved choice (null = default model).
    pub model_choice: Option<String>,
    /// True when `SHREDLAB_MODEL` overrides the choice (dev).
    pub model_from_env: bool,
    /// Quality used for the next separation.
    pub quality: Quality,
    /// The user's saved choice (null = default for the device).
    pub quality_choice: Option<Quality>,
    /// Default for the device of `model_id`: high on GPU, standard on CPU.
    pub default_quality: Quality,
    /// Predicted device of `model_id`: "GPU" or "CPU".
    pub device_label: String,
    /// UI theme: "stage" (default), "rosewood", "valve" or "manuscript".
    pub theme: String,
    /// A count-in from the song start skips a quiet intro (default on).
    pub count_in_to_music_start: bool,
    /// Click and count-in volume, linear 0..1 (default 0.8).
    pub click_volume: f32,
    /// Master playback volume, linear 0..1 (default 1). Not in exports.
    pub master_volume: f32,
    /// Format of newly written stems: "mp3" (default) or "flac".
    pub stem_format: StemFormat,
}

/// `set_app_settings` payload. Null = use the default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppSettingsUpdate {
    pub model_id: Option<String>,
    pub quality: Option<Quality>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LibraryEntry {
    pub song_id: String,
    pub path: String,
    pub title: String,
    pub artist: Option<String>,
    pub duration_sec: f64,
    pub stems_cached: bool,
    pub last_opened_ms: u64,
}

/// One song in the storage view (`list_library`).
#[derive(Debug, Clone, Serialize)]
pub struct StoredSongInfo {
    pub song_id: String,
    pub path: String,
    pub title: String,
    pub artist: Option<String>,
    pub size_bytes: u64,
    pub last_opened_ms: u64,
    pub source_exists: bool,
    /// Removed from the recent list.
    pub hidden: bool,
    /// Currently open (cannot be deleted).
    pub open: bool,
}

/// Everything stored in the library (`list_library`).
#[derive(Debug, Clone, Serialize)]
pub struct LibraryListing {
    /// The library folder.
    pub dir: String,
    pub total_bytes: u64,
    /// Largest first.
    pub songs: Vec<StoredSongInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SeparationStage {
    LoadingModel,
    Decoding,
    Separating,
    Writing,
    Beats,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StemMixState {
    pub stem: String,
    pub on: bool,
    pub volume: f32,
    pub solo: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct LoopState {
    pub a_sec: Option<f64>,
    pub b_sec: Option<f64>,
    pub enabled: bool,
}

impl LoopState {
    /// The active loop range, if enabled and valid.
    pub fn range(&self) -> Option<(f64, f64)> {
        match (self.enabled, self.a_sec, self.b_sec) {
            (true, Some(a), Some(b)) if a.is_finite() && b.is_finite() && a < b => Some((a, b)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClickState {
    pub enabled: bool,
    pub count_in_enabled: bool,
    pub count_in_beats: u32,
    pub subdivision: u32,
    pub beats_per_bar: u32,
    pub volume: f32,
    /// With a loop and the count-in on: count in again before each repeat.
    #[serde(default)]
    pub count_in_each_repeat: bool,
}

impl Default for ClickState {
    fn default() -> Self {
        Self {
            enabled: false,
            count_in_enabled: false,
            count_in_beats: 4,
            subdivision: 1,
            beats_per_bar: 4,
            volume: crate::settings::DEFAULT_CLICK_VOLUME,
            count_in_each_repeat: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MeterOverride {
    BeatOne {
        beat_index: usize,
    },
    BeatsPerBar {
        beat_index: usize,
        beats_per_bar: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeterOverridesPayload {
    pub overrides: Vec<MeterOverride>,
    pub downbeats: Vec<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Waveform {
    pub duration_sec: f64,
    pub peaks: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BeatGrid {
    pub beats: Vec<f64>,
    pub downbeats: Vec<f64>,
    pub bpm: Option<f64>,
    /// Where the music starts after a quiet intro (seconds), if the song
    /// has one; see `music_start.rs`. Independent of the setting.
    pub music_start: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    Wav,
    Flac,
    Mp3,
}

impl ExportFormat {
    pub fn ext(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Flac => "flac",
            Self::Mp3 => "mp3",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportStem {
    pub stem: String,
    pub volume: f32,
    /// The row's on switch. Decides which part a stem with a stereo
    /// exception plays (on = all but the window, off = the window alone).
    /// Missing = true.
    #[serde(default = "yes")]
    pub on: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportRequest {
    pub path: String,
    pub format: ExportFormat,
    pub stems: Vec<ExportStem>,
    /// The stems are the soloed ones: always the plain sum, never the P1
    /// backing track. Missing = false.
    #[serde(default)]
    pub solo: bool,
    pub speed_percent: f64,
    pub semitones: f64,
    pub cents: f64,
    /// Include the click track (current click settings). Missing = false.
    #[serde(default)]
    pub click: bool,
    /// Start with the count-in (current count-in settings). Missing = false.
    #[serde(default)]
    pub count_in: bool,
    /// Range start in song seconds. Missing = the song start.
    #[serde(default)]
    pub start_sec: Option<f64>,
    /// Range end in song seconds. Missing = the song end.
    #[serde(default)]
    pub end_sec: Option<f64>,
}

// ------------------------------------------------------------------ events

#[derive(Debug, Clone, Copy, Serialize)]
pub struct PlaybackPosition {
    pub position_sec: f64,
    pub playing: bool,
    pub count_in_beats_left: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeparationProgress {
    pub song_id: String,
    pub stage: SeparationStage,
    pub fraction: f32,
    pub model_name: String,
    pub device_label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeparationDone {
    pub song_id: String,
    pub separated_with: SeparatedWith,
    /// Same as `SongInfo.original_available` after the switch to stems.
    pub original_available: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeparationError {
    pub song_id: String,
    pub message: String,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SongIdPayload {
    pub song_id: String,
}

/// `set_refine` payload.
#[derive(Debug, Clone, Deserialize)]
pub struct RefineUpdate {
    pub stem: String,
    pub params: RefineParams,
}

/// One EQ preset of the Refine panel.
#[derive(Debug, Clone, Serialize)]
pub struct RefinePresetInfo {
    pub id: String,
    pub name: String,
    pub eq: EqParams,
}

/// `refine-field` event: stereo field of the stem whose panel is open.
#[derive(Debug, Clone, Serialize)]
pub struct RefineField {
    pub stem: String,
    /// 3 bands (low < 250 Hz <= mid < 2.5 kHz <= high) x 41 pan bins from
    /// hard left to hard right, band-major; power per hop, linear.
    pub bins: Vec<f32>,
}

/// `set_strength` result: the job id (0 = nothing to compute).
#[derive(Debug, Clone, Serialize)]
pub struct StrengthStarted {
    pub job: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StrengthProgress {
    pub song_id: String,
    pub job: u64,
    pub fraction: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct StrengthDone {
    pub song_id: String,
    pub job: u64,
    pub strength: StrengthSetting,
}

#[derive(Debug, Clone, Serialize)]
pub struct StrengthError {
    pub song_id: String,
    pub job: u64,
    pub message: String,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportProgress {
    pub fraction: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportDone {
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportError {
    pub message: String,
    pub cancelled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_shapes_match_contract() {
        let o: Vec<MeterOverride> = serde_json::from_str(
            r#"[{"kind":"beat_one","beat_index":3},{"kind":"beats_per_bar","beat_index":5,"beats_per_bar":3}]"#,
        )
        .unwrap();
        assert_eq!(o[0], MeterOverride::BeatOne { beat_index: 3 });
        let s = serde_json::to_value(SeparationStage::LoadingModel).unwrap();
        assert_eq!(s, "loading_model");
        let r: ExportRequest = serde_json::from_str(
            r#"{"path":"x.mp3","format":"mp3","stems":[{"stem":"bass","volume":0.5}],"speed_percent":75,"semitones":-2,"cents":10}"#,
        )
        .unwrap();
        assert_eq!(r.format, ExportFormat::Mp3);
        assert!(r.stems[0].on);
        assert!(!r.solo);
        assert_eq!((r.start_sec, r.end_sec), (None, None));
        let l: LoopState =
            serde_json::from_str(r#"{"a_sec":null,"b_sec":2.0,"enabled":true}"#).unwrap();
        assert_eq!(l.range(), None);
        let c: ClickState = serde_json::from_str(
            r#"{"enabled":true,"count_in_enabled":true,"count_in_beats":7,"subdivision":2,"beats_per_bar":4,"volume":0.5}"#,
        )
        .unwrap();
        assert_eq!(c.count_in_beats, 7);
        // Refine: older params (no shelf fields, the removed pan `mode`) and
        // partial objects load.
        let u: RefineUpdate = serde_json::from_str(
            r#"{"stem":"guitar","params":{"pan":{"enabled":true,"center":-1,"width":1.34,"mode":"keep"},"eq":{"low_cut":{"enabled":true,"freq":120}}}}"#,
        )
        .unwrap();
        assert!(u.params.pan.enabled && !u.params.pan.invert && u.params.eq.low_cut.cut_on());
        assert_eq!(u.params.eq.bands, EqParams::default().bands);
    }
}
