//! Playback safety limiter: linked-stereo, soft-knee, short look-ahead.
//!
//! Real-time safe (fixed buffers, no allocation after construction, O(L)
//! per sample with L = [`LOOKAHEAD`] = 64). Used only on the playback path;
//! export has its own whole-file limiter in [`crate::encode`].
//!
//! Design:
//! 1. Static soft-knee curve on the linked peak `p = max(|l|, |r|)`:
//!    `f(p) = p` up to [`THRESHOLD`] (-1 dBFS), above it
//!    `f(p) = T + (C - T) * tanh((p - T) / (C - T))`, which is smooth at `T`
//!    and approaches the ceiling [`CEILING`] (-0.1 dBFS) without reaching
//!    it. The required gain is `f(p) / p`.
//! 2. Minimum of the required gain over the last L samples (look-ahead
//!    window), then an exponential release (~80 ms) that can only move the
//!    gain up slowly and never above that minimum.
//! 3. A length-L moving average of that envelope: every value averaged
//!    for output sample `n - L + 1` is at most its own required gain, so the
//!    attack is a smooth ramp that completes exactly at the peak.
//! 4. The audio is delayed by L - 1 samples to line up; a final clamp at
//!    the ceiling guards against float rounding.
//!
//! Below the threshold (and once released) the gain is exactly 1, so the
//! limiter is bit-transparent apart from the delay.

/// Look-ahead window in samples (~1.45 ms at 44.1 kHz).
pub const LOOKAHEAD: usize = 64;
/// Gain reduction starts above this peak (-1 dBFS).
pub const THRESHOLD: f32 = 0.891_250_9;
/// Output never exceeds this (-0.1 dBFS).
pub const CEILING: f32 = 0.988_553_1;
/// Release time constant in seconds.
const RELEASE_SECS: f32 = 0.08;

pub struct Limiter {
    /// Audio delay lines (ring, LOOKAHEAD - 1 samples used).
    dl: [f32; LOOKAHEAD],
    dr: [f32; LOOKAHEAD],
    /// Required gains of the last LOOKAHEAD samples (ring).
    req: [f32; LOOKAHEAD],
    /// Released envelope of the last LOOKAHEAD samples (ring) and its sum.
    env: [f32; LOOKAHEAD],
    env_sum: f64,
    /// Entries of `env` that are not exactly 1.
    env_not_one: usize,
    released: f32,
    /// 1 - released, tracked separately for precision near 1.
    gap: f32,
    idx: usize,
    release_coef: f32,
    /// Samples since the envelope sum was last recomputed (drift guard).
    since_resum: usize,
}

/// Static soft-knee curve: output peak for input peak `p`.
#[inline]
pub fn knee(p: f32) -> f32 {
    if p <= THRESHOLD {
        p
    } else {
        let span = CEILING - THRESHOLD;
        THRESHOLD + span * ((p - THRESHOLD) / span).tanh()
    }
}

impl Limiter {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate.max(1) as f32;
        Self {
            dl: [0.0; LOOKAHEAD],
            dr: [0.0; LOOKAHEAD],
            req: [1.0; LOOKAHEAD],
            env: [1.0; LOOKAHEAD],
            env_sum: LOOKAHEAD as f64,
            env_not_one: 0,
            released: 1.0,
            gap: 0.0,
            idx: 0,
            release_coef: 1.0 - (-1.0 / (RELEASE_SECS * sr)).exp(),
            since_resum: 0,
        }
    }

    /// Latency in samples.
    pub const fn latency() -> usize {
        LOOKAHEAD - 1
    }

    /// Clears the state (silence in the delay line, gain 1).
    pub fn reset(&mut self) {
        self.dl = [0.0; LOOKAHEAD];
        self.dr = [0.0; LOOKAHEAD];
        self.req = [1.0; LOOKAHEAD];
        self.env = [1.0; LOOKAHEAD];
        self.env_sum = LOOKAHEAD as f64;
        self.env_not_one = 0;
        self.released = 1.0;
        self.gap = 0.0;
        self.since_resum = 0;
    }

    /// Processes in place.
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        for (x, y) in l.iter_mut().zip(r.iter_mut()) {
            let (xl, xr) = (sanitize(*x), sanitize(*y));
            let p = xl.abs().max(xr.abs());
            let g_req = if p > THRESHOLD { knee(p) / p } else { 1.0 };
            let i = self.idx;
            self.req[i] = g_req;
            // Minimum over the look-ahead window.
            let m = self.req.iter().copied().fold(1.0f32, f32::min);
            // Release: rise slowly, never above the window minimum.
            // The gap below 1 decays exponentially (kept as a gap so f32
            // resolution near 1 doesn't stall it); it snaps to exactly 1
            // once tiny (keeps the limiter transparent).
            let mut gap = self.gap * (1.0 - self.release_coef);
            if gap < 1e-5 && m >= 1.0 {
                gap = 0.0;
            }
            let rel = 1.0 - gap;
            if m < rel {
                self.released = m;
                self.gap = 1.0 - m;
            } else {
                self.released = rel;
                self.gap = gap;
            }
            // Moving average over the window.
            self.env_sum += self.released as f64 - self.env[i] as f64;
            self.env_not_one =
                self.env_not_one + (self.released != 1.0) as usize - (self.env[i] != 1.0) as usize;
            self.env[i] = self.released;
            self.since_resum += 1;
            if self.since_resum >= 4096 {
                self.env_sum = self.env.iter().map(|&v| v as f64).sum();
                self.since_resum = 0;
            }
            let g = if self.env_not_one == 0 {
                1.0
            } else {
                ((self.env_sum / LOOKAHEAD as f64) as f32).min(1.0)
            };
            // Delay line: read the sample from LOOKAHEAD - 1 ago, then write.
            let j = (i + 1) % LOOKAHEAD;
            let (ol, or) = (self.dl[j], self.dr[j]);
            self.dl[i] = xl;
            self.dr[i] = xr;
            self.idx = j;
            if g == 1.0 {
                *x = ol;
                *y = or;
            } else {
                *x = (ol * g).clamp(-CEILING, CEILING);
                *y = (or * g).clamp(-CEILING, CEILING);
            }
        }
    }
}

#[inline]
fn sanitize(v: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}

#[cfg(test)]
#[allow(clippy::needless_range_loop)]
mod tests {
    use super::*;

    fn run(input: &[f32]) -> Vec<f32> {
        let mut lim = Limiter::new(44_100);
        let mut l = input.to_vec();
        let mut r = input.to_vec();
        // Flush the delay with silence.
        l.extend(std::iter::repeat_n(0.0, LOOKAHEAD));
        r.extend(std::iter::repeat_n(0.0, LOOKAHEAD));
        for (cl, cr) in l.chunks_mut(100).zip(r.chunks_mut(100)) {
            lim.process(cl, cr);
        }
        assert_eq!(l, r);
        l[Limiter::latency()..Limiter::latency() + input.len()].to_vec()
    }

    #[test]
    fn transparent_below_threshold() {
        let x: Vec<f32> = (0..20_000)
            .map(|i| 0.88 * (i as f32 * 0.05).sin())
            .collect();
        let y = run(&x);
        assert!(x.iter().zip(&y).all(|(a, b)| a.to_bits() == b.to_bits()));
    }

    #[test]
    fn never_exceeds_ceiling_even_with_boost() {
        // +6 dB boosted music-like signal with sharp transients.
        let x: Vec<f32> = (0..44_100)
            .map(|i| {
                let t = i as f32;
                let tone = 1.2 * (t * 0.031).sin() + 0.5 * (t * 0.17).sin();
                let hit = if i % 5000 < 3 { 2.0 } else { 0.0 };
                tone + hit
            })
            .collect();
        let y = run(&x);
        let peak = y.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak <= CEILING, "peak {peak}");
        // Not a clipper: the waveform stays close in shape (mostly gain change).
        let corr: f32 = x.iter().zip(&y).map(|(a, b)| a * b).sum::<f32>()
            / (x.iter().map(|a| a * a).sum::<f32>().sqrt()
                * y.iter().map(|b| b * b).sum::<f32>().sqrt());
        assert!(corr > 0.98, "corr {corr}");
    }

    #[test]
    fn single_spike_is_caught_by_lookahead_and_released() {
        let mut x = vec![0.5f32; 70_000];
        x[1000] = 4.0;
        let y = run(&x);
        assert!(y.iter().all(|v| v.abs() <= CEILING));
        // Gain ramps down before the spike (look-ahead), not after it.
        assert!(y[1000 - LOOKAHEAD - 2] == 0.5);
        assert!(y[999] < 0.5);
        // Fully released (exactly transparent again) well after the spike.
        assert_eq!(y[60_000], 0.5);
    }

    #[test]
    fn knee_is_smooth_and_bounded() {
        assert_eq!(knee(0.5), 0.5);
        assert_eq!(knee(THRESHOLD), THRESHOLD);
        let a = knee(THRESHOLD + 1e-4);
        assert!((a - (THRESHOLD + 1e-4)).abs() < 1e-6);
        assert!(knee(1.0) < CEILING && knee(100.0) <= CEILING);
        assert!(knee(1.0) > THRESHOLD);
    }

    #[test]
    fn non_finite_input_is_silenced() {
        let mut lim = Limiter::new(44_100);
        let mut l = vec![f32::NAN; 200];
        let mut r = vec![f32::INFINITY; 200];
        lim.process(&mut l, &mut r);
        assert!(l.iter().chain(&r).all(|v| v.is_finite()));
    }
}
