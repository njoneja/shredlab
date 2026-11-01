//! Audio I/O: decoding (symphonia: MP3, FLAC, WAV, OGG Vorbis, M4A AAC/ALAC),
//! resampling (rubato) and WAV writing (hound).
//!
//! Audio is returned as planar f32 stereo ([`Audio`]); mono input is
//! duplicated to both channels, inputs with more than two channels keep the
//! first two.

use std::fs::File;
use std::path::Path;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Version of this crate, from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Crate name, from `Cargo.toml`.
pub const NAME: &str = env!("CARGO_PKG_NAME");

/// Sample rate the separation models expect.
pub const MODEL_SAMPLE_RATE: u32 = 44_100;

#[derive(Debug, thiserror::Error)]
pub enum IoError {
    #[error("cannot open {path}: {source}")]
    Open {
        path: String,
        source: std::io::Error,
    },
    #[error("unsupported or corrupt audio file {path}: {msg}")]
    Decode { path: String, msg: String },
    #[error("no audio track in {0}")]
    NoTrack(String),
    #[error("resampling failed: {0}")]
    Resample(String),
    #[error("cannot write {path}: {msg}")]
    Write { path: String, msg: String },
}

pub type Result<T> = std::result::Result<T, IoError>;

/// Planar f32 audio.
#[derive(Debug, Clone, PartialEq)]
pub struct Audio {
    pub sample_rate: u32,
    /// One buffer per channel, all the same length.
    pub channels: Vec<Vec<f32>>,
}

impl Audio {
    pub fn len(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn duration_secs(&self) -> f64 {
        self.len() as f64 / self.sample_rate as f64
    }

    /// Channel slices, for the DSP functions.
    pub fn as_slices(&self) -> Vec<&[f32]> {
        self.channels.iter().map(Vec::as_slice).collect()
    }
}

/// Decodes an audio file to planar f32 at its native sample rate, stereo.
pub fn decode_file(path: &Path) -> Result<Audio> {
    let p = path.display().to_string();
    let file = File::open(path).map_err(|source| IoError::Open {
        path: p.clone(),
        source,
    })?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let dec_err = |e: SymError| IoError::Decode {
        path: p.clone(),
        msg: e.to_string(),
    };
    let fmt_opts = FormatOptions {
        enable_gapless: true,
        ..Default::default()
    };
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &fmt_opts, &MetadataOptions::default())
        .map_err(dec_err)?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| IoError::NoTrack(p.clone()))?;
    let track_id = track.id;
    let mut sample_rate = track.codec_params.sample_rate;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(dec_err)?;

    let mut channels: Vec<Vec<f32>> = Vec::new();
    let mut sbuf: Option<SampleBuffer<f32>> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(dec_err(e)),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // Recoverable: skip the corrupt packet.
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(dec_err(e)),
        };
        let spec = *decoded.spec();
        sample_rate.get_or_insert(spec.rate);
        let nc = spec.channels.count();
        if channels.is_empty() {
            channels = vec![Vec::new(); nc.max(1)];
        }
        if sbuf
            .as_ref()
            .is_none_or(|b| b.capacity() < decoded.capacity() * nc)
        {
            sbuf = Some(SampleBuffer::new(decoded.capacity() as u64, spec));
        }
        let buf = sbuf.as_mut().expect("sample buffer");
        buf.copy_interleaved_ref(decoded);
        let samples = buf.samples();
        if nc == channels.len() {
            for frame in samples.chunks_exact(nc) {
                for (ch, &v) in channels.iter_mut().zip(frame) {
                    ch.push(v);
                }
            }
        }
    }
    let sample_rate = sample_rate.ok_or_else(|| IoError::Decode {
        path: p.clone(),
        msg: "unknown sample rate".into(),
    })?;
    Ok(Audio {
        sample_rate,
        channels: to_stereo(channels),
    })
}

/// Mono -> duplicated stereo; more than two channels -> first two.
fn to_stereo(mut channels: Vec<Vec<f32>>) -> Vec<Vec<f32>> {
    match channels.len() {
        0 => vec![Vec::new(), Vec::new()],
        1 => {
            let c = channels.pop().expect("one channel");
            vec![c.clone(), c]
        }
        _ => {
            channels.truncate(2);
            channels
        }
    }
}

/// Resamples to `rate` (no-op if already there). Output length is
/// `round(len * rate / input_rate)`, delay-compensated.
pub fn resample(audio: &Audio, rate: u32) -> Result<Audio> {
    use rubato::{FftFixedIn, Resampler};

    if audio.sample_rate == rate || audio.is_empty() {
        return Ok(Audio {
            sample_rate: rate,
            channels: audio.channels.clone(),
        });
    }
    let nc = audio.channels.len();
    let len = audio.len();
    let expected = ((len as u128 * rate as u128 + audio.sample_rate as u128 / 2)
        / audio.sample_rate as u128) as usize;
    let err = |e: &dyn std::fmt::Display| IoError::Resample(e.to_string());
    let mut rs = FftFixedIn::<f32>::new(audio.sample_rate as usize, rate as usize, 1024, 2, nc)
        .map_err(|e| err(&e))?;
    let delay = rs.output_delay();
    let mut out: Vec<Vec<f32>> = vec![Vec::with_capacity(expected + delay + 4096); nc];
    let mut pos = 0;
    let mut obuf = rs.output_buffer_allocate(true);
    while pos < len {
        let need = rs.input_frames_next();
        let (_, written) = if len - pos >= need {
            let input: Vec<&[f32]> = audio.channels.iter().map(|c| &c[pos..pos + need]).collect();
            pos += need;
            rs.process_into_buffer(&input, &mut obuf, None)
                .map_err(|e| err(&e))?
        } else {
            let input: Vec<&[f32]> = audio.channels.iter().map(|c| &c[pos..]).collect();
            pos = len;
            rs.process_partial_into_buffer(Some(&input), &mut obuf, None)
                .map_err(|e| err(&e))?
        };
        for (o, b) in out.iter_mut().zip(&obuf) {
            o.extend_from_slice(&b[..written]);
        }
    }
    while out[0].len() < expected + delay {
        let (_, written) = rs
            .process_partial_into_buffer(None::<&[&[f32]]>, &mut obuf, None)
            .map_err(|e| err(&e))?;
        for (o, b) in out.iter_mut().zip(&obuf) {
            o.extend_from_slice(&b[..written]);
        }
    }
    let channels = out
        .into_iter()
        .map(|c| c[delay..delay + expected].to_vec())
        .collect();
    Ok(Audio {
        sample_rate: rate,
        channels,
    })
}

/// Decodes a file and resamples it to [`MODEL_SAMPLE_RATE`].
pub fn load_for_separation(path: &Path) -> Result<Audio> {
    let audio = decode_file(path)?;
    resample(&audio, MODEL_SAMPLE_RATE)
}

/// WAV sample format for [`write_wav`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WavFormat {
    F32,
    I16,
}

/// Writes planar audio as an interleaved WAV file.
pub fn write_wav(
    path: &Path,
    sample_rate: u32,
    channels: &[&[f32]],
    format: WavFormat,
) -> Result<()> {
    let werr = |e: hound::Error| IoError::Write {
        path: path.display().to_string(),
        msg: e.to_string(),
    };
    let spec = hound::WavSpec {
        channels: channels.len() as u16,
        sample_rate,
        bits_per_sample: if format == WavFormat::F32 { 32 } else { 16 },
        sample_format: if format == WavFormat::F32 {
            hound::SampleFormat::Float
        } else {
            hound::SampleFormat::Int
        },
    };
    let len = channels.first().map_or(0, |c| c.len());
    let mut w = hound::WavWriter::create(path, spec).map_err(werr)?;
    for i in 0..len {
        for ch in channels {
            match format {
                WavFormat::F32 => w.write_sample(ch[i]).map_err(werr)?,
                WavFormat::I16 => {
                    let v = (ch[i].clamp(-1.0, 1.0) * 32767.0).round() as i16;
                    w.write_sample(v).map_err(werr)?
                }
            }
        }
    }
    w.finalize().map_err(werr)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("stemx-io-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn sine(len: usize, rate: u32, freq: f32) -> Vec<f32> {
        (0..len)
            .map(|i| 0.5 * (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin())
            .collect()
    }

    #[test]
    fn metadata_is_set() {
        assert_eq!(NAME, "stemx-io");
        assert!(!VERSION.is_empty());
    }

    #[test]
    fn wav_f32_roundtrip() {
        let l = sine(1000, 44100, 440.0);
        let r = sine(1000, 44100, 220.0);
        let path = tmp("f32.wav");
        write_wav(&path, 44100, &[&l, &r], WavFormat::F32).unwrap();
        let a = decode_file(&path).unwrap();
        assert_eq!(a.sample_rate, 44100);
        assert_eq!(a.channels, vec![l, r]);
    }

    #[test]
    fn wav_i16_mono_duplicates() {
        let m = sine(500, 22050, 300.0);
        let path = tmp("i16.wav");
        write_wav(&path, 22050, &[&m], WavFormat::I16).unwrap();
        let a = decode_file(&path).unwrap();
        assert_eq!(a.sample_rate, 22050);
        assert_eq!(a.len(), 500);
        assert_eq!(a.channels[0], a.channels[1]);
        let err = m
            .iter()
            .zip(&a.channels[0])
            .map(|(x, y)| (x - y).abs())
            .fold(0.0, f32::max);
        assert!(err < 1.0 / 32767.0 + 1e-6, "err {err}");
    }

    #[test]
    fn resample_48k_to_44k1() {
        let rate = 48000;
        let x = sine(48000, rate, 1000.0);
        let a = Audio {
            sample_rate: rate,
            channels: vec![x.clone(), x],
        };
        let b = resample(&a, 44100).unwrap();
        assert_eq!(b.len(), 44100);
        // compare against the ideal sine in the middle (delay compensated)
        let ideal = sine(44100, 44100, 1000.0);
        let err = (2000..42000)
            .map(|i| (b.channels[0][i] - ideal[i]).abs())
            .fold(0.0, f32::max);
        assert!(err < 1e-3, "max err {err}");
    }

    #[test]
    fn resample_noop() {
        let a = Audio {
            sample_rate: 44100,
            channels: vec![vec![0.1; 10], vec![0.2; 10]],
        };
        assert_eq!(resample(&a, 44100).unwrap(), a);
    }
}
