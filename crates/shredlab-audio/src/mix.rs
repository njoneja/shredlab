// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Summing gained tracks into a stereo mix, with click-free gain ramps and a
//! crossfade when the track set is replaced.
//!
//! # Backing track from the original
//!
//! A track set may carry the original mix next to the stems (same length,
//! sample-aligned: both come from the same 44.1 kHz decode). The output is
//! then a blend controlled by a weight `w` in 0..=1:
//!
//! ```text
//! out = w * original + Σ_i (gain_i - w) * stem_i
//! ```
//!
//! - `w = 1` ([`MixMode::Original`]): `original + Σ (gain_i - 1) * stem_i`.
//!   All stems at unity gives exactly the original (bit for bit); an off
//!   stem (gain 0) is subtracted.
//! - `w = 0` ([`MixMode::Stems`]): the plain sum `Σ gain_i * stem_i`.
//!
//! A mode change ramps `w` linearly over [`MODE_XFADE`] samples, which is a
//! linear crossfade between the two formulas (no discontinuity). Gain ramps
//! run at the same time. [`choose_mode`] is the rule shared by playback and
//! export.
//!
//! # Refined stems (Refine panel)
//!
//! Every stem runs through a [`RefineChain`] (stereo selection, then EQ),
//! refined or not. With `s_i` the whole stem and `r_i` the part that is
//! heard (the chain's output: the stem, or the stem with a stereo window
//! removed or alone, then EQ'd):
//!
//! ```text
//! out = w * original + Σ_i (gain_i * r_i - w * s_i)
//! ```
//!
//! The whole stem always leaves the backing and only the heard part comes
//! back, so a part that is not heard is not in the mix at all. Which part
//! is heard is the caller's business ([`PanParams::invert`]: ShredLab makes
//! the stereo window an exception to the row's on switch). An unrefined
//! chain passes the stem through bit-exact (`r_i = s_i`), so this is
//! exactly the formula above: while a chain is bypassed the mixer adds
//! `(gain_i - w) * r_i` as before, otherwise it reads `s_i` straight from
//! the track (the chain's output is aligned with it, see below).
//!
//! [`PanParams::invert`]: shredlab_dsp::refine::PanParams::invert
//!
//! The chain has a constant latency of [`PAN_LATENCY`] samples. Instead of
//! delaying everything else, the mixer feeds each chain the stem `L`
//! samples ahead of the output position, so all paths stay aligned and
//! seeks respond as before. Whenever a mix call does not continue where the
//! previous one ended (seek, loop wrap, play start, stretcher pre-roll, a new
//! track set), the chains are reset and primed with the next `L` samples;
//! the chain is block-size independent, so playback and export match.

use shredlab_dsp::refine::{RefineChain, RefineParams, FIELD_LEN, PAN_LATENCY};

use crate::Track;

/// Samples over which a gain change ramps (~12 ms at 44.1 kHz).
pub const GAIN_RAMP: usize = 512;
/// Samples over which an old track set fades out after [`Mixer::load`] (~46 ms).
pub const LOAD_XFADE: usize = 2048;
/// Samples over which a [`MixMode`] change crossfades (30 ms at 44.1 kHz).
pub const MODE_XFADE: usize = 1323;
/// Refine processing sub-block (scratch size; no allocation while mixing).
const CHUNK: usize = 1024;
/// Look-ahead of the refine chains, in samples.
const LA: i64 = PAN_LATENCY as i64;
/// Audio run through the chains before a (re)start point, so filters and
/// the mask are settled there (~186 ms of audio, a fraction of a ms of CPU
/// per refined stem). A multiple of the mask hop, so a start at 0 matches
/// `refine_offline` exactly.
const WARM: i64 = 8192;

/// How the stems are combined when the original mix is available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MixMode {
    /// Plain sum of the gained stems.
    #[default]
    Stems,
    /// `original + Σ (gain_i - 1) * stem_i`. Without an original track this
    /// behaves like [`MixMode::Stems`].
    Original,
}

/// The backing-track rule, shared by playback and export: with any stem soloed, the
/// plain sum (of the soloed stems); otherwise the original-based mix when at
/// least half of the stems are on (`2 * n_on >= n_stems`), else the plain
/// sum. "On" ignores solo and volume (an on stem at volume 0 still counts).
pub fn choose_mode(n_stems: usize, n_on: usize, any_solo: bool) -> MixMode {
    if any_solo || n_stems == 0 || 2 * n_on < n_stems {
        MixMode::Stems
    } else {
        MixMode::Original
    }
}

/// Linear gain for every track, by name. A track whose name is not in `gains`
/// gets 0; if a name appears twice the last value wins. Negative or non-finite
/// gains become 0.
pub fn gains_for(tracks: &[Track], gains: &[(String, f32)]) -> Vec<f32> {
    tracks
        .iter()
        .map(|t| {
            gains
                .iter()
                .rev()
                .find(|(n, _)| *n == t.name)
                .map_or(0.0, |&(_, g)| if g.is_finite() { g.max(0.0) } else { 0.0 })
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct Gain {
    cur: f32,
    target: f32,
    step: f32,
}

impl Gain {
    fn fixed(g: f32) -> Self {
        Self {
            cur: g,
            target: g,
            step: 0.0,
        }
    }

    fn set(&mut self, g: f32) {
        self.set_over(g, GAIN_RAMP);
    }

    fn set_over(&mut self, g: f32, len: usize) {
        self.target = g;
        self.step = (g - self.cur) / len.max(1) as f32;
    }

    fn steady(&self) -> bool {
        self.cur == self.target
    }

    /// Advances the ramp by `n` samples.
    fn skip(&mut self, n: usize) {
        for _ in 0..n {
            if self.steady() {
                break;
            }
            self.next();
        }
    }

    #[inline]
    fn next(&mut self) -> f32 {
        if self.cur != self.target {
            let n = self.cur + self.step;
            self.cur =
                if (self.step > 0.0 && n >= self.target) || (self.step < 0.0 && n <= self.target) {
                    self.target
                } else {
                    n
                };
        }
        self.cur
    }
}

/// Refine parameters for `name` (identity when not listed).
fn refine_for(list: &[(String, RefineParams)], name: &str) -> RefineParams {
    list.iter()
        .rev()
        .find(|(n, _)| n == name)
        .map_or_else(RefineParams::default, |(_, p)| *p)
}

struct TrackSet {
    tracks: Vec<Track>,
    gains: Vec<Gain>,
    original: Option<Track>,
    /// Weight of the original (see the module docs); stays 0 without one.
    w: Gain,
    /// One refine chain per track.
    chains: Vec<RefineChain>,
    /// Output position the chains are aligned with (`None`: prime first).
    chain_pos: Option<i64>,
    /// Heard part (`r`, the chain output) and whole stem (`s`) of one
    /// sub-block.
    sel: [Vec<f32>; 2],
    dry: [Vec<f32>; 2],
}

impl TrackSet {
    fn new(
        tracks: Vec<Track>,
        original: Option<Track>,
        names: &[(String, f32)],
        mode: MixMode,
        refine: &[(String, RefineParams)],
        analysis: Option<&str>,
    ) -> Self {
        let gains = gains_for(&tracks, names)
            .into_iter()
            .map(Gain::fixed)
            .collect();
        let w = Gain::fixed(Self::weight(original.is_some(), mode));
        let chains = tracks
            .iter()
            .map(|t| {
                let mut c = RefineChain::new(t.sample_rate);
                c.set_params_now(&refine_for(refine, &t.name));
                c.set_analysis(analysis == Some(t.name.as_str()));
                c
            })
            .collect();
        Self {
            tracks,
            gains,
            original,
            w,
            chains,
            chain_pos: None,
            sel: [vec![0.0; CHUNK], vec![0.0; CHUNK]],
            dry: [vec![0.0; CHUNK], vec![0.0; CHUNK]],
        }
    }

    /// Resets every chain and runs it over the [`WARM`] samples before `pos`
    /// (output discarded), so the next processed sample (stem at `pos + L`)
    /// outputs position `pos` with the EQ filters and the mask's overlap-add
    /// already settled: no start-up transient after a seek. Playback and
    /// export prime the same way, so they still match exactly.
    fn prime(&mut self, pos: i64) {
        let [sl, sr] = &mut self.sel;
        let from = pos - WARM;
        for (t, c) in self.tracks.iter().zip(self.chains.iter_mut()) {
            c.reset();
            let mut off = 0;
            while off < LA {
                let m = (CHUNK as i64).min(LA - off) as usize;
                read_track(t, from + off, &mut sl[..m], &mut sr[..m]);
                c.prime(&sl[..m], &sr[..m]);
                off += m as i64;
            }
            let mut off = 0;
            while off < WARM {
                let m = (CHUNK as i64).min(WARM - off) as usize;
                read_track(t, from + LA + off, &mut sl[..m], &mut sr[..m]);
                c.process_inplace(&mut sl[..m], &mut sr[..m]);
                off += m as i64;
            }
        }
        self.chain_pos = Some(pos);
    }

    fn set_refine(&mut self, name: &str, p: &RefineParams) {
        for (t, c) in self.tracks.iter().zip(self.chains.iter_mut()) {
            if t.name == name {
                c.set_params(p);
            }
        }
    }

    fn set_analysis(&mut self, name: Option<&str>) {
        for (t, c) in self.tracks.iter().zip(self.chains.iter_mut()) {
            c.set_analysis(name == Some(t.name.as_str()));
        }
    }

    fn weight(has_original: bool, mode: MixMode) -> f32 {
        if has_original && mode == MixMode::Original {
            1.0
        } else {
            0.0
        }
    }

    fn set_mode(&mut self, mode: MixMode) {
        let w = Self::weight(self.original.is_some(), mode);
        if w != self.w.target {
            self.w.set_over(w, MODE_XFADE);
        }
    }

    /// Adds `fade(i) * mix` of `[pos, pos+n)` into the outputs. The refine
    /// chains always advance (they follow the read position, not the ramps).
    fn add(
        &mut self,
        pos: i64,
        l: &mut [f32],
        r: &mut [f32],
        advance: bool,
        fade: impl Fn(usize) -> f32,
    ) {
        let n = l.len();
        if self.chain_pos != Some(pos) {
            self.prime(pos);
        }
        let has_orig = self.original.is_some();
        if let Some(o) = &self.original {
            let mut w = self.w;
            if !(w.cur == 0.0 && w.steady()) {
                add_track(o, pos, l, r, |i| w.next() * fade(i));
            }
        }
        let [sl, sr] = &mut self.sel;
        let [dl, dr] = &mut self.dry;
        for ((t, g), c) in self
            .tracks
            .iter()
            .zip(self.gains.iter_mut())
            .zip(self.chains.iter_mut())
        {
            let mut gain = *g;
            let mut w = self.w;
            let mut off = 0;
            while off < n {
                let m = CHUNK.min(n - off);
                let (pl, pr) = (&mut sl[..m], &mut sr[..m]);
                read_track(t, pos + off as i64 + LA, pl, pr);
                c.process_inplace(pl, pr);
                // Bypassed chain: r == s bit-exact, so mix the output alone
                // (checked after processing: a fade that just ended left
                // only identity frames, a fade that just started is not
                // bypassed).
                let bypass = c.is_bypassed();
                let w_cur = if has_orig { w.cur } else { 0.0 };
                // Fast path: a steady zero contribution (this also keeps
                // "all stems at unity" bit-exact).
                let steady = gain.steady() && (!has_orig || w.steady());
                let zero = if bypass {
                    gain.cur - w_cur == 0.0
                } else {
                    gain.cur == 0.0 && w_cur == 0.0
                };
                if !(steady && zero) {
                    let (lo, ro) = (&mut l[off..off + m], &mut r[off..off + m]);
                    let need_dry = !bypass && has_orig;
                    if need_dry {
                        read_track(t, pos + off as i64, &mut dl[..m], &mut dr[..m]);
                    }
                    for i in 0..m {
                        let gi = gain.next();
                        let wi = if has_orig { w.next() } else { 0.0 };
                        let f = fade(off + i);
                        if bypass {
                            let k = (gi - wi) * f;
                            lo[i] += pl[i] * k;
                            ro[i] += pr[i] * k;
                        } else if need_dry {
                            let (a, b) = (gi * f, wi * f);
                            lo[i] += pl[i] * a - dl[i] * b;
                            ro[i] += pr[i] * a - dr[i] * b;
                        } else {
                            let a = gi * f;
                            lo[i] += pl[i] * a;
                            ro[i] += pr[i] * a;
                        }
                    }
                }
                off += m;
            }
            if advance {
                *g = gain;
            }
        }
        self.chain_pos = Some(pos + n as i64);
        if advance {
            self.w.skip(n);
        }
    }
}

/// Copies `[start, start + l.len())` of `t` into `l`/`r`; zeros outside the track.
fn read_track(t: &Track, start: i64, l: &mut [f32], r: &mut [f32]) {
    let len = t.len() as i64;
    let n = l.len() as i64;
    let a = start.clamp(0, len);
    let b = (start + n).clamp(0, len);
    l.fill(0.0);
    r.fill(0.0);
    if b > a {
        let (da, db) = ((a - start) as usize, (b - start) as usize);
        t.channels[0].read(a as usize, &mut l[da..db]);
        t.channels[1].read(a as usize, &mut r[da..db]);
    }
}

/// `l[i] += ch[pos + i] * coef(i)` for every `i`. `coef` is called for
/// every `i` in order (also outside the track), so ramps advance.
#[inline]
fn add_track(
    t: &Track,
    pos: i64,
    l: &mut [f32],
    r: &mut [f32],
    mut coef: impl FnMut(usize) -> f32,
) {
    let len = t.len() as i64;
    let in_range = |i: usize| {
        let p = pos + i as i64;
        (0..len).contains(&p).then_some(p as usize)
    };
    // The original is f32; other storage takes the per-sample path.
    if let (Some(cl), Some(cr)) = (t.channels[0].as_f32(), t.channels[1].as_f32()) {
        for i in 0..l.len() {
            let f = coef(i);
            if let Some(p) = in_range(i) {
                l[i] += cl[p] * f;
                r[i] += cr[p] * f;
            }
        }
    } else {
        let [cl, cr] = &t.channels;
        for i in 0..l.len() {
            let f = coef(i);
            if let Some(p) = in_range(i) {
                l[i] += cl.get(p) * f;
                r[i] += cr.get(p) * f;
            }
        }
    }
}

/// The current track set plus, briefly after a `load`, the previous one fading out.
pub struct Mixer {
    cur: TrackSet,
    prev: Option<(TrackSet, usize)>,
    names: Vec<(String, f32)>,
    mode: MixMode,
    /// Refine parameters by track name (missing = unrefined).
    refine: Vec<(String, RefineParams)>,
    /// Track whose stereo field is analyzed (the open Refine panel).
    analysis: Option<String>,
    len: usize,
    sample_rate: u32,
}

impl Mixer {
    pub fn new(tracks: Vec<Track>, gains: &[(String, f32)]) -> Self {
        let (len, sample_rate) = dims(&tracks, None);
        Self {
            cur: TrackSet::new(tracks, None, gains, MixMode::Stems, &[], None),
            prev: None,
            names: gains.to_vec(),
            mode: MixMode::Stems,
            refine: Vec::new(),
            analysis: None,
            len,
            sample_rate,
        }
    }

    /// Length in samples (shortest track).
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Replaces the tracks; the old set fades out over [`LOAD_XFADE`] samples
    /// while the new one fades in. With `crossfade == false` the switch is immediate.
    pub fn load(&mut self, tracks: Vec<Track>, crossfade: bool) {
        self.load_with_original(tracks, None, crossfade);
    }

    /// Like [`load`](Self::load), plus the original mix as the backing-track
    /// source for [`MixMode::Original`]. It must be sample-aligned with the
    /// stems; the length is that of the shortest track.
    pub fn load_with_original(
        &mut self,
        tracks: Vec<Track>,
        original: Option<Track>,
        crossfade: bool,
    ) {
        let (len, sr) = dims(&tracks, original.as_ref());
        let new = TrackSet::new(
            tracks,
            original,
            &self.names,
            self.mode,
            &self.refine,
            self.analysis.as_deref(),
        );
        let old = std::mem::replace(&mut self.cur, new);
        let old_audible = !old.tracks.is_empty() || old.original.is_some();
        self.prev = (crossfade && old_audible).then_some((old, 0));
        self.len = len;
        self.sample_rate = sr;
    }

    /// Sets target gains by track name (missing = 0); changes ramp over [`GAIN_RAMP`].
    pub fn set_gains(&mut self, gains: Vec<(String, f32)>) {
        for (g, v) in self
            .cur
            .gains
            .iter_mut()
            .zip(gains_for(&self.cur.tracks, &gains))
        {
            g.set(v);
        }
        if let Some((p, _)) = &mut self.prev {
            for (g, v) in p.gains.iter_mut().zip(gains_for(&p.tracks, &gains)) {
                g.set(v);
            }
        }
        self.names = gains;
    }

    /// Sets the mode; a change crossfades over [`MODE_XFADE`] samples.
    pub fn set_mode(&mut self, mode: MixMode) {
        self.mode = mode;
        self.cur.set_mode(mode);
        if let Some((p, _)) = &mut self.prev {
            p.set_mode(mode);
        }
    }

    pub fn mode(&self) -> MixMode {
        self.mode
    }

    /// Sets the refine parameters of track `name` (smoothed while playing;
    /// kept for track sets loaded later). Allocates only the first time a
    /// name is seen.
    pub fn set_refine(&mut self, name: &str, p: RefineParams) {
        match self.refine.iter_mut().find(|(n, _)| n == name) {
            Some(e) => e.1 = p,
            None => self.refine.push((name.to_string(), p)),
        }
        self.cur.set_refine(name, &p);
        if let Some((s, _)) = &mut self.prev {
            s.set_refine(name, &p);
        }
    }

    /// Replaces all refine parameters (missing names become unrefined).
    pub fn set_refine_all(&mut self, list: Vec<(String, RefineParams)>) {
        self.refine = list;
        let Self {
            cur, prev, refine, ..
        } = self;
        for set in std::iter::once(cur).chain(prev.as_mut().map(|(s, _)| s)) {
            for (t, c) in set.tracks.iter().zip(set.chains.iter_mut()) {
                c.set_params(&refine_for(refine, &t.name));
            }
        }
    }

    /// The refine parameters in effect, by track name.
    pub fn refine(&self) -> &[(String, RefineParams)] {
        &self.refine
    }

    /// Stereo-field analysis on track `name` only (`None` = off).
    pub fn set_analysis(&mut self, name: Option<&str>) {
        self.analysis = name.map(str::to_string);
        self.cur.set_analysis(name);
    }

    /// The analyzed track's stereo field since the last call (see
    /// [`RefineChain::take_field`]); returns the hop count (0 = no data).
    pub fn take_field(&mut self, out: &mut [f32; FIELD_LEN]) -> u32 {
        let Some(name) = self.analysis.as_deref() else {
            return 0;
        };
        match self.cur.tracks.iter().position(|t| t.name == name) {
            Some(i) => self.cur.chains[i].take_field(out),
            None => 0,
        }
    }

    /// True when the current track set carries the original mix.
    pub fn has_original(&self) -> bool {
        self.cur.original.is_some()
    }

    /// Writes the mix of `[pos, pos + l.len())` into `l`/`r` (overwrites).
    /// Positions outside the song are silence. `advance == false` renders
    /// without moving gain ramps or crossfades (for stretcher pre-roll).
    pub fn mix(&mut self, pos: i64, l: &mut [f32], r: &mut [f32], advance: bool) {
        debug_assert_eq!(l.len(), r.len());
        l.fill(0.0);
        r.fill(0.0);
        let n = l.len();
        match &mut self.prev {
            None => self.cur.add(pos, l, r, advance, |_| 1.0),
            Some((prev, done)) => {
                let d = *done;
                let fade_in = move |i: usize| ((d + i) as f32 / LOAD_XFADE as f32).min(1.0);
                self.cur.add(pos, l, r, advance, fade_in);
                prev.add(pos, l, r, advance, move |i| 1.0 - fade_in(i));
                if advance {
                    *done += n;
                    if *done >= LOAD_XFADE {
                        self.prev = None;
                    }
                }
            }
        }
    }
}

fn dims(tracks: &[Track], original: Option<&Track>) -> (usize, u32) {
    let len = tracks
        .iter()
        .chain(original)
        .map(Track::len)
        .min()
        .unwrap_or(0);
    let sr = tracks
        .first()
        .or(original)
        .map_or(44_100, |t| t.sample_rate);
    (len, sr)
}

#[cfg(test)]
#[allow(clippy::needless_range_loop)]
mod tests {
    use super::*;
    use crate::Channel;

    pub(crate) fn track(name: &str, v: f32, len: usize) -> Track {
        Track {
            name: name.into(),
            channels: [Channel::from(vec![v; len]), Channel::from(vec![-v; len])],
            sample_rate: 44_100,
        }
    }

    fn g(pairs: &[(&str, f32)]) -> Vec<(String, f32)> {
        pairs.iter().map(|(n, v)| (n.to_string(), *v)).collect()
    }

    #[test]
    fn gain_mapping_by_name_missing_is_zero() {
        let tracks = vec![
            track("vocals", 1.0, 4),
            track("bass", 1.0, 4),
            track("drums", 1.0, 4),
        ];
        let gains = gains_for(
            &tracks,
            &g(&[("bass", 0.5), ("vocals", 2.0), ("nope", 1.0)]),
        );
        assert_eq!(gains, vec![2.0, 0.5, 0.0]);
        // Duplicates: last wins. Negative / NaN: 0.
        let gains = gains_for(
            &tracks,
            &g(&[
                ("bass", 0.5),
                ("bass", 0.7),
                ("vocals", -1.0),
                ("drums", f32::NAN),
            ]),
        );
        assert_eq!(gains, vec![0.0, 0.7, 0.0]);
    }

    #[test]
    fn solo_style_mapping_mixes_only_audible_tracks() {
        // The caller maps solo/on switches to gains; e.g. "only bass" = bass 0.8, rest 0.
        let tracks = vec![
            track("vocals", 0.1, 2000),
            track("bass", 0.2, 2000),
            track("drums", 0.4, 2000),
        ];
        let mut m = Mixer::new(tracks, &g(&[("bass", 0.5)]));
        let (mut l, mut r) = (vec![0.0; 8], vec![0.0; 8]);
        m.mix(0, &mut l, &mut r, true);
        assert!(l.iter().all(|&x| (x - 0.1).abs() < 1e-7));
        assert!(r.iter().all(|&x| (x + 0.1).abs() < 1e-7));
        m.set_gains(g(&[("vocals", 1.0), ("drums", 1.0)]));
        let (mut l, mut r) = (vec![0.0; GAIN_RAMP + 8], vec![0.0; GAIN_RAMP + 8]);
        m.mix(0, &mut l, &mut r, true);
        // Ramp: monotonic and lands exactly on the target.
        assert!(l.windows(2).all(|w| w[1] >= w[0] - 1e-7));
        assert!((l.last().unwrap() - 0.5).abs() < 1e-6);
        let (mut l, mut r) = (vec![0.0; 4], vec![0.0; 4]);
        m.mix(2, &mut l, &mut r, true);
        assert!(l.iter().all(|&x| (x - 0.5).abs() < 1e-6), "{l:?}");
    }

    #[test]
    fn unity_single_track_is_bit_exact() {
        let data: Vec<f32> = (0..100).map(|i| (i as f32 * 0.37).sin() * 0.9).collect();
        let t = Track {
            name: "mix".into(),
            channels: [Channel::from(data.clone()), Channel::from(data.clone())],
            sample_rate: 44_100,
        };
        let mut m = Mixer::new(vec![t], &g(&[("mix", 1.0)]));
        let (mut l, mut r) = (vec![0.0; 100], vec![0.0; 100]);
        m.mix(0, &mut l, &mut r, true);
        assert_eq!(l, data);
        assert_eq!(r, data);
    }

    #[test]
    fn load_crossfades_between_track_sets() {
        let mut m = Mixer::new(
            vec![track("mix", 1.0, 10_000)],
            &g(&[("mix", 1.0), ("a", 1.0)]),
        );
        m.load(vec![track("a", 0.0, 10_000)], true);
        let (mut l, mut r) = (vec![0.0; LOAD_XFADE + 10], vec![0.0; LOAD_XFADE + 10]);
        m.mix(0, &mut l, &mut r, true);
        assert!((l[0] - 1.0).abs() < 1e-6);
        assert!((l[LOAD_XFADE / 2] - 0.5).abs() < 1e-3);
        assert_eq!(l[LOAD_XFADE + 5], 0.0);
        assert!(l.windows(2).all(|w| w[1] <= w[0] + 1e-7));
    }

    // ------------------------------------------ backing-track-from-original tests

    fn sig(name: &str, seed: f32, len: usize) -> Track {
        let a: Vec<f32> = (0..len)
            .map(|i| (i as f32 * 0.013 * seed).sin() * 0.3 + (i as f32 * 0.0007).cos() * 0.1)
            .collect();
        let b: Vec<f32> = a.iter().map(|x| -0.7 * x + 0.01 * seed).collect();
        Track {
            name: name.into(),
            channels: [Channel::from(a), Channel::from(b)],
            sample_rate: 44_100,
        }
    }

    /// Stems plus an "original" that is NOT their sum (a residual is added),
    /// like a real separation.
    fn p1_set(len: usize) -> (Vec<Track>, Track) {
        let stems: Vec<Track> = ["vocals", "guitar", "bass", "drums", "piano", "other"]
            .iter()
            .enumerate()
            .map(|(i, n)| sig(n, 1.0 + i as f32, len))
            .collect();
        let mut o = [vec![0.0f32; len], vec![0.0f32; len]];
        for (c, oc) in o.iter_mut().enumerate() {
            for (i, v) in oc.iter_mut().enumerate() {
                *v = stems
                    .iter()
                    .map(|t| t.channels[c].as_f32().unwrap()[i])
                    .sum::<f32>()
                    + 0.05 * (i as f32 * 0.31).sin();
            }
        }
        let [a, b] = o;
        let orig = Track {
            name: "original".into(),
            channels: [Channel::from(a), Channel::from(b)],
            sample_rate: 44_100,
        };
        (stems, orig)
    }

    fn all(v: f32) -> Vec<(String, f32)> {
        ["vocals", "guitar", "bass", "drums", "piano", "other"]
            .iter()
            .map(|n| (n.to_string(), v))
            .collect()
    }

    fn render(m: &mut Mixer, pos: i64, n: usize) -> [Vec<f32>; 2] {
        let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
        m.mix(pos, &mut l, &mut r, true);
        [l, r]
    }

    #[test]
    fn choose_mode_rule() {
        assert_eq!(choose_mode(6, 6, false), MixMode::Original);
        assert_eq!(choose_mode(6, 3, false), MixMode::Original);
        assert_eq!(choose_mode(6, 2, false), MixMode::Stems);
        assert_eq!(choose_mode(6, 6, true), MixMode::Stems);
        assert_eq!(choose_mode(4, 2, false), MixMode::Original);
        assert_eq!(choose_mode(4, 1, false), MixMode::Stems);
        assert_eq!(choose_mode(0, 0, false), MixMode::Stems);
    }

    #[test]
    fn p1_all_on_unity_is_original_bit_for_bit() {
        let n = 5000;
        let (stems, orig) = p1_set(n);
        let mut m = Mixer::new(vec![], &all(1.0));
        m.set_mode(MixMode::Original);
        m.load_with_original(stems, Some(orig.clone()), false);
        let out = render(&mut m, 0, n);
        for c in 0..2 {
            assert!(out[c]
                .iter()
                .zip(orig.channels[c].as_f32().unwrap().iter())
                .all(|(a, b)| a.to_bits() == b.to_bits()));
        }
    }

    #[test]
    fn quantized_stems_keep_the_original_exact_and_subtract_exactly() {
        let n = 4000;
        let (stems, orig) = p1_set(n);
        let q: Vec<Track> = stems
            .iter()
            .map(|t| {
                let [l, r] = t.channels.clone().map(|c| c.to_vec());
                Track::quantized(t.name.clone(), [&l, &r], t.sample_rate)
            })
            .collect();
        let guitar = q[1].clone();
        let mut m = Mixer::new(vec![], &all(1.0));
        m.set_mode(MixMode::Original);
        m.load_with_original(q.clone(), Some(orig.clone()), false);
        assert_eq!(
            render(&mut m, 0, n),
            orig.channels.clone().map(|c| c.to_vec())
        );
        let mut gains = all(1.0);
        gains[1].1 = 0.0;
        let mut m = Mixer::new(vec![], &gains);
        m.set_mode(MixMode::Original);
        m.load_with_original(q, Some(orig.clone()), false);
        let out = render(&mut m, 0, n);
        for c in 0..2 {
            for i in 0..n {
                assert_eq!(
                    out[c][i],
                    orig.channels[c].get(i) - guitar.channels[c].get(i)
                );
            }
        }
    }

    #[test]
    fn p1_one_stem_off_is_original_minus_stem() {
        let n = 3000;
        let (stems, orig) = p1_set(n);
        let guitar = stems[1].clone();
        let mut gains = all(1.0);
        gains[1].1 = 0.0;
        let mut m = Mixer::new(vec![], &gains);
        m.set_mode(MixMode::Original);
        m.load_with_original(stems, Some(orig.clone()), false);
        let out = render(&mut m, 0, n);
        for c in 0..2 {
            for i in 0..n {
                assert_eq!(
                    out[c][i],
                    orig.channels[c].as_f32().unwrap()[i] - guitar.channels[c].as_f32().unwrap()[i]
                );
            }
        }
    }

    #[test]
    fn p1_isolation_and_solo_are_plain_sum() {
        let n = 3000;
        let (stems, orig) = p1_set(n);
        // "Only bass" at 0.8 (stems mode): exactly 0.8 * bass, no residual.
        let mut gains = all(0.0);
        gains[2].1 = 0.8;
        let mut m = Mixer::new(vec![], &gains);
        m.set_mode(choose_mode(6, 1, false));
        m.load_with_original(stems.clone(), Some(orig), false);
        let out = render(&mut m, 0, n);
        for c in 0..2 {
            for i in 0..n {
                assert_eq!(out[c][i], stems[2].channels[c].as_f32().unwrap()[i] * 0.8);
            }
        }
        // Volume boost in original mode: original + (g - 1) * stem.
        let mut m2 = Mixer::new(vec![], &all(1.0));
        let (stems, orig) = p1_set(n);
        m2.set_mode(MixMode::Original);
        m2.load_with_original(stems.clone(), Some(orig.clone()), false);
        let mut boosted = all(1.0);
        boosted[3].1 = 2.0;
        m2.set_gains(boosted);
        let out = render(&mut m2, 0, GAIN_RAMP + 100);
        let i = GAIN_RAMP + 50;
        let want =
            orig.channels[0].as_f32().unwrap()[i] + stems[3].channels[0].as_f32().unwrap()[i];
        assert!((out[0][i] - want).abs() < 1e-6);
    }

    #[test]
    fn p1_mode_crossfade_is_continuous() {
        let n = 20_000;
        let (stems, orig) = p1_set(n);
        // Guitar off: original mode = orig - guitar; stems mode = sum of the others.
        let mut gains = all(1.0);
        gains[1].1 = 0.0;
        let mut m = Mixer::new(vec![], &gains);
        m.set_mode(MixMode::Original);
        m.load_with_original(stems.clone(), Some(orig.clone()), false);
        let a = render(&mut m, 0, 1000);
        m.set_mode(MixMode::Stems);
        let b = render(&mut m, 1000, MODE_XFADE + 1000);
        let mut out = a[0].clone();
        out.extend_from_slice(&b[0]);
        // The two targets differ by the residual (0.05 sin); max per-sample
        // jump must stay at the signal's own slope plus the ramp share.
        let max_jump = out
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        let sig_jump = (1..out.len())
            .map(|i| {
                let s = |k: usize| -> f32 {
                    stems
                        .iter()
                        .enumerate()
                        .filter(|(j, _)| *j != 1)
                        .map(|(_, t)| t.channels[0].as_f32().unwrap()[k])
                        .sum()
                };
                (s(i) - s(i - 1)).abs()
            })
            .fold(0.0f32, f32::max);
        assert!(
            max_jump < sig_jump + 0.06 + 0.1 / MODE_XFADE as f32,
            "{max_jump} vs {sig_jump}"
        );
        // Before: orig - guitar; after the crossfade: plain sum.
        let k = 500;
        assert_eq!(
            out[k],
            orig.channels[0].as_f32().unwrap()[k] - stems[1].channels[0].as_f32().unwrap()[k]
        );
        let k = 1000 + MODE_XFADE + 500;
        let sum: f32 = stems
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != 1)
            .map(|(_, t)| t.channels[0].as_f32().unwrap()[k])
            .sum();
        assert!((out[k] - sum).abs() < 1e-5);
        // Halfway through the crossfade it is the average of the two.
        let k = 1000 + MODE_XFADE / 2;
        let s: f32 = stems
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != 1)
            .map(|(_, t)| t.channels[0].as_f32().unwrap()[k])
            .sum();
        let o = orig.channels[0].as_f32().unwrap()[k] - stems[1].channels[0].as_f32().unwrap()[k];
        assert!((out[k] - 0.5 * (s + o)).abs() < 2e-3, "{} {}", out[k], s);
    }

    #[test]
    fn p1_without_original_falls_back_to_sum() {
        let n = 1000;
        let (stems, _) = p1_set(n);
        let mut m = Mixer::new(vec![], &all(1.0));
        m.set_mode(MixMode::Original);
        m.load_with_original(stems.clone(), None, false);
        assert!(!m.has_original());
        let out = render(&mut m, 0, n);
        let sum: f32 = stems
            .iter()
            .map(|t| t.channels[0].as_f32().unwrap()[10])
            .sum();
        assert!((out[0][10] - sum).abs() < 1e-6);
    }
}
