//! Real-time audio engine and offline export renderer for Stem Extractor.
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
//! crossfades → click → resample to the device rate) and keeps a small
//! lock-free ring buffer ([`rtrb`]) about 30 ms ahead of the device. The
//! cpal callback only pops frames from the ring and publishes the position
//! through atomics: no locks or allocation in the callback. Controls go to
//! the producer thread over a channel; gain, speed and pitch changes are
//! heard within roughly the ring latency plus the device buffer (< 60 ms).
//!
//! Speed 1.0 with pitch 0 bypasses the stretcher (bit-transparent at unity
//! gain). Playback runs the music through a soft-knee look-ahead safety
//! limiter ([`limiter`], transparent below -1 dBFS) before the click is
//! added; export uses its own whole-file limiter instead (on the music
//! before the click is added, then on the final sum). The mixer can
//! build the backing track from the original mix ([`mix`], P1). Each stem
//! first runs through its Refine chain (stereo selection + EQ, at the song
//! rate, fed ahead by the chain's latency so nothing else is delayed); then
//! the gained tracks are mixed and the stereo mix is stretched once. So
//! refine sits before the stretcher, inside the mixer, next to the P1
//! original path. Seeks, loop wraps, pause and stretch on/off are
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
    pub channels: [Arc<Vec<f32>>; 2],
    pub sample_rate: u32,
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
    #[error("cancelled")]
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_set() {
        assert_eq!(NAME, "shredlab-audio");
        assert!(!VERSION.is_empty());
    }
}
