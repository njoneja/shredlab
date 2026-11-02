// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Streaming stereo pan/width mask (Refine panel, SPEC §5.4 P5).
//!
//! Each STFT bin gets a pan estimate from the channel magnitudes,
//! `pan = (|R| - |L|) / (|L| + |R|)` (-1 hard left, +1 hard right, as in
//! `scripts/pan_split.py`), and one real gain applied to both channels, so a
//! kept part keeps its own stereo image. The selection is a window
//! `center ± width/2` with raised-cosine edges `softness` wide, centered on
//! the window edges: the gain is 0.5 exactly at `center ± width/2`. Because
//! the edges are complementary, a window and its inverse (`invert`) sum to
//! the input, and the Left / Center / Right presets tile the field (they also
//! sum to the input).
//!
//! # Framing and latency
//!
//! STFT `n_fft = 4096`, `hop = 1024` (75 % overlap), periodic Hann analysis
//! and synthesis windows (Σ w² = 1.5 at this overlap, so identity masks
//! reconstruct exactly up to f32 rounding). Every hop of input completes one
//! frame. The output is the input delayed by exactly [`PAN_LATENCY`]
//! (= `n_fft`) samples, constant for any block size and whether or not the
//! mask is active.
//!
//! # Bypass
//!
//! While the mask is disabled (and its fade-out has finished) the processor
//! does no FFTs: the output is the input delayed by [`PAN_LATENCY`],
//! bit-exact, read straight from the history buffer. Turning it on at a hop
//! boundary seeds the overlap-add state with what identity frames would have
//! produced (no FFT needed: an identity frame contributes `x · w² / 1.5`), so
//! the switch is seamless; the mask then fades in. Turning it off fades the
//! mask out and drops back to the delay once only identity frames remain in
//! the overlap-add buffer.
//!
//! # Real time
//!
//! All buffers are allocated in [`PanMask::new`]; [`PanMask::process`] and
//! friends never allocate or lock. Parameters are smoothed once per hop
//! (one-pole, [`PARAM_TAU_SECS`]) and the overlap-add crossfades each frame's
//! mask into the next, so parameter changes don't click.

use std::f32::consts::PI;
use std::sync::Arc;

use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use serde::{Deserialize, Serialize};

use crate::hann_periodic;

/// FFT size of the pan mask (93 ms at 44.1 kHz). Matches `pan_split.py`.
pub const PAN_N_FFT: usize = 4096;
/// Hop of the pan mask.
pub const PAN_HOP: usize = 1024;
/// Constant latency of [`PanMask`] (and [`super::RefineChain`]) in samples.
pub const PAN_LATENCY: usize = PAN_N_FFT;

/// Number of pan bins in the stereo-field analysis (bin `k` is centered on
/// pan `-1 + 2k / (FIELD_BINS - 1)`; bin 20 is dead center).
pub const FIELD_BINS: usize = 41;
/// Number of frequency bands in the stereo-field analysis (low, mid, high).
pub const FIELD_BANDS: usize = 3;
/// Band edges of the stereo-field analysis in Hz: low < 250 ≤ mid < 2500 ≤ high.
pub const FIELD_BAND_EDGES_HZ: [f32; 2] = [250.0, 2500.0];
/// Length of a stereo-field buffer: `FIELD_BANDS * FIELD_BINS`, band-major
/// (`out[band * FIELD_BINS + bin]`).
pub const FIELD_LEN: usize = FIELD_BANDS * FIELD_BINS;

/// Time constant of the parameter smoothing.
pub const PARAM_TAU_SECS: f32 = 0.03;

const SNAP: f32 = 1e-4;

/// Stereo-position selection of one stem.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PanParams {
    /// Off means no stereo selection (the stem passes unchanged).
    pub enabled: bool,
    /// Window center, -1 (hard left) … +1 (hard right).
    pub center: f32,
    /// Full width between the half-gain (-6 dB) points, 0 … 2+.
    pub width: f32,
    /// Width of each raised-cosine edge in pan units (0.02 … 1).
    pub softness: f32,
    /// Pass everything except the window instead of the window alone.
    /// Not saved and not set by the UI: the owner derives it at run time
    /// (ShredLab: the row's on switch, so the window is an exception to the
    /// switch). Old saved data with a `mode` field still loads (ignored).
    #[serde(skip)]
    pub invert: bool,
}

impl Default for PanParams {
    fn default() -> Self {
        Self {
            enabled: false,
            center: 0.0,
            width: 0.66,
            softness: 0.2,
            invert: false,
        }
    }
}

impl PanParams {
    /// Quick preset: the left third of the field (pan ≤ -0.33).
    pub fn left() -> Self {
        Self {
            enabled: true,
            center: -1.0,
            width: 1.34,
            ..Self::default()
        }
    }

    /// Quick preset: the center third (|pan| ≤ 0.33).
    pub fn center() -> Self {
        Self {
            enabled: true,
            center: 0.0,
            width: 0.66,
            ..Self::default()
        }
    }

    /// Quick preset: the right third (pan ≥ +0.33).
    pub fn right() -> Self {
        Self {
            enabled: true,
            center: 1.0,
            width: 1.34,
            ..Self::default()
        }
    }

    /// True when this selection passes the input unchanged.
    pub fn is_identity(&self) -> bool {
        !self.enabled
    }

    /// The mask's gain for a bin at `pan` (ignoring `enabled`).
    pub fn gain(&self, pan: f32) -> f32 {
        let sel = window_gain(pan, self.center, self.width, self.softness);
        if self.invert {
            1.0 - sel
        } else {
            sel
        }
    }

    fn sanitized(mut self) -> Self {
        let fin = |v: f32, d: f32| if v.is_finite() { v } else { d };
        self.center = fin(self.center, 0.0).clamp(-2.0, 2.0);
        self.width = fin(self.width, 0.66).clamp(0.0, 4.0);
        self.softness = fin(self.softness, 0.2).clamp(0.02, 2.0);
        self
    }
}

/// Raised-cosine window: 1 for `|pan - center| <= width/2 - softness/2`,
/// 0 beyond `width/2 + softness/2`, 0.5 at `width/2`.
pub fn window_gain(pan: f32, center: f32, width: f32, softness: f32) -> f32 {
    let d = (pan - center).abs();
    let t = ((d - (width * 0.5 - softness * 0.5)) / softness).clamp(0.0, 1.0);
    0.5 + 0.5 * (PI * t).cos()
}

/// Pan estimate of one bin from the channel magnitudes, -1 … +1.
#[inline]
pub fn pan_of(ml: f32, mr: f32) -> f32 {
    (mr - ml) / (ml + mr + 1e-20)
}

/// Smoothed copy of the parameters the mask uses per frame.
#[derive(Clone, Copy, Debug)]
struct Cur {
    center: f32,
    width: f32,
    softness: f32,
    /// 0 = the window, 1 = everything but the window (crossfades when
    /// `invert` changes).
    remove: f32,
    /// 0 = identity, 1 = full mask.
    amount: f32,
}

impl Cur {
    fn target(p: &PanParams) -> Self {
        Self {
            center: p.center,
            width: p.width,
            softness: p.softness,
            remove: if p.invert { 1.0 } else { 0.0 },
            amount: if p.enabled { 1.0 } else { 0.0 },
        }
    }
}

fn approach(cur: &mut f32, target: f32, alpha: f32) {
    *cur += (target - *cur) * alpha;
    if (target - *cur).abs() < SNAP {
        *cur = target;
    }
}

/// Streaming pan/width mask for one stereo stem. See the module docs.
pub struct PanMask {
    n_fft: usize,
    hop: usize,
    window: Vec<f32>,
    /// Identity overlap-add partial sums, `n_fft - hop` long (warm-up).
    partial: Vec<f32>,
    /// irfft scale and the Σw² normalization, `1 / (n_fft · Σw²)`.
    ola_scale: f32,
    fwd: Arc<dyn RealToComplex<f32>>,
    inv: Arc<dyn ComplexToReal<f32>>,
    /// Input history, `n_fft + hop`: `[0, n_fft)` completed input, new
    /// samples written at `n_fft + pos`.
    hist: [Vec<f32>; 2],
    /// Overlap-add accumulator, `n_fft`.
    acc: [Vec<f32>; 2],
    /// Finished output of the current hop, `hop`.
    ready: [Vec<f32>; 2],
    pos: usize,
    tbuf: Vec<f32>,
    spec: [Vec<Complex32>; 2],
    fscratch: Vec<Complex32>,
    iscratch: Vec<Complex32>,
    /// Stereo-field band index per FFT bin (`u8::MAX` = skipped, DC).
    band_of_bin: Vec<u8>,
    field: Vec<f32>,
    field_hops: u32,
    field_scale: f32,
    analysis: bool,
    target: PanParams,
    cur: Cur,
    alpha: f32,
    /// STFT path running (false = exact delay).
    active: bool,
    /// Consecutive identity frames while active.
    idle_frames: usize,
}

impl PanMask {
    /// New mask for `sample_rate` (used for smoothing and the field bands).
    pub fn new(sample_rate: u32) -> Self {
        let (n_fft, hop) = (PAN_N_FFT, PAN_HOP);
        let window = hann_periodic(n_fft);
        // Σ_d w²[i + d·hop] over the frames covering a position is constant
        // (1.5 for Hann at 75 % overlap); computed rather than assumed.
        let ola_norm: f32 = (0..n_fft / hop)
            .map(|d| window[hop / 3 + d * hop].powi(2))
            .sum();
        let partial = (0..n_fft - hop)
            .map(|i| {
                let mut s = 0.0f32;
                let mut k = hop + i;
                while k < n_fft {
                    s += window[k] * window[k];
                    k += hop;
                }
                s / ola_norm
            })
            .collect();
        let mut planner = RealFftPlanner::<f32>::new();
        let fwd = planner.plan_fft_forward(n_fft);
        let inv = planner.plan_fft_inverse(n_fft);
        let bins = n_fft / 2 + 1;
        let sr = sample_rate.max(1) as f32;
        let band_of_bin = (0..bins)
            .map(|f| {
                if f == 0 {
                    return u8::MAX;
                }
                let hz = f as f32 * sr / n_fft as f32;
                FIELD_BAND_EDGES_HZ.iter().filter(|&&e| hz >= e).count() as u8
            })
            .collect();
        let wsum: f32 = window.iter().sum();
        let hop_secs = hop as f32 / sr;
        let target = PanParams::default();
        Self {
            n_fft,
            hop,
            ola_scale: 1.0 / (n_fft as f32 * ola_norm),
            partial,
            fscratch: fwd.make_scratch_vec(),
            iscratch: inv.make_scratch_vec(),
            spec: [fwd.make_output_vec(), fwd.make_output_vec()],
            tbuf: fwd.make_input_vec(),
            fwd,
            inv,
            window,
            hist: [vec![0.0; n_fft + hop], vec![0.0; n_fft + hop]],
            acc: [vec![0.0; n_fft], vec![0.0; n_fft]],
            ready: [vec![0.0; hop], vec![0.0; hop]],
            pos: 0,
            band_of_bin,
            field: vec![0.0; FIELD_LEN],
            field_hops: 0,
            // A sine of amplitude A peaks at |X| = A·Σw/2 in its bin.
            field_scale: 4.0 / (wsum * wsum),
            analysis: false,
            target,
            cur: Cur::target(&target),
            alpha: 1.0 - (-hop_secs / PARAM_TAU_SECS).exp(),
            active: false,
            idle_frames: 0,
        }
    }

    /// Latency in samples (constant): [`PAN_LATENCY`].
    pub fn latency(&self) -> usize {
        self.n_fft
    }

    /// Sets the target parameters; the mask moves there smoothly.
    pub fn set_params(&mut self, p: &PanParams) {
        self.target = p.sanitized();
    }

    /// Sets the parameters without smoothing (for a fresh start or offline).
    /// Use with [`reset`](Self::reset) or before the first sample; mid-stream
    /// it can click.
    pub fn set_params_now(&mut self, p: &PanParams) {
        self.target = p.sanitized();
        self.cur = Cur::target(&self.target);
        if self.target.enabled && !self.active {
            self.warm_up();
            self.active = true;
            self.idle_frames = 0;
        }
    }

    pub fn params(&self) -> &PanParams {
        &self.target
    }

    /// True while the output is exactly the delayed input (no STFT path).
    pub fn is_bypassed(&self) -> bool {
        !self.active
    }

    /// Clears all signal state (history, overlap-add) and jumps to the
    /// target parameters. For seeks: call, then [`prime`](Self::prime) with
    /// the next `latency()` input samples.
    pub fn reset(&mut self) {
        for c in 0..2 {
            self.hist[c].fill(0.0);
            self.acc[c].fill(0.0);
            self.ready[c].fill(0.0);
        }
        self.pos = 0;
        self.cur = Cur::target(&self.target);
        // History is zero, so the identity overlap-add state is zero too.
        self.active = self.target.enabled;
        self.idle_frames = 0;
        self.field.fill(0.0);
        self.field_hops = 0;
    }

    /// Enables the stereo-field analysis (it runs a forward FFT per hop even
    /// while bypassed).
    pub fn set_analysis(&mut self, on: bool) {
        self.analysis = on;
        if !on {
            self.field.fill(0.0);
            self.field_hops = 0;
        }
    }

    /// Copies the stereo-field energy averaged over the hops since the last
    /// call into `out` (`FIELD_LEN`, band-major, see [`FIELD_LEN`]) and
    /// clears it. Returns the number of hops averaged (0: `out` is zeroed).
    /// Values are power of the stem's input per hop (a steady sine of
    /// amplitude A reads about 1.5·A² summed over its bins: A² in its own
    /// bin plus Hann leakage), measured on the input as it enters the
    /// processor, i.e. `latency()` samples ahead of the output.
    pub fn take_field(&mut self, out: &mut [f32]) -> u32 {
        let n = self.field_hops;
        let k = if n > 0 { 1.0 / n as f32 } else { 0.0 };
        for (o, f) in out.iter_mut().zip(self.field.iter_mut()) {
            *o = *f * k;
            *f = 0.0;
        }
        self.field_hops = 0;
        n
    }

    /// Processes a block in place (any length, including 0).
    pub fn process_inplace(&mut self, l: &mut [f32], r: &mut [f32]) {
        assert_eq!(l.len(), r.len());
        let n = l.len();
        let (hop, n_fft) = (self.hop, self.n_fft);
        let mut i = 0;
        while i < n {
            let m = (hop - self.pos).min(n - i);
            let p = self.pos;
            for (c, x) in [&mut *l, &mut *r].into_iter().enumerate() {
                let x = &mut x[i..i + m];
                self.hist[c][n_fft + p..n_fft + p + m].copy_from_slice(x);
                let src = if self.active {
                    &self.ready[c][p..p + m]
                } else {
                    &self.hist[c][p..p + m]
                };
                x.copy_from_slice(src);
            }
            self.pos += m;
            i += m;
            if self.pos == hop {
                self.frame();
                self.pos = 0;
            }
        }
    }

    /// Processes a block: `out = mask(in)` delayed by `latency()`.
    pub fn process(&mut self, in_l: &[f32], in_r: &[f32], out_l: &mut [f32], out_r: &mut [f32]) {
        out_l.copy_from_slice(in_l);
        out_r.copy_from_slice(in_r);
        self.process_inplace(out_l, out_r);
    }

    /// Feeds input without producing output (priming after a seek).
    pub fn prime(&mut self, l: &[f32], r: &[f32]) {
        assert_eq!(l.len(), r.len());
        let (hop, n_fft) = (self.hop, self.n_fft);
        let mut i = 0;
        while i < l.len() {
            let m = (hop - self.pos).min(l.len() - i);
            let p = self.pos;
            self.hist[0][n_fft + p..n_fft + p + m].copy_from_slice(&l[i..i + m]);
            self.hist[1][n_fft + p..n_fft + p + m].copy_from_slice(&r[i..i + m]);
            self.pos += m;
            i += m;
            if self.pos == hop {
                self.frame();
                self.pos = 0;
            }
        }
    }

    /// Seeds the overlap-add state with what identity frames would have
    /// produced up to now (before the next frame). `ready` gets the exact
    /// delayed input, so this also works mid-hop.
    fn warm_up(&mut self) {
        let (hop, n_fft) = (self.hop, self.n_fft);
        for c in 0..2 {
            let (hist, acc) = (&self.hist[c], &mut self.acc[c]);
            self.ready[c].copy_from_slice(&hist[..hop]);
            for i in 0..n_fft - hop {
                acc[i] = hist[hop + i] * self.partial[i];
            }
            acc[n_fft - hop..].fill(0.0);
        }
    }

    fn frame(&mut self) {
        let (hop, n_fft) = (self.hop, self.n_fft);
        let want = self.target.enabled;
        if !self.active {
            // Track the target so a later enable starts from it.
            self.cur = Cur::target(&self.target);
            self.cur.amount = 0.0;
        } else {
            let t = Cur::target(&self.target);
            let a = self.alpha;
            if want {
                approach(&mut self.cur.center, t.center, a);
                approach(&mut self.cur.width, t.width, a);
                approach(&mut self.cur.softness, t.softness, a);
                approach(&mut self.cur.remove, t.remove, a);
            }
            approach(&mut self.cur.amount, t.amount, a);
        }
        if !self.active && want {
            self.warm_up();
            self.active = true;
            self.idle_frames = 0;
            approach(&mut self.cur.amount, 1.0, self.alpha);
        }
        if self.active || self.analysis {
            for c in 0..2 {
                let frame = &self.hist[c][hop..hop + n_fft];
                for ((b, &x), &w) in self.tbuf.iter_mut().zip(frame).zip(&self.window) {
                    *b = x * w;
                }
                self.fwd
                    .process_with_scratch(&mut self.tbuf, &mut self.spec[c], &mut self.fscratch)
                    .expect("rfft size mismatch");
            }
            if self.analysis {
                self.accumulate_field();
            }
        }
        if self.active {
            let cur = self.cur;
            let (sl, sr) = self.spec.split_at_mut(1);
            for (a, b) in sl[0].iter_mut().zip(sr[0].iter_mut()) {
                let pan = pan_of(a.norm(), b.norm());
                let sel = window_gain(pan, cur.center, cur.width, cur.softness);
                let g = sel + cur.remove * (1.0 - 2.0 * sel);
                let g = 1.0 - cur.amount * (1.0 - g);
                *a *= g;
                *b *= g;
            }
            for c in 0..2 {
                let spec = &mut self.spec[c];
                spec[0].im = 0.0;
                let last = spec.len() - 1;
                spec[last].im = 0.0;
                self.inv
                    .process_with_scratch(spec, &mut self.tbuf, &mut self.iscratch)
                    .expect("irfft size mismatch");
                let acc = &mut self.acc[c];
                let s = self.ola_scale;
                for ((o, &y), &w) in acc.iter_mut().zip(&self.tbuf).zip(&self.window) {
                    *o += y * w * s;
                }
                self.ready[c].copy_from_slice(&acc[..hop]);
                acc.copy_within(hop.., 0);
                acc[n_fft - hop..].fill(0.0);
            }
            if !want && self.cur.amount == 0.0 {
                self.idle_frames += 1;
                // Every frame still overlapping the output is identity now.
                if self.idle_frames >= n_fft / hop {
                    self.active = false;
                    self.idle_frames = 0;
                }
            } else {
                self.idle_frames = 0;
            }
        }
        for c in 0..2 {
            self.hist[c].copy_within(hop.., 0);
        }
    }

    fn accumulate_field(&mut self) {
        let k = self.field_scale;
        for (f, (a, b)) in self.spec[0].iter().zip(&self.spec[1]).enumerate() {
            let band = self.band_of_bin[f];
            if band == u8::MAX {
                continue;
            }
            let (pl, pr) = (a.norm_sqr(), b.norm_sqr());
            let e = pl + pr;
            if e <= 0.0 {
                continue;
            }
            let pan = pan_of(pl.sqrt(), pr.sqrt());
            let bin = (((pan + 1.0) * 0.5 * (FIELD_BINS - 1) as f32).round() as usize)
                .min(FIELD_BINS - 1);
            self.field[band as usize * FIELD_BINS + bin] += e * k;
        }
        self.field_hops += 1;
    }
}

/// Whole-buffer pan mask, aligned (latency removed). Same code path as
/// streaming, so it equals a streaming run shifted by [`PAN_LATENCY`].
pub fn pan_offline(p: &PanParams, sample_rate: u32, l: &[f32], r: &[f32]) -> [Vec<f32>; 2] {
    assert_eq!(l.len(), r.len());
    let mut m = PanMask::new(sample_rate);
    m.set_params_now(p);
    let lat = m.latency();
    let mut ol = vec![0.0; l.len() + lat];
    let mut or = vec![0.0; r.len() + lat];
    ol[..l.len()].copy_from_slice(l);
    or[..r.len()].copy_from_slice(r);
    m.process_inplace(&mut ol, &mut or);
    ol.drain(..lat);
    or.drain(..lat);
    [ol, or]
}
