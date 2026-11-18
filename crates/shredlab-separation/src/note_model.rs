// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! ONNX session for Spotify's Basic Pitch note model (`nmp.onnx`), on the
//! CPU.
//!
//! The model glue (resampling, windowing, note decoding, the lead line)
//! lives in `shredlab-notes`, which is ORT-free; this module only provides
//! the inference closure: [`NoteModel::run`] takes one window of audio and
//! returns its note, onset and contour activations in the order
//! `shredlab_notes::WindowOutput` holds them.

use std::path::Path;

use ort::session::Session;
use ort::value::TensorRef;

use crate::manifest::EpOptions;
use crate::runtime::{build_session, Ep, SessionOverrides};
use crate::{Error, Result};

const INPUT: &str = "serving_default_input_2:0";
const CONTOUR: &str = "StatefulPartitionedCall:0";
const NOTE: &str = "StatefulPartitionedCall:1";
const ONSET: &str = "StatefulPartitionedCall:2";

/// Activations of one window: `(note, onset, contour)`, each row-major
/// `[frames][bins]`.
pub type NoteWindow = (Vec<f32>, Vec<f32>, Vec<f32>);

pub struct NoteModel {
    session: Session,
}

impl NoteModel {
    /// Loads `path` on the CPU execution provider (the model is small; a
    /// window takes about 10 ms).
    pub fn load(path: &Path, cpu_threads: Option<usize>) -> Result<Self> {
        if !path.is_file() {
            return Err(Error::Model(format!(
                "note model not found: {}",
                path.display()
            )));
        }
        let ov = SessionOverrides {
            cpu_threads,
            ..Default::default()
        };
        let opts = EpOptions {
            enable_mem_pattern: Some(false),
            sequential: Some(true),
            ..Default::default()
        };
        let session = build_session(path, Ep::Cpu, &opts, &ov, None)?;
        Ok(Self { session })
    }

    /// One network call: `window` (mono samples at the model rate, the
    /// input `[1, window.len(), 1]`) -> its activations.
    pub fn run(&mut self, window: &[f32]) -> Result<NoteWindow> {
        let rt = |e: ort::Error| Error::Runtime(format!("note model: {e}"));
        let t = TensorRef::from_array_view(([1usize, window.len(), 1], window)).map_err(rt)?;
        let out = self.session.run(ort::inputs![INPUT => t]).map_err(rt)?;
        let get = |name: &str| -> Result<Vec<f32>> {
            let (_, v) = out[name].try_extract_tensor::<f32>().map_err(rt)?;
            Ok(v.to_vec())
        };
        Ok((get(NOTE)?, get(ONSET)?, get(CONTOUR)?))
    }
}
