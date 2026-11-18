// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Where models come from.
//!
//! Separation models (folders with `model.json`), in priority order:
//! 1. bundled: `<resource_dir>/models` (Tauri `bundle.resources`)
//! 2. personal: `<app_data_dir>/models`
//! 3. `SHREDLAB_MODELS_DIR` (dev override, `;`-separated on Windows)
//! 4. debug builds only: the repo's `models/registry` and `models/personal`
//!
//! The beat model file `beat_this_final0.onnx`: `SHREDLAB_BEAT_MODEL`, then
//! `<resource_dir>/models/`, `<app_data_dir>/models/`, each
//! `SHREDLAB_MODELS_DIR` entry, and in debug builds the repo's `models/onnx/`.
//!
//! The note model file `basic_pitch_nmp.onnx` (Basic Pitch): the same order
//! with `SHREDLAB_NOTE_MODEL`, and in debug builds the repo's
//! `models/basic_pitch/nmp.onnx` (where `tools/setup_models.py` puts it).

use std::path::{Path, PathBuf};

use shredlab_separation::registry::env_dirs;
use shredlab_separation::{ModelRegistry, ModelSource};

pub const BEAT_MODEL_FILE: &str = "beat_this_final0.onnx";
pub const BEAT_MODEL_ENV: &str = "SHREDLAB_BEAT_MODEL";
pub const NOTE_MODEL_FILE: &str = "basic_pitch_nmp.onnx";
pub const NOTE_MODEL_ENV: &str = "SHREDLAB_NOTE_MODEL";
/// Preferred separation model id.
pub const DEFAULT_MODEL: &str = "htdemucs_6s";

/// Repo root (debug fallbacks only).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// Separation model search directories.
pub fn model_dirs(resource_dir: Option<&Path>, data_dir: &Path) -> Vec<(PathBuf, ModelSource)> {
    let bundled = resource_dir.map(|r| r.join("models"));
    let personal = data_dir.join("models");
    let mut dirs = ModelRegistry::search_dirs(bundled.as_deref(), Some(&personal));
    if cfg!(debug_assertions) {
        let root = repo_root();
        dirs.push((root.join("models").join("registry"), ModelSource::Dev));
        dirs.push((root.join("models").join("personal"), ModelSource::Personal));
    }
    dirs
}

/// Candidate paths for a single-file model, in priority order: `env`, then
/// `file` in the bundled and personal model folders and each
/// `SHREDLAB_MODELS_DIR` entry, then (debug builds) `dev` under the repo.
fn file_candidates(
    resource_dir: Option<&Path>,
    data_dir: &Path,
    env: &str,
    file: &str,
    dev: &[&str],
) -> Vec<PathBuf> {
    let mut c = Vec::new();
    if let Some(p) = std::env::var_os(env) {
        c.push(PathBuf::from(p));
    }
    if let Some(r) = resource_dir {
        c.push(r.join("models").join(file));
    }
    c.push(data_dir.join("models").join(file));
    for d in env_dirs() {
        c.push(d.join(file));
    }
    if cfg!(debug_assertions) {
        c.push(dev.iter().fold(repo_root(), |p, s| p.join(s)));
    }
    c
}

/// Candidate paths for the beat model, in priority order.
pub fn beat_model_candidates(resource_dir: Option<&Path>, data_dir: &Path) -> Vec<PathBuf> {
    file_candidates(
        resource_dir,
        data_dir,
        BEAT_MODEL_ENV,
        BEAT_MODEL_FILE,
        &["models", "onnx", BEAT_MODEL_FILE],
    )
}

/// First existing beat model file.
pub fn find_beat_model(resource_dir: Option<&Path>, data_dir: &Path) -> Option<PathBuf> {
    beat_model_candidates(resource_dir, data_dir)
        .into_iter()
        .find(|p| p.is_file())
}

/// Candidate paths for the note model, in priority order.
pub fn note_model_candidates(resource_dir: Option<&Path>, data_dir: &Path) -> Vec<PathBuf> {
    file_candidates(
        resource_dir,
        data_dir,
        NOTE_MODEL_ENV,
        NOTE_MODEL_FILE,
        &["models", "basic_pitch", "nmp.onnx"],
    )
}

/// First existing note model file.
pub fn find_note_model(resource_dir: Option<&Path>, data_dir: &Path) -> Option<PathBuf> {
    note_model_candidates(resource_dir, data_dir)
        .into_iter()
        .find(|p| p.is_file())
}

/// The model to separate with: `SHREDLAB_MODEL` if set and present, else
/// [`DEFAULT_MODEL`], else the first non-personal model, else the first.
pub fn pick_model(reg: &ModelRegistry) -> Option<&shredlab_separation::ModelEntry> {
    if let Ok(id) = std::env::var("SHREDLAB_MODEL") {
        if let Some(e) = reg.get(&id) {
            return Some(e);
        }
    }
    reg.get(DEFAULT_MODEL)
        .or_else(|| reg.models.iter().find(|m| !m.is_personal()))
        .or_else(|| reg.models.first())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_order() {
        let res = Path::new("R");
        let data = Path::new("D");
        let dirs = model_dirs(Some(res), data);
        assert_eq!(dirs[0], (res.join("models"), ModelSource::Bundled));
        assert_eq!(dirs[1], (data.join("models"), ModelSource::Personal));
        let beats = beat_model_candidates(Some(res), data);
        assert!(beats.contains(&res.join("models").join(BEAT_MODEL_FILE)));
        let notes = note_model_candidates(Some(res), data);
        assert!(notes.contains(&res.join("models").join(NOTE_MODEL_FILE)));
        assert!(notes.contains(&data.join("models").join(NOTE_MODEL_FILE)));
        if cfg!(debug_assertions) {
            assert!(notes
                .last()
                .is_some_and(|p| p.ends_with("basic_pitch/nmp.onnx")));
        }
        if cfg!(debug_assertions) {
            assert!(dirs.iter().any(|(d, _)| d.ends_with("registry")));
        }
    }
}
