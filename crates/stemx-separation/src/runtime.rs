//! ONNX Runtime environment, execution providers and session construction.

use std::path::Path;
use std::sync::OnceLock;

use ort::environment::Environment;
use ort::session::builder::SessionBuilder;
use ort::session::Session;

use crate::manifest::EpOptions;
use crate::{Error, Result};

/// Execution provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ep {
    Cpu,
    DirectMl,
    CoreMl,
    Cuda,
}

impl Ep {
    /// Key used in the manifest's `session` block and in caches.
    pub fn key(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::DirectMl => "dml",
            Self::CoreMl => "coreml",
            Self::Cuda => "cuda",
        }
    }

    /// Human-readable name.
    pub fn label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::DirectMl => "DirectML",
            Self::CoreMl => "CoreML",
            Self::Cuda => "CUDA",
        }
    }

    pub fn is_gpu(self) -> bool {
        self != Self::Cpu
    }

    /// GPU execution providers compiled into this build, in preference order.
    pub fn gpu_candidates() -> Vec<Ep> {
        [
            (cfg!(feature = "cuda"), Ep::Cuda),
            (cfg!(windows), Ep::DirectMl),
            (cfg!(target_os = "macos"), Ep::CoreMl),
        ]
        .into_iter()
        .filter_map(|(on, ep)| on.then_some(ep))
        .collect()
    }
}

impl std::fmt::Display for Ep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Device preference from the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DevicePref {
    /// GPU if available and it passes the self-test, else CPU.
    #[default]
    Auto,
    Cpu,
    /// GPU (still falls back to CPU if the GPU fails).
    Gpu,
}

impl std::str::FromStr for DevicePref {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "cpu" => Ok(Self::Cpu),
            "gpu" => Ok(Self::Gpu),
            _ => Err(format!("invalid device {s:?} (expected auto, cpu or gpu)")),
        }
    }
}

/// Debug overrides applied on top of the manifest's session options.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionOverrides {
    /// Force DirectML graph fusion on (`Some(true)`) or off (`Some(false)`).
    pub dml_graph_fusion: Option<bool>,
    /// CPU intra-op thread count.
    pub cpu_threads: Option<usize>,
}

impl SessionOverrides {
    /// Short fingerprint for cache keys ("" when no overrides).
    pub fn fingerprint(&self) -> String {
        let mut s = String::new();
        if let Some(f) = self.dml_graph_fusion {
            s.push_str(if f { "fusion=on;" } else { "fusion=off;" });
        }
        s
    }
}

static ENV: OnceLock<std::result::Result<Environment, String>> = OnceLock::new();

/// The process-wide ONNX Runtime environment.
pub fn environment() -> Result<&'static Environment> {
    ENV.get_or_init(|| {
        ort::init()
            .with_name("stemx")
            .build()
            .map_err(|e| e.to_string())
    })
    .as_ref()
    .map_err(|e| Error::Runtime(format!("cannot initialize ONNX Runtime: {e}")))
}

/// ONNX Runtime build info string (version, git commit).
pub fn ort_build_info() -> String {
    match environment() {
        Ok(_) => ort::info().to_string(),
        Err(e) => e.to_string(),
    }
}

/// ONNX Runtime version (`1.x.y`) parsed from the build info, if present.
pub fn ort_version() -> String {
    let info = ort_build_info();
    info.split([',', ' '])
        .find_map(|p| p.strip_prefix("git-branch=rel-"))
        .map(str::to_string)
        .unwrap_or(info)
}

/// Default CPU intra-op threads: min(physical cores, 16). ORT's own default
/// was ~5x slower on a hybrid (P/E core) i9-14900K.
pub fn default_cpu_threads() -> usize {
    num_cpus::get_physical().clamp(1, 16)
}

/// Creates a session for `ep` with the manifest's options for that EP plus
/// overrides. Registration failure of a GPU EP is an error (no silent CPU
/// fallback inside ORT).
pub fn build_session(
    model: &Path,
    ep: Ep,
    opts: &EpOptions,
    ov: &SessionOverrides,
) -> Result<Session> {
    let env = environment()?;
    let rt = |e: ort::Error| Error::Runtime(format!("{} session: {e}", ep.label()));
    let rtb =
        |e: ort::Error<SessionBuilder>| Error::Runtime(format!("{} session: {e}", ep.label()));
    let mut b = Session::builder(env).map_err(rt)?;

    if opts.enable_mem_pattern == Some(false) {
        b = b.with_memory_pattern(false).map_err(rtb)?;
    }
    if opts.sequential == Some(true) {
        b = b.with_parallel_execution(false).map_err(rtb)?;
    }
    match ep {
        Ep::Cpu => {
            let n = ov
                .cpu_threads
                .or(opts.intra_op_threads.filter(|&n| n > 0))
                .unwrap_or_else(default_cpu_threads);
            b = b.with_intra_threads(n).map_err(rtb)?;
        }
        Ep::DirectMl => {
            let disable_fusion = match ov.dml_graph_fusion {
                Some(fusion) => !fusion,
                None => opts.disable_graph_fusion.unwrap_or(false),
            };
            if disable_fusion {
                b = b
                    .with_config_entry("ep.dml.disable_graph_fusion", "1")
                    .map_err(rtb)?;
            }
            // DML requires these regardless of the manifest.
            b = b.with_memory_pattern(false).map_err(rtb)?;
            b = b.with_parallel_execution(false).map_err(rtb)?;
            b = register_gpu(b, ep)?;
        }
        Ep::CoreMl | Ep::Cuda => {
            b = register_gpu(b, ep)?;
        }
    }
    b.commit_from_file(model).map_err(rt)
}

#[allow(unreachable_code, unused_variables)]
fn register_gpu(b: SessionBuilder, ep: Ep) -> Result<SessionBuilder> {
    let dispatch = match ep {
        #[cfg(windows)]
        Ep::DirectMl => ort::ep::DirectML::default().build(),
        #[cfg(target_os = "macos")]
        Ep::CoreMl => ort::ep::CoreML::default().build(),
        #[cfg(feature = "cuda")]
        Ep::Cuda => ort::ep::CUDA::default().build(),
        _ => {
            return Err(Error::Runtime(format!(
                "{} is not available in this build",
                ep.label()
            )))
        }
    };
    b.with_execution_providers([dispatch.error_on_failure()])
        .map_err(|e| Error::Runtime(format!("cannot enable {}: {e}", ep.label())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_pref_parses() {
        assert_eq!("auto".parse::<DevicePref>().unwrap(), DevicePref::Auto);
        assert_eq!("GPU".parse::<DevicePref>().unwrap(), DevicePref::Gpu);
        assert!("tpu".parse::<DevicePref>().is_err());
    }

    #[test]
    fn cpu_threads_reasonable() {
        let n = default_cpu_threads();
        assert!((1..=16).contains(&n));
    }
}
