// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Reference tone voice for tuning by ear: a plucked string or a drone,
//! rendered at the output device rate (so speed, pitch and the rate
//! correction never touch it) and mixed after the device-rate resampler.

use std::f64::consts::PI;

/// Sound of a reference tone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToneSound {
    /// Karplus-Strong string, about 4 s decay, plucked again every 2.5 s.
    Pluck,
    /// Sawtooth through a one-pole lowpass at 4×f0, 50 ms attack, steady.
    Drone,
}

/// Tone frequency range accepted by [`Tone::start`] (clamped).
pub const TONE_MIN_HZ: f32 = 20.0;
pub const TONE_MAX_HZ: f32 = 5000.0;
/// Peak level, -12 dBFS (before the master volume).
pub(crate) const LEVEL: f32 = 0.251_188_64;
/// Time for a plucked string's fundamental to fall by 60 dB.
const PLUCK_DECAY_SECS: f64 = 4.0;
const PLUCK_EVERY_SECS: f64 = 2.5;
const DRONE_ATTACK_SECS: f32 = 0.05;
/// Release after `stop`.
const RELEASE_SECS: f32 = 0.03;
/// Release of the old tone before a new one starts.
const RESTART_SECS: f32 = 0.01;

/// Mono tone generator; owned by the producer thread. Allocates only when
/// the sample rate changes.
pub(crate) struct Tone {
    sr: f32,
    hz: f32,
    sound: ToneSound,
    /// Sounding (including a release).
    on: bool,
    releasing: bool,
    /// Started while sounding: begins when the release ends.
    pending: Option<(f32, ToneSound)>,
    env: f32,
    env_step: f32,
    // Pluck: delay line, loop lowpass (stretch `s`, gain `g`), allpass
    // fractional delay (`ap`), retrigger timer.
    line: Vec<f32>,
    w: usize,
    delay: usize,
    s: f32,
    g: f32,
    ap: f32,
    v1: f32,
    ap_x1: f32,
    ap_y1: f32,
    since: usize,
    every: usize,
    rng: u32,
    // Drone.
    phase: f32,
    inc: f32,
    lp: f32,
    lp_a: f32,
}

impl Tone {
    pub fn new(sample_rate: f32) -> Self {
        let mut t = Self {
            sr: 0.0,
            hz: 440.0,
            sound: ToneSound::Drone,
            on: false,
            releasing: false,
            pending: None,
            env: 0.0,
            env_step: 0.0,
            line: Vec::new(),
            w: 0,
            delay: 1,
            s: 0.5,
            g: 1.0,
            ap: 0.0,
            v1: 0.0,
            ap_x1: 0.0,
            ap_y1: 0.0,
            since: 0,
            every: 1,
            rng: 0x9e37_79b9,
            phase: 0.0,
            inc: 0.0,
            lp: 0.0,
            lp_a: 1.0,
        };
        t.set_sample_rate(sample_rate);
        t
    }

    /// Sounding, including the release after [`Self::stop`].
    pub fn is_active(&self) -> bool {
        self.on
    }

    /// Follows the output device rate; a sounding tone restarts.
    pub fn set_sample_rate(&mut self, sr: f32) {
        if sr.is_nan() || sr <= 0.0 || (sr - self.sr).abs() < 0.5 {
            return;
        }
        self.sr = sr;
        self.line = vec![0.0; (sr / TONE_MIN_HZ).ceil() as usize + 4];
        if self.on {
            let (hz, sound) = self.pending.take().unwrap_or((self.hz, self.sound));
            self.begin(hz, sound);
        }
    }

    /// Plays `hz` (clamped to [`TONE_MIN_HZ`]..=[`TONE_MAX_HZ`]; non-finite
    /// is ignored). A sounding tone with another frequency or sound releases
    /// briefly, then the new one starts.
    pub fn start(&mut self, hz: f32, sound: ToneSound) {
        if !hz.is_finite() {
            return;
        }
        let hz = hz.clamp(TONE_MIN_HZ, TONE_MAX_HZ);
        if !self.on {
            self.begin(hz, sound);
        } else if self.releasing || hz != self.hz || sound != self.sound {
            self.pending = Some((hz, sound));
            self.release(RESTART_SECS);
        }
    }

    /// Releases the tone (short fade, no click).
    pub fn stop(&mut self) {
        self.pending = None;
        if self.on {
            self.release(RELEASE_SECS);
        }
    }

    fn release(&mut self, secs: f32) {
        let step = 1.0 / (secs * self.sr);
        // Never slower than a release already running.
        self.env_step = if self.releasing {
            self.env_step.max(step)
        } else {
            step
        };
        self.releasing = true;
    }

    fn begin(&mut self, hz: f32, sound: ToneSound) {
        self.hz = hz;
        self.sound = sound;
        self.on = true;
        self.releasing = false;
        self.pending = None;
        match sound {
            ToneSound::Pluck => {
                self.tune_string();
                self.env = 1.0;
                self.env_step = 0.0;
                self.every = (PLUCK_EVERY_SECS * self.sr as f64).round() as usize;
                self.excite();
            }
            ToneSound::Drone => {
                self.env = 0.0;
                self.env_step = 1.0 / (DRONE_ATTACK_SECS * self.sr);
                self.inc = hz / self.sr;
                // Starts mid-ramp, where the sawtooth crosses zero.
                self.phase = 0.5;
                self.lp = 0.0;
                self.lp_a = (1.0 - (-2.0 * PI * 4.0 * hz as f64 / self.sr as f64).exp()) as f32;
            }
        }
    }

    /// Loop delay, damping and fractional delay for the current `hz`, so
    /// the string's fundamental is exact and decays by 60 dB in
    /// [`PLUCK_DECAY_SECS`].
    fn tune_string(&mut self) {
        let sr = self.sr as f64;
        let hz = self.hz as f64;
        let period = sr / hz;
        let w = 2.0 * PI / period;
        // Fundamental gain per period for the decay time.
        let rho = 10f64.powf(-3.0 / (hz * PLUCK_DECAY_SECS));
        // Loop lowpass (1-s) + s·z⁻¹: s = 0.5 is the classic average; high
        // notes need less damping per period to ring as long.
        let q = (1.0 - rho * rho) / (2.0 * (1.0 - w.cos()));
        let s = if q >= 0.25 {
            0.5
        } else {
            0.5 * (1.0 - (1.0 - 4.0 * q).sqrt())
        };
        let mag = (1.0 - 2.0 * s * (1.0 - s) * (1.0 - w.cos())).sqrt();
        let g = (rho / mag).min(1.0);
        let tau_lp = -(-s * w.sin()).atan2(1.0 - s + s * w.cos()) / w;
        // The allpass supplies the rest of the period, kept in 0.1..1.1.
        let need = period - tau_lp;
        let delay = ((need - 0.1).floor() as usize).clamp(1, self.line.len() - 2);
        let target = need - delay as f64;
        let tau_ap = |a: f64| {
            let num = (-w.sin()).atan2(a + w.cos());
            let den = (-a * w.sin()).atan2(1.0 + a * w.cos());
            -(num - den) / w
        };
        let mut d = target;
        let mut a = 0.0;
        for _ in 0..8 {
            a = (1.0 - d) / (1.0 + d);
            d += target - tau_ap(a);
        }
        self.delay = delay;
        self.s = s as f32;
        self.g = g as f32;
        self.ap = a as f32;
    }

    fn noise(&mut self) -> f32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    /// Fills the string with a softened noise burst (no DC, peak 1).
    fn excite(&mut self) {
        let size = self.line.len();
        let mut lp = 0.0;
        let mut sum = 0.0;
        for k in 0..self.delay {
            let i = (self.w + size - self.delay + k) % size;
            lp += 0.5 * (self.noise() - lp);
            self.line[i] = lp;
            sum += lp;
        }
        let mean = sum / self.delay as f32;
        let mut peak = 0.0f32;
        for k in 0..self.delay {
            let i = (self.w + size - self.delay + k) % size;
            self.line[i] -= mean;
            peak = peak.max(self.line[i].abs());
        }
        let norm = if peak > 0.0 { 1.0 / peak } else { 0.0 };
        for k in 0..self.delay {
            let i = (self.w + size - self.delay + k) % size;
            self.line[i] *= norm;
        }
        self.v1 = 0.0;
        self.ap_x1 = 0.0;
        self.ap_y1 = 0.0;
        self.since = 0;
    }

    fn pluck_sample(&mut self) -> f32 {
        if self.since >= self.every {
            self.excite();
        }
        self.since += 1;
        let size = self.line.len();
        let v = self.line[(self.w + size - self.delay) % size];
        let lp = self.g * ((1.0 - self.s) * v + self.s * self.v1);
        self.v1 = v;
        let y = self.ap * lp + self.ap_x1 - self.ap * self.ap_y1;
        self.ap_x1 = lp;
        self.ap_y1 = y;
        self.line[self.w] = y;
        self.w = (self.w + 1) % size;
        y
    }

    fn drone_sample(&mut self) -> f32 {
        let (t, dt) = (self.phase, self.inc);
        let blep = if t < dt {
            let t = t / dt;
            t + t - t * t - 1.0
        } else if t > 1.0 - dt {
            let t = (t - 1.0) / dt;
            t * t + t + t + 1.0
        } else {
            0.0
        };
        let saw = 2.0 * t - 1.0 - blep;
        self.phase += dt;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        self.lp += self.lp_a * (saw - self.lp);
        self.lp
    }

    /// Overwrites `out` with the tone (mono, at [`LEVEL`]); zeros when off.
    pub fn render(&mut self, out: &mut [f32]) {
        for o in out.iter_mut() {
            if !self.on {
                *o = 0.0;
                continue;
            }
            if self.releasing {
                self.env -= self.env_step;
                if self.env <= 0.0 {
                    self.env = 0.0;
                    match self.pending.take() {
                        Some((hz, sound)) => self.begin(hz, sound),
                        None => {
                            self.on = false;
                            self.releasing = false;
                            *o = 0.0;
                            continue;
                        }
                    }
                }
            } else if self.env < 1.0 {
                self.env = (self.env + self.env_step).min(1.0);
            }
            let x = match self.sound {
                ToneSound::Pluck => self.pluck_sample(),
                ToneSound::Drone => self.drone_sample(),
            };
            *o = x * self.env * LEVEL;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rate::test_util::{cents, periodic_hz};

    fn run(t: &mut Tone, secs: f32) -> Vec<f32> {
        let mut v = vec![0.0; (secs * t.sr) as usize];
        t.render(&mut v);
        v
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    fn max_step(x: &[f32]) -> f32 {
        x.windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max)
    }

    #[test]
    fn pluck_and_drone_are_in_tune() {
        for sr in [44_100.0f32, 48_000.0] {
            for hz in [41.2f32, 82.41, 146.83, 329.63, 659.26, 1318.5] {
                for sound in [ToneSound::Pluck, ToneSound::Drone] {
                    let mut t = Tone::new(sr);
                    t.start(hz, sound);
                    let v = run(&mut t, 1.2);
                    let seg = &v[(0.1 * sr) as usize..];
                    let f = periodic_hz(seg, sr as f64, hz as f64);
                    let c = cents(f, hz as f64);
                    assert!(c.abs() < 0.5, "{sound:?} {hz} Hz at {sr}: {f} ({c} cents)");
                }
            }
        }
    }

    #[test]
    fn level_is_minus_12_dbfs() {
        for sound in [ToneSound::Pluck, ToneSound::Drone] {
            for hz in [41.2f32, 110.0, 1318.5] {
                let mut t = Tone::new(48_000.0);
                t.start(hz, sound);
                let v = run(&mut t, 0.5);
                let peak = v.iter().fold(0.0f32, |m, x| m.max(x.abs()));
                assert!(
                    peak <= LEVEL * 1.12 && peak > LEVEL * 0.5,
                    "{sound:?} {hz}: {peak}"
                );
            }
        }
    }

    #[test]
    fn pluck_decays_and_retriggers_every_2_5_s() {
        let sr = 48_000.0;
        let mut t = Tone::new(sr);
        t.start(110.0, ToneSound::Pluck);
        let v = run(&mut t, 3.0);
        let seg = |a: f32, b: f32| rms(&v[(a * sr) as usize..(b * sr) as usize]);
        let db = |x: f32| 20.0 * x.log10();
        let early = seg(0.1, 0.25);
        let late = seg(2.3, 2.45);
        // About -60 dB per 4 s: roughly -33 dB by 2.3 s.
        let drop = db(late / early);
        assert!((-45.0..-22.0).contains(&drop), "{drop} dB");
        let again = seg(2.6, 2.75);
        assert!(db(again / early).abs() < 4.0, "{}", db(again / early));
    }

    #[test]
    fn drone_attack_is_gradual() {
        let mut t = Tone::new(48_000.0);
        t.start(110.0, ToneSound::Drone);
        let v = run(&mut t, 0.5);
        let steady = v[24_000 - 4800..]
            .iter()
            .fold(0.0f32, |m, x| m.max(x.abs()));
        let first = v[..480].iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(v[0].abs() < 1e-3);
        assert!(first < 0.3 * steady, "{first} vs {steady}");
    }

    #[test]
    fn stop_releases_without_a_click_then_goes_silent() {
        for sound in [ToneSound::Pluck, ToneSound::Drone] {
            for hz in [55.0f32, 440.0] {
                let sr = 48_000.0;
                let mut t = Tone::new(sr);
                t.start(hz, sound);
                let before = run(&mut t, 0.6);
                t.stop();
                assert!(t.is_active());
                let after = run(&mut t, 0.1);
                assert!(!t.is_active());
                let rel = (RELEASE_SECS * sr) as usize + 1;
                assert!(after[rel..].iter().all(|&x| x == 0.0));
                // Across the stop and through the release, no step larger
                // than the tone's own steps just before.
                let tail = &before[before.len() - rel..];
                let mut joined = vec![*tail.last().unwrap()];
                joined.extend_from_slice(&after[..rel + 1]);
                assert!(
                    max_step(&joined) <= max_step(tail) * 1.01,
                    "{sound:?} {hz}: {} vs {}",
                    max_step(&joined),
                    max_step(tail)
                );
            }
        }
    }

    #[test]
    fn changing_frequency_restarts_cleanly() {
        let sr = 48_000.0;
        let mut t = Tone::new(sr);
        t.start(110.0, ToneSound::Drone);
        let a = run(&mut t, 0.5);
        t.start(220.0, ToneSound::Drone);
        let b = run(&mut t, 1.0);
        let mut joined = a[a.len() - 4800..].to_vec();
        joined.extend_from_slice(&b[..4800]);
        // The steady 220 Hz drone has the larger steps; the switch adds none.
        assert!(max_step(&joined) <= max_step(&b[24_000..]) * 1.01);
        let f = periodic_hz(&b[9600..], sr as f64, 220.0);
        assert!(cents(f, 220.0).abs() < 0.5, "{f}");
        // The same tone again does not restart.
        let env = t.env;
        t.start(220.0, ToneSound::Drone);
        assert!(!t.releasing && t.env == env);
        // Non-finite is ignored; out of range is clamped.
        t.start(f32::NAN, ToneSound::Drone);
        assert!(!t.releasing);
        let mut u = Tone::new(sr);
        u.start(1.0, ToneSound::Pluck);
        assert_eq!(u.hz, TONE_MIN_HZ);
    }

    #[test]
    fn silent_when_off() {
        let mut t = Tone::new(44_100.0);
        let mut v = vec![1.0; 64];
        t.render(&mut v);
        assert!(v.iter().all(|&x| x == 0.0));
        assert!(!t.is_active());
    }
}
