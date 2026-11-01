//! Per-stem EQ for the Refine panel (SPEC §5.4 P5): low cut, high cut and
//! three peaking bands, RBJ cookbook biquads (Robert Bristow-Johnson,
//! "Cookbook formulae for audio EQ biquad filter coefficients").
//!
//! - Low cut / high cut: 4th-order Butterworth (24 dB/octave), two cascaded
//!   biquads with Q = 0.5412 and 1.3066; -3 dB at the set frequency. Steep
//!   enough to take bass bleed out of a guitar stem without a long skirt.
//! - Bands: RBJ peaking EQ (gain in dB, Q).
//!
//! Real time: coefficients and state are fixed-size, [`Eq::process_inplace`]
//! never allocates. Parameters are smoothed (one-pole, [`EQ_TAU_SECS`]) and
//! coefficients recomputed every [`EQ_SUBBLOCK`] samples while moving
//! (frequency and Q in the log domain, gain in dB). Turning a cut on or off
//! crossfades its output with the dry signal (per sample); a band turned off
//! glides to 0 dB. Filters run in f64.
//!
//! An EQ with nothing enabled is skipped entirely (output == input,
//! bit-exact); see [`Eq::is_bypassed`].

use std::f64::consts::PI;

use serde::{Deserialize, Serialize};

/// Smoothing time constant of EQ parameter changes.
pub const EQ_TAU_SECS: f64 = 0.02;
/// Coefficient update interval while parameters move, in samples.
pub const EQ_SUBBLOCK: usize = 32;
/// Number of peaking bands.
pub const EQ_BANDS: usize = 3;
/// Butterworth section Qs of a 4th-order cut.
const BUTTER4_Q: [f64; 2] = [0.541_196_100_146_197, 1.306_562_964_876_376_5];

/// Low or high cut.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CutParams {
    pub enabled: bool,
    /// -3 dB frequency in Hz.
    pub freq: f32,
}

/// One peaking band.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BandParams {
    pub enabled: bool,
    /// Centre frequency in Hz.
    pub freq: f32,
    /// Gain at the centre in dB (about ±18 in the UI).
    pub gain_db: f32,
    /// Bandwidth (RBJ Q; 0.7 broad … 4 narrow).
    pub q: f32,
}

impl Default for CutParams {
    fn default() -> Self {
        Self {
            enabled: false,
            freq: 80.0,
        }
    }
}

impl Default for BandParams {
    fn default() -> Self {
        Self {
            enabled: false,
            freq: 1000.0,
            gain_db: 0.0,
            q: 1.0,
        }
    }
}

impl BandParams {
    pub const fn new(freq: f32, gain_db: f32, q: f32) -> Self {
        Self {
            enabled: true,
            freq,
            gain_db,
            q,
        }
    }

    fn effective_gain(&self) -> f32 {
        if self.enabled && self.gain_db.is_finite() {
            self.gain_db
        } else {
            0.0
        }
    }
}

/// EQ settings of one stem.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqParams {
    pub low_cut: CutParams,
    pub high_cut: CutParams,
    pub bands: [BandParams; EQ_BANDS],
}

impl Default for EqParams {
    /// Flat: everything off. Band frequencies default to 200 Hz / 1 kHz /
    /// 5 kHz so a band the user switches on starts somewhere sensible.
    fn default() -> Self {
        Self {
            low_cut: CutParams {
                enabled: false,
                freq: 80.0,
            },
            high_cut: CutParams {
                enabled: false,
                freq: 12_000.0,
            },
            bands: [
                BandParams {
                    freq: 200.0,
                    ..BandParams::default()
                },
                BandParams {
                    freq: 1000.0,
                    ..BandParams::default()
                },
                BandParams {
                    freq: 5000.0,
                    ..BandParams::default()
                },
            ],
        }
    }
}

impl EqParams {
    /// True when this EQ is flat (no cut on, no band with non-zero gain).
    pub fn is_identity(&self) -> bool {
        !self.low_cut.enabled
            && !self.high_cut.enabled
            && self.bands.iter().all(|b| b.effective_gain() == 0.0)
    }
}

/// Normalised biquad coefficients (`a0 = 1`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coefs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Coefs {
    const IDENTITY: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    fn norm(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    /// RBJ high-pass.
    pub fn highpass(fs: f64, f0: f64, q: f64) -> Self {
        let w0 = 2.0 * PI * clamp_freq(f0, fs) / fs;
        let (s, c) = w0.sin_cos();
        let al = s / (2.0 * q);
        Self::norm(
            (1.0 + c) / 2.0,
            -(1.0 + c),
            (1.0 + c) / 2.0,
            1.0 + al,
            -2.0 * c,
            1.0 - al,
        )
    }

    /// RBJ low-pass.
    pub fn lowpass(fs: f64, f0: f64, q: f64) -> Self {
        let w0 = 2.0 * PI * clamp_freq(f0, fs) / fs;
        let (s, c) = w0.sin_cos();
        let al = s / (2.0 * q);
        Self::norm(
            (1.0 - c) / 2.0,
            1.0 - c,
            (1.0 - c) / 2.0,
            1.0 + al,
            -2.0 * c,
            1.0 - al,
        )
    }

    /// RBJ peaking EQ.
    pub fn peaking(fs: f64, f0: f64, q: f64, gain_db: f64) -> Self {
        let w0 = 2.0 * PI * clamp_freq(f0, fs) / fs;
        let (s, c) = w0.sin_cos();
        let al = s / (2.0 * q);
        let a = 10f64.powf(gain_db / 40.0);
        Self::norm(
            1.0 + al * a,
            -2.0 * c,
            1.0 - al * a,
            1.0 + al / a,
            -2.0 * c,
            1.0 - al / a,
        )
    }

    /// |H(e^{jω})| at `f` Hz.
    pub fn magnitude(&self, fs: f64, f: f64) -> f64 {
        let w = 2.0 * PI * f / fs;
        let (c1, s1) = (w.cos(), -w.sin());
        let (c2, s2) = ((2.0 * w).cos(), -(2.0 * w).sin());
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = self.b1 * s1 + self.b2 * s2;
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = self.a1 * s1 + self.a2 * s2;
        ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt()
    }
}

fn clamp_freq(f: f64, fs: f64) -> f64 {
    if f.is_finite() {
        f.clamp(10.0, 0.45 * fs)
    } else {
        1000.0
    }
}

fn clamp_q(q: f64) -> f64 {
    if q.is_finite() {
        q.clamp(0.1, 18.0)
    } else {
        1.0
    }
}

/// Transposed direct form II state of one channel.
#[derive(Clone, Copy, Debug, Default)]
struct Tdf2 {
    s1: f64,
    s2: f64,
}

impl Tdf2 {
    #[inline]
    fn tick(&mut self, c: &Coefs, x: f64) -> f64 {
        let y = c.b0 * x + self.s1;
        self.s1 = c.b1 * x - c.a1 * y + self.s2;
        self.s2 = c.b2 * x - c.a2 * y;
        y
    }
}

/// Magnitude response of `p` in dB at each of `freqs` (for the UI curve).
/// Uses the target settings (not the smoothed ones).
pub fn response_db(p: &EqParams, sample_rate: f32, freqs: &[f32], out: &mut [f32]) {
    let fs = sample_rate as f64;
    let mut secs: Vec<Coefs> = Vec::with_capacity(7);
    if p.low_cut.enabled {
        for q in BUTTER4_Q {
            secs.push(Coefs::highpass(fs, p.low_cut.freq as f64, q));
        }
    }
    if p.high_cut.enabled {
        for q in BUTTER4_Q {
            secs.push(Coefs::lowpass(fs, p.high_cut.freq as f64, q));
        }
    }
    for b in &p.bands {
        let g = b.effective_gain();
        if g != 0.0 {
            secs.push(Coefs::peaking(
                fs,
                b.freq as f64,
                clamp_q(b.q as f64),
                g as f64,
            ));
        }
    }
    for (o, &f) in out.iter_mut().zip(freqs) {
        let mag: f64 = secs.iter().map(|c| c.magnitude(fs, f as f64)).product();
        *o = (20.0 * mag.max(1e-12).log10()) as f32;
    }
}

/// `n` log-spaced frequencies from `lo` to `hi` Hz inclusive.
pub fn log_freqs(n: usize, lo: f32, hi: f32) -> Vec<f32> {
    if n == 1 {
        return vec![lo];
    }
    let (a, b) = (lo.ln(), hi.ln());
    (0..n)
        .map(|i| (a + (b - a) * i as f32 / (n - 1) as f32).exp())
        .collect()
}

/// Smoothed state of a 4th-order cut.
#[derive(Clone, Copy, Debug)]
struct Cut {
    high: bool,
    log_f: f64,
    mix: f64,
    coefs: [Coefs; 2],
    state: [[Tdf2; 2]; 2],
    active: bool,
}

impl Cut {
    fn new(high: bool) -> Self {
        Self {
            high,
            log_f: 1000f64.ln(),
            mix: 0.0,
            coefs: [Coefs::IDENTITY; 2],
            state: Default::default(),
            active: false,
        }
    }

    fn design(&mut self, fs: f64) {
        let f = self.log_f.exp();
        for (c, q) in self.coefs.iter_mut().zip(BUTTER4_Q) {
            *c = if self.high {
                Coefs::lowpass(fs, f, q)
            } else {
                Coefs::highpass(fs, f, q)
            };
        }
    }
}

/// Smoothed state of a peaking band.
#[derive(Clone, Copy, Debug)]
struct Band {
    log_f: f64,
    gain: f64,
    log_q: f64,
    coefs: Coefs,
    state: [Tdf2; 2],
    active: bool,
}

const SNAP_LOG: f64 = 1e-4;
const SNAP_DB: f64 = 1e-3;
const SNAP_MIX: f64 = 1e-4;

fn glide(cur: &mut f64, target: f64, alpha: f64, snap: f64) -> bool {
    if *cur == target {
        return false;
    }
    *cur += (target - *cur) * alpha;
    if (target - *cur).abs() < snap {
        *cur = target;
    }
    true
}

/// Stereo EQ of one stem. See the module docs.
pub struct Eq {
    fs: f64,
    alpha: f64,
    target: EqParams,
    lo: Cut,
    hi: Cut,
    bands: [Band; EQ_BANDS],
}

impl Eq {
    pub fn new(sample_rate: u32) -> Self {
        let fs = sample_rate.max(1) as f64;
        let band = Band {
            log_f: 1000f64.ln(),
            gain: 0.0,
            log_q: 0.0,
            coefs: Coefs::IDENTITY,
            state: Default::default(),
            active: false,
        };
        let mut eq = Self {
            fs,
            alpha: 1.0 - (-(EQ_SUBBLOCK as f64) / (EQ_TAU_SECS * fs)).exp(),
            target: EqParams::default(),
            lo: Cut::new(false),
            hi: Cut::new(true),
            bands: [band; EQ_BANDS],
        };
        eq.set_params_now(&EqParams::default());
        eq
    }

    /// Sets the target; the filters glide there.
    pub fn set_params(&mut self, p: &EqParams) {
        self.target = *p;
        // A cut being switched on starts from a clean state at the edge of
        // the band (10 Hz / 0.45·fs, where it is nearly transparent and its
        // start-up transient is slow) and glides to its frequency while its
        // output fades in.
        for (cut, t) in [(&mut self.lo, p.low_cut), (&mut self.hi, p.high_cut)] {
            if t.enabled && !cut.active {
                let edge = if cut.high { 0.45 * self.fs } else { 10.0 };
                cut.log_f = clamp_freq(edge, self.fs).ln();
                cut.state = Default::default();
                cut.design(self.fs);
                cut.mix = 0.0;
                cut.active = true;
            }
        }
        for (b, t) in self.bands.iter_mut().zip(&p.bands) {
            if t.effective_gain() != 0.0 && !b.active {
                b.log_f = clamp_freq(t.freq as f64, self.fs).ln();
                b.log_q = clamp_q(t.q as f64).ln();
                b.gain = 0.0;
                b.state = Default::default();
                b.active = true;
            }
        }
    }

    /// Sets the parameters without smoothing (fresh start / offline).
    pub fn set_params_now(&mut self, p: &EqParams) {
        self.target = *p;
        let fs = self.fs;
        for (cut, t) in [(&mut self.lo, p.low_cut), (&mut self.hi, p.high_cut)] {
            cut.log_f = clamp_freq(t.freq as f64, fs).ln();
            cut.mix = if t.enabled { 1.0 } else { 0.0 };
            cut.design(fs);
            if t.enabled != cut.active {
                cut.state = Default::default();
            }
            cut.active = t.enabled;
        }
        for (b, t) in self.bands.iter_mut().zip(&p.bands) {
            b.log_f = clamp_freq(t.freq as f64, fs).ln();
            b.log_q = clamp_q(t.q as f64).ln();
            b.gain = t.effective_gain() as f64;
            b.coefs = Coefs::peaking(fs, b.log_f.exp(), b.log_q.exp(), b.gain);
            let on = b.gain != 0.0;
            if on != b.active {
                b.state = Default::default();
            }
            b.active = on;
        }
    }

    pub fn params(&self) -> &EqParams {
        &self.target
    }

    /// True when no section is running (output == input, bit-exact).
    pub fn is_bypassed(&self) -> bool {
        !self.lo.active && !self.hi.active && self.bands.iter().all(|b| !b.active)
    }

    /// Clears the filter state and jumps to the target.
    pub fn reset(&mut self) {
        for c in [&mut self.lo, &mut self.hi] {
            c.state = Default::default();
        }
        for b in &mut self.bands {
            b.state = Default::default();
        }
        let t = self.target;
        self.set_params_now(&t);
    }

    /// Magnitude response of the current target, see [`response_db`].
    pub fn response_db(&self, freqs: &[f32], out: &mut [f32]) {
        response_db(&self.target, self.fs as f32, freqs, out);
    }

    /// Advances the smoothing by one sub-block; returns the cut mixes at the
    /// start of the sub-block (for per-sample interpolation).
    fn step(&mut self) -> [f64; 2] {
        let (fs, a) = (self.fs, self.alpha);
        let t = self.target;
        let mut prev = [0.0; 2];
        for (k, (cut, tp)) in [(&mut self.lo, t.low_cut), (&mut self.hi, t.high_cut)]
            .into_iter()
            .enumerate()
        {
            prev[k] = cut.mix;
            if !cut.active {
                continue;
            }
            if tp.enabled
                && glide(
                    &mut cut.log_f,
                    clamp_freq(tp.freq as f64, fs).ln(),
                    a,
                    SNAP_LOG,
                )
            {
                cut.design(fs);
            }
            glide(
                &mut cut.mix,
                if tp.enabled { 1.0 } else { 0.0 },
                a,
                SNAP_MIX,
            );
        }
        for (b, tp) in self.bands.iter_mut().zip(&t.bands) {
            if !b.active {
                continue;
            }
            let g = tp.effective_gain() as f64;
            let mut moved = glide(&mut b.gain, g, a, SNAP_DB);
            if g != 0.0 {
                moved |= glide(
                    &mut b.log_f,
                    clamp_freq(tp.freq as f64, fs).ln(),
                    a,
                    SNAP_LOG,
                );
                moved |= glide(&mut b.log_q, clamp_q(tp.q as f64).ln(), a, SNAP_LOG);
            }
            if moved {
                b.coefs = Coefs::peaking(fs, b.log_f.exp(), b.log_q.exp(), b.gain);
            }
        }
        prev
    }

    /// Processes a stereo block in place.
    pub fn process_inplace(&mut self, l: &mut [f32], r: &mut [f32]) {
        assert_eq!(l.len(), r.len());
        if self.is_bypassed() {
            return;
        }
        let mut i = 0;
        while i < l.len() {
            let n = EQ_SUBBLOCK.min(l.len() - i);
            let prev = self.step();
            let (bl, br) = (&mut l[i..i + n], &mut r[i..i + n]);
            for (k, cut) in [&mut self.lo, &mut self.hi].into_iter().enumerate() {
                if !cut.active {
                    continue;
                }
                let (m0, m1) = (prev[k], cut.mix);
                for (ch, buf) in [&mut *bl, &mut *br].into_iter().enumerate() {
                    for (j, x) in buf.iter_mut().enumerate() {
                        let dry = *x as f64;
                        let mut y = dry;
                        for (s, c) in cut.state.iter_mut().zip(&cut.coefs) {
                            y = s[ch].tick(c, y);
                        }
                        let m = m0 + (m1 - m0) * (j + 1) as f64 / n as f64;
                        *x = (dry + m * (y - dry)) as f32;
                    }
                }
                if m1 == 0.0 {
                    cut.active = false;
                    cut.state = Default::default();
                }
            }
            for b in &mut self.bands {
                if !b.active {
                    continue;
                }
                for (ch, buf) in [&mut *bl, &mut *br].into_iter().enumerate() {
                    let st = &mut b.state[ch];
                    for x in buf.iter_mut() {
                        *x = st.tick(&b.coefs, *x as f64) as f32;
                    }
                }
                if b.gain == 0.0 {
                    b.active = false;
                    b.state = Default::default();
                }
            }
            i += n;
        }
    }

    /// Processes a stereo block.
    pub fn process(&mut self, in_l: &[f32], in_r: &[f32], out_l: &mut [f32], out_r: &mut [f32]) {
        out_l.copy_from_slice(in_l);
        out_r.copy_from_slice(in_r);
        self.process_inplace(out_l, out_r);
    }
}
