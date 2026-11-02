//! App-wide settings persisted as `<data dir>/settings.json`: the chosen
//! separation model, separation quality, UI theme and "count in to where
//! the music starts". `None` means "use the default" (default model; High
//! on GPU, Standard on CPU; Stage; on). Env overrides
//! (`SHREDLAB_MODEL`, `SHREDLAB_DEVICE`) still win at runtime for development.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const FILE: &str = "settings.json";

/// UI themes (from the brand guide); the first is the default.
pub const THEMES: [&str; 4] = ["stage", "rosewood", "valve", "manuscript"];

/// Click and count-in volume when none is saved (linear).
pub const DEFAULT_CLICK_VOLUME: f32 = 0.8;
/// Master (playback) volume when none is saved (linear).
pub const DEFAULT_MASTER_VOLUME: f32 = 1.0;

/// Separation quality: number of time-shifted passes averaged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    /// 1 pass.
    Standard,
    /// 2 passes (Demucs `shifts=2`, deterministic offsets 0 and 0.25 s).
    High,
}

impl Quality {
    pub fn shifts(self) -> u32 {
        match self {
            Self::Standard => 1,
            Self::High => 2,
        }
    }

    pub fn from_shifts(shifts: u32) -> Self {
        if shifts >= 2 {
            Self::High
        } else {
            Self::Standard
        }
    }

    /// Default for a device label: High on GPU, Standard on CPU.
    pub fn default_for(device_label: &str) -> Self {
        if device_label.starts_with("GPU") {
            Self::High
        } else {
            Self::Standard
        }
    }
}

/// How new stems are stored in the library. MP3 VBR V0 by default (about
/// half the size of FLAC; listening-tested, `spikes/mp3_cache_test.py`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StemFormat {
    #[default]
    Mp3,
    /// Lossless.
    Flac,
}

impl StemFormat {
    pub fn cache_format(self) -> shredlab_library::CacheFormat {
        match self {
            Self::Mp3 => shredlab_library::CacheFormat::Mp3,
            Self::Flac => shredlab_library::CacheFormat::Flac,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AppSettings {
    /// Chosen model id (`None` = default model).
    #[serde(default)]
    pub model_id: Option<String>,
    /// Chosen quality (`None` = default for the device).
    #[serde(default)]
    pub quality: Option<Quality>,
    /// Chosen UI theme (`None` = Stage). The UI owns the names; see
    /// [`THEMES`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// Count-in from the song start skips a quiet intro (`None` = on).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count_in_to_music_start: Option<bool>,
    /// Click and count-in volume, linear 0..1 (`None` = 0.8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub click_volume: Option<f32>,
    /// Master playback volume, linear 0..1 (`None` = 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master_volume: Option<f32>,
    /// Format of newly written stems (`None` = MP3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stem_format: Option<StemFormat>,
}

impl AppSettings {
    /// The theme to use: the saved one if known, else the default.
    pub fn theme(&self) -> &'static str {
        self.theme
            .as_deref()
            .and_then(|t| THEMES.iter().find(|x| **x == t))
            .copied()
            .unwrap_or(THEMES[0])
    }

    /// "Count in to where the music starts" (on by default).
    pub fn count_in_to_music_start(&self) -> bool {
        self.count_in_to_music_start.unwrap_or(true)
    }

    /// Click and count-in volume (0.8 by default).
    pub fn click_volume(&self) -> f32 {
        self.click_volume
            .filter(|v| v.is_finite())
            .map_or(DEFAULT_CLICK_VOLUME, |v| v.clamp(0.0, 1.0))
    }

    /// Master playback volume (1 by default).
    pub fn master_volume(&self) -> f32 {
        self.master_volume
            .filter(|v| v.is_finite())
            .map_or(DEFAULT_MASTER_VOLUME, |v| v.clamp(0.0, 1.0))
    }

    /// Format of newly written stems (MP3 by default).
    pub fn stem_format(&self) -> StemFormat {
        self.stem_format.unwrap_or_default()
    }

    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join(FILE)
    }

    /// Loads the settings; a missing or unreadable file gives the defaults
    /// (and the error message, for the log).
    pub fn load(data_dir: &Path) -> (Self, Option<String>) {
        let path = Self::path(data_dir);
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(s) => (s, None),
                Err(e) => (
                    Self::default(),
                    Some(format!("{}: {e}; using defaults", path.display())),
                ),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::default(), None),
            Err(e) => (
                Self::default(),
                Some(format!("{}: {e}; using defaults", path.display())),
            ),
        }
    }

    /// Writes atomically (temp file + rename).
    pub fn save(&self, data_dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(data_dir).map_err(|e| e.to_string())?;
        let path = Self::path(data_dir);
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("shredlab-settings-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn persistence_roundtrip() {
        let d = dir("rt");
        assert_eq!(AppSettings::load(&d), (AppSettings::default(), None));
        let s = AppSettings {
            model_id: Some("bs-roformer-sw".into()),
            quality: Some(Quality::Standard),
            theme: Some("manuscript".into()),
            count_in_to_music_start: Some(false),
            click_volume: Some(0.5),
            master_volume: Some(0.7),
            stem_format: Some(StemFormat::Flac),
        };
        s.save(&d).unwrap();
        assert_eq!(AppSettings::load(&d).0, s);
        let text = std::fs::read_to_string(AppSettings::path(&d)).unwrap();
        assert!(text.contains("\"standard\""), "{text}");
        assert!(text.contains("\"stem_format\": \"flac\""), "{text}");
        assert_eq!(AppSettings::default().stem_format(), StemFormat::Mp3);
        // Back to defaults.
        AppSettings::default().save(&d).unwrap();
        assert_eq!(AppSettings::load(&d).0, AppSettings::default());
    }

    #[test]
    fn corrupt_file_gives_defaults() {
        let d = dir("bad");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(AppSettings::path(&d), "{nope").unwrap();
        let (s, err) = AppSettings::load(&d);
        assert_eq!(s, AppSettings::default());
        assert!(err.is_some());
        // Unknown fields and partial files are fine.
        std::fs::write(AppSettings::path(&d), r#"{"quality":"high","x":1}"#).unwrap();
        assert_eq!(AppSettings::load(&d).0.quality, Some(Quality::High));
    }

    #[test]
    fn theme_defaults_to_stage() {
        let mut s = AppSettings::default();
        assert_eq!(s.theme(), "stage");
        s.theme = Some("valve".into());
        assert_eq!(s.theme(), "valve");
        s.theme = Some("neon".into());
        assert_eq!(s.theme(), "stage");
        // Old files without a theme still load.
        let old: AppSettings =
            serde_json::from_str(r#"{"model_id":null,"quality":"high"}"#).unwrap();
        assert_eq!(old.theme, None);
        assert!(old.count_in_to_music_start());
    }

    #[test]
    fn quality_mapping() {
        assert_eq!(Quality::High.shifts(), 2);
        assert_eq!(Quality::from_shifts(1), Quality::Standard);
        assert_eq!(Quality::default_for("GPU"), Quality::High);
        assert_eq!(Quality::default_for("CPU"), Quality::Standard);
    }
}
