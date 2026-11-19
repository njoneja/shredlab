// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Fast bends at a lead note's start, checked on the stem's audio. Basic
//! Pitch's contour (a third of a semitone, 11.6 ms frames) misses a bend
//! that reaches its target within a few tens of milliseconds of the pick:
//! it reports the target as a plain note, often starting where the pitch
//! arrives. A finer spectrum around the note's start shows the lower pitch
//! sounding first and the target taking over from it.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

/// Analysis span around a note's start (s).
const BEFORE_S: f64 = 0.15;
const AFTER_S: f64 = 0.15;
/// Spectrum window (s, rounded to a power of two in samples) and hop (s).
const WINDOW_S: f64 = 0.046;
const HOP_S: f64 = 0.005;
/// Per hop, the strongest peak from this far under the note to this far
/// over it (semitones).
const BAND_UNDER_ST: f32 = 3.0;
const BAND_OVER_ST: f32 = 0.5;
/// A peak counts when it is at most this far under the frame's strongest
/// bin between [`REF_LO_HZ`] and [`REF_HI_HZ`] (dB).
const PEAK_RANGE_DB: f32 = 20.0;
const REF_LO_HZ: f64 = 60.0;
const REF_HI_HZ: f64 = 5000.0;
/// The lower pitch: [`MIN_DROP_ST`] to [`MAX_DROP_ST`] under the note
/// (semitones; a fast bend is a half or whole step, and a note a minor
/// third under the target is more often a note of its own), the
/// strongest in the band for [`LOW_HOPS`] to [`LOW_MAX_HOPS`] hops in a
/// row, steady within [`LOW_SPREAD_ST`], and new (not the same pitch in
/// the hop before).
const MIN_DROP_ST: f32 = 0.75;
const MAX_DROP_ST: f32 = 2.6;
const LOW_HOPS: usize = 3;
const LOW_MAX_HOPS: usize = 30;
const LOW_SPREAD_ST: f32 = 0.35;
/// The note takes over within [`ARRIVE_HOPS`] hops after the lower pitch's
/// last, within [`ARRIVE_ST`] of the note and without dipping under the
/// lower pitch on the way, holds there for [`HOLD_HOPS`] hops, and gets at
/// least as strong as the lower pitch was.
const ARRIVE_HOPS: usize = 12;
const ARRIVE_ST: f32 = 0.3;
const HOLD_HOPS: usize = 4;
/// The lower pitch's level rises at least this much (dB) from its quietest
/// in the [`ONSET_HOPS`] hops before its first hop to its loudest.
const ONSET_HOPS: usize = 15;
const ONSET_RISE_DB: f32 = 6.0;
/// The lower pitch's level falls at least this much (dB) under its loudest
/// within [`FADE_FROM`] to [`FADE_TO`] hops after the arrival.
const FADE_DB: f32 = 10.0;
const FADE_FROM: usize = 3;
const FADE_TO: usize = 10;
/// A hop between the lower pitch (at least this far over it, semitones)
/// and the note makes a glide. Without one, the lower pitch is at least
/// [`OFF_GRID_ST`] from a semitone (after the song's offset) and lasts at
/// most [`JUMP_LOW_MAX_HOPS`] hops.
const GLIDE_ST: f32 = 0.3;
const OFF_GRID_ST: f32 = 0.2;
const JUMP_LOW_MAX_HOPS: usize = 12;
/// Notes under this (MIDI) are not checked: a whole step there is under
/// two bins of the window, too close to tell the two pitches apart.
const MIN_PITCH: f32 = 64.0;
/// The arrival is at most this long before the model's note start (s).
const ARRIVE_EARLY_S: f64 = 0.08;

/// A bend found at a note's start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct QuickBend {
    /// When the lower pitch starts (s).
    pub onset_s: f64,
    /// When the pitch leaves it (s).
    pub start_s: f64,
    /// When it reaches the note (s).
    pub peak_s: f64,
    /// The lower pitch and the pitch reached, MIDI (fractional, the same
    /// reference as the note's pitch).
    pub from: f32,
    pub to: f32,
}

/// One hop of the analysis: the strongest peak in the band, relative to
/// the note (semitones), and its level under the frame's strongest bin
/// (dB), when within [`PEAK_RANGE_DB`].
type Hop = Option<(f32, f32)>;

/// The band's magnitudes (dBFS-like, 20 log10 of the bin magnitude) per
/// hop, from bin `first` on.
struct Band {
    first: usize,
    bin_hz: f64,
    db: Vec<Vec<f32>>,
}

impl Band {
    /// The level near `midi` (the strongest of the bins within one of it)
    /// at hop `i`.
    fn level(&self, i: usize, midi: f32) -> f32 {
        let k = (midi_hz(midi) / self.bin_hz).round() as usize;
        let row = &self.db[i];
        (k.saturating_sub(1)..=k + 1)
            .filter_map(|k| k.checked_sub(self.first).and_then(|k| row.get(k)))
            .copied()
            .fold(f32::MIN, f32::max)
    }
}

/// Reusable spectrum state for one stem.
pub(crate) struct Probe<'a> {
    samples: &'a [f32],
    rate: f64,
    n: usize,
    hop: usize,
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    input: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    mags: Vec<f32>,
}

fn midi_hz(midi: f32) -> f64 {
    440.0 * 2f64.powf((f64::from(midi) - 69.0) / 12.0)
}

fn hz_midi(hz: f64) -> f32 {
    (69.0 + 12.0 * (hz / 440.0).log2()) as f32
}

impl<'a> Probe<'a> {
    /// `samples` are the mono stem at `rate`.
    pub fn new(samples: &'a [f32], rate: u32) -> Self {
        let rate = f64::from(rate);
        let n = 1usize << ((WINDOW_S * rate).log2().round().max(6.0) as u32);
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(n);
        // Periodic Hann.
        let window = (0..n)
            .map(|i| (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos()) as f32)
            .collect();
        Self {
            samples,
            rate,
            n,
            hop: ((HOP_S * rate).round() as usize).max(1),
            input: fft.make_input_vec(),
            spectrum: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            mags: vec![0.0; n / 2 + 1],
            fft,
            window,
        }
    }

    /// Magnitudes of the window centered on sample `center`.
    fn frame(&mut self, center: usize) {
        let s0 = center as isize - (self.n / 2) as isize;
        for (i, x) in self.input.iter_mut().enumerate() {
            let k = s0 + i as isize;
            let v = if k >= 0 {
                self.samples.get(k as usize).copied().unwrap_or(0.0)
            } else {
                0.0
            };
            *x = v * self.window[i];
        }
        self.fft
            .process_with_scratch(&mut self.input, &mut self.spectrum, &mut self.scratch)
            .expect("buffer sizes come from the plan");
        for (m, c) in self.mags.iter_mut().zip(&self.spectrum) {
            *m = c.norm();
        }
    }

    fn bin_hz(&self) -> f64 {
        self.rate / self.n as f64
    }

    fn bins(&self, lo_hz: f64, hi_hz: f64) -> (usize, usize) {
        (
            ((lo_hz / self.bin_hz()).ceil() as usize).max(1),
            ((hi_hz / self.bin_hz()).floor() as usize).min(self.n / 2 - 1),
        )
    }

    /// The strongest bin between two frequencies.
    fn max_in(&self, lo_hz: f64, hi_hz: f64) -> f32 {
        let (lo, hi) = self.bins(lo_hz, hi_hz);
        self.mags
            .get(lo..=hi)
            .map_or(0.0, |b| b.iter().copied().fold(0.0, f32::max))
    }

    /// The strongest local maximum between two frequencies: its frequency
    /// (refined on the log magnitude) and magnitude.
    fn peak_in(&self, lo_hz: f64, hi_hz: f64) -> Option<(f64, f32)> {
        let (lo, hi) = self.bins(lo_hz, hi_hz);
        let m = &self.mags;
        let k = (lo..=hi)
            .filter(|&k| m[k] > m[k - 1] && m[k] >= m[k + 1])
            .max_by(|&a, &b| m[a].total_cmp(&m[b]))?;
        let (a, b, c) = (
            f64::from(m[k - 1].max(1e-12)).ln(),
            f64::from(m[k].max(1e-12)).ln(),
            f64::from(m[k + 1].max(1e-12)).ln(),
        );
        let den = a - 2.0 * b + c;
        let p = if den < 0.0 {
            (0.5 * (a - c) / den).clamp(-0.5, 0.5)
        } else {
            0.0
        };
        Some(((k as f64 + p) * self.bin_hz(), m[k]))
    }

    /// Every hop from `t0_s` to `t1_s`: the strongest peak between
    /// [`BAND_UNDER_ST`] under `pitch` and [`BAND_OVER_ST`] over it.
    fn track(&mut self, t0_s: f64, t1_s: f64, pitch: f32) -> (Vec<Hop>, Band) {
        let lo = midi_hz(pitch - BAND_UNDER_ST);
        let hi = midi_hz(pitch + BAND_OVER_ST);
        let (b0, b1) = self.bins(0.97 * lo, 1.03 * hi);
        let mut band = Band {
            first: b0,
            bin_hz: self.bin_hz(),
            db: Vec::new(),
        };
        let c0 = (t0_s.max(0.0) * self.rate).round() as usize;
        let c1 = ((t1_s * self.rate).round() as usize).min(self.samples.len());
        let mut out = Vec::new();
        let mut c = c0;
        while c < c1 {
            self.frame(c);
            let reference = self.max_in(REF_LO_HZ, REF_HI_HZ.min(0.45 * self.rate));
            let hop = self.peak_in(lo, hi).and_then(|(f, m)| {
                let db = 20.0 * (m / reference.max(f32::MIN_POSITIVE)).log10();
                (reference > 0.0 && db >= -PEAK_RANGE_DB).then(|| (hz_midi(f) - pitch, db))
            });
            out.push(hop);
            band.db.push(
                self.mags[b0..=b1.max(b0)]
                    .iter()
                    .map(|&m| 20.0 * m.max(1e-9).log10())
                    .collect(),
            );
            c += self.hop;
        }
        (out, band)
    }

    /// Looks for a fast bend into a note at `pitch` (MIDI, fractional)
    /// that the model starts at `start_s` and ends at `end_s`.
    pub fn find(&mut self, pitch: f32, offset: f32, start_s: f64, end_s: f64) -> Option<QuickBend> {
        let t0 = (start_s - BEFORE_S).max(0.0);
        let t1 = (start_s + AFTER_S).min(end_s);
        if pitch < MIN_PITCH {
            return None;
        }
        let (track, band) = self.track(t0, t1, pitch);
        let hop_s = self.hop as f64 / self.rate;
        let first = ((start_s - ARRIVE_EARLY_S - t0) / hop_s).max(0.0) as usize;
        let (onset, leave, arrive, from) = rise(&track, &band, pitch, offset, first)?;
        let mut held: Vec<f32> = track[arrive..arrive + HOLD_HOPS]
            .iter()
            .filter_map(|h| h.map(|h| h.0))
            .collect();
        held.sort_by(f32::total_cmp);
        let to = held[held.len() / 2];
        let at = |i: usize| t0 + i as f64 * hop_s;
        Some(QuickBend {
            onset_s: at(onset),
            start_s: at(leave),
            peak_s: at(arrive),
            from: pitch + from,
            to: pitch + to,
        })
    }
}

/// A lower pitch taken over by the note in a track (see the constants): the
/// hops the lower pitch starts and ends, the hop the note arrives (at or
/// after `first`), and the lower pitch (semitones from the note).
fn rise(
    track: &[Hop],
    band: &Band,
    pitch: f32,
    offset: f32,
    first: usize,
) -> Option<(usize, usize, usize, f32)> {
    let n = track.len();
    let low_at = |i: usize| {
        track[i]
            .map(|h| h.0)
            .filter(|r| (-MAX_DROP_ST..=-MIN_DROP_ST).contains(r))
    };
    let mut j = 1;
    while j < n {
        let Some(r0) = low_at(j) else {
            j += 1;
            continue;
        };
        // The run of the lower pitch from j.
        let (mut lo, mut hi, mut e) = (r0, r0, j);
        while e + 1 < n {
            match low_at(e + 1) {
                Some(r) if r.max(hi) - r.min(lo) <= LOW_SPREAD_ST => {
                    lo = lo.min(r);
                    hi = hi.max(r);
                    e += 1;
                }
                _ => break,
            }
        }
        let next = e + 1;
        // New: the hop before the run is not at the same pitch.
        let fresh = low_at(j - 1).is_none_or(|r| r.max(hi) - r.min(lo) > LOW_SPREAD_ST);
        if !fresh || !(LOW_HOPS..=LOW_MAX_HOPS).contains(&(e + 1 - j)) {
            j = next;
            continue;
        }
        let mut lows: Vec<f32> = (j..=e).filter_map(low_at).collect();
        lows.sort_by(f32::total_cmp);
        let base = lows[lows.len() / 2];
        let low_db = (j..=e)
            .filter_map(|i| track[i].map(|h| h.1))
            .fold(f32::MIN, f32::max);
        // Struck: the lower pitch gets louder at its start, rather than
        // showing when the note dips (a ringing string under a re-pick).
        let loudest = (j..=e)
            .map(|i| band.level(i, pitch + base))
            .fold(f32::MIN, f32::max);
        let before = (j.saturating_sub(ONSET_HOPS)..j)
            .map(|i| band.level(i, pitch + base))
            .fold(f32::MAX, f32::min);
        if loudest - before < ONSET_RISE_DB {
            j = next;
            continue;
        }
        // The note takes over.
        let arrive = (next..(next + ARRIVE_HOPS).min(n))
            .take_while(|&i| track[i].is_some_and(|h| h.0 >= lo - LOW_SPREAD_ST))
            .find(|&i| track[i].is_some_and(|h| h.0 >= -ARRIVE_ST));
        if let Some(k) = arrive.filter(|&k| k >= first) {
            let strong = track.get(k..k + HOLD_HOPS).is_some_and(|h| {
                h.iter().all(|h| h.is_some_and(|h| h.0 >= -ARRIVE_ST))
                    && h.iter()
                        .filter_map(|h| h.map(|h| h.1))
                        .fold(f32::MIN, f32::max)
                        >= low_db
            });
            // Gone: the string that sounded the lower pitch now sounds the
            // note, so the lower pitch fades once the window has left it.
            let faded = (k + FADE_FROM..=k + FADE_TO)
                .filter(|&i| i < n)
                .map(|i| band.level(i, pitch + base))
                .fold(f32::MAX, f32::min)
                <= loudest - FADE_DB;
            // Moving: the pitch glides into the note (a hop on the way), or
            // jumps from a short lower pitch between two semitones (the
            // string was already rising; a fretted note sits on one).
            let glided = (next..k)
                .any(|i| track[i].is_some_and(|h| h.0 > base + GLIDE_ST && h.0 < -ARRIVE_ST));
            let off = pitch + base - offset;
            let between =
                (off - off.round()).abs() >= OFF_GRID_ST && e + 1 - j <= JUMP_LOW_MAX_HOPS;
            if strong && faded && (glided || between) {
                return Some((j, e, k, base));
            }
        }
        j = next;
    }
    None
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::f64::consts::TAU;

    pub(crate) const SR: u32 = 44_100;

    /// Adds a tone with five harmonics whose pitch follows `pitch(t)` from
    /// `at_s` to `until_s`, with a noise burst (a pick) of `burst` at its
    /// start.
    pub(crate) fn tone(
        x: &mut [f32],
        at_s: f64,
        until_s: f64,
        pitch: impl Fn(f64) -> f64,
        burst: f64,
    ) {
        let rate = f64::from(SR);
        let mut phase = 0.0f64;
        let mut seed = 12_345u64;
        for (i, y) in x.iter_mut().enumerate() {
            let t = i as f64 / rate - at_s;
            if t < 0.0 || t > until_s - at_s {
                continue;
            }
            let f = 440.0 * 2f64.powf((pitch(t) - 69.0) / 12.0);
            phase += TAU * f / rate;
            let env = (t / 0.002).min(1.0) * (-2.0 * t).exp();
            let mut v = 0.0;
            for h in 1..=5 {
                v += (phase * f64::from(h)).sin() / f64::from(h);
            }
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let noise = ((seed >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0;
            *y += (0.3 * env * v + burst * noise * (-t / 0.004).exp()) as f32;
        }
    }

    pub(crate) fn buffer() -> Vec<f32> {
        vec![0.0; (1.2 * f64::from(SR)) as usize]
    }

    #[test]
    fn a_quick_glide_up_is_a_bend_from_the_lower_note() {
        // C6 picked at 0.5 s, held 30 ms, bent to D6 over 40 ms, held.
        let mut x = buffer();
        tone(
            &mut x,
            0.5,
            1.2,
            |t| 84.0 + 2.0 * ((t - 0.03) / 0.04).clamp(0.0, 1.0),
            0.2,
        );
        let mut p = Probe::new(&x, SR);
        // The model reports a D6 starting where the pitch arrives.
        let b = p.find(86.0, 0.0, 0.56, 1.0).expect("bend");
        assert!((b.from - 84.0).abs() < 0.4, "{b:?}");
        assert!((b.to - 86.0).abs() < 0.2, "{b:?}");
        assert!((b.onset_s - 0.5).abs() < 0.03, "{b:?}");
        assert!(b.start_s > b.onset_s && b.peak_s > b.start_s, "{b:?}");
        assert!(b.peak_s < 0.6, "{b:?}");
    }

    #[test]
    fn a_steady_note_with_a_sharp_attack_is_not_a_bend() {
        let mut x = buffer();
        tone(&mut x, 0.5, 1.2, |_| 86.0, 1.5);
        let mut p = Probe::new(&x, SR);
        assert_eq!(p.find(86.0, 0.0, 0.5, 1.0), None);
    }

    #[test]
    fn a_lower_note_ringing_into_the_next_is_not_a_bend() {
        // C6 from 0.2 s, still ringing when D6 is picked at 0.5 s.
        let mut x = buffer();
        tone(&mut x, 0.2, 0.7, |_| 84.0, 0.2);
        tone(&mut x, 0.5, 1.2, |_| 86.0, 0.2);
        let mut p = Probe::new(&x, SR);
        assert_eq!(p.find(86.0, 0.0, 0.5, 1.0), None);
    }
}
