//! Offline render for export: the same mix and stretch path as playback,
//! whole song, no click, no loop.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::pipeline::{Meta, Pipeline};
use crate::{AudioError, Track};

/// What to render.
#[derive(Debug, Clone)]
pub struct RenderRequest {
    pub tracks: Vec<Track>,
    /// Linear gain per track name; missing names are silent.
    pub gains: Vec<(String, f32)>,
    /// Tempo ratio, 0.5..=1.5 (clamped).
    pub speed: f64,
    /// Pitch shift in semitones (semitones + cents/100), -12.5..=12.5 (clamped).
    pub semitones: f64,
}

const BLOCK: usize = 16_384;

/// Renders the whole song at the tracks' sample rate. Output length is
/// `round(len / speed)` (exactly `len` without stretching). `progress` gets
/// 0..=1 (at most ~100 calls); `cancel` is polled between blocks.
pub fn render(
    req: &RenderRequest,
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<[Vec<f32>; 2], AudioError> {
    if req.tracks.is_empty() {
        return Err(AudioError::Invalid("no tracks to render".into()));
    }
    let mut p = Pipeline::new(req.tracks.clone(), &req.gains, false);
    p.set_speed(req.speed);
    p.set_pitch(req.semitones);
    let duration = p.duration();
    if duration <= 0.0 {
        return Err(AudioError::Invalid("tracks are empty".into()));
    }
    let speed = req.speed.clamp(0.5, 1.5);
    let expect = (duration * p.sample_rate() as f64 / speed).round() as usize;
    let mut out = [
        Vec::with_capacity(expect + BLOCK),
        Vec::with_capacity(expect + BLOCK),
    ];
    let mut l = vec![0.0f32; BLOCK];
    let mut r = vec![0.0f32; BLOCK];
    let mut meta = vec![Meta::default(); BLOCK];
    p.play();
    progress(0.0);
    let mut last_report = 0.0f32;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(AudioError::Cancelled);
        }
        p.render(&mut l, &mut r, &mut meta);
        let n = meta.iter().position(|m| !m.playing()).unwrap_or(BLOCK);
        out[0].extend_from_slice(&l[..n]);
        out[1].extend_from_slice(&r[..n]);
        if n < BLOCK {
            break;
        }
        let f = (meta[n - 1].pos / duration) as f32;
        if f - last_report >= 0.01 {
            progress(f.min(1.0));
            last_report = f;
        }
    }
    progress(1.0);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn sine_track(name: &str, freq: f32, secs: f32) -> Track {
        let n = (secs * 44_100.0) as usize;
        let v: Vec<f32> = (0..n)
            .map(|i| 0.4 * (2.0 * std::f32::consts::PI * freq * i as f32 / 44_100.0).sin())
            .collect();
        Track {
            name: name.into(),
            channels: [Arc::new(v.clone()), Arc::new(v)],
            sample_rate: 44_100,
        }
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn unity_render_is_identity_and_gains_select_tracks() {
        let a = sine_track("bass", 110.0, 1.0);
        let b = sine_track("drums", 1000.0, 1.0);
        let req = RenderRequest {
            tracks: vec![a.clone(), b],
            gains: vec![("bass".into(), 1.0)],
            speed: 1.0,
            semitones: 0.0,
        };
        let mut calls = 0;
        let out = render(&req, &mut |_| calls += 1, &AtomicBool::new(false)).unwrap();
        assert_eq!(out[0], *a.channels[0]);
        assert_eq!(out[1], *a.channels[1]);
        assert!(calls >= 2);
    }

    #[test]
    fn stretched_render_has_scaled_duration_and_similar_level() {
        let a = sine_track("mix", 220.0, 2.0);
        for (speed, semis) in [(0.8, 0.0), (1.25, -2.0), (1.0, 3.0)] {
            let req = RenderRequest {
                tracks: vec![a.clone()],
                gains: vec![("mix".into(), 1.0)],
                speed,
                semitones: semis,
            };
            let out = render(&req, &mut |_| {}, &AtomicBool::new(false)).unwrap();
            let want = (88_200.0 / speed).round() as usize;
            assert!(
                out[0].len().abs_diff(want) <= 2,
                "{} vs {want}",
                out[0].len()
            );
            let mid = &out[0][want / 4..want * 3 / 4];
            let r = rms(mid);
            assert!((r / rms(&a.channels[0]) - 1.0).abs() < 0.15, "rms {r}");
        }
    }

    #[test]
    fn cancel_stops_render() {
        let req = RenderRequest {
            tracks: vec![sine_track("mix", 220.0, 1.0)],
            gains: vec![],
            speed: 0.9,
            semitones: 0.0,
        };
        let r = render(&req, &mut |_| {}, &AtomicBool::new(true));
        assert!(matches!(r, Err(AudioError::Cancelled)));
    }
}
