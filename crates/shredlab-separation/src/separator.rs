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
use crate::runtime::{
    build_session, coreml_cache_dir, ort_version, DevicePref, Ep, SessionOverrides,
};
use crate::selftest::{self, Cache, SelfTestReport};
use crate::{Error, Result};

#[derive(Debug, Clone)]
pub struct LoadOptions {
    pub device: DevicePref,
    /// Directory for `shredlab-cache.json` (self-test results, file hashes).
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
        let coreml_cache = opts
            .cache_dir
            .as_deref()
            .map(|d| coreml_cache_dir(d, &m.id, &m.sha256));
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
            let session = match build_session(
                &onnx,
                ep,
                &m.ep_options(ep.key()),
                &opts.overrides,
                coreml_cache.as_deref(),
            ) {
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

    /// Separates a song (planar, at the model's sample rate) in one pass.
    /// Same as [`Separator::separate_shifts`] with `shifts = 1`.
    pub fn separate(
        &mut self,
        channels: &[&[f32]],
        progress: &mut dyn FnMut(f32),
        cancel: &AtomicBool,
    ) -> Result<Stems> {
        self.separate_shifts(channels, 1, progress, cancel)
    }

    /// Separates a song (planar, at the model's sample rate), averaging
    /// `shifts` time-shifted passes (demucs `apply_model(shifts=N)` with
    /// deterministic offsets, see [`shift_offsets`]). `shifts <= 1` is a
    /// single plain pass.
    ///
    /// `progress` gets the overall fraction done (0..=1, across all passes)
    /// after each chunk; `cancel` is checked before each chunk
    /// ([`Error::Cancelled`]). If a GPU fails mid-run (runtime error), the
    /// model is reloaded on CPU and the song is restarted (progress goes back
    /// to 0).
    pub fn separate_shifts(
        &mut self,
        channels: &[&[f32]],
        shifts: usize,
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
        let sr = m.sample_rate;
        progress(0.0);
        let result = run_separation(
            self.adapter.as_mut(),
            channels,
            shifts,
            sr,
            progress,
            cancel,
        );
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
                    None,
                )?;
                self.adapter = adapters::create(m, session)?;
                self.ep = Ep::Cpu;
                self.selftest = SelfTestStatus::NotRun;
                progress(0.0);
                run_separation(
                    self.adapter.as_mut(),
                    channels,
                    shifts,
                    sr,
                    progress,
                    cancel,
                )?
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

/// Maximum time shift of a shifted pass: `int(0.5 * sample_rate)` samples,
/// as in demucs `apply_model`.
pub fn max_shift(sample_rate: u32) -> usize {
    sample_rate as usize / 2
}

/// Deterministic shift offsets (samples) of the `shifts` passes:
/// `offset_k = round(k * max_shift / shifts)` (half up) for `k` in
/// `0..shifts`, with `max_shift = int(0.5 * sample_rate)`. Upstream demucs
/// draws `random.randint(0, max_shift)` per pass; evenly spaced offsets give
/// the same averaging benefit and reproducible output. For `shifts = 2` at
/// 44.1 kHz: `[0, 11025]` (0 and 0.25 s). Empty for `shifts = 0`.
pub fn shift_offsets(shifts: usize, sample_rate: u32) -> Vec<usize> {
    let ms = max_shift(sample_rate);
    (0..shifts)
        .map(|k| (2 * k * ms + shifts) / (2 * shifts))
        .collect()
}

/// `shifts <= 1`: one plain [`Adapter::separate`] pass. Otherwise demucs
/// `apply_model` shifts: the song is normalized once over the real audio
/// (if the adapter normalizes), zero-padded by `max_shift` on both sides; for
/// each offset the view `padded[offset .. max_shift + len]` (which equals
/// `zeros(max_shift - offset) ++ x ++ zeros(offset)`, since chunk context
/// outside the view is zeros either way) goes through
/// [`Adapter::separate_normalized`]; the result is cropped back by
/// `max_shift - offset`, summed, divided by `shifts` and denormalized.
fn run_separation(
    adapter: &mut dyn Adapter,
    channels: &[&[f32]],
    shifts: usize,
    sample_rate: u32,
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<Vec<f32>> {
    if shifts <= 1 {
        return run_pass(adapter, channels, progress, cancel);
    }
    let len = channels[0].len();
    let ms = max_shift(sample_rate);
    let stats = adapter.song_stats(channels);
    let x: Vec<Vec<f32>> = match stats {
        Some((mean, std)) => adapters::normalized(channels, mean, std),
        None => channels.iter().map(|c| c.to_vec()).collect(),
    };
    let mut out: Vec<f32> = Vec::new();
    for (k, offset) in shift_offsets(shifts, sample_rate).into_iter().enumerate() {
        let lead = ms - offset;
        let shifted: Vec<Vec<f32>> = x
            .iter()
            .map(|c| {
                let mut v = vec![0.0f32; lead + len + offset];
                v[lead..lead + len].copy_from_slice(c);
                v
            })
            .collect();
        let views: Vec<&[f32]> = shifted.iter().map(Vec::as_slice).collect();
        let base = k as f32;
        let n = shifts as f32;
        let total_len = lead + len + offset;
        let y = adapter.separate_normalized(&views, &mut |done, total| {
            if done > 0 {
                progress((base + done as f32 / total as f32) / n);
            }
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            Ok(())
        })?;
        progress((base + 1.0) / n);
        let rows = y.len() / total_len;
        if out.is_empty() {
            out = vec![0.0f32; rows * len];
        }
        for (dst, src) in out.chunks_mut(len).zip(y.chunks(total_len)) {
            for (d, v) in dst.iter_mut().zip(&src[lead..lead + len]) {
                *d += v;
            }
        }
    }
    let n = shifts as f32;
    for v in &mut out {
        *v /= n;
    }
    if let Some((mean, std)) = stats {
        adapters::denormalize(&mut out, mean, std);
    }
    Ok(out)
}

/// One plain pass (today's single-pass path).
fn run_pass(
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
/// `cache_dir` is given). For diagnostics (`shredlab-cli selftest`).
pub fn run_selftest(
    entry: &ModelEntry,
    ep: Ep,
    overrides: &SessionOverrides,
    cache_dir: Option<&std::path::Path>,
) -> Result<SelfTestReport> {
    let m = &entry.manifest;
    let spec = entry.load_selftest()?;
    let coreml_cache = cache_dir.map(|d| coreml_cache_dir(d, &m.id, &m.sha256));
    let session = build_session(
        &entry.onnx_path(),
        ep,
        &m.ep_options(ep.key()),
        overrides,
        coreml_cache.as_deref(),
    )?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use shredlab_dsp::chunk;

    /// Fake model: stem 0 = input, stem 1 = 2 * input + 0.5, through the
    /// real demucs chunker (small segment) with demucs song normalization.
    struct Fake {
        segment: usize,
        normalize: bool,
    }

    impl Adapter for Fake {
        fn segment(&self) -> usize {
            self.segment
        }
        fn n_stems(&self) -> usize {
            2
        }
        fn run_window(&mut self, seg: &[f32]) -> Result<Vec<f32>> {
            let mut o = seg.to_vec();
            o.extend(seg.iter().map(|v| 2.0 * v + 0.5));
            Ok(o)
        }
        fn run_single_segment(&mut self, channels: &[&[f32]]) -> Result<Vec<f32>> {
            self.run_window(&channels.concat())
        }
        fn song_stats(&self, channels: &[&[f32]]) -> Option<(f32, f32)> {
            self.normalize.then(|| chunk::song_stats(channels))
        }
        fn separate_normalized(
            &mut self,
            channels: &[&[f32]],
            before_chunk: &mut dyn FnMut(usize, usize) -> Result<()>,
        ) -> Result<Vec<f32>> {
            let seg = self.segment;
            let total = chunk::chunk_offsets(channels[0].len(), seg, 0.25).len();
            let mut k = 0;
            chunk::apply_demucs(
                channels,
                seg,
                0.25,
                2,
                |w| {
                    before_chunk(k, total)?;
                    k += 1;
                    self.run_window(w)
                },
                &mut |_, _| {},
            )
        }
    }

    /// Pointwise fake without chunking: output depends only on each sample.
    struct Pointwise;

    impl Adapter for Pointwise {
        fn segment(&self) -> usize {
            16
        }
        fn n_stems(&self) -> usize {
            2
        }
        fn run_window(&mut self, seg: &[f32]) -> Result<Vec<f32>> {
            let mut o = seg.to_vec();
            o.extend(seg.iter().map(|v| v * v));
            Ok(o)
        }
        fn run_single_segment(&mut self, channels: &[&[f32]]) -> Result<Vec<f32>> {
            self.run_window(&channels.concat())
        }
        fn song_stats(&self, channels: &[&[f32]]) -> Option<(f32, f32)> {
            Some(chunk::song_stats(channels))
        }
        fn separate_normalized(
            &mut self,
            channels: &[&[f32]],
            before_chunk: &mut dyn FnMut(usize, usize) -> Result<()>,
        ) -> Result<Vec<f32>> {
            before_chunk(0, 1)?;
            let per: Vec<Vec<f32>> = channels
                .iter()
                .map(|c| self.run_window(c).unwrap())
                .collect();
            // (C, 2, L) -> (2, C, L)
            let l = channels[0].len();
            let mut o = Vec::new();
            for s in 0..2 {
                for p in &per {
                    o.extend_from_slice(&p[s * l..(s + 1) * l]);
                }
            }
            Ok(o)
        }
    }

    const SR: u32 = 100; // max_shift 50

    fn song(len: usize) -> (Vec<f32>, Vec<f32>) {
        let l = (0..len)
            .map(|i| (i as f32 * 0.1).sin() * 0.5 + 0.1)
            .collect();
        let r = (0..len).map(|i| (i as f32 * 0.037).cos() * 0.3).collect();
        (l, r)
    }

    fn run(a: &mut dyn Adapter, x: &[&[f32]], shifts: usize) -> (Vec<f32>, Vec<f32>) {
        let mut p = Vec::new();
        let y = run_separation(
            a,
            x,
            shifts,
            SR,
            &mut |f| p.push(f),
            &AtomicBool::new(false),
        )
        .unwrap();
        (y, p)
    }

    #[test]
    fn offsets_are_deterministic() {
        assert_eq!(max_shift(44100), 22050);
        assert_eq!(shift_offsets(2, 44100), vec![0, 11025]);
        assert_eq!(shift_offsets(2, 44100), shift_offsets(2, 44100));
        assert_eq!(shift_offsets(1, 44100), vec![0]);
        assert_eq!(shift_offsets(0, 44100), Vec::<usize>::new());
        assert_eq!(shift_offsets(3, 44100), vec![0, 7350, 14700]);
        assert_eq!(shift_offsets(4, 48000), vec![0, 6000, 12000, 18000]);
        // half rounds up: 1 * 25 / 2 = 12.5 -> 13
        assert_eq!(shift_offsets(2, 50), vec![0, 13]);
        assert!(shift_offsets(10, 44100)
            .iter()
            .all(|&o| o <= max_shift(44100)));
    }

    #[test]
    fn one_shift_is_the_plain_pass() {
        let (l, r) = song(300);
        let x = [&l[..], &r[..]];
        let mut a = Fake {
            segment: 64,
            normalize: true,
        };
        let plain = a.separate(&x, &mut |_, _| Ok(())).unwrap();
        for shifts in [0, 1] {
            let (y, p) = run(&mut a, &x, shifts);
            assert_eq!(y, plain, "shifts={shifts}");
            assert_eq!(p.last(), Some(&1.0));
        }
    }

    #[test]
    fn pointwise_model_is_shift_invariant() {
        // A pointwise model gives the same output for every shift, so the
        // average equals the single pass bit for bit iff alignment is right.
        let (l, r) = song(173);
        let x = [&l[..], &r[..]];
        let (one, _) = run(&mut Pointwise, &x, 1);
        let (two, _) = run(&mut Pointwise, &x, 2);
        assert_eq!(one, two);
    }

    #[test]
    fn shifted_chunked_identity_reconstructs_input() {
        let (l, r) = song(300);
        let x = [&l[..], &r[..]];
        for normalize in [true, false] {
            let mut a = Fake {
                segment: 64,
                normalize,
            };
            let (y, _) = run(&mut a, &x, 2);
            assert_eq!(y.len(), 2 * 2 * 300);
            for i in 0..300 {
                assert!((y[i] - l[i]).abs() < 1e-5, "L {i}");
                assert!((y[300 + i] - r[i]).abs() < 1e-5, "R {i}");
                // stem 1 is affine in the normalized domain
                let e = if normalize {
                    let (m, s) = chunk::song_stats(&x);
                    (2.0 * (l[i] - m) / s + 0.5) * s + m
                } else {
                    2.0 * l[i] + 0.5
                };
                assert!((y[600 + i] - e).abs() < 1e-4, "S1 L {i}");
            }
        }
    }

    #[test]
    fn shifted_progress_is_monotonic_and_cancellable() {
        let (l, r) = song(300);
        let x = [&l[..], &r[..]];
        let mut a = Fake {
            segment: 64,
            normalize: true,
        };
        let (_, p) = run(&mut a, &x, 3);
        assert!(p.windows(2).all(|w| w[0] <= w[1]), "{p:?}");
        assert!(p.iter().all(|&f| (0.0..=1.0).contains(&f)));
        assert_eq!(p.last(), Some(&1.0));
        assert!(p.contains(&(1.0 / 3.0)) && p.contains(&(2.0 / 3.0)));
        // every pass contributes intermediate values
        assert!(p.iter().filter(|&&f| f > 0.0 && f < 1.0 / 3.0).count() > 1);

        let cancel = AtomicBool::new(true);
        let r = run_separation(&mut a, &x, 2, SR, &mut |_| {}, &cancel);
        assert!(matches!(r, Err(Error::Cancelled)));
    }
}
