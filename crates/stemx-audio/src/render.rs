//! Offline render for export: the same mix and stretch path as playback,
//! the whole song or a time range (played once), optionally with the click
//! track and a count-in.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::encode::{limiter_gain, CEILING};
use crate::mix::MixMode;
use crate::pipeline::{Meta, Pipeline};
use crate::{AudioError, ClickConfig, Track};
use stemx_dsp::refine::RefineParams;

/// What to render.
#[derive(Debug, Clone)]
pub struct RenderRequest {
    pub tracks: Vec<Track>,
    /// The original mix, sample-aligned with `tracks`, for
    /// [`MixMode::Original`] (same rule and code as playback).
    pub original: Option<Track>,
    pub mode: MixMode,
    /// Linear gain per track name; missing names are silent (and, in
    /// [`MixMode::Original`], subtracted from the original).
    pub gains: Vec<(String, f32)>,
    /// Tempo ratio, 0.5..=1.5 (clamped).
    pub speed: f64,
    /// Pitch shift in semitones (semitones + cents/100), -12.5..=12.5 (clamped).
    pub semitones: f64,
    /// Click track (`enabled`) and count-in (`count_in_beats > 0`), exactly
    /// as in playback. [`ClickConfig::default`] = neither (plain music).
    /// With a count-in and [`ClickConfig::music_start`], a range that starts
    /// at the song start begins at the music start instead
    /// ([`crate::click::effective_start`]).
    pub click: ClickConfig,
    /// Range start, song seconds (0 = the song start).
    pub start: f64,
    /// Range end, song seconds (`None` = the song end).
    pub end: Option<f64>,
    /// Refine parameters per track name (missing = unrefined), as in
    /// playback ([`crate::mix`]).
    pub refine: Vec<(String, RefineParams)>,
}

const BLOCK: usize = 16_384;

/// Fade at cut points inside the song (range edges that aren't the song's
/// real start or end), so the file doesn't start or stop with a click.
pub const EDGE_FADE_SECS: f64 = 0.008;

/// The range [`render`] really renders for `req`, in song seconds:
/// `(start, end)` after clamping to `[0, duration]` and moving the start to
/// the music start if that applies. `None` if the range is empty.
pub fn render_range(req: &RenderRequest, duration: f64) -> Option<(f64, f64)> {
    let end = req
        .end
        .filter(|e| e.is_finite())
        .map_or(duration, |e| e.clamp(0.0, duration));
    let start = if req.start.is_finite() {
        req.start.clamp(0.0, duration)
    } else {
        0.0
    };
    if end - start < 0.01 {
        return None;
    }
    Some((crate::click::effective_start(&req.click, start, end), end))
}

/// Renders the range ([`render_range`]; the whole song by default) at the
/// tracks' sample rate. Output length is `round(range / speed)` (exactly the
/// range's frames without stretching), plus the count-in if `req.click` has
/// one; the count-in leads into the first beat at or after the range start.
/// Range edges inside the song get an [`EDGE_FADE_SECS`] fade. `progress`
/// gets 0..=1 (at most ~100 calls); `cancel` is polled between blocks.
///
/// Without click and count-in the output is the raw mix (the encoder's
/// limiter takes care of peaks). With them, it mirrors playback, where
/// clicks are added after the music's safety limiter: the music alone goes
/// through the export peak limiter ([`limiter_gain`], -0.1 dBFS), then the
/// clicks are added unlimited. Music + click can still exceed full scale;
/// [`crate::encode`] limits that final sum, so the file never clips.
pub fn render(
    req: &RenderRequest,
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<[Vec<f32>; 2], AudioError> {
    if req.tracks.is_empty() && req.original.is_none() {
        return Err(AudioError::Invalid("no tracks to render".into()));
    }
    let with_click = req.click.enabled || req.click.count_in_beats > 0;
    let mut p = Pipeline::new(Vec::new(), &req.gains, false);
    p.set_mix(req.gains.clone(), req.mode);
    p.set_refine_all(req.refine.clone());
    p.load_with_original(req.tracks.clone(), req.original.clone());
    p.set_speed(req.speed);
    p.set_pitch(req.semitones);
    let duration = p.duration();
    if duration <= 0.0 {
        return Err(AudioError::Invalid("tracks are empty".into()));
    }
    let (start, end) =
        render_range(req, duration).ok_or(AudioError::Invalid("empty export range".into()))?;
    if with_click {
        // The start is resolved above (against the range end); the pipeline
        // must not move it again.
        p.set_click(ClickConfig {
            music_start: None,
            ..req.click.clone()
        });
        p.set_click_track(req.click.enabled);
    }
    let cut_start = start > 0.0;
    let cut_end = end < duration;
    p.seek(start);
    let sr = p.sample_rate();
    let speed = req.speed.clamp(0.5, 1.5);
    let count_in =
        crate::click::plan_count_in(&req.click, start, speed).map_or(0.0, |c| c.audio_start());
    let expect = (((end - start) / speed + count_in) * sr as f64).round() as usize;
    let mut out = [
        Vec::with_capacity(expect + BLOCK),
        Vec::with_capacity(expect + BLOCK),
    ];
    let mut clicks: Vec<f32> = Vec::with_capacity(if with_click { expect + BLOCK } else { 0 });
    let mut l = vec![0.0f32; BLOCK];
    let mut r = vec![0.0f32; BLOCK];
    let mut c = vec![0.0f32; if with_click { BLOCK } else { 0 }];
    let mut meta = vec![Meta::default(); BLOCK];
    p.play();
    progress(0.0);
    let mut last_report = 0.0f32;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(AudioError::Cancelled);
        }
        if with_click {
            p.render_split(&mut l, &mut r, &mut c, &mut meta);
        } else {
            p.render(&mut l, &mut r, &mut meta);
        }
        let n = meta
            .iter()
            .position(|m| !m.playing() || (cut_end && m.pos >= end))
            .unwrap_or(BLOCK);
        out[0].extend_from_slice(&l[..n]);
        out[1].extend_from_slice(&r[..n]);
        if with_click {
            clicks.extend_from_slice(&c[..n]);
        }
        if n < BLOCK {
            break;
        }
        let f = ((meta[n - 1].pos - start) / (end - start)) as f32;
        if f - last_report >= 0.01 {
            progress(f.clamp(0.0, 1.0));
            last_report = f;
        }
    }
    let fade = ((EDGE_FADE_SECS * sr as f64).round() as usize).max(1);
    if cut_start {
        // The music starts after the count-in's lead (silence before it).
        let a0 = ((count_in * sr as f64).round() as usize).min(out[0].len());
        for ch in out.iter_mut() {
            fade_in(&mut ch[a0..], fade);
        }
    }
    if cut_end {
        for ch in out.iter_mut() {
            fade_out(ch, fade);
        }
        fade_out(&mut clicks, fade);
    }
    if with_click {
        add_clicks_after_limiter(&mut out, &clicks, sr);
    }
    progress(1.0);
    Ok(out)
}

/// Linear fade-in over the first `len` samples.
fn fade_in(x: &mut [f32], len: usize) {
    let n = len.min(x.len());
    for (i, v) in x[..n].iter_mut().enumerate() {
        *v *= i as f32 / len as f32;
    }
}

/// Linear fade-out over the last `len` samples (the last one is silent).
fn fade_out(x: &mut [f32], len: usize) {
    let n = len.min(x.len());
    let from = x.len() - n;
    for (i, v) in x[from..].iter_mut().enumerate() {
        *v *= (n - 1 - i) as f32 / len as f32;
    }
}

/// Limits the music to [`CEILING`] (as the encoder would), then adds the
/// mono click track to both channels, like playback adds clicks after its
/// safety limiter.
fn add_clicks_after_limiter(music: &mut [Vec<f32>; 2], clicks: &[f32], sample_rate: u32) {
    if let Some(g) = limiter_gain(music, sample_rate, CEILING) {
        for ch in music.iter_mut() {
            for (x, g) in ch.iter_mut().zip(&g) {
                *x *= g;
            }
        }
    }
    for ch in music.iter_mut() {
        for (x, c) in ch.iter_mut().zip(clicks) {
            *x += c;
        }
    }
}

#[cfg(test)]
#[allow(clippy::needless_range_loop)]
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
            original: None,
            mode: MixMode::Stems,
            gains: vec![("bass".into(), 1.0)],
            speed: 1.0,
            semitones: 0.0,
            click: ClickConfig::default(),
            start: 0.0,
            end: None,
            refine: vec![],
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
                original: None,
                mode: MixMode::Stems,
                gains: vec![("mix".into(), 1.0)],
                speed,
                semitones: semis,
                click: ClickConfig::default(),
                start: 0.0,
                end: None,
                refine: vec![],
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
    fn original_mode_render_matches_rule() {
        // "All but drums" in original mode = original - drums, bit-exact
        // when unstretched.
        let bass = sine_track("bass", 110.0, 1.0);
        let drums = sine_track("drums", 1000.0, 1.0);
        let n = bass.channels[0].len();
        let o: Vec<f32> = (0..n)
            .map(|i| bass.channels[0][i] + drums.channels[0][i] + 0.01)
            .collect();
        let orig = Track {
            name: "original".into(),
            channels: [Arc::new(o.clone()), Arc::new(o.clone())],
            sample_rate: 44_100,
        };
        let req = RenderRequest {
            tracks: vec![bass.clone(), drums.clone()],
            original: Some(orig),
            mode: crate::mix::choose_mode(2, 1, false),
            gains: vec![("bass".into(), 1.0)],
            speed: 1.0,
            semitones: 0.0,
            click: ClickConfig::default(),
            start: 0.0,
            end: None,
            refine: vec![],
        };
        let out = render(&req, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        for i in 0..n {
            assert_eq!(out[0][i], o[i] - drums.channels[0][i]);
        }
    }

    #[test]
    fn cancel_stops_render() {
        let req = RenderRequest {
            tracks: vec![sine_track("mix", 220.0, 1.0)],
            original: None,
            mode: MixMode::Stems,
            gains: vec![],
            speed: 0.9,
            semitones: 0.0,
            click: ClickConfig::default(),
            start: 0.0,
            end: None,
            refine: vec![],
        };
        let r = render(&req, &mut |_| {}, &AtomicBool::new(true));
        assert!(matches!(r, Err(AudioError::Cancelled)));
    }

    fn click_cfg(beats: Vec<f64>, downbeats: Vec<f64>) -> ClickConfig {
        ClickConfig {
            enabled: true,
            beats,
            downbeats,
            subdivision: 1,
            count_in_beats: 0,
            beats_per_bar: 4,
            volume: 0.8,
            music_start: None,
        }
    }

    /// Rising edges after at least `gap` quiet samples.
    fn onsets(x: &[f32], gap: usize) -> Vec<usize> {
        let mut out = vec![];
        let mut quiet = gap + 1;
        for (i, &v) in x.iter().enumerate() {
            if v.abs() > 1e-4 {
                if quiet > gap {
                    out.push(i);
                }
                quiet = 0;
            } else {
                quiet += 1;
            }
        }
        out
    }

    fn peak_at(x: &[f32], i: usize) -> f32 {
        x[i..(i + 1000).min(x.len())]
            .iter()
            .fold(0.0f32, |m, v| m.max(v.abs()))
    }

    #[test]
    fn click_off_render_is_unchanged() {
        // Beats present but click and count-in off: bit-identical to no click.
        let a = sine_track("mix", 220.0, 1.0);
        let base = RenderRequest {
            tracks: vec![a],
            original: None,
            mode: MixMode::Stems,
            gains: vec![("mix".into(), 1.0)],
            speed: 0.9,
            semitones: 1.0,
            click: ClickConfig::default(),
            start: 0.0,
            end: None,
            refine: vec![],
        };
        let mut off = click_cfg(vec![0.0, 0.5], vec![0.0]);
        off.enabled = false;
        let with_cfg = RenderRequest {
            click: off,
            ..base.clone()
        };
        let none = AtomicBool::new(false);
        let x = render(&base, &mut |_| {}, &none).unwrap();
        let y = render(&with_cfg, &mut |_| {}, &none).unwrap();
        assert_eq!(x, y);
    }

    #[test]
    fn click_lands_on_beats_at_export_speed() {
        // Silent music so only the clicks remain; speed 0.8 stretches.
        let beats = vec![0.5, 1.0, 1.5, 2.0, 2.5, 3.0];
        let req = RenderRequest {
            tracks: vec![sine_track("mix", 220.0, 4.0)],
            original: None,
            mode: MixMode::Stems,
            gains: vec![],
            speed: 0.8,
            semitones: 2.0,
            click: click_cfg(beats.clone(), vec![0.5, 2.5]),
            start: 0.0,
            end: None,
            refine: vec![],
        };
        let out = render(&req, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        assert!(out[0].len().abs_diff((4.0 * 44_100.0 / 0.8) as usize) <= 2);
        assert_eq!(out[0], out[1]);
        let on = onsets(&out[0], 2000);
        assert_eq!(on.len(), beats.len(), "{on:?}");
        for (&o, &b) in on.iter().zip(&beats) {
            let want = (b * 44_100.0 / 0.8).round() as usize;
            assert!(o.abs_diff(want) <= 2, "onset {o}, want {want}: {on:?}");
        }
        // Downbeats (0.5 and 2.5 s) are accented: louder, at the click volume.
        let p: Vec<f32> = on.iter().map(|&o| peak_at(&out[0], o)).collect();
        assert!(p[0] > p[1] * 1.3 && p[4] > p[3] * 1.3, "{p:?}");
        let bank = crate::click::ClickBank::new(44_100);
        let accent = peak_at(bank.get(crate::click::ClickKind::Accent), 0);
        assert!((p[0] - 0.8 * accent).abs() < 0.01, "{p:?}");
    }

    #[test]
    fn count_in_is_prepended_in_silence_before_the_song() {
        let speed = 0.8;
        let beats: Vec<f64> = (0..8).map(|i| i as f64 * 0.5).collect(); // 120 bpm
        let base = RenderRequest {
            tracks: vec![sine_track("mix", 220.0, 2.0)],
            original: None,
            mode: MixMode::Stems,
            gains: vec![("mix".into(), 1.0)],
            speed,
            semitones: 0.0,
            click: ClickConfig::default(),
            start: 0.0,
            end: None,
            refine: vec![],
        };
        let mut c = click_cfg(beats, vec![0.0, 2.0]);
        c.enabled = false;
        c.count_in_beats = 4;
        c.beats_per_bar = 4;
        let req = RenderRequest {
            click: c,
            ..base.clone()
        };
        let none = AtomicBool::new(false);
        let plain = render(&base, &mut |_| {}, &none).unwrap();
        let out = render(&req, &mut |_| {}, &none).unwrap();
        // 4 clicks at the song-start tempo (0.5 s) / speed, music right after.
        let interval = 0.5 / speed * 44_100.0;
        let ci = out[0].len() - plain[0].len();
        assert!(ci.abs_diff((4.0 * interval).round() as usize) <= 1, "{ci}");
        let on = onsets(&out[0][..ci], 2000);
        assert_eq!(on.len(), 4, "{on:?}");
        for (j, &o) in on.iter().enumerate() {
            assert!(
                o.abs_diff((j as f64 * interval).round() as usize) <= 1,
                "{on:?}"
            );
        }
        let interval = interval as usize;
        // Click "4" (a full bar before the start) is accented.
        let p: Vec<f32> = on.iter().map(|&o| peak_at(&out[0], o)).collect();
        assert!(p[0] > p[1] * 1.3, "{p:?}");
        // No music under the count-in: silent between the clicks.
        let snd = (0.030 * 44_100.0f64).round() as usize;
        assert!(out[0][snd + 10..interval - 10].iter().all(|&x| x == 0.0));
        // After it: the same music as without the count-in (the stretcher
        // runs in different block phases, so not bit-identical when stretching).
        let diff: Vec<f32> = out[0][ci..]
            .iter()
            .zip(&plain[0])
            .map(|(a, b)| a - b)
            .collect();
        let rel = rms(&diff) / rms(&plain[0]);
        assert!(rel < 0.05, "music after the count-in differs: {rel}");

        // Unstretched: bit-identical music after the count-in.
        let base1 = RenderRequest { speed: 1.0, ..base };
        let req1 = RenderRequest { speed: 1.0, ..req };
        let plain = render(&base1, &mut |_| {}, &none).unwrap();
        let out = render(&req1, &mut |_| {}, &none).unwrap();
        let ci = out[0].len() - plain[0].len();
        assert_eq!(ci, 4 * 22_050);
        assert_eq!(&out[0][ci..], &plain[0][..]);
        assert_eq!(&out[1][ci..], &plain[1][..]);
    }

    fn range_req(secs: f32, start: f64, end: Option<f64>) -> RenderRequest {
        RenderRequest {
            tracks: vec![sine_track("mix", 220.0, secs)],
            original: None,
            mode: MixMode::Stems,
            gains: vec![("mix".into(), 1.0)],
            speed: 1.0,
            semitones: 0.0,
            click: ClickConfig::default(),
            start,
            end,
            refine: vec![],
        }
    }

    #[test]
    fn range_render_has_exact_length_and_faded_edges() {
        let req = range_req(2.0, 0.5, Some(1.25));
        let src = req.tracks[0].channels[0].clone();
        let out = render(&req, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        let (a, b) = (22_050usize, 55_125usize);
        assert_eq!(out[0].len(), b - a);
        let fade = (EDGE_FADE_SECS * 44_100.0).round() as usize;
        // Inside the fades: the source, bit for bit.
        assert_eq!(&out[0][fade..out[0].len() - fade], &src[a + fade..b - fade]);
        // Fades: silent at the very edges, ramping in between.
        assert_eq!(out[0][0], 0.0);
        assert_eq!(*out[0].last().unwrap(), 0.0);
        for i in 0..fade {
            assert!(out[0][i].abs() <= src[a + i].abs() + 1e-7);
        }
        // The song's real start and end are not faded.
        let whole = range_req(2.0, 0.0, Some(2.0));
        let out = render(&whole, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!(out[0], *src);
        let tail = range_req(2.0, 1.0, None);
        let out = render(&tail, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!(out[0].len(), 44_100);
        assert_eq!(out[0][44_099], src[88_199]);
        // Empty range: error.
        let bad = range_req(2.0, 1.0, Some(1.0));
        assert!(render(&bad, &mut |_| {}, &AtomicBool::new(false)).is_err());
    }

    #[test]
    fn stretched_range_has_scaled_length() {
        let mut req = range_req(3.0, 1.0, Some(2.0));
        req.speed = 0.8;
        let out = render(&req, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        let want = (44_100.0 / 0.8f64).round() as usize;
        assert!(
            out[0].len().abs_diff(want) <= 2,
            "{} vs {want}",
            out[0].len()
        );
        let mid = &out[0][want / 4..want * 3 / 4];
        assert!((rms(mid) / rms(&req.tracks[0].channels[0]) - 1.0).abs() < 0.15);
    }

    #[test]
    fn count_in_and_click_follow_the_range_start() {
        // 120 bpm grid; range starts mid-beat at 1.2 s, so the count-in
        // leads into the beat at 1.5 s and the 0.3 s pickup plays under it.
        let beats: Vec<f64> = (0..8).map(|i| i as f64 * 0.5).collect();
        let mut c = click_cfg(beats.clone(), vec![0.0, 2.0]);
        c.enabled = false;
        c.count_in_beats = 4;
        let mut req = range_req(4.0, 1.2, Some(2.6));
        req.click = c.clone();
        let out = render(&req, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        let lead = 2.0 - 0.3;
        let want = ((2.6 - 1.2 + lead) * 44_100.0f64).round() as usize;
        assert!(
            out[0].len().abs_diff(want) <= 1,
            "{} vs {want}",
            out[0].len()
        );
        let a0 = (lead * 44_100.0f64).round() as usize;
        // Silence before the music, except the count-in clicks.
        let on = onsets(&out[0][..a0], 2000);
        assert_eq!(on.len(), 4, "{on:?}");
        for (j, &o) in on.iter().enumerate() {
            assert!(o.abs_diff(j * 22_050) <= 2, "{on:?}");
        }
        assert!(out[0][on[3] + 1400..a0].iter().all(|&x| x == 0.0));
        // The music fades in at the cut.
        assert!(out[0][a0].abs() < 1e-6 && peak_at(&out[0], a0 + 400) > 0.3);

        // Click track: only the beats inside the range (silent music).
        let mut c = click_cfg(beats, vec![0.0, 2.0]);
        c.count_in_beats = 0;
        let mut req = range_req(4.0, 1.0, Some(2.0));
        req.gains = vec![];
        req.click = c;
        let out = render(&req, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!(out[0].len(), 44_100);
        let on = onsets(&out[0], 2000);
        assert_eq!(on.len(), 2, "{on:?}");
        assert!(on[0] <= 2 && on[1].abs_diff(22_050) <= 2, "{on:?}");
    }

    #[test]
    fn whole_song_export_with_count_in_starts_at_the_music_start() {
        let beats: Vec<f64> = (0..12).map(|i| 1.0 + i as f64 * 0.5).collect();
        let mut c = click_cfg(beats, vec![]);
        c.enabled = false;
        c.count_in_beats = 4;
        c.music_start = Some(2.0);
        let mut req = range_req(4.0, 0.0, None);
        req.click = c;
        assert_eq!(render_range(&req, 4.0), Some((2.0, 4.0)));
        let out = render(&req, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!(out[0].len(), 4 * 22_050 + 2 * 44_100);
        // The music after the count-in is the song from 2.0 s (faded in).
        let src = &req.tracks[0].channels[0];
        let fade = (EDGE_FADE_SECS * 44_100.0).round() as usize;
        assert_eq!(&out[0][4 * 22_050 + fade..], &src[88_200 + fade..]);
        // A loop range ending before the music start: no skip.
        req.end = Some(1.5);
        assert_eq!(render_range(&req, 4.0), Some((0.0, 1.5)));
        // Count-in off: the whole song, unchanged.
        req.end = None;
        req.click.count_in_beats = 0;
        assert_eq!(render_range(&req, 4.0), Some((0.0, 4.0)));
    }

    #[test]
    fn click_is_added_after_the_music_limiter_and_the_file_cannot_clip() {
        // Music far above full scale (+6 dB on a 0.8 sine) plus clicks.
        let n = 2 * 44_100;
        let v: Vec<f32> = (0..n)
            .map(|i| 0.8 * (2.0 * std::f32::consts::PI * 100.0 * i as f32 / 44_100.0).sin())
            .collect();
        let t = Track {
            name: "mix".into(),
            channels: [Arc::new(v.clone()), Arc::new(v)],
            sample_rate: 44_100,
        };
        let mut c = click_cfg(vec![0.25, 0.75, 1.25, 1.75], vec![0.25]);
        c.volume = 1.0;
        let base = RenderRequest {
            tracks: vec![t],
            original: None,
            mode: MixMode::Stems,
            gains: vec![("mix".into(), 2.0)],
            speed: 1.0,
            semitones: 0.0,
            click: ClickConfig::default(),
            start: 0.0,
            end: None,
            refine: vec![],
        };
        let req = RenderRequest {
            click: c,
            ..base.clone()
        };
        let none = AtomicBool::new(false);
        let mut music = render(&base, &mut |_| {}, &none).unwrap();
        let out = render(&req, &mut |_| {}, &none).unwrap();
        // out = limited music + full-level clicks (the click is not ducked).
        let g = limiter_gain(&music, 44_100, CEILING).unwrap();
        for ch in music.iter_mut() {
            for (x, g) in ch.iter_mut().zip(&g) {
                *x *= g;
            }
        }
        let click: Vec<f32> = out[0].iter().zip(&music[0]).map(|(a, b)| a - b).collect();
        let got = peak_at(&click, (0.25 * 44_100.0) as usize);
        let bank = crate::click::ClickBank::new(44_100);
        let want = peak_at(bank.get(crate::click::ClickKind::Accent), 0);
        assert!((got - want).abs() < 1e-3, "accent peak {got}, want {want}");
        assert!(crate::peak(&out) > 1.0, "test needs a sum above full scale");
        // The encoder limits the final sum: the file stays below full scale.
        let dir = std::env::temp_dir().join(format!("stemx-render-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("click.wav");
        crate::encode(&path, &out, 44_100, crate::ExportFormat::Wav24).unwrap();
        let mut rd = hound::WavReader::open(&path).unwrap();
        let max = rd
            .samples::<i32>()
            .map(|s| s.unwrap().unsigned_abs())
            .max()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(max as f32 / 8_388_607.0 <= CEILING + 1e-6, "peak {max}");
    }
}
