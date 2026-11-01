//! Source separation: ONNX Runtime sessions, model registry (`model.json`
//! manifests), adapters (`demucs-hybrid`, `roformer-mask`) and the device
//! self-test.
//!
//! Typical use (e.g. from a background thread in the app):
//!
//! ```no_run
//! use std::sync::atomic::AtomicBool;
//! use stemx_separation::{LoadOptions, ModelRegistry, Separator};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let dirs = ModelRegistry::search_dirs(Some("res/models".as_ref()), Some("user/models".as_ref()));
//! let registry = ModelRegistry::scan(&dirs);
//! let entry = registry.require("htdemucs_6s")?;
//! let mut sep = Separator::load(entry, LoadOptions::default(), &mut |msg| eprintln!("{msg}"))?;
//! let (left, right) = (vec![0.0f32; 44100], vec![0.0f32; 44100]);
//! let cancel = AtomicBool::new(false);
//! let stems = sep.separate(&[&left, &right], &mut |f| eprintln!("{:.0}%", f * 100.0), &cancel)?;
//! # Ok(()) }
//! ```
//!
//! Paths (bundled resources, personal models, cache) are always passed in
//! by the caller; this crate has no Tauri dependency.

pub mod adapters;
pub mod beat_model;
pub mod manifest;
pub mod registry;
pub mod runtime;
pub mod selftest;
pub mod separator;

use std::path::Path;

pub use beat_model::BeatModel;
pub use manifest::{AdapterKind, Manifest, SelfTestSpec};
pub use registry::{ModelEntry, ModelRegistry, ModelSource};
pub use runtime::{DevicePref, Ep, SessionOverrides};
pub use selftest::SelfTestReport;
pub use separator::{
    max_shift, run_selftest, shift_offsets, LoadOptions, Rejected, SelfTestStatus, Separator, Stems,
};

/// Version of this crate, from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Crate name, from `Cargo.toml`.
pub const NAME: &str = env!("CARGO_PKG_NAME");

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid model: {0}")]
    Manifest(String),
    #[error("model error: {0}")]
    Model(String),
    #[error("ONNX Runtime: {0}")]
    Runtime(String),
    #[error("invalid input: {0}")]
    Input(String),
    #[error("cancelled")]
    Cancelled,
}

impl Error {
    pub(crate) fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.display().to_string(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_set() {
        assert_eq!(NAME, "stemx-separation");
        assert!(!VERSION.is_empty());
    }
}
