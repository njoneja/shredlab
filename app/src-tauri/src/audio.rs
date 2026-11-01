//! Audio engine facade: the real engine from `crates/stemx-audio`
//! (cpal + Signalsmith Stretch, offline render, WAV/FLAC/MP3 encoders).

pub use stemx_audio::*;

/// True when this build uses a silent stand-in engine (no longer the case).
pub const IS_STUB: bool = false;
