//! App-wide settings persisted as `<data dir>/settings.json`: the chosen
//! separation model and separation quality. `None` means "use the
//! default" (default model; High on GPU, Standard on CPU). Env overrides
//! (`STEMX_MODEL`, `STEMX_DEVICE`) still win at runtime for development.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const FILE: &str = "settings.json";

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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSettings {
    /// Chosen model id (`None` = default model).
    #[serde(default)]
    pub model_id: Option<String>,
    /// Chosen quality (`None` = default for the device).
    #[serde(default)]
    pub quality: Option<Quality>,
}

impl AppSettings {
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
        let d = std::env::temp_dir().join(format!("stemx-settings-{}-{name}", std::process::id()));
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
        };
        s.save(&d).unwrap();
        assert_eq!(AppSettings::load(&d).0, s);
        let text = std::fs::read_to_string(AppSettings::path(&d)).unwrap();
        assert!(text.contains("\"standard\""), "{text}");
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
    fn quality_mapping() {
        assert_eq!(Quality::High.shifts(), 2);
        assert_eq!(Quality::from_shifts(1), Quality::Standard);
        assert_eq!(Quality::default_for("GPU"), Quality::High);
        assert_eq!(Quality::default_for("CPU"), Quality::Standard);
    }
}
