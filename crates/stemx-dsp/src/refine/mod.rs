//! Refine panel DSP (SPEC §5.4 P5): per-stem pan/width mask, then EQ.
//!
//! [`RefineChain`] is what the audio engine runs per stem. It has a constant
//! latency of [`PAN_LATENCY`] samples whether or not anything is enabled:
//! with an identity [`RefineParams`] its output is the input delayed by that
//! amount, bit-exact (no STFT round-trip, EQ skipped). The engine therefore
//! feeds each refined stem `latency()` samples ahead of the play position
//! (see `the unpublished Refine notes`), or delays every other path equally.

pub mod eq;
pub mod pan;
pub mod presets;

use serde::{Deserialize, Serialize};

pub use eq::{log_freqs, response_db, BandParams, CutParams, EndMode, Eq, EqParams, SHELF_MAX_DB};
pub use pan::{
    pan_offline, PanMask, PanParams, FIELD_BANDS, FIELD_BAND_EDGES_HZ, FIELD_BINS, FIELD_LEN,
    PAN_HOP, PAN_LATENCY, PAN_N_FFT,
};
pub use presets::{apply_preset, eq_preset, EqPreset, EQ_PRESETS};

/// Everything the Refine panel sets for one stem (saved per song).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RefineParams {
    pub pan: PanParams,
    pub eq: EqParams,
}

impl RefineParams {
    /// True when this passes the stem unchanged (the panel's Reset state).
    pub fn is_identity(&self) -> bool {
        self.pan.is_identity() && self.eq.is_identity()
    }
}

/// Pan mask then EQ for one stereo stem. Allocation-free after `new`.
pub struct RefineChain {
    pan: PanMask,
    eq: Eq,
    params: RefineParams,
}

impl RefineChain {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            pan: PanMask::new(sample_rate),
            eq: Eq::new(sample_rate),
            params: RefineParams::default(),
        }
    }

    /// Constant latency in samples ([`PAN_LATENCY`]).
    pub fn latency(&self) -> usize {
        self.pan.latency()
    }

    /// Sets the target parameters (smoothed, no clicks). Real-time safe.
    pub fn set_params(&mut self, p: &RefineParams) {
        self.params = *p;
        self.pan.set_params(&p.pan);
        self.eq.set_params(&p.eq);
    }

    /// Sets the parameters without smoothing (fresh start, after `reset`,
    /// offline render).
    pub fn set_params_now(&mut self, p: &RefineParams) {
        self.params = *p;
        self.pan.set_params_now(&p.pan);
        self.eq.set_params_now(&p.eq);
    }

    pub fn params(&self) -> &RefineParams {
        &self.params
    }

    /// True when the output is currently the input delayed by `latency()`,
    /// bit-exact (identity params and all fades finished).
    pub fn is_bypassed(&self) -> bool {
        self.pan.is_bypassed() && self.eq.is_bypassed()
    }

    /// Clears signal state and jumps to the target params (seek). Follow with
    /// [`prime`](Self::prime) of the next `latency()` input samples.
    pub fn reset(&mut self) {
        self.pan.reset();
        self.eq.reset();
    }

    /// Feeds input without producing output (after `reset`). The EQ comes
    /// after the delay, so it needs no priming.
    pub fn prime(&mut self, l: &[f32], r: &[f32]) {
        self.pan.prime(l, r);
    }

    /// Processes a block in place (any length).
    pub fn process_inplace(&mut self, l: &mut [f32], r: &mut [f32]) {
        self.pan.process_inplace(l, r);
        self.eq.process_inplace(l, r);
    }

    /// Processes a block: `out` = refined `in`, `latency()` samples late.
    pub fn process(&mut self, in_l: &[f32], in_r: &[f32], out_l: &mut [f32], out_r: &mut [f32]) {
        out_l.copy_from_slice(in_l);
        out_r.copy_from_slice(in_r);
        self.process_inplace(out_l, out_r);
    }

    /// Turns the stereo-field analysis on or off (see [`PanMask::take_field`]).
    pub fn set_analysis(&mut self, on: bool) {
        self.pan.set_analysis(on);
    }

    /// Stereo-field energy since the last call; see [`PanMask::take_field`].
    pub fn take_field(&mut self, out: &mut [f32]) -> u32 {
        self.pan.take_field(out)
    }

    /// EQ magnitude response of the current target in dB.
    pub fn eq_response_db(&self, freqs: &[f32], out: &mut [f32]) {
        self.eq.response_db(freqs, out);
    }
}

/// Whole-buffer refine with the latency removed: equals a streaming run of
/// [`RefineChain`] (started with `set_params_now`) shifted by `latency()`.
pub fn refine_offline(p: &RefineParams, sample_rate: u32, l: &[f32], r: &[f32]) -> [Vec<f32>; 2] {
    assert_eq!(l.len(), r.len());
    let mut chain = RefineChain::new(sample_rate);
    chain.set_params_now(p);
    let lat = chain.latency();
    let mut ol = vec![0.0; l.len() + lat];
    let mut or = vec![0.0; r.len() + lat];
    ol[..l.len()].copy_from_slice(l);
    or[..r.len()].copy_from_slice(r);
    chain.process_inplace(&mut ol, &mut or);
    ol.drain(..lat);
    or.drain(..lat);
    [ol, or]
}
