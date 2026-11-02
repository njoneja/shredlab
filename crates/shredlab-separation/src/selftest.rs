// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Device self-test (`docs/MODEL_MANIFEST.md`): run a deterministic input
//! through one model segment and compare probes/RMS with `selftest.json`.
//! Detects GPU backends that silently produce wrong output (e.g. DirectML
//! graph fusion on htdemucs).

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::manifest::{Manifest, SelfTestSpec};
use crate::{Error, Result};

/// Deterministic self-test input "synth-v1": `(channels, length)`, computed
/// in f64 and cast to f32.
pub fn synth_v1(length: usize, sample_rate: u32, channels: usize) -> Vec<Vec<f32>> {
    use std::f64::consts::PI;
    let two_pi = 2.0 * PI;
    (0..channels)
        .map(|c| {
            let c = c as f64;
            (0..length)
                .map(|i| {
                    let t = i as f64 / sample_rate as f64;
                    let x = 0.30 * (two_pi * 110.0 * t).sin()
                        + 0.20 * (two_pi * (440.0 + 3.0 * c) * t).sin()
                        + 0.15
                            * (two_pi * 1250.0 * t).sin()
                            * (0.5 + 0.5 * (two_pi * 2.0 * t).sin())
                        + 0.05
                            * (two_pi * 3.0 * (1.0 + c) * t + 7.0 * (two_pi * 0.5 * t).sin()).sin();
                    x as f32
                })
                .collect()
        })
        .collect()
}

/// Per-stem self-test result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StemCheck {
    pub stem: String,
    /// Probe SNR against max(probe energy, floor energy).
    pub snr_db: f64,
    /// |rms - expected| / max(expected, floor).
    pub rms_err: f64,
    /// rms / expected (informational).
    pub rms_ratio: f64,
    pub pass: bool,
}

/// Result of a self-test run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelfTestReport {
    pub passed: bool,
    pub stems: Vec<StemCheck>,
    /// Seconds spent in the model run.
    pub seconds: f64,
}

impl SelfTestReport {
    pub fn worst_snr_db(&self) -> f64 {
        self.stems
            .iter()
            .map(|s| s.snr_db)
            .fold(f64::INFINITY, f64::min)
    }
}

/// Compares a single-segment output `(N, C, S)` against the spec
/// (pass rule of `docs/MODEL_MANIFEST.md`, with the level floor
/// `floor = floor_rel * input_rms`).
pub fn check(
    spec: &SelfTestSpec,
    manifest: &Manifest,
    out: &[f32],
    seconds: f64,
) -> SelfTestReport {
    let s = spec.length;
    let nc = manifest.channels;
    let floor = spec.floor_rel * spec.input_rms;
    let p = spec.probe_offsets.len() as f64;
    let mut stems = Vec::new();
    for (i, name) in manifest.stems.iter().enumerate() {
        let exp = &spec.stems[name];
        let y = &out[i * nc * s..(i + 1) * nc * s];
        let ch0 = &y[..s];
        let (mut sig, mut err) = (0.0f64, 0.0f64);
        for (&o, &r) in spec.probe_offsets.iter().zip(&exp.probe) {
            let g = ch0[o] as f64;
            sig += r * r;
            err += (g - r) * (g - r);
        }
        let snr_db = 10.0 * (sig.max(p * floor * floor) / (err + 1e-30)).log10();
        let rms = (y.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / y.len() as f64).sqrt();
        let level = exp.rms.max(floor);
        let rms_err = if level > 0.0 {
            (rms - exp.rms).abs() / level
        } else if rms == 0.0 {
            0.0
        } else {
            f64::INFINITY
        };
        let rms_ratio = if exp.rms > 0.0 { rms / exp.rms } else { 0.0 };
        let pass = snr_db >= spec.tolerance_db && rms_err < spec.rms_tolerance;
        stems.push(StemCheck {
            stem: name.clone(),
            snr_db,
            rms_err,
            rms_ratio,
            pass,
        });
    }
    SelfTestReport {
        passed: stems.iter().all(|s| s.pass),
        stems,
        seconds,
    }
}

// --- caches ------------------------------------------------------------------

/// Cached self-test outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedResult {
    pub passed: bool,
    pub worst_snr_db: f64,
    pub unix_time: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    #[serde(default)]
    selftest: BTreeMap<String, CachedResult>,
    /// "path|size|mtime" -> sha256
    #[serde(default)]
    sha256: BTreeMap<String, String>,
}

/// JSON cache in a caller-provided directory (`shredlab-cache.json`) holding
/// self-test results per (model sha256, EP, ORT version, overrides) and
/// file hashes per (path, size, mtime). Without a directory nothing is
/// cached.
#[derive(Debug, Default)]
pub struct Cache {
    path: Option<PathBuf>,
    data: CacheFile,
}

impl Cache {
    pub const FILE_NAME: &'static str = "shredlab-cache.json";

    pub fn open(dir: Option<&Path>) -> Self {
        let path = dir.map(|d| d.join(Self::FILE_NAME));
        let data = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Self { path, data }
    }

    pub fn selftest_key(sha256: &str, ep_key: &str, ort_version: &str, overrides: &str) -> String {
        // "st1": bump when the pass rule changes so stale results are ignored.
        format!("st1|{sha256}|{ep_key}|ort {ort_version}|{overrides}")
    }

    pub fn get_selftest(&self, key: &str) -> Option<&CachedResult> {
        self.data.selftest.get(key)
    }

    pub fn put_selftest(&mut self, key: String, report: &SelfTestReport) {
        let unix_time = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        self.data.selftest.insert(
            key,
            CachedResult {
                passed: report.passed,
                worst_snr_db: report.worst_snr_db(),
                unix_time,
            },
        );
        self.save();
    }

    /// sha256 of a file, cached by (path, size, mtime).
    pub fn sha256_file(&mut self, path: &Path) -> Result<String> {
        let meta = std::fs::metadata(path).map_err(|e| Error::io(path, e))?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos());
        let key = format!("{}|{}|{}", path.display(), meta.len(), mtime);
        if let Some(h) = self.data.sha256.get(&key) {
            return Ok(h.clone());
        }
        let h = sha256_file(path)?;
        self.data.sha256.insert(key, h.clone());
        self.save();
        Ok(h)
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string_pretty(&self.data) {
            // Write-then-rename so a crash never leaves a truncated cache.
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, path);
            }
        }
    }
}

/// sha256 of a file as lowercase hex.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path).map_err(|e| Error::io(path, e))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| Error::io(path, e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synth_matches_formula_samples() {
        let x = synth_v1(44100, 44100, 2);
        assert_eq!(x.len(), 2);
        assert_eq!(x[0][0], 0.0);
        // t = 0.25 s, c = 0, computed with numpy (float64 -> float32)
        let t = 0.25f64;
        let p = 2.0 * std::f64::consts::PI;
        let want = 0.30 * (p * 110.0 * t).sin()
            + 0.20 * (p * 440.0 * t).sin()
            + 0.15 * (p * 1250.0 * t).sin() * (0.5 + 0.5 * (p * 2.0 * t).sin())
            + 0.05 * (p * 3.0 * t + 7.0 * (p * 0.5 * t).sin()).sin();
        assert_eq!(x[0][11025], want as f32);
        assert_ne!(x[0][11025], x[1][11025]);
    }

    #[test]
    fn sha256_known_value() {
        let p = std::env::temp_dir().join(format!("shredlab-sha-{}.txt", std::process::id()));
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(
            sha256_file(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn check_pass_and_fail() {
        let m = crate::manifest::Manifest::from_json(crate::manifest::tests::EXAMPLE).unwrap();
        let s = 8;
        let mut stems = BTreeMap::new();
        let out: Vec<f32> = (0..6 * 2 * s)
            .map(|i| ((i % 5) as f32 - 2.0) * 0.1)
            .collect();
        let offs = vec![1usize, 3, 6];
        for (i, name) in m.stems.iter().enumerate() {
            let y = &out[i * 2 * s..(i + 1) * 2 * s];
            let rms = (y.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / y.len() as f64).sqrt();
            let probe = offs.iter().map(|&o| y[o] as f64).collect();
            stems.insert(name.clone(), crate::manifest::StemProbe { rms, probe });
        }
        let spec = SelfTestSpec {
            schema: 1,
            input: "synth-v1".into(),
            length: s,
            stems,
            probe_offsets: offs,
            input_rms: 0.0,
            floor_rel: 0.0,
            tolerance_db: 30.0,
            rms_tolerance: 0.05,
        };
        assert!(check(&spec, &m, &out, 0.0).passed);
        let bad: Vec<f32> = out.iter().map(|v| v * 1.5).collect();
        let r = check(&spec, &m, &bad, 0.0);
        assert!(!r.passed);
        // A tiny error on a near-silent stem passes with a level floor.
        let mut spec2 = spec.clone();
        spec2.input_rms = 0.2;
        spec2.floor_rel = 0.1;
        for st in spec2.stems.values_mut() {
            for v in &mut st.probe {
                *v *= 1e-6;
            }
            st.rms *= 1e-6;
        }
        let tiny: Vec<f32> = out.iter().map(|v| v * 1e-6 * 1.5).collect();
        assert!(check(&spec2, &m, &tiny, 0.0).passed);
        assert!(!check(&spec, &m, &tiny, 0.0).passed);
    }
}
