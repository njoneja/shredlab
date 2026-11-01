//! ONNX session for the Beat This! beat/downbeat model.
//!
//! The model glue (log-mel preprocessing, chunking, peak picking) lives in
//! `stemx-beats`, which is ORT-free; this module only provides the
//! inference closure: [`BeatModel::run`] matches the signature
//! `stemx_beats::infer` expects.

use std::path::Path;

use ort::session::Session;
use ort::value::TensorRef;

use crate::manifest::EpOptions;
use crate::runtime::{build_session, DevicePref, Ep, SessionOverrides};
use crate::{Error, Result};

pub struct BeatModel {
    session: Session,
    ep: Ep,
}

impl BeatModel {
    /// Loads `path`. With `Auto`/`Gpu`, each compiled-in GPU provider is
    /// tried first and CPU is the fallback (there is no self-test for this
    /// model, so callers that care about GPU correctness should use `Cpu`).
    pub fn load(
        path: &Path,
        device: DevicePref,
        cpu_threads: Option<usize>,
        log: &mut dyn FnMut(&str),
    ) -> Result<Self> {
        if !path.is_file() {
            return Err(Error::Model(format!(
                "beat model not found: {}",
                path.display()
            )));
        }
        let mut candidates = match device {
            DevicePref::Cpu => vec![],
            DevicePref::Auto | DevicePref::Gpu => Ep::gpu_candidates(),
        };
        candidates.push(Ep::Cpu);
        let ov = SessionOverrides {
            cpu_threads,
            ..Default::default()
        };
        let opts = EpOptions {
            enable_mem_pattern: Some(false),
            sequential: Some(true),
            ..Default::default()
        };
        let mut last_err = None;
        for ep in candidates {
            match build_session(path, ep, &opts, &ov, None) {
                Ok(session) => return Ok(Self { session, ep }),
                Err(e) => {
                    log(&format!("beat model: {} unavailable ({e})", ep.label()));
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| Error::Runtime("no execution provider".into())))
    }

    pub fn ep(&self) -> Ep {
        self.ep
    }

    /// One network call: `input` with `shape` `[1, frames, mels]` -> beat and
    /// downbeat logits (`frames` values each).
    pub fn run(&mut self, input: &[f32], shape: [usize; 3]) -> Result<(Vec<f32>, Vec<f32>)> {
        let rt = |e: ort::Error| Error::Runtime(format!("beat model: {e}"));
        let t = TensorRef::from_array_view((shape, input)).map_err(rt)?;
        let out = self.session.run(ort::inputs!["spect" => t]).map_err(rt)?;
        let (_, b) = out["beat"].try_extract_tensor::<f32>().map_err(rt)?;
        let (_, d) = out["downbeat"].try_extract_tensor::<f32>().map_err(rt)?;
        Ok((b.to_vec(), d.to_vec()))
    }
}
