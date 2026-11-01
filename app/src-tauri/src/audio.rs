//! Audio engine facade.
//!
//! STUB: `crates/stemx-audio` (real-time engine on cpal + Signalsmith
//! Stretch, offline render, encoders) is being built in parallel. Until it
//! lands, this module provides the same public interface with a silent
//! engine that only tracks the play position with a clock, a renderer that
//! mixes without time-stretch/pitch, and a WAV-only encoder. Once the real
//! crate is in, this whole file becomes `pub use stemx_audio::*;`.

#![allow(dead_code)]

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub const IS_STUB: bool = true;

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("{0}")]
    Msg(String),
    #[error("cancelled")]
    Cancelled,
}

#[derive(Clone)]
pub struct Track {
    pub name: String,
    pub channels: [Arc<Vec<f32>>; 2],
    pub sample_rate: u32,
}

#[derive(Debug, Clone, Default)]
pub struct ClickConfig {
    pub enabled: bool,
    pub beats: Vec<f64>,
    pub downbeats: Vec<f64>,
    pub subdivision: u32,
    pub count_in_beats: u32,
    pub beats_per_bar: u32,
    pub volume: f32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Position {
    pub seconds: f64,
    pub playing: bool,
    pub count_in_beats_left: u32,
}

type OnEnd = Arc<dyn Fn() + Send + Sync>;

struct State {
    duration: f64,
    playing: bool,
    /// Song position at `since`.
    base: f64,
    since: Instant,
    speed: f64,
    looping: Option<(f64, f64)>,
    click: ClickConfig,
    /// Count-in: (start instant, beat spacing in wall seconds, beats).
    count_in: Option<(Instant, f64, u32)>,
}

pub struct Engine {
    st: Mutex<State>,
    on_end: Mutex<Option<OnEnd>>,
}

impl Engine {
    pub fn new() -> Result<Engine, AudioError> {
        Ok(Engine {
            st: Mutex::new(State {
                duration: 0.0,
                playing: false,
                base: 0.0,
                since: Instant::now(),
                speed: 1.0,
                looping: None,
                click: ClickConfig::default(),
                count_in: None,
            }),
            on_end: Mutex::new(None),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Advances the clock; returns true if the end was reached just now.
    fn advance(st: &mut State) -> bool {
        let now = Instant::now();
        if let Some((t0, spacing, n)) = st.count_in {
            let total = spacing * n as f64;
            let el = now.duration_since(t0).as_secs_f64();
            if el < total {
                return false;
            }
            st.count_in = None;
            st.since = t0 + std::time::Duration::from_secs_f64(total);
        }
        if !st.playing {
            return false;
        }
        let el = now.duration_since(st.since).as_secs_f64();
        let mut pos = st.base + el * st.speed;
        if let Some((a, b)) = st.looping {
            if st.base < b && pos >= b && b > a {
                pos = a + (pos - b) % (b - a);
            }
        }
        st.base = pos;
        st.since = now;
        if pos >= st.duration {
            st.base = st.duration;
            st.playing = false;
            return true;
        }
        false
    }

    pub fn load(&self, tracks: Vec<Track>) {
        let mut st = self.lock();
        Self::advance(&mut st);
        st.duration = tracks
            .iter()
            .map(|t| t.channels[0].len() as f64 / t.sample_rate.max(1) as f64)
            .fold(0.0, f64::max);
        st.base = st.base.min(st.duration);
    }

    pub fn play(&self) {
        let mut st = self.lock();
        Self::advance(&mut st);
        if st.playing {
            return;
        }
        if st.base >= st.duration {
            st.base = 0.0;
        }
        st.playing = true;
        st.since = Instant::now();
        let n = st.click.count_in_beats;
        if n > 0 {
            let pos = st.base;
            let spacing = local_interval(&st.click.beats, pos).unwrap_or(0.5) / st.speed;
            st.count_in = Some((Instant::now(), spacing, n));
        }
    }

    pub fn pause(&self) {
        let mut st = self.lock();
        Self::advance(&mut st);
        st.count_in = None;
        st.playing = false;
    }

    pub fn seek(&self, s: f64) {
        let mut st = self.lock();
        Self::advance(&mut st);
        st.base = s.clamp(0.0, st.duration);
        st.since = Instant::now();
    }

    pub fn set_gains(&self, _g: Vec<(String, f32)>) {}

    pub fn set_speed(&self, ratio: f64) {
        let mut st = self.lock();
        Self::advance(&mut st);
        st.speed = ratio.clamp(0.25, 4.0);
    }

    pub fn set_pitch(&self, _semitones: f64) {}

    pub fn set_loop(&self, r: Option<(f64, f64)>) {
        let mut st = self.lock();
        Self::advance(&mut st);
        st.looping = r;
    }

    pub fn set_click(&self, c: ClickConfig) {
        self.lock().click = c;
    }

    pub fn position(&self) -> Position {
        let (pos, ended) = {
            let mut st = self.lock();
            let ended = Self::advance(&mut st);
            let left = st.count_in.map_or(0, |(t0, spacing, n)| {
                let done = (Instant::now().duration_since(t0).as_secs_f64() / spacing) as u32;
                n.saturating_sub(done)
            });
            (
                Position {
                    seconds: st.base,
                    playing: st.playing,
                    count_in_beats_left: left,
                },
                ended,
            )
        };
        if ended {
            let cb = self
                .on_end
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Some(cb) = cb {
                cb();
            }
        }
        pos
    }

    pub fn duration(&self) -> f64 {
        self.lock().duration
    }

    pub fn set_on_end(&self, f: Box<dyn Fn() + Send + Sync>) {
        *self.on_end.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::from(f));
    }
}

fn local_interval(beats: &[f64], pos: f64) -> Option<f64> {
    if beats.len() < 2 {
        return None;
    }
    let i = beats
        .partition_point(|&b| b <= pos)
        .clamp(1, beats.len() - 1);
    Some(beats[i] - beats[i - 1]).filter(|d| *d > 0.0)
}

pub struct RenderRequest {
    pub tracks: Vec<Track>,
    pub gains: Vec<(String, f32)>,
    pub speed: f64,
    pub semitones: f64,
}

/// STUB: mixes the tracks with their gains; speed and pitch are ignored.
pub fn render(
    req: &RenderRequest,
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<[Vec<f32>; 2], AudioError> {
    let len = req
        .tracks
        .iter()
        .map(|t| t.channels[0].len())
        .max()
        .unwrap_or(0);
    let mut out = [vec![0.0f32; len], vec![0.0f32; len]];
    for (i, t) in req.tracks.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(AudioError::Cancelled);
        }
        let g = req
            .gains
            .iter()
            .find(|(n, _)| *n == t.name)
            .map_or(1.0, |(_, g)| *g);
        for (oc, tc) in out.iter_mut().zip(&t.channels) {
            for (o, s) in oc.iter_mut().zip(tc.iter()) {
                *o += s * g;
            }
        }
        progress((i + 1) as f32 / req.tracks.len() as f32);
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExportFormat {
    Wav16,
    Wav24,
    Flac,
    Mp3 { kbps: u32 },
}

/// STUB: WAV only.
pub fn encode(
    path: &Path,
    channels: &[Vec<f32>; 2],
    sample_rate: u32,
    format: ExportFormat,
) -> Result<(), AudioError> {
    let bits = match format {
        ExportFormat::Wav16 => 16,
        ExportFormat::Wav24 => 24,
        _ => {
            return Err(AudioError::Msg(
                "FLAC/MP3 export needs the real audio engine (not built yet)".into(),
            ))
        }
    };
    let err = |e: hound::Error| AudioError::Msg(e.to_string());
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate,
        bits_per_sample: bits,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).map_err(err)?;
    let max = ((1i32 << (bits - 1)) - 1) as f32;
    for i in 0..channels[0].len() {
        for c in channels {
            let v = (c[i].clamp(-1.0, 1.0) * max).round() as i32;
            w.write_sample(v).map_err(err)?;
        }
    }
    w.finalize().map_err(err)
}
