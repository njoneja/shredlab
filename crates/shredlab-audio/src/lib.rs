// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Real-time audio engine and offline export renderer for ShredLab.
//!
//! - [`Engine`]: plays a set of [`Track`]s (stems or the original mix) on the
//!   default output device with per-track gains, tempo change without pitch
//!   change, pitch shift, A–B loop, click track and count-in.
//! - [`render`] + [`encode`]: offline "export what you hear" (same mix and
//!   stretch path, the whole song or a time range; click track and count-in
//!   optional) to WAV, FLAC or MP3.
//!
//! # Architecture
//!
//! A producer thread owns the [`pipeline`] (mix → Signalsmith Stretch →
//! crossfades → safety limiter → click → master volume → resample to the
//! device rate) and keeps a small lock-free ring buffer ([`rtrb`]) about
//! 30 ms ahead of the device. The cpal callback only pops frames from the
//! ring and publishes the position through atomics: no locks or allocation
//! in the callback. Controls go to the producer thread over a channel;
//! gain, speed and pitch changes are heard within roughly the ring latency
//! plus the device buffer (< 60 ms).
//!
//! Speed 1.0 with pitch 0 bypasses the stretcher (bit-transparent at unity
//! gain). Playback runs the music through a soft-knee look-ahead safety
//! limiter ([`limiter`], transparent below -1 dBFS) before the click is
//! added; export uses its own whole-file limiter instead (on the music
//! before the click is added, then on the final sum). The mixer can
//! build the backing track from the original mix ([`mix`]). Each stem
//! first runs through its Refine chain (stereo selection + EQ, at the song
//! rate, fed ahead by the chain's latency so nothing else is delayed); then
//! the gained tracks are mixed and the stereo mix is stretched once. So
//! refine sits before the stretcher, inside the mixer, next to the
//! original-mix path. Seeks, loop wraps, pause and stretch on/off are
//! crossfaded (~10 ms).

pub mod click;
mod encode;
mod engine;
pub mod limiter;
pub mod mix;
pub mod pipeline;
mod render;
pub mod stretch;

use std::sync::Arc;

pub use encode::{encode, limiter_gain, peak, ExportFormat};
pub use engine::{Engine, EngineStats};
pub use mix::{choose_mode, MixMode};
pub use render::{render, render_range, RenderRequest, EDGE_FADE_SECS};
/// Refine DSP types used in this crate's API (parameters, field size).
pub use shredlab_dsp::refine::{RefineParams, FIELD_LEN};

/// Version of this crate, from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Crate name, from `Cargo.toml`.
pub const NAME: &str = env!("CARGO_PKG_NAME");

/// One playable track: planar stereo. All tracks given together must have the
/// same length and sample rate (44.1 kHz in practice).
#[derive(Debug, Clone)]
pub struct Track {
    pub name: String,
    pub channels: [Channel; 2],
    pub sample_rate: u32,
}

impl Track {
    /// A track holding `channels` as f32.
    pub fn new(name: impl Into<String>, channels: [Vec<f32>; 2], sample_rate: u32) -> Self {
        let [l, r] = channels;
        Self {
            name: name.into(),
            channels: [l.into(), r.into()],
            sample_rate,
        }
    }

    /// A track holding `channels` as 16-bit integers, half the memory of
    /// f32. Both channels are divided by `scale = max(peak, 1)` (NaN and
    /// infinity count as 0), so loud stems don't clip, and rounded with the
    /// same arithmetic as the library's FLAC stem cache: a stem read back
    /// from a FLAC cache quantizes to exactly the integers in the file, and
    /// a fresh stem to the integers its FLAC cache would hold. The rounding
    /// noise sits about 100 dB below the stem's peak.
    pub fn quantized(name: impl Into<String>, channels: [&[f32]; 2], sample_rate: u32) -> Self {
        let peak = channels
            .iter()
            .flat_map(|c| c.iter())
            .fold(
                0.0f32,
                |m, &v| if v.is_finite() { m.max(v.abs()) } else { m },
            );
        // The FLAC writer's widened scale: the peak maps to 32767.
        let scale = peak.max(1.0) * (32768.0 / 32767.0);
        let q = |c: &[f32]| -> Vec<i16> {
            c.iter()
                .map(|&v| {
                    let v = if v.is_finite() { v / scale } else { 0.0 };
                    (v * 32768.0).round().clamp(-32768.0, 32767.0) as i16
                })
                .collect()
        };
        let step = scale / 32768.0;
        Self {
            name: name.into(),
            channels: channels.map(|c| Channel::I16 {
                data: Arc::new(q(c)),
                step,
            }),
            sample_rate,
        }
    }

    /// Frames (the shorter channel).
    pub fn len(&self) -> usize {
        self.channels[0].len().min(self.channels[1].len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One channel of a [`Track`]: f32 samples, or 16-bit integers times `step`
/// (see [`Track::quantized`]). Cloning shares the samples.
#[derive(Debug, Clone)]
pub enum Channel {
    F32(Arc<Vec<f32>>),
    I16 { data: Arc<Vec<i16>>, step: f32 },
}

impl Channel {
    pub fn len(&self) -> usize {
        match self {
            Channel::F32(v) => v.len(),
            Channel::I16 { data, .. } => data.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Sample `i` as f32. Panics if out of range.
    pub fn get(&self, i: usize) -> f32 {
        match self {
            Channel::F32(v) => v[i],
            Channel::I16 { data, step } => data[i] as f32 * step,
        }
    }

    /// Writes samples `start..start + out.len()` to `out`. Panics if out of
    /// range.
    pub fn read(&self, start: usize, out: &mut [f32]) {
        let end = start + out.len();
        match self {
            Channel::F32(v) => out.copy_from_slice(&v[start..end]),
            Channel::I16 { data, step } => {
                for (o, &s) in out.iter_mut().zip(&data[start..end]) {
                    *o = s as f32 * step;
                }
            }
        }
    }

    /// The samples as f32, without copying when they are stored that way.
    pub fn as_f32(&self) -> Option<&[f32]> {
        match self {
            Channel::F32(v) => Some(v),
            Channel::I16 { .. } => None,
        }
    }

    /// All samples as a new f32 vector.
    pub fn to_vec(&self) -> Vec<f32> {
        let mut v = vec![0.0; self.len()];
        self.read(0, &mut v);
        v
    }
}

impl From<Vec<f32>> for Channel {
    fn from(v: Vec<f32>) -> Self {
        Channel::F32(Arc::new(v))
    }
}

impl From<Arc<Vec<f32>>> for Channel {
    fn from(v: Arc<Vec<f32>>) -> Self {
        Channel::F32(v)
    }
}

/// Click track and count-in settings.
#[derive(Debug, Clone, PartialEq)]
pub struct ClickConfig {
    /// Click on every beat during playback.
    pub enabled: bool,
    /// Beat times, seconds, song time, ascending.
    pub beats: Vec<f64>,
    /// Effective downbeats (seconds); a beat is accented if a downbeat lies within 1 ms.
    pub downbeats: Vec<f64>,
    /// 1 = beats only; 2 or 3 = extra softer clicks evenly between beats.
    pub subdivision: u32,
    /// Count-in clicks before playback starts (0 = off).
    pub count_in_beats: u32,
    /// For count-in accents: click `k` (beats before the start) is accented iff `k % beats_per_bar == 0`.
    pub beats_per_bar: u32,
    /// Linear click gain (0..1).
    pub volume: f32,
    /// Where the music really starts (song seconds), when the song opens
    /// with a quiet intro (e.g. studio talk) that the count-in should skip.
    /// With a count-in, a start at the song start jumps here (see
    /// [`click::effective_start`]). `None` = never skip. The crate does not
    /// detect this itself; the caller decides.
    pub music_start: Option<f64>,
    /// With a loop and a count-in: each time playback reaches the loop end
    /// it stops there and counts in again before repeating from the loop
    /// start, instead of jumping straight back.
    pub count_in_each_repeat: bool,
}

impl Default for ClickConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            beats: Vec::new(),
            downbeats: Vec::new(),
            subdivision: 1,
            count_in_beats: 0,
            beats_per_bar: 4,
            volume: 0.8,
            music_start: None,
            count_in_each_repeat: false,
        }
    }
}

/// Playback position as heard at the output.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Position {
    /// Song time in seconds (independent of speed).
    pub seconds: f64,
    /// True while playing, including during a count-in.
    pub playing: bool,
    /// Count-in clicks still to come (0 when not counting in).
    pub count_in_beats_left: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("audio output device: {0}")]
    Device(String),
    #[error("cannot write {path}: {msg}")]
    Write { path: String, msg: String },
    #[error("encoder: {0}")]
    Encode(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("canceled")]
    Canceled,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_set() {
        assert_eq!(NAME, "shredlab-audio");
        assert!(!VERSION.is_empty());
    }

    fn ints(t: &Track) -> [Vec<i16>; 2] {
        t.channels.clone().map(|c| match c {
            Channel::I16 { data, .. } => data.to_vec(),
            Channel::F32(_) => panic!("not quantized"),
        })
    }

    #[test]
    fn quantized_is_within_half_a_step_and_never_clips() {
        let l: Vec<f32> = (0..5000).map(|i| 1.6 * (i as f32 * 0.013).sin()).collect();
        let r: Vec<f32> = (0..5000).map(|i| 0.3 * (i as f32 * 0.007).cos()).collect();
        let t = Track::quantized("x", [&l, &r], 44_100);
        let Channel::I16 { step, .. } = t.channels[0] else {
            panic!()
        };
        assert!((step - 1.6 / 32767.0).abs() < 1e-9);
        for (c, src) in [&l, &r].into_iter().enumerate() {
            for (i, &v) in src.iter().enumerate() {
                assert!((t.channels[c].get(i) - v).abs() <= step * 0.5 + 1e-6);
            }
        }
        assert_eq!(
            ints(&t)[0].iter().map(|v| v.unsigned_abs()).max(),
            Some(32767)
        );
        // A quiet track keeps scale 1.
        let q = Track::quantized("q", [&[0.25], &[f32::NAN]], 44_100);
        assert_eq!(ints(&q), [vec![8192], vec![0]]);
    }

    #[test]
    fn quantizing_dequantized_samples_gives_the_same_integers() {
        // What reopening a FLAC-cached stem does: decoded samples are
        // `int * step`, and quantizing them again must be lossless.
        let l: Vec<f32> = (0..20_000)
            .map(|i| 1.2 * (i as f32 * 0.031).sin())
            .collect();
        let r: Vec<f32> = (0..20_000)
            .map(|i| 0.9 * (i as f32 * 0.017).sin())
            .collect();
        let a = Track::quantized("a", [&l, &r], 44_100);
        let [dl, dr] = a.channels.clone().map(|c| c.to_vec());
        let b = Track::quantized("b", [&dl, &dr], 44_100);
        assert_eq!(ints(&a), ints(&b));
    }

    #[test]
    fn channel_reads_match_get() {
        let t = Track::quantized("x", [&[0.5, -0.25, 1.0], &[0.0, 0.1, -1.0]], 44_100);
        let mut out = [0.0; 2];
        t.channels[1].read(1, &mut out);
        assert_eq!(out, [t.channels[1].get(1), t.channels[1].get(2)]);
        assert_eq!(t.len(), 3);
        let f = Track::new("f", [vec![1.0, 2.0], vec![3.0]], 44_100);
        assert_eq!(f.len(), 1);
        assert_eq!(f.channels[0].as_f32(), Some(&[1.0, 2.0][..]));
        assert_eq!(f.channels[1].to_vec(), vec![3.0]);
    }
}
