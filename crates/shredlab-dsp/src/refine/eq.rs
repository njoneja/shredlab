// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Per-stem EQ for the Refine panel (SPEC §5.4 P5): low cut, high cut and
//! three peaking bands, RBJ cookbook biquads (Robert Bristow-Johnson,
//! "Cookbook formulae for audio EQ biquad filter coefficients").
//!
//! - Low cut / high cut: 4th-order Butterworth (24 dB/octave), two cascaded
//!   biquads with Q = 0.5412 and 1.3066; -3 dB at the set frequency. Steep
//!   enough to take bass bleed out of a guitar stem without a long skirt.
//! - Either end can instead be a shelf ([`EndMode::Shelf`]): RBJ low or
//!   high shelf with slope S = 1, gain ±12 dB, half-gain point at the set
//!   frequency. A shelf is a separate section, so switching Cut <-> Shelf
//!   fades the cut out while the shelf gain glides in from 0 dB (no click).
//! - Bands: RBJ peaking EQ (gain in dB, Q).
//!
//! Real time: coefficients and state are fixed-size, [`Eq::process_inplace`]
//! never allocates. Parameters are smoothed (one-pole, [`EQ_TAU_SECS`]) and
//! coefficients recomputed every [`EQ_SUBBLOCK`] samples while moving
//! (frequency and Q in the log domain, gain in dB). Turning a cut on or off
//! crossfades its output with the dry signal (per sample); a band turned off
//! glides to 0 dB. Filters run in f64 as trapezoidal state-variable filters (same responses as
//! the RBJ biquads) with per-sample coefficient interpolation, so glides
//! have no zipper steps.
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

/// Largest shelf gain, ± dB.
pub const SHELF_MAX_DB: f32 = 12.0;

/// What an end of the EQ does: a steep cut or a shelf.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EndMode {
    /// 24 dB/oct Butterworth high-pass (low end) or low-pass (high end).
    #[default]
    Cut,
    /// RBJ shelf (S = 1) with `gain_db`.
    Shelf,
}

/// Low or high end of the EQ: a cut, or a shelf (see [`EndMode`]).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CutParams {
    pub enabled: bool,
    /// Cut: the -3 dB frequency. Shelf: the half-gain frequency. Hz.
    pub freq: f32,
    pub mode: EndMode,
    /// Shelf gain in dB (±[`SHELF_MAX_DB`]); ignored in Cut mode.
    pub gain_db: f32,
}

impl CutParams {
    /// An enabled cut at `freq` (const, for presets).
    pub const fn cut(freq: f32) -> Self {
        Self {
            enabled: true,
            freq,
            mode: EndMode::Cut,
            gain_db: 0.0,
        }
    }

    /// A disabled end remembering `freq` (const, for presets).
    pub const fn off(freq: f32) -> Self {
        Self {
            enabled: false,
            freq,
            mode: EndMode::Cut,
            gain_db: 0.0,
        }
    }

    /// The steep cut is running.
    pub fn cut_on(&self) -> bool {
        self.enabled && self.mode == EndMode::Cut
    }

    /// Effective shelf gain (0 unless enabled in Shelf mode).
    pub fn shelf_gain(&self) -> f32 {
        if self.enabled && self.mode == EndMode::Shelf && self.gain_db.is_finite() {
            self.gain_db.clamp(-SHELF_MAX_DB, SHELF_MAX_DB)
        } else {
            0.0
        }
    }
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
            mode: EndMode::Cut,
            gain_db: 0.0,
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
                freq: 80.0,
                ..CutParams::default()
            },
            high_cut: CutParams {
                freq: 12_000.0,
                ..CutParams::default()
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
    /// True when this EQ is flat (no cut on, no shelf or band with non-zero
    /// gain).
    pub fn is_identity(&self) -> bool {
        !self.low_cut.cut_on()
            && !self.high_cut.cut_on()
            && self.low_cut.shelf_gain() == 0.0
            && self.high_cut.shelf_gain() == 0.0
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

    /// RBJ low shelf (`high == false`) or high shelf, slope S = 1.
    pub fn shelf(fs: f64, f0: f64, gain_db: f64, high: bool) -> Self {
        let w0 = 2.0 * PI * clamp_freq(f0, fs) / fs;
        let (s, c) = w0.sin_cos();
        let a = 10f64.powf(gain_db / 40.0);
        // alpha = sin/2 * sqrt((A + 1/A)(1/S - 1) + 2) with S = 1.
        let al = s / 2.0 * 2f64.sqrt();
        let k = 2.0 * a.sqrt() * al;
        if high {
            Self::norm(
                a * ((a + 1.0) + (a - 1.0) * c + k),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
                a * ((a + 1.0) + (a - 1.0) * c - k),
                (a + 1.0) - (a - 1.0) * c + k,
                2.0 * ((a - 1.0) - (a + 1.0) * c),
                (a + 1.0) - (a - 1.0) * c - k,
            )
        } else {
            Self::norm(
                a * ((a + 1.0) - (a - 1.0) * c + k),
                2.0 * a * ((a - 1.0) - (a + 1.0) * c),
                a * ((a + 1.0) - (a - 1.0) * c - k),
                (a + 1.0) + (a - 1.0) * c + k,
                -2.0 * ((a - 1.0) + (a + 1.0) * c),
                (a + 1.0) + (a - 1.0) * c - k,
            )
        }
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

/// Magnitude response of `p` in dB at each of `freqs` (for the UI curve).
/// Uses the target settings (not the smoothed ones).
pub fn response_db(p: &EqParams, sample_rate: f32, freqs: &[f32], out: &mut [f32]) {
    let fs = sample_rate as f64;
    let mut secs: Vec<Coefs> = Vec::with_capacity(9);
    if p.low_cut.cut_on() {
        for q in BUTTER4_Q {
            secs.push(Coefs::highpass(fs, p.low_cut.freq as f64, q));
        }
    }
    if p.high_cut.cut_on() {
        for q in BUTTER4_Q {
            secs.push(Coefs::lowpass(fs, p.high_cut.freq as f64, q));
        }
    }
    for (end, high) in [(&p.low_cut, false), (&p.high_cut, true)] {
        let g = end.shelf_gain();
        if g != 0.0 {
            secs.push(Coefs::shelf(fs, end.freq as f64, g as f64, high));
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
    /// Section coefficients at the start of the sub-block (see [`Band`]).
    from: [Svf; 2],
    coefs: [Svf; 2],
    state: [[SvfState; 2]; 2],
    active: bool,
}

impl Cut {
    fn new(high: bool) -> Self {
        Self {
            high,
            log_f: 1000f64.ln(),
            mix: 0.0,
            from: [Svf::IDENTITY; 2],
            coefs: [Svf::IDENTITY; 2],
            state: Default::default(),
            active: false,
        }
    }

    /// Butterworth sections as SVF high-/low-passes (same response as
    /// [`Coefs::highpass`] / [`Coefs::lowpass`]).
    fn design(&mut self, fs: f64) {
        let g = (PI * clamp_freq(self.log_f.exp(), fs) / fs).tan();
        for (c, q) in self.coefs.iter_mut().zip(BUTTER4_Q) {
            let k = 1.0 / q;
            *c = if self.high {
                Svf::new(g, k, 0.0, 0.0, 1.0)
            } else {
                Svf::new(g, k, 1.0, -k, -1.0)
            };
        }
    }

    fn design_now(&mut self, fs: f64) {
        self.design(fs);
        self.from = self.coefs;
    }
}

/// What a [`Band`] section computes.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Peak,
    LowShelf,
    HighShelf,
}

/// Smoothed state of a peaking band or a shelf.
#[derive(Clone, Copy, Debug)]
struct Band {
    kind: Kind,
    log_f: f64,
    gain: f64,
    log_q: f64,
    /// Coefficients at the start of the current sub-block (interpolated
    /// per sample towards `coefs`, so a glide has no zipper steps).
    from: Svf,
    coefs: Svf,
    state: [SvfState; 2],
    active: bool,
}

/// Trapezoidal state-variable filter (A. Simper, Cytomic, "Solving the
/// continuous SVF equations using trapezoidal integration"). Same analog
/// prototypes and bilinear transform as the RBJ bell and shelves, so the
/// response equals [`Coefs::peaking`] / [`Coefs::shelf`]; unlike direct
/// forms it stays clean while the coefficients glide (no bumps when a low
/// band or shelf moves or turns off).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Svf {
    a1: f64,
    a2: f64,
    a3: f64,
    m0: f64,
    m1: f64,
    m2: f64,
}

#[derive(Clone, Copy, Debug, Default)]
struct SvfState {
    ic1: f64,
    ic2: f64,
}

impl Svf {
    const IDENTITY: Self = Self {
        a1: 1.0,
        a2: 0.0,
        a3: 0.0,
        m0: 1.0,
        m1: 0.0,
        m2: 0.0,
    };

    fn new(g: f64, k: f64, m0: f64, m1: f64, m2: f64) -> Self {
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        Self {
            a1,
            a2,
            a3: g * a2,
            m0,
            m1,
            m2,
        }
    }

    fn design(kind: Kind, fs: f64, f0: f64, q: f64, gain_db: f64) -> Self {
        let a = 10f64.powf(gain_db / 40.0);
        let g = (PI * clamp_freq(f0, fs) / fs).tan();
        match kind {
            Kind::Peak => {
                let k = 1.0 / (q * a);
                Self::new(g, k, 1.0, k * (a * a - 1.0), 0.0)
            }
            Kind::LowShelf => {
                let k = std::f64::consts::SQRT_2;
                Self::new(g / a.sqrt(), k, 1.0, k * (a - 1.0), a * a - 1.0)
            }
            Kind::HighShelf => {
                let k = std::f64::consts::SQRT_2;
                Self::new(g * a.sqrt(), k, a * a, k * (1.0 - a) * a, 1.0 - a * a)
            }
        }
    }
}

impl Svf {
    #[inline]
    fn lerp(&self, o: &Self, t: f64) -> Self {
        let m = |a: f64, b: f64| a + (b - a) * t;
        Self {
            a1: m(self.a1, o.a1),
            a2: m(self.a2, o.a2),
            a3: m(self.a3, o.a3),
            m0: m(self.m0, o.m0),
            m1: m(self.m1, o.m1),
            m2: m(self.m2, o.m2),
        }
    }
}

impl SvfState {
    #[inline]
    fn tick(&mut self, c: &Svf, v0: f64) -> f64 {
        let v3 = v0 - self.ic2;
        let v1 = c.a1 * self.ic1 + c.a2 * v3;
        let v2 = self.ic2 + c.a2 * self.ic1 + c.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        c.m0 * v0 + c.m1 * v1 + c.m2 * v2
    }
}

impl Band {
    fn design(&mut self, fs: f64) {
        self.coefs = Svf::design(self.kind, fs, self.log_f.exp(), self.log_q.exp(), self.gain);
    }

    /// Designs and jumps there (no interpolation).
    fn design_now(&mut self, fs: f64) {
        self.design(fs);
        self.from = self.coefs;
    }

    /// Runs the section over a stereo block; deactivates it once at 0 dB.
    fn run(&mut self, bl: &mut [f32], br: &mut [f32]) {
        if !self.active {
            return;
        }
        let (c0, c1) = (self.from, self.coefs);
        let n = bl.len();
        for (ch, buf) in [bl, br].into_iter().enumerate() {
            let st = &mut self.state[ch];
            if c0 == c1 {
                for x in buf.iter_mut() {
                    *x = st.tick(&c1, *x as f64) as f32;
                }
            } else {
                for (j, x) in buf.iter_mut().enumerate() {
                    let c = c0.lerp(&c1, (j + 1) as f64 / n as f64);
                    *x = st.tick(&c, *x as f64) as f32;
                }
            }
        }
        self.from = c1;
        // At 0 dB the SVF output is exactly its input (m1 = m2 = 0), so the
        // state can go without a step.
        if self.gain == 0.0 {
            self.active = false;
            self.state = Default::default();
        }
    }
}

/// (gain, frequency, Q) a section glides to: bands use their own settings,
/// shelves the end's frequency and shelf gain. `i` is the band index.
fn band_target(kind: Kind, t: &EqParams, i: usize) -> (f32, f32, f32) {
    match kind {
        Kind::Peak => (t.bands[i].effective_gain(), t.bands[i].freq, t.bands[i].q),
        Kind::LowShelf => (t.low_cut.shelf_gain(), t.low_cut.freq, 1.0),
        Kind::HighShelf => (t.high_cut.shelf_gain(), t.high_cut.freq, 1.0),
    }
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
    /// Low shelf, high shelf.
    shelves: [Band; 2],
    bands: [Band; EQ_BANDS],
}

impl Eq {
    pub fn new(sample_rate: u32) -> Self {
        let fs = sample_rate.max(1) as f64;
        let band = Band {
            kind: Kind::Peak,
            log_f: 1000f64.ln(),
            gain: 0.0,
            log_q: 0.0,
            from: Svf::IDENTITY,
            coefs: Svf::IDENTITY,
            state: Default::default(),
            active: false,
        };
        let mut eq = Self {
            fs,
            alpha: 1.0 - (-(EQ_SUBBLOCK as f64) / (EQ_TAU_SECS * fs)).exp(),
            target: EqParams::default(),
            lo: Cut::new(false),
            hi: Cut::new(true),
            shelves: [
                Band {
                    kind: Kind::LowShelf,
                    ..band
                },
                Band {
                    kind: Kind::HighShelf,
                    ..band
                },
            ],
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
            if t.cut_on() && !cut.active {
                let edge = if cut.high { 0.45 * self.fs } else { 10.0 };
                cut.log_f = clamp_freq(edge, self.fs).ln();
                cut.state = Default::default();
                cut.design_now(self.fs);
                cut.mix = 0.0;
                cut.active = true;
            }
        }
        // A band or shelf switched on starts at 0 dB and glides to its gain.
        let fs = self.fs;
        for (i, b) in self.shelves.iter_mut().chain(&mut self.bands).enumerate() {
            let (g, f, q) = band_target(b.kind, p, i.saturating_sub(2));
            if g != 0.0 && !b.active {
                b.log_f = clamp_freq(f as f64, fs).ln();
                b.log_q = clamp_q(q as f64).ln();
                b.gain = 0.0;
                b.state = Default::default();
                b.design_now(fs);
                b.active = true;
            }
        }
    }

    /// Sets the parameters without smoothing (fresh start / offline).
    pub fn set_params_now(&mut self, p: &EqParams) {
        self.target = *p;
        let fs = self.fs;
        for (cut, t) in [(&mut self.lo, p.low_cut), (&mut self.hi, p.high_cut)] {
            let on = t.cut_on();
            cut.log_f = clamp_freq(t.freq as f64, fs).ln();
            cut.mix = if on { 1.0 } else { 0.0 };
            cut.design_now(fs);
            if on != cut.active {
                cut.state = Default::default();
            }
            cut.active = on;
        }
        for (i, b) in self.shelves.iter_mut().chain(&mut self.bands).enumerate() {
            let (g, f, q) = band_target(b.kind, p, i.saturating_sub(2));
            b.log_f = clamp_freq(f as f64, fs).ln();
            b.log_q = clamp_q(q as f64).ln();
            b.gain = g as f64;
            b.design_now(fs);
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
        !self.lo.active
            && !self.hi.active
            && self.shelves.iter().chain(&self.bands).all(|b| !b.active)
    }

    /// Clears the filter state and jumps to the target.
    pub fn reset(&mut self) {
        for c in [&mut self.lo, &mut self.hi] {
            c.state = Default::default();
        }
        for b in self.shelves.iter_mut().chain(&mut self.bands) {
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
            if tp.cut_on()
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
                if tp.cut_on() { 1.0 } else { 0.0 },
                a,
                SNAP_MIX,
            );
        }
        for (i, b) in self.shelves.iter_mut().chain(&mut self.bands).enumerate() {
            if !b.active {
                continue;
            }
            let (g, f, q) = band_target(b.kind, &t, i.saturating_sub(2));
            let g = g as f64;
            let mut moved = glide(&mut b.gain, g, a, SNAP_DB);
            if g != 0.0 {
                moved |= glide(&mut b.log_f, clamp_freq(f as f64, fs).ln(), a, SNAP_LOG);
                moved |= glide(&mut b.log_q, clamp_q(q as f64).ln(), a, SNAP_LOG);
            }
            if moved {
                b.design(fs);
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
                let moving = cut.from != cut.coefs;
                for (ch, buf) in [&mut *bl, &mut *br].into_iter().enumerate() {
                    for (j, x) in buf.iter_mut().enumerate() {
                        let dry = *x as f64;
                        let mut y = dry;
                        let t = (j + 1) as f64 / n as f64;
                        for ((s, c0), c1) in cut.state.iter_mut().zip(&cut.from).zip(&cut.coefs) {
                            y = if moving {
                                s[ch].tick(&c0.lerp(c1, t), y)
                            } else {
                                s[ch].tick(c1, y)
                            };
                        }
                        let m = m0 + (m1 - m0) * (j + 1) as f64 / n as f64;
                        *x = (dry + m * (y - dry)) as f32;
                    }
                }
                cut.from = cut.coefs;
                if m1 == 0.0 {
                    cut.active = false;
                    cut.state = Default::default();
                }
            }
            for b in self.shelves.iter_mut().chain(&mut self.bands) {
                b.run(bl, br);
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
