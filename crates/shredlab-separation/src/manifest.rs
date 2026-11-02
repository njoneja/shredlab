// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! `model.json` and `selftest.json` (format: `docs/MODEL_MANIFEST.md`).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Manifest schema version this code understands.
pub const SCHEMA: u32 = 1;

/// Adapter (model I/O family).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdapterKind {
    #[serde(rename = "demucs-hybrid")]
    DemucsHybrid,
    #[serde(rename = "roformer-mask")]
    RoformerMask,
}

impl AdapterKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DemucsHybrid => "demucs-hybrid",
            Self::RoformerMask => "roformer-mask",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StftParams {
    pub n_fft: usize,
    pub hop: usize,
}

/// Session options for one execution provider. All optional; unknown keys
/// are ignored.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EpOptions {
    /// DirectML: `ep.dml.disable_graph_fusion=1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_graph_fusion: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_mem_pattern: Option<bool>,
    /// Sequential execution mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequential: Option<bool>,
    /// CPU: intra-op threads; 0 = app decides (min(physical cores, 16)).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intra_op_threads: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct License {
    pub spdx: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub notes: String,
}

/// `model.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub id: String,
    pub name: String,
    pub adapter: AdapterKind,
    pub file: String,
    pub sha256: String,
    pub sample_rate: u32,
    pub channels: usize,
    pub stems: Vec<String>,
    pub segment_samples: usize,
    /// demucs-hybrid: overlap fraction; roformer-mask: number of overlaps.
    pub overlap: f64,
    pub stft: StftParams,
    /// Per execution provider (`"dml"`, `"cpu"`, `"coreml"`, `"cuda"`).
    #[serde(default)]
    pub session: BTreeMap<String, EpOptions>,
    #[serde(default = "default_self_test")]
    pub self_test: String,
    pub license: License,
    #[serde(default)]
    pub personal: bool,
}

fn default_self_test() -> String {
    "selftest.json".into()
}

impl Manifest {
    pub fn from_json(text: &str) -> Result<Self> {
        let m: Manifest = serde_json::from_str(text).map_err(|e| Error::Manifest(e.to_string()))?;
        m.validate()?;
        Ok(m)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
        Self::from_json(&text).map_err(|e| Error::Manifest(format!("{}: {e}", path.display())))
    }

    /// Session options for an execution provider key (empty if absent).
    pub fn ep_options(&self, key: &str) -> EpOptions {
        self.session.get(key).cloned().unwrap_or_default()
    }

    fn validate(&self) -> Result<()> {
        let bad = |msg: String| Err(Error::Manifest(msg));
        if self.schema != SCHEMA {
            return bad(format!(
                "unsupported schema {} (expected {SCHEMA})",
                self.schema
            ));
        }
        if self.id.is_empty()
            || !self
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        {
            return bad(format!("invalid id {:?} (allowed: [a-z0-9_-])", self.id));
        }
        if self.channels != 2 {
            return bad(format!(
                "{}: only stereo models are supported (channels = {})",
                self.id, self.channels
            ));
        }
        if self.stems.is_empty() {
            return bad(format!("{}: no stems", self.id));
        }
        if self.segment_samples == 0 || self.stft.n_fft == 0 || self.stft.hop == 0 {
            return bad(format!("{}: segment_samples / stft must be > 0", self.id));
        }
        if self.file.contains(['/', '\\']) || self.file.contains("..") {
            return bad(format!("{}: `file` must be a plain file name", self.id));
        }
        match self.adapter {
            AdapterKind::DemucsHybrid if !(0.0..1.0).contains(&self.overlap) => bad(format!(
                "{}: demucs-hybrid overlap must be in [0, 1)",
                self.id
            )),
            AdapterKind::RoformerMask if self.overlap < 1.0 || self.overlap.fract() != 0.0 => bad(
                format!("{}: roformer-mask overlap must be an integer >= 1", self.id),
            ),
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StemProbe {
    pub rms: f64,
    pub probe: Vec<f64>,
}

/// `selftest.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelfTestSpec {
    pub schema: u32,
    /// Input generator name; only `"synth-v1"` is defined.
    pub input: String,
    pub length: usize,
    pub stems: BTreeMap<String, StemProbe>,
    pub probe_offsets: Vec<usize>,
    /// RMS of the synth input (both channels).
    #[serde(default)]
    pub input_rms: f64,
    /// Level floor relative to `input_rms` (near-silent stems are judged
    /// against `floor_rel * input_rms` instead of their own level).
    #[serde(default)]
    pub floor_rel: f64,
    pub tolerance_db: f64,
    /// Pass requires the relative rms error < rms_tolerance (default 0.05).
    #[serde(default = "default_rms_tolerance")]
    pub rms_tolerance: f64,
}

fn default_rms_tolerance() -> f64 {
    0.05
}

impl SelfTestSpec {
    pub fn from_json(text: &str) -> Result<Self> {
        serde_json::from_str(text).map_err(|e| Error::Manifest(format!("selftest: {e}")))
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
        Self::from_json(&text).map_err(|e| Error::Manifest(format!("{}: {e}", path.display())))
    }

    /// Checks consistency with the model manifest.
    pub fn validate_for(&self, m: &Manifest) -> Result<()> {
        let bad = |msg: String| Err(Error::Manifest(format!("{} selftest: {msg}", m.id)));
        if self.input != "synth-v1" {
            return bad(format!("unknown input generator {:?}", self.input));
        }
        if self.length != m.segment_samples {
            return bad(format!(
                "length {} != segment_samples {}",
                self.length, m.segment_samples
            ));
        }
        if self.probe_offsets.iter().any(|&o| o >= self.length) {
            return bad("probe offset out of range".into());
        }
        for s in &m.stems {
            match self.stems.get(s) {
                None => return bad(format!("missing stem {s:?}")),
                Some(p) if p.probe.len() != self.probe_offsets.len() => {
                    return bad(format!("stem {s:?}: probe length != probe_offsets length"))
                }
                _ => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const EXAMPLE: &str = r#"{
      "schema": 1, "id": "htdemucs_6s", "name": "Demucs v4 (6 stems)",
      "adapter": "demucs-hybrid", "file": "htdemucs_6s_own.onnx", "sha256": "ab",
      "sample_rate": 44100, "channels": 2,
      "stems": ["drums", "bass", "other", "vocals", "guitar", "piano"],
      "segment_samples": 343980, "overlap": 0.25,
      "stft": { "n_fft": 4096, "hop": 1024 },
      "session": {
        "dml": { "disable_graph_fusion": true, "enable_mem_pattern": false, "sequential": true },
        "cpu": { "intra_op_threads": 0 }
      },
      "self_test": "selftest.json",
      "license": { "spdx": "MIT", "source": "https://github.com/facebookresearch/demucs", "notes": "" },
      "personal": false
    }"#;

    #[test]
    fn parses_example() {
        let m = Manifest::from_json(EXAMPLE).unwrap();
        assert_eq!(m.adapter, AdapterKind::DemucsHybrid);
        assert_eq!(m.stems.len(), 6);
        assert_eq!(m.ep_options("dml").disable_graph_fusion, Some(true));
        assert_eq!(m.ep_options("dml").enable_mem_pattern, Some(false));
        assert_eq!(m.ep_options("cpu").intra_op_threads, Some(0));
        assert_eq!(m.ep_options("coreml"), EpOptions::default());
        assert!(!m.personal);
    }

    #[test]
    fn rejects_bad_manifests() {
        let bad_id = EXAMPLE.replace("\"htdemucs_6s\"", "\"Bad ID\"");
        assert!(Manifest::from_json(&bad_id).is_err());
        let bad_adapter = EXAMPLE.replace("demucs-hybrid", "mystery");
        assert!(Manifest::from_json(&bad_adapter).is_err());
        let bad_schema = EXAMPLE.replace("\"schema\": 1", "\"schema\": 2");
        assert!(Manifest::from_json(&bad_schema).is_err());
        let bad_file = EXAMPLE.replace("htdemucs_6s_own.onnx", "../x.onnx");
        assert!(Manifest::from_json(&bad_file).is_err());
    }

    #[test]
    fn parses_selftest() {
        let st = SelfTestSpec::from_json(
            r#"{"schema": 1, "input": "synth-v1", "length": 343980,
                "stems": {"drums": {"rms": 0.1, "probe": [0.0, 1.0]}},
                "probe_offsets": [0, 5], "tolerance_db": 30.0}"#,
        )
        .unwrap();
        assert_eq!(st.rms_tolerance, 0.05);
        let m = Manifest::from_json(EXAMPLE).unwrap();
        // missing stems -> invalid for this manifest
        assert!(st.validate_for(&m).is_err());
    }
}
