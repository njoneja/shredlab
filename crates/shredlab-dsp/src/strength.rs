// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Separation strength (SPEC §5.4 P4), offline.
//!
//! Every stem is re-derived from the original mix with a sharpened ratio
//! mask: per STFT bin and channel,
//!
//! ```text
//! stem_i' = |S_i|^p / Σ_j |S_j|^p · X
//! ```
//!
//! where `X` is the original's complex spectrum and `S_j` the cached stems'.
//! The masks sum to 1 in every bin, so the derived stems always sum to the
//! original (up to f32 rounding), for any `p`. `p = 1` is a plain ratio mask
//! ("Natural"); larger `p` pushes each bin towards its loudest stem (less
//! bleed, thinner sound), up to `p = 4` ("Tight").
//!
//! `p = 1` is *not* the cached stems: the stems don't sum to the original
//! (the model's leftover, about -20 dB), so the ratio mask redistributes that
//! leftover and replaces the stems' phases with the original's. The caller
//! should use the cached stems at the Natural end and this function only for
//! `p > 1` (see `the unpublished Refine notes` for measured deviations).
//!
//! Framing: periodic Hann analysis/synthesis windows, `n_fft` 4096, hop 1024
//! (the Demucs STFT size), no padding beyond zeros outside the song; f32 FFTs.
//! Memory: nothing song-sized besides the caller's inputs and the outputs
//! (which the streaming variant hands out in order instead of keeping);
//! work is split into chunks of frames processed on several threads.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

use crate::hann_periodic;

/// Strongest supported mask exponent.
pub const STRENGTH_MAX_P: f32 = 4.0;

/// A stereo signal as two channel slices of equal length.
pub type Stereo<'a> = [&'a [f32]; 2];

/// Options for [`strength_stream`] / [`apply_strength`].
#[derive(Clone, Copy, Debug)]
pub struct StrengthOptions {
    pub n_fft: usize,
    pub hop: usize,
    /// Worker threads; 0 = available parallelism.
    pub threads: usize,
    /// Frames per work item (~1.5 s at 64 frames of hop 1024).
    pub chunk_frames: usize,
    /// "Smooth" variant: scale each stem's own spectrum by
    /// `min(1, m_i(p) / m_i(1))` instead of splitting the original, so
    /// small `p` stays close to the model's stems. The result no longer sums
    /// to the original (see the module docs).
    pub smooth: bool,
}

impl Default for StrengthOptions {
    fn default() -> Self {
        Self {
            n_fft: 4096,
            hop: 1024,
            threads: 0,
            chunk_frames: 64,
            smooth: false,
        }
    }
}

/// The run was cancelled through the cancel flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

struct Plan {
    n_fft: usize,
    hop: usize,
    len: usize,
    stems: usize,
    frames: usize,
    chunk_frames: usize,
    chunks: usize,
    /// Samples a chunk's local buffer spans per lane.
    span: usize,
    p: f32,
    smooth: bool,
    window: Vec<f32>,
    ola_scale: f32,
}

impl Plan {
    /// Local output position of frame `k` in its chunk, and the song position
    /// of local index 0 of chunk `c` (may be negative).
    fn chunk_origin(&self, c: usize) -> isize {
        (c * self.chunk_frames * self.hop) as isize - (self.n_fft - self.hop) as isize
    }

    fn chunk_frames_of(&self, c: usize) -> usize {
        self.chunk_frames.min(self.frames - c * self.chunk_frames)
    }
}

struct Worker {
    fwd: Arc<dyn RealToComplex<f32>>,
    inv: Arc<dyn ComplexToReal<f32>>,
    tbuf: Vec<f32>,
    xs: Vec<Complex32>,
    ss: Vec<Vec<Complex32>>,
    ys: Vec<Complex32>,
    fscratch: Vec<Complex32>,
    iscratch: Vec<Complex32>,
    /// Per-stem weights of one bin.
    w: Vec<f32>,
    /// Per-stem squared magnitudes, `stems * bins`.
    mag2: Vec<f32>,
}

impl Worker {
    fn new(plan: &Plan) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fwd = planner.plan_fft_forward(plan.n_fft);
        let inv = planner.plan_fft_inverse(plan.n_fft);
        let bins = plan.n_fft / 2 + 1;
        Self {
            tbuf: fwd.make_input_vec(),
            xs: fwd.make_output_vec(),
            ss: (0..plan.stems).map(|_| fwd.make_output_vec()).collect(),
            ys: fwd.make_output_vec(),
            fscratch: fwd.make_scratch_vec(),
            iscratch: inv.make_scratch_vec(),
            w: vec![0.0; plan.stems],
            mag2: vec![0.0; plan.stems * bins],
            fwd,
            inv,
        }
    }

    fn analyze(&mut self, plan: &Plan, x: &[f32], start: isize, out: Option<usize>) {
        let n = x.len() as isize;
        for (k, b) in self.tbuf.iter_mut().enumerate() {
            let i = start + k as isize;
            *b = if i >= 0 && i < n {
                x[i as usize] * plan.window[k]
            } else {
                0.0
            };
        }
        let dst = match out {
            None => &mut self.xs,
            Some(s) => &mut self.ss[s],
        };
        self.fwd
            .process_with_scratch(&mut self.tbuf, dst, &mut self.fscratch)
            .expect("rfft size mismatch");
    }

    /// Processes chunk `c` into a local buffer, lanes `(stem, channel)`.
    fn chunk(&mut self, plan: &Plan, original: &Stereo, stems: &[Stereo], c: usize) -> Vec<f32> {
        let span = plan.span;
        let bins = plan.n_fft / 2 + 1;
        let ns = plan.stems;
        let mut local = vec![0.0f32; ns * 2 * span];
        let p_half = plan.p * 0.5;
        let uniform = 1.0 / ns as f32;
        for kk in 0..plan.chunk_frames_of(c) {
            let k = c * plan.chunk_frames + kk;
            let start = (k * plan.hop) as isize - (plan.n_fft - plan.hop) as isize;
            let off = kk * plan.hop;
            for ch in 0..2 {
                self.analyze(plan, original[ch], start, None);
                for (s, st) in stems.iter().enumerate() {
                    self.analyze(plan, st[ch], start, Some(s));
                }
                for (s, spec) in self.ss.iter().enumerate() {
                    for (m, v) in self.mag2[s * bins..(s + 1) * bins].iter_mut().zip(spec) {
                        *m = v.norm_sqr();
                    }
                }
                // Turn the magnitudes into masks in place (they sum to 1).
                for f in 0..bins {
                    let mut mx = 0.0f32;
                    for j in 0..ns {
                        mx = mx.max(self.mag2[j * bins + f]);
                    }
                    if mx <= 1e-30 {
                        for j in 0..ns {
                            self.mag2[j * bins + f] = uniform;
                        }
                        continue;
                    }
                    let inv = 1.0 / mx;
                    let mut sum = 0.0f32;
                    let mut sum1 = 0.0f32;
                    for j in 0..ns {
                        let r2 = self.mag2[j * bins + f] * inv;
                        let wj = weight(r2, plan.p, p_half);
                        self.w[j] = wj;
                        sum += wj;
                        if plan.smooth {
                            sum1 += r2.sqrt();
                        }
                    }
                    let k = 1.0 / sum;
                    for j in 0..ns {
                        let m = self.w[j] * k;
                        self.mag2[j * bins + f] = if plan.smooth {
                            // min(1, m(p) / m(1)), applied to the stem itself.
                            let m1 = (self.mag2[j * bins + f] * inv).sqrt() / sum1;
                            if m1 > 0.0 {
                                (m / m1).min(1.0)
                            } else {
                                0.0
                            }
                        } else {
                            m
                        };
                    }
                }
                for s in 0..ns {
                    let mask = &self.mag2[s * bins..(s + 1) * bins];
                    let src = if plan.smooth { &self.ss[s] } else { &self.xs };
                    for ((y, x), &m) in self.ys.iter_mut().zip(src).zip(mask) {
                        *y = x * m;
                    }
                    self.ys[0].im = 0.0;
                    self.ys[bins - 1].im = 0.0;
                    self.inv
                        .process_with_scratch(&mut self.ys, &mut self.tbuf, &mut self.iscratch)
                        .expect("irfft size mismatch");
                    let lane = &mut local[(s * 2 + ch) * span + off..][..plan.n_fft];
                    for ((o, &y), &w) in lane.iter_mut().zip(&self.tbuf).zip(&plan.window) {
                        *o += y * w * plan.ola_scale;
                    }
                }
            }
        }
        local
    }
}

/// `(|S|² / max²)^(p/2)`, i.e. `(|S| / max)^p`, with fast paths.
#[inline]
fn weight(r2: f32, p: f32, p_half: f32) -> f32 {
    if p == 2.0 {
        r2
    } else if p == 1.0 {
        r2.sqrt()
    } else if p == 4.0 {
        r2 * r2
    } else if r2 <= 0.0 {
        0.0
    } else {
        r2.powf(p_half)
    }
}

/// Derives the stems with mask exponent `p` (clamped to 0.5 … 4) and hands
/// the result out in order: `sink(start, out)` gets `out[stem][channel]`
/// covering song samples `start .. start + out[0][0].len()`, consecutive and
/// non-overlapping, together covering the whole song. `progress` gets
/// 0 … 1. `cancel` is checked between chunks.
pub fn strength_stream(
    original: Stereo,
    stems: &[Stereo],
    p: f32,
    opts: &StrengthOptions,
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
    sink: &mut dyn FnMut(usize, &[Stereo<'_>]),
) -> Result<(), Cancelled> {
    let len = original[0].len();
    assert_eq!(original[1].len(), len, "channel length mismatch");
    for s in stems {
        assert!(
            s[0].len() == len && s[1].len() == len,
            "stem length mismatch"
        );
    }
    assert!(!stems.is_empty(), "no stems");
    let (n_fft, hop) = (opts.n_fft, opts.hop);
    assert!(n_fft.is_multiple_of(hop) && n_fft / hop >= 2 && n_fft.is_multiple_of(2));
    if len == 0 {
        progress(1.0);
        return Ok(());
    }
    let window = hann_periodic(n_fft);
    let ola_norm: f32 = (0..n_fft / hop)
        .map(|d| window[hop / 3 + d * hop].powi(2))
        .sum();
    let frames = (len - 1 + n_fft - hop) / hop + 1;
    let chunk_frames = opts.chunk_frames.max(1);
    let chunks = frames.div_ceil(chunk_frames);
    let p = if p.is_finite() {
        p.clamp(0.5, STRENGTH_MAX_P)
    } else {
        1.0
    };
    let plan = Plan {
        n_fft,
        hop,
        len,
        stems: stems.len(),
        frames,
        chunk_frames,
        chunks,
        span: chunk_frames * hop + n_fft - hop,
        p,
        smooth: opts.smooth,
        window,
        ola_scale: 1.0 / (n_fft as f32 * ola_norm),
    };
    let threads = if opts.threads == 0 {
        std::thread::available_parallelism().map_or(4, |n| n.get())
    } else {
        opts.threads
    }
    .clamp(1, chunks);

    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let ns = plan.stems;
    let ov = n_fft - hop;
    let mut result = Ok(());
    std::thread::scope(|scope| {
        let (tx, rx) = mpsc::sync_channel::<(usize, Vec<f32>)>(threads * 2);
        for _ in 0..threads {
            let tx = tx.clone();
            let (plan, next, stop) = (&plan, &next, &stop);
            scope.spawn(move || {
                let mut w = Worker::new(plan);
                loop {
                    if stop.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let c = next.fetch_add(1, Ordering::Relaxed);
                    if c >= plan.chunks {
                        break;
                    }
                    let local = w.chunk(plan, &original, stems, c);
                    if tx.send((c, local)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);

        let mut pending: BTreeMap<usize, Vec<f32>> = BTreeMap::new();
        let mut carry = vec![0.0f32; ns * 2 * ov];
        let mut expect = 0usize;
        let span = plan.span;
        while expect < plan.chunks {
            if cancel.load(Ordering::Relaxed) {
                result = Err(Cancelled);
                break;
            }
            let Ok((c, local)) = rx.recv() else {
                result = Err(Cancelled);
                break;
            };
            pending.insert(c, local);
            while let Some(mut local) = pending.remove(&expect) {
                let c = expect;
                let nf = plan.chunk_frames_of(c);
                let fin = nf * hop;
                for lane in 0..ns * 2 {
                    let l = &mut local[lane * span..(lane + 1) * span];
                    for (a, b) in l[..ov].iter_mut().zip(&carry[lane * ov..(lane + 1) * ov]) {
                        *a += b;
                    }
                    carry[lane * ov..(lane + 1) * ov].copy_from_slice(&l[fin..fin + ov]);
                }
                // Finished song range of this chunk, clipped to [0, len).
                let origin = plan.chunk_origin(c);
                let lo = origin.max(0) as usize;
                let hi = ((origin + fin as isize).max(0) as usize).min(plan.len);
                if hi > lo {
                    let skip = (lo as isize - origin) as usize;
                    let out: Vec<[&[f32]; 2]> = (0..ns)
                        .map(|s| {
                            let lane = |ch: usize| {
                                let base = (s * 2 + ch) * span + skip;
                                &local[base..base + (hi - lo)]
                            };
                            [lane(0), lane(1)]
                        })
                        .collect();
                    sink(lo, &out);
                }
                expect += 1;
                progress(expect as f32 / plan.chunks as f32);
            }
        }
        stop.store(true, Ordering::Relaxed);
        // Drain so blocked senders can finish.
        while rx.recv().is_ok() {}
    });
    if cancel.load(Ordering::Relaxed) {
        return Err(Cancelled);
    }
    result
}

/// [`strength_stream`] collected into full-length buffers,
/// `out[stem] = [left, right]`.
pub fn apply_strength(
    original: Stereo,
    stems: &[Stereo],
    p: f32,
    opts: &StrengthOptions,
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<Vec<[Vec<f32>; 2]>, Cancelled> {
    let len = original[0].len();
    let mut out: Vec<[Vec<f32>; 2]> = (0..stems.len())
        .map(|_| [vec![0.0; len], vec![0.0; len]])
        .collect();
    strength_stream(
        original,
        stems,
        p,
        opts,
        progress,
        cancel,
        &mut |start, part| {
            for (o, s) in out.iter_mut().zip(part) {
                for ch in 0..2 {
                    o[ch][start..start + s[ch].len()].copy_from_slice(s[ch]);
                }
            }
        },
    )?;
    Ok(out)
}
