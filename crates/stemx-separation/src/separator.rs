//! High-level entry point: load a model on the best working device and
//! separate songs.
//!
//! Device choice ([`Separator::load`]): for `Auto`/`Gpu`, each compiled-in
//! GPU execution provider is tried in order; it is used only if its session
//! builds and the self-test passes (results cached per model sha256, EP, ORT
//! version and overrides). Otherwise CPU is used.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::adapters::{self, Adapter};
use crate::manifest::Manifest;
use crate::registry::ModelEntry;
use crate::runtime::{build_session, ort_version, DevicePref, Ep, SessionOverrides};
use crate::selftest::{self, Cache, SelfTestReport};
use crate::{Error, Result};

#[derive(Debug, Clone)]
pub struct LoadOptions {
    pub device: DevicePref,
    /// Directory for `stemx-cache.json` (self-test results, file hashes).
    pub cache_dir: Option<PathBuf>,
    pub overrides: SessionOverrides,
    /// Check the ONNX file against the manifest's sha256 (cached by size+mtime).
    pub verify_sha256: bool,
    /// Run (or reuse a cached) self-test before using a device.
    pub self_test: bool,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            device: DevicePref::Auto,
            cache_dir: None,
            overrides: SessionOverrides::default(),
            verify_sha256: true,
            self_test: true,
        }
    }
}

/// Self-test outcome for the device in use.
#[derive(Debug, Clone)]
pub enum SelfTestStatus {
    Passed {
        cached: bool,
        worst_snr_db: f64,
    },
    /// Only possible on CPU (a failing GPU is never used).
    Failed {
        worst_snr_db: f64,
    },
    NotRun,
}

impl std::fmt::Display for SelfTestStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Passed {
                cached: true,
                worst_snr_db,
            } => {
                write!(f, "passed (cached, worst probe SNR {worst_snr_db:.1} dB)")
            }
            Self::Passed {
                cached: false,
                worst_snr_db,
            } => write!(f, "passed (worst probe SNR {worst_snr_db:.1} dB)"),
            Self::Failed { worst_snr_db } => {
                write!(f, "FAILED (worst probe SNR {worst_snr_db:.1} dB)")
            }
            Self::NotRun => f.write_str("not run"),
        }
    }
}

/// A device that was tried and rejected during loading.
#[derive(Debug, Clone)]
pub struct Rejected {
    pub ep: Ep,
    pub reason: String,
}

/// Separated stems: flat `(stems, channels, samples)`.
#[derive(Debug, Clone)]
pub struct Stems {
    pub names: Vec<String>,
    pub sample_rate: u32,
    pub channels: usize,
    pub len: usize,
    pub data: Vec<f32>,
}

impl Stems {
    /// Channel slices of stem `i`.
    pub fn stem(&self, i: usize) -> Vec<&[f32]> {
        (0..self.channels)
            .map(|c| &self.data[(i * self.channels + c) * self.len..][..self.len])
            .collect()
    }
}

pub struct Separator {
    entry: ModelEntry,
    opts: LoadOptions,
    adapter: Box<dyn Adapter>,
    ep: Ep,
    selftest: SelfTestStatus,
    rejected: Vec<Rejected>,
}

impl Separator {
    /// Loads a model on the preferred device, with self-test and fallback.
    /// `log` receives human-readable progress messages.
    pub fn load(entry: &ModelEntry, opts: LoadOptions, log: &mut dyn FnMut(&str)) -> Result<Self> {
        let m = &entry.manifest;
        let mut cache = Cache::open(opts.cache_dir.as_deref());
        let onnx = entry.onnx_path();
        if opts.verify_sha256 {
            let h = cache.sha256_file(&onnx)?;
            if !h.eq_ignore_ascii_case(&m.sha256) {
                return Err(Error::Model(format!(
                    "{}: sha256 mismatch (manifest {}, file {h})",
                    onnx.display(),
                    m.sha256
                )));
            }
        }
        let spec = if opts.self_test {
            Some(entry.load_selftest()?)
        } else {
            None
        };
        let mut candidates = match opts.device {
            DevicePref::Cpu => vec![],
            DevicePref::Auto | DevicePref::Gpu => Ep::gpu_candidates(),
        };
        candidates.push(Ep::Cpu);
        let ort = ort_version();
        let fp = opts.overrides.fingerprint();
        let mut rejected = Vec::new();

        for ep in candidates {
            let key = Cache::selftest_key(&m.sha256, ep.key(), &ort, &fp);
            let cached = cache.get_selftest(&key).cloned();
            if ep.is_gpu() && spec.is_some() {
                if let Some(c) = cached.as_ref().filter(|c| !c.passed) {
                    let reason = format!(
                        "self-test failed earlier (cached, worst probe SNR {:.1} dB)",
                        c.worst_snr_db
                    );
                    log(&format!("{}: skipped, {reason}", ep.label()));
                    rejected.push(Rejected { ep, reason });
                    continue;
                }
            }
            let t0 = Instant::now();
            let session = match build_session(&onnx, ep, &m.ep_options(ep.key()), &opts.overrides) {
                Ok(s) => s,
                Err(e) if ep.is_gpu() => {
                    log(&format!("{}: unavailable ({e})", ep.label()));
                    rejected.push(Rejected {
                        ep,
                        reason: e.to_string(),
                    });
                    continue;
                }
                Err(e) => return Err(e),
            };
            log(&format!(
                "{}: session ready in {:.1}s",
                ep.label(),
                t0.elapsed().as_secs_f64()
            ));
            let mut adapter = adapters::create(m, session)?;
            let status = match (&spec, cached) {
                (None, _) => SelfTestStatus::NotRun,
                (Some(_), Some(c)) if c.passed => SelfTestStatus::Passed {
                    cached: true,
                    worst_snr_db: c.worst_snr_db,
                },
                (Some(spec), _) => match run_selftest_on(adapter.as_mut(), m, spec) {
                    Ok(report) => {
                        cache.put_selftest(key, &report);
                        log(&format!(
                            "{}: self-test {} in {:.1}s (worst probe SNR {:.1} dB)",
                            ep.label(),
                            if report.passed { "passed" } else { "FAILED" },
                            report.seconds,
                            report.worst_snr_db()
                        ));
                        if report.passed {
                            SelfTestStatus::Passed {
                                cached: false,
                                worst_snr_db: report.worst_snr_db(),
                            }
                        } else if ep.is_gpu() {
                            rejected.push(Rejected {
                                ep,
                                reason: format!(
                                    "self-test failed (worst probe SNR {:.1} dB)",
                                    report.worst_snr_db()
                                ),
                            });
                            continue;
                        } else {
                            SelfTestStatus::Failed {
                                worst_snr_db: report.worst_snr_db(),
                            }
                        }
                    }
                    Err(e) if ep.is_gpu() => {
                        log(&format!("{}: self-test run failed ({e})", ep.label()));
                        rejected.push(Rejected {
                            ep,
                            reason: format!("self-test run failed: {e}"),
                        });
                        continue;
                    }
                    Err(e) => return Err(e),
                },
            };
            return Ok(Self {
                entry: entry.clone(),
                opts,
                adapter,
                ep,
                selftest: status,
                rejected,
            });
        }
        unreachable!("CPU is always a candidate")
    }

    pub fn manifest(&self) -> &Manifest {
        &self.entry.manifest
    }

    /// Execution provider in use.
    pub fn ep(&self) -> Ep {
        self.ep
    }

    pub fn selftest_status(&self) -> &SelfTestStatus {
        &self.selftest
    }

    /// Devices tried before the one in use, with the reason.
    pub fn rejected(&self) -> &[Rejected] {
        &self.rejected
    }

    /// Separates a song (planar, at the model's sample rate).
    ///
    /// `progress` gets the fraction done (0..=1) after each chunk; `cancel`
    /// is checked before each chunk ([`Error::Cancelled`]). If a GPU fails
    /// mid-run (runtime error), the model is reloaded on CPU and the song is
    /// restarted.
    pub fn separate(
        &mut self,
        channels: &[&[f32]],
        progress: &mut dyn FnMut(f32),
        cancel: &AtomicBool,
    ) -> Result<Stems> {
        let m = &self.entry.manifest;
        if channels.len() != m.channels {
            return Err(Error::Input(format!(
                "expected {} channels, got {}",
                m.channels,
                channels.len()
            )));
        }
        let len = channels[0].len();
        if len == 0 || channels.iter().any(|c| c.len() != len) {
            return Err(Error::Input(
                "empty input or channels of different lengths".into(),
            ));
        }
        progress(0.0);
        let result = run_separation(self.adapter.as_mut(), channels, progress, cancel);
        let data = match result {
            Err(Error::Runtime(e)) if self.ep.is_gpu() => {
                let failed = self.ep;
                self.rejected.push(Rejected {
                    ep: failed,
                    reason: format!("failed during separation: {e}"),
                });
                let session = build_session(
                    &self.entry.onnx_path(),
                    Ep::Cpu,
                    &m.ep_options("cpu"),
                    &self.opts.overrides,
                )?;
                self.adapter = adapters::create(m, session)?;
                self.ep = Ep::Cpu;
                self.selftest = SelfTestStatus::NotRun;
                progress(0.0);
                run_separation(self.adapter.as_mut(), channels, progress, cancel)?
            }
            r => r?,
        };
        Ok(Stems {
            names: m.stems.clone(),
            sample_rate: m.sample_rate,
            channels: m.channels,
            len,
            data,
        })
    }
}

fn run_separation(
    adapter: &mut dyn Adapter,
    channels: &[&[f32]],
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<Vec<f32>> {
    let data = adapter.separate(channels, &mut |done, total| {
        if done > 0 {
            progress(done as f32 / total as f32);
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        Ok(())
    })?;
    progress(1.0);
    Ok(data)
}

fn run_selftest_on(
    adapter: &mut dyn Adapter,
    m: &Manifest,
    spec: &crate::manifest::SelfTestSpec,
) -> Result<SelfTestReport> {
    let x = selftest::synth_v1(spec.length, m.sample_rate, m.channels);
    let views: Vec<&[f32]> = x.iter().map(Vec::as_slice).collect();
    let t0 = Instant::now();
    let out = adapter.run_single_segment(&views)?;
    Ok(selftest::check(spec, m, &out, t0.elapsed().as_secs_f64()))
}

/// Runs the self-test of `entry` on one specific execution provider (no
/// fallback, no cache lookup; the result is written to the cache if
/// `cache_dir` is given). For diagnostics (`stemx selftest`).
pub fn run_selftest(
    entry: &ModelEntry,
    ep: Ep,
    overrides: &SessionOverrides,
    cache_dir: Option<&std::path::Path>,
) -> Result<SelfTestReport> {
    let m = &entry.manifest;
    let spec = entry.load_selftest()?;
    let session = build_session(&entry.onnx_path(), ep, &m.ep_options(ep.key()), overrides)?;
    let mut adapter = adapters::create(m, session)?;
    let report = run_selftest_on(adapter.as_mut(), m, &spec)?;
    if cache_dir.is_some() {
        let mut cache = Cache::open(cache_dir);
        let key = Cache::selftest_key(
            &m.sha256,
            ep.key(),
            &ort_version(),
            &overrides.fingerprint(),
        );
        cache.put_selftest(key, &report);
    }
    Ok(report)
}
