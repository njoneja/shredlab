//! Summing gained tracks into a stereo mix, with click-free gain ramps and a
//! crossfade when the track set is replaced.

use crate::Track;

/// Samples over which a gain change ramps (~12 ms at 44.1 kHz).
pub const GAIN_RAMP: usize = 512;
/// Samples over which an old track set fades out after [`Mixer::load`] (~46 ms).
pub const LOAD_XFADE: usize = 2048;

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
        self.target = g;
        self.step = (g - self.cur) / GAIN_RAMP as f32;
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

struct TrackSet {
    tracks: Vec<Track>,
    gains: Vec<Gain>,
}

impl TrackSet {
    fn new(tracks: Vec<Track>, names: &[(String, f32)]) -> Self {
        let gains = gains_for(&tracks, names)
            .into_iter()
            .map(Gain::fixed)
            .collect();
        Self { tracks, gains }
    }

    /// Adds `fade(i) * mix` of `[pos, pos+n)` into the outputs.
    fn add(
        &mut self,
        pos: i64,
        l: &mut [f32],
        r: &mut [f32],
        advance: bool,
        fade: impl Fn(usize) -> f32,
    ) {
        let n = l.len();
        for (t, g) in self.tracks.iter().zip(self.gains.iter_mut()) {
            let len = t.channels[0].len().min(t.channels[1].len()) as i64;
            let mut gain = *g;
            // Fast path: steady gain 0 contributes nothing (but still advance ramps).
            if gain.cur == 0.0 && gain.target == 0.0 {
                continue;
            }
            let (cl, cr) = (&t.channels[0], &t.channels[1]);
            for i in 0..n {
                let gi = gain.next();
                let p = pos + i as i64;
                if p < 0 || p >= len {
                    continue;
                }
                let p = p as usize;
                let f = gi * fade(i);
                l[i] += cl[p] * f;
                r[i] += cr[p] * f;
            }
            if advance {
                *g = gain;
            }
        }
    }
}

/// The current track set plus, briefly after a `load`, the previous one fading out.
pub struct Mixer {
    cur: TrackSet,
    prev: Option<(TrackSet, usize)>,
    names: Vec<(String, f32)>,
    len: usize,
    sample_rate: u32,
}

impl Mixer {
    pub fn new(tracks: Vec<Track>, gains: &[(String, f32)]) -> Self {
        let (len, sample_rate) = dims(&tracks);
        Self {
            cur: TrackSet::new(tracks, gains),
            prev: None,
            names: gains.to_vec(),
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
        let (len, sr) = dims(&tracks);
        let new = TrackSet::new(tracks, &self.names);
        let old = std::mem::replace(&mut self.cur, new);
        self.prev = (crossfade && !old.tracks.is_empty()).then_some((old, 0));
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

fn dims(tracks: &[Track]) -> (usize, u32) {
    let len = tracks
        .iter()
        .map(|t| t.channels[0].len().min(t.channels[1].len()))
        .min()
        .unwrap_or(0);
    let sr = tracks.first().map_or(44_100, |t| t.sample_rate);
    (len, sr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    pub(crate) fn track(name: &str, v: f32, len: usize) -> Track {
        Track {
            name: name.into(),
            channels: [Arc::new(vec![v; len]), Arc::new(vec![-v; len])],
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
            channels: [Arc::new(data.clone()), Arc::new(data.clone())],
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
}
