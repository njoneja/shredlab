//! Click track: synthesized click sounds and beat/count-in scheduling (pure functions).

use crate::ClickConfig;

/// Which click sound to play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickKind {
    /// Downbeat / accented count-in click: higher and louder.
    Accent,
    /// Ordinary beat.
    Beat,
    /// Subdivision between beats: softer.
    Sub,
}

/// Downbeat match tolerance (seconds).
pub const DOWNBEAT_TOLERANCE: f64 = 0.001;

/// Count-in spacing when there is no usable beat grid (seconds, song time).
pub const DEFAULT_BEAT_INTERVAL: f64 = 0.5;

/// Pre-rendered click sounds at one sample rate.
#[derive(Debug, Clone)]
pub struct ClickBank {
    accent: Vec<f32>,
    beat: Vec<f32>,
    sub: Vec<f32>,
}

impl ClickBank {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            accent: synth(sample_rate, 2000.0, 1.0),
            beat: synth(sample_rate, 1500.0, 0.6),
            sub: synth(sample_rate, 1500.0, 0.3),
        }
    }

    pub fn get(&self, kind: ClickKind) -> &[f32] {
        match kind {
            ClickKind::Accent => &self.accent,
            ClickKind::Beat => &self.beat,
            ClickKind::Sub => &self.sub,
        }
    }
}

/// ~30 ms decaying sine with a 0.5 ms attack and a short fade at the end.
fn synth(sample_rate: u32, freq: f32, amp: f32) -> Vec<f32> {
    let sr = sample_rate as f32;
    let n = (0.030 * sr).round() as usize;
    let attack = 0.0005 * sr;
    let tail = (0.003 * sr).max(1.0);
    (0..n)
        .map(|i| {
            let t = i as f32 / sr;
            let a = (i as f32 / attack).min(1.0);
            let end = ((n - i) as f32 / tail).min(1.0);
            amp * a * end * (-t / 0.008).exp() * (2.0 * std::f32::consts::PI * freq * t).sin()
        })
        .collect()
}

/// True if a downbeat lies within [`DOWNBEAT_TOLERANCE`] of `t`.
pub fn is_downbeat(downbeats: &[f64], t: f64) -> bool {
    let i = downbeats.partition_point(|&d| d < t - DOWNBEAT_TOLERANCE);
    downbeats
        .get(i)
        .is_some_and(|&d| (d - t).abs() <= DOWNBEAT_TOLERANCE)
}

/// Calls `emit(time, kind)` for every click (beats and subdivisions) with
/// `t0 <= time < t1`, in ascending order. Times are song seconds.
pub fn clicks_in_range(cfg: &ClickConfig, t0: f64, t1: f64, mut emit: impl FnMut(f64, ClickKind)) {
    let beats = &cfg.beats;
    if beats.is_empty() || t1 <= t0 {
        return;
    }
    let sub = cfg.subdivision.clamp(1, 16);
    // Start at the beat before t0 so subdivisions of that interval are included.
    let mut i = beats.partition_point(|&b| b < t0).saturating_sub(1);
    while i < beats.len() && beats[i] < t1 {
        let b = beats[i];
        if b >= t0 {
            let kind = if is_downbeat(&cfg.downbeats, b) {
                ClickKind::Accent
            } else {
                ClickKind::Beat
            };
            emit(b, kind);
        }
        if sub > 1 {
            if let Some(&next) = beats.get(i + 1) {
                for k in 1..sub {
                    let t = b + (next - b) * k as f64 / sub as f64;
                    if t >= t0 && t < t1 {
                        emit(t, ClickKind::Sub);
                    }
                }
            }
        }
        i += 1;
    }
}

/// The beat interval (song seconds) of the beat pair around `t`: the pair
/// `beats[i] <= t < beats[i+1]`, or the first/last pair outside the grid.
/// [`DEFAULT_BEAT_INTERVAL`] without at least two beats.
pub fn local_beat_interval(beats: &[f64], t: f64) -> f64 {
    if beats.len() < 2 {
        return DEFAULT_BEAT_INTERVAL;
    }
    let i = beats
        .partition_point(|&b| b <= t)
        .saturating_sub(1)
        .min(beats.len() - 2);
    let d = beats[i + 1] - beats[i];
    if d.is_finite() && d > 1e-3 {
        d
    } else {
        DEFAULT_BEAT_INTERVAL
    }
}

/// A planned count-in, in wall-clock seconds from the moment `play` starts.
///
/// The clicks lead into the first beat at or after the start position: the
/// last click sounds one interval before that beat. Music before the beat
/// (a pickup, or the rest of the bar when starting mid-beat) plays under the
/// last clicks; with a long intro before the first beat, the audio starts
/// right away and the clicks come in over it.
#[derive(Debug, Clone, PartialEq)]
pub struct CountInPlan {
    /// Spacing between clicks (wall seconds) = local beat interval / speed.
    pub interval: f64,
    /// Click `j` (0-based) sounds at `first_click + j * interval`; its number
    /// (beats before the target beat) is `count - j`. Accent flag per click.
    pub accents: Vec<bool>,
    /// When click 0 sounds (wall seconds).
    pub first_click: f64,
    /// When the audio starts at the start position (wall seconds).
    pub audio_start: f64,
}

impl CountInPlan {
    pub fn count(&self) -> u32 {
        self.accents.len() as u32
    }

    /// When the audio starts (wall seconds).
    pub fn audio_start(&self) -> f64 {
        self.audio_start
    }

    /// When click `j` sounds (wall seconds).
    pub fn click_time(&self, j: usize) -> f64 {
        self.first_click + j as f64 * self.interval
    }
}

/// Beats closer than this after the start count as "on" it.
const ON_BEAT: f64 = 0.005;

/// Plans the count-in for playback starting at song time `start`.
/// `None` when the count-in is off. Without a beat at or after `start`, the
/// count-in plays over silence and the audio starts one interval after the
/// last click.
pub fn plan_count_in(cfg: &ClickConfig, start: f64, speed: f64) -> Option<CountInPlan> {
    let n = cfg.count_in_beats.min(64);
    if n == 0 {
        return None;
    }
    let speed = speed.clamp(0.25, 4.0);
    let bpb = cfg.beats_per_bar.max(1);
    let target = cfg
        .beats
        .get(cfg.beats.partition_point(|&b| b < start - ON_BEAT))
        .copied()
        .filter(|b| b.is_finite());
    let (interval, gap) = match target {
        Some(b) => (
            local_beat_interval(&cfg.beats, b) / speed,
            (b - start).max(0.0) / speed,
        ),
        None => (local_beat_interval(&cfg.beats, start) / speed, 0.0),
    };
    let lead = n as f64 * interval;
    let (first_click, audio_start) = if gap <= lead {
        (0.0, lead - gap)
    } else {
        (gap - lead, 0.0)
    };
    let accents = (0..n).map(|j| (n - j).is_multiple_of(bpb)).collect();
    Some(CountInPlan {
        interval,
        accents,
        first_click,
        audio_start,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(beats: Vec<f64>, downbeats: Vec<f64>, sub: u32) -> ClickConfig {
        ClickConfig {
            enabled: true,
            beats,
            downbeats,
            subdivision: sub,
            count_in_beats: 0,
            beats_per_bar: 4,
            volume: 1.0,
        }
    }

    #[test]
    fn count_in_times_scale_with_speed_and_accent_bar_lines() {
        let beats: Vec<f64> = (0..20).map(|i| i as f64 * 0.5).collect(); // 120 bpm
        let mut c = cfg(beats, vec![], 1);
        c.count_in_beats = 7;
        c.beats_per_bar = 4;
        let p = plan_count_in(&c, 3.0, 1.0).unwrap();
        assert!((p.interval - 0.5).abs() < 1e-12);
        // Clicks numbered 7 6 5 4 3 2 1: only 4 is accented.
        assert_eq!(
            p.accents,
            vec![false, false, false, true, false, false, false]
        );
        assert!((p.audio_start() - 3.5).abs() < 1e-12);

        let slow = plan_count_in(&c, 3.0, 0.8).unwrap();
        assert!((slow.interval - 0.625).abs() < 1e-12);
        assert!((slow.audio_start() - 7.0 * 0.625).abs() < 1e-12);

        c.count_in_beats = 8;
        let p = plan_count_in(&c, 3.0, 1.0).unwrap();
        assert_eq!(p.accents.iter().filter(|&&a| a).count(), 2);
        assert!(p.accents[0] && p.accents[4]);

        c.count_in_beats = 0;
        assert!(plan_count_in(&c, 3.0, 1.0).is_none());
    }

    #[test]
    fn count_in_leads_into_the_first_beat_after_the_start() {
        // First beat at 0.8 s, then 120 bpm.
        let beats: Vec<f64> = (0..20).map(|i| 0.8 + i as f64 * 0.5).collect();
        let mut c = cfg(beats, vec![], 1);
        c.count_in_beats = 4;
        // From 0: the last click is one beat before 0.8 s of song time, so
        // the audio starts 0.8 s before that beat lands.
        let p = plan_count_in(&c, 0.0, 1.0).unwrap();
        assert_eq!(p.first_click, 0.0);
        assert!((p.audio_start - (2.0 - 0.8)).abs() < 1e-12);
        assert!((p.click_time(3) + p.interval - (p.audio_start + 0.8)).abs() < 1e-12);
        // Slowed down: gaps and spacing scale together.
        let p = plan_count_in(&c, 0.0, 0.5).unwrap();
        assert!((p.audio_start - (4.0 - 1.6)).abs() < 1e-12);
        // Mid-beat start: lead into the next beat.
        let p = plan_count_in(&c, 1.0, 1.0).unwrap();
        assert!((p.audio_start - (2.0 - 0.3)).abs() < 1e-12);
        // On a beat (within 5 ms): unchanged, audio one interval after the last click.
        let p = plan_count_in(&c, 1.302, 1.0).unwrap();
        assert!((p.audio_start - (2.0 - 0.0)).abs() < 1e-12);
        // Long intro: audio starts at once, the clicks come in over it.
        let beats: Vec<f64> = (0..20).map(|i| 5.0 + i as f64 * 0.5).collect();
        let mut c = cfg(beats, vec![], 1);
        c.count_in_beats = 4;
        let p = plan_count_in(&c, 0.0, 1.0).unwrap();
        assert_eq!(p.audio_start, 0.0);
        assert!((p.first_click - 3.0).abs() < 1e-12);
        // Past the last beat: count-in over silence, at the last tempo.
        let p = plan_count_in(&c, 50.0, 1.0).unwrap();
        assert!((p.audio_start - 2.0).abs() < 1e-12 && p.first_click == 0.0);
    }

    #[test]
    fn count_in_uses_local_interval_or_default() {
        let beats = vec![0.0, 0.5, 1.0, 1.6, 2.2];
        assert!((local_beat_interval(&beats, 1.2) - 0.6).abs() < 1e-12);
        assert!((local_beat_interval(&beats, 0.0) - 0.5).abs() < 1e-12);
        assert!((local_beat_interval(&beats, -3.0) - 0.5).abs() < 1e-12); // before grid: first pair
        assert!((local_beat_interval(&beats, 99.0) - 0.6).abs() < 1e-12); // after: last pair
        assert_eq!(local_beat_interval(&[], 1.0), DEFAULT_BEAT_INTERVAL);
        assert_eq!(local_beat_interval(&[1.0], 1.0), DEFAULT_BEAT_INTERVAL);
    }

    #[test]
    fn clicks_accent_downbeats_with_tolerance() {
        let c = cfg(vec![0.0, 0.5, 1.0, 1.5, 2.0], vec![0.0, 2.0005], 1);
        let mut got = vec![];
        clicks_in_range(&c, 0.0, 10.0, |t, k| got.push((t, k)));
        let kinds: Vec<_> = got.iter().map(|g| g.1).collect();
        use ClickKind::*;
        assert_eq!(kinds, vec![Accent, Beat, Beat, Beat, Accent]);
        assert!(!is_downbeat(&[1.0], 1.0015));
    }

    #[test]
    fn subdivisions_evenly_between_beats_and_range_is_half_open() {
        let c = cfg(vec![0.0, 0.6, 1.2], vec![0.0], 3);
        let mut got = vec![];
        clicks_in_range(&c, 0.0, 1.2, |t, k| got.push((t, k)));
        let want = [
            (0.0, ClickKind::Accent),
            (0.2, ClickKind::Sub),
            (0.4, ClickKind::Sub),
            (0.6, ClickKind::Beat),
            (0.8, ClickKind::Sub),
            (1.0, ClickKind::Sub),
        ];
        assert_eq!(got.len(), want.len());
        for (g, w) in got.iter().zip(want) {
            assert!((g.0 - w.0).abs() < 1e-9 && g.1 == w.1, "{got:?}");
        }
        // A range inside one interval still finds its subdivisions.
        let mut got = vec![];
        clicks_in_range(&c, 0.7, 0.9, |t, k| got.push((t, k)));
        assert_eq!(got.len(), 1);
        assert!((got[0].0 - 0.8).abs() < 1e-9);
        // No subdivision after the last beat.
        let mut got = vec![];
        clicks_in_range(&c, 1.1, 5.0, |t, k| got.push((t, k)));
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn click_sounds_are_short_and_accent_is_louder() {
        let b = ClickBank::new(44_100);
        let peak = |s: &[f32]| s.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert_eq!(b.get(ClickKind::Beat).len(), 1323);
        assert!(peak(b.get(ClickKind::Accent)) > peak(b.get(ClickKind::Beat)));
        assert!(peak(b.get(ClickKind::Beat)) > peak(b.get(ClickKind::Sub)));
        assert!(peak(b.get(ClickKind::Accent)) <= 1.0);
    }
}
