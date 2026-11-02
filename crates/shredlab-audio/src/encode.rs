// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Export encoders: WAV 16/24-bit (hound), FLAC 24-bit (flacenc, pure Rust)
//! and MP3 CBR (LAME via mp3lame-encoder, LGPL). Peaks above -0.1 dBFS go
//! through a look-ahead peak limiter first, so nothing clips.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use crate::AudioError;

/// Output file format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// 16-bit PCM with TPDF dither.
    Wav16,
    /// 24-bit PCM.
    Wav24,
    /// 24-bit FLAC.
    Flac,
    /// Constant bitrate MP3; `kbps` is snapped to the nearest standard rate (32..=320).
    Mp3 { kbps: u32 },
}

/// Limiter ceiling: -0.1 dBFS.
pub const CEILING: f32 = 0.988_553;
/// Limiter look-ahead (samples): ~2 ms at 44.1 kHz.
const LOOKAHEAD: usize = 88;
/// Limiter release time constant (seconds).
const RELEASE_SECS: f32 = 0.08;

/// Writes `channels` (planar stereo, same length) to `path`. On failure the
/// partial file is removed.
pub fn encode(
    path: &Path,
    channels: &[Vec<f32>; 2],
    sample_rate: u32,
    format: ExportFormat,
) -> Result<(), AudioError> {
    if channels[0].len() != channels[1].len() {
        return Err(AudioError::Invalid("channel lengths differ".into()));
    }
    if sample_rate == 0 {
        return Err(AudioError::Invalid("sample rate is 0".into()));
    }
    let gain = limiter_gain(channels, sample_rate, CEILING);
    let g = |i: usize| gain.as_ref().map_or(1.0, |g| g[i]);
    let src = Samples {
        ch: channels,
        g: &g,
    };
    let r = match format {
        ExportFormat::Wav16 => write_wav(path, &src, sample_rate, 16),
        ExportFormat::Wav24 => write_wav(path, &src, sample_rate, 24),
        ExportFormat::Flac => write_flac(path, &src, sample_rate),
        ExportFormat::Mp3 { kbps } => write_mp3(path, &src, sample_rate, kbps),
    };
    if r.is_err() {
        let _ = std::fs::remove_file(path);
    }
    r
}

/// Source samples with the limiter gain applied, clamped to [-1, 1].
struct Samples<'a> {
    ch: &'a [Vec<f32>; 2],
    g: &'a dyn Fn(usize) -> f32,
}

impl Samples<'_> {
    fn len(&self) -> usize {
        self.ch[0].len()
    }
    #[inline]
    fn get(&self, c: usize, i: usize) -> f32 {
        let x = self.ch[c][i] * (self.g)(i);
        if x.is_finite() {
            x.clamp(-1.0, 1.0)
        } else {
            0.0
        }
    }
}

/// Peak (max |sample|) over both channels.
pub fn peak(channels: &[Vec<f32>; 2]) -> f32 {
    channels
        .iter()
        .flat_map(|c| c.iter())
        .fold(
            0.0f32,
            |m, &x| if x.is_finite() { m.max(x.abs()) } else { m },
        )
}

/// Linked-stereo gain envelope that keeps every sample at or below `ceiling`,
/// or `None` when the peak is already below it. Look-ahead (no overshoot),
/// smooth attack, exponential release.
pub fn limiter_gain(channels: &[Vec<f32>; 2], sample_rate: u32, ceiling: f32) -> Option<Vec<f32>> {
    if peak(channels) <= ceiling {
        return None;
    }
    let n = channels[0].len();
    let w = LOOKAHEAD;
    // Required gain per sample.
    let req = |i: usize| {
        let a = channels[0][i].abs().max(channels[1][i].abs());
        if a > ceiling && a.is_finite() {
            ceiling / a
        } else {
            1.0
        }
    };
    // Sliding minimum over [i - w, i + w] (monotonic deque).
    let mut minw = vec![1.0f32; n];
    let mut dq: VecDeque<(usize, f32)> = VecDeque::new();
    for j in 0..n + w {
        if j < n {
            let v = req(j);
            while dq.back().is_some_and(|&(_, b)| b >= v) {
                dq.pop_back();
            }
            dq.push_back((j, v));
        }
        if j >= w {
            let i = j - w;
            while dq.front().is_some_and(|&(k, _)| k + 2 * w < j) {
                dq.pop_front();
            }
            minw[i] = dq.front().map_or(1.0, |&(_, v)| v);
        }
    }
    // Box average over [i - w, i + w]: every window around a peak sample only
    // contains values <= its required gain, so the average is too.
    let span = 2 * w + 1;
    let at = |k: isize| {
        if k < 0 || k as usize >= n {
            1.0
        } else {
            minw[k as usize]
        }
    };
    let mut gain = vec![1.0f32; n];
    let mut sum: f64 = (-(w as isize)..=w as isize).map(|k| at(k) as f64).sum();
    let rel = 1.0 - (-1.0 / (RELEASE_SECS * sample_rate as f32)).exp();
    let mut g_prev = 1.0f32;
    for (i, out) in gain.iter_mut().enumerate() {
        let avg = (sum / span as f64) as f32;
        // Release: recover slowly; never above the smoothed requirement.
        let up = g_prev + (1.0 - g_prev) * rel;
        let g = avg.min(up);
        *out = g;
        g_prev = g;
        let ii = i as isize;
        sum += at(ii + w as isize + 1) as f64 - at(ii - w as isize) as f64;
    }
    Some(gain)
}

fn werr(path: &Path, e: impl std::fmt::Display) -> AudioError {
    AudioError::Write {
        path: path.display().to_string(),
        msg: e.to_string(),
    }
}

/// Small xorshift RNG for dither (deterministic).
struct Rng(u64);

impl Rng {
    fn uniform(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
    /// Triangular PDF in (-1, 1) LSB.
    fn tpdf(&mut self) -> f32 {
        self.uniform() - self.uniform()
    }
}

fn write_wav(path: &Path, src: &Samples, sample_rate: u32, bits: u16) -> Result<(), AudioError> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate,
        bits_per_sample: bits,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).map_err(|e| werr(path, e))?;
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for i in 0..src.len() {
        for c in 0..2 {
            let x = src.get(c, i);
            if bits == 16 {
                let v = (x * 32767.0 + rng.tpdf()).round().clamp(-32768.0, 32767.0) as i16;
                w.write_sample(v).map_err(|e| werr(path, e))?;
            } else {
                let v = (x as f64 * 8_388_607.0)
                    .round()
                    .clamp(-8_388_608.0, 8_388_607.0) as i32;
                w.write_sample(v).map_err(|e| werr(path, e))?;
            }
        }
    }
    w.finalize().map_err(|e| werr(path, e))
}

fn write_flac(path: &Path, src: &Samples, sample_rate: u32) -> Result<(), AudioError> {
    use flacenc::component::BitRepr;
    use flacenc::error::Verify;

    let mut interleaved = Vec::with_capacity(src.len() * 2);
    for i in 0..src.len() {
        for c in 0..2 {
            let v = (src.get(c, i) as f64 * 8_388_607.0)
                .round()
                .clamp(-8_388_608.0, 8_388_607.0);
            interleaved.push(v as i32);
        }
    }
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|(_, e)| AudioError::Encode(format!("FLAC config: {e:?}")))?;
    let source =
        flacenc::source::MemSource::from_samples(&interleaved, 2, 24, sample_rate as usize);
    let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|e| AudioError::Encode(format!("FLAC: {e:?}")))?;
    drop(interleaved);
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| AudioError::Encode(format!("FLAC: {e:?}")))?;
    let mut bytes = sink.as_slice().to_vec();
    fix_streaminfo_min_block(&mut bytes);
    std::fs::write(path, &bytes).map_err(|e| werr(path, e))
}

/// flacenc stores the (shorter) last block's size as the minimum block size.
/// The spec excludes the last block, and decoders such as symphonia treat
/// min != max as a variable-blocksize stream and then reject the frame-numbered
/// (fixed-blocksize) frames. Set min = max.
fn fix_streaminfo_min_block(bytes: &mut [u8]) {
    // "fLaC", 4-byte block header (type 0 = STREAMINFO), min block (u16), max block (u16).
    if bytes.len() >= 12 && &bytes[..4] == b"fLaC" && bytes[4] & 0x7f == 0 {
        let (max0, max1) = (bytes[10], bytes[11]);
        bytes[8] = max0;
        bytes[9] = max1;
    }
}

/// Standard MP3 bitrate nearest to `kbps`.
fn mp3_bitrate(kbps: u32) -> mp3lame_encoder::Bitrate {
    use mp3lame_encoder::Bitrate::*;
    let table = [
        (32, Kbps32),
        (40, Kbps40),
        (48, Kbps48),
        (64, Kbps64),
        (80, Kbps80),
        (96, Kbps96),
        (112, Kbps112),
        (128, Kbps128),
        (160, Kbps160),
        (192, Kbps192),
        (224, Kbps224),
        (256, Kbps256),
        (320, Kbps320),
    ];
    table
        .iter()
        .min_by_key(|(k, _)| (*k as u32).abs_diff(kbps))
        .map(|&(_, b)| b)
        .unwrap_or(Kbps320)
}

fn write_mp3(path: &Path, src: &Samples, sample_rate: u32, kbps: u32) -> Result<(), AudioError> {
    use mp3lame_encoder::{Builder, DualPcm, FlushNoGap, Quality};

    let e =
        |what: &str, err: &dyn std::fmt::Debug| AudioError::Encode(format!("MP3 {what}: {err:?}"));
    let mut b =
        Builder::new().ok_or_else(|| AudioError::Encode("cannot create LAME encoder".into()))?;
    b.set_num_channels(2).map_err(|x| e("channels", &x))?;
    b.set_sample_rate(sample_rate)
        .map_err(|x| e("sample rate", &x))?;
    b.set_brate(mp3_bitrate(kbps))
        .map_err(|x| e("bitrate", &x))?;
    b.set_quality(Quality::NearBest)
        .map_err(|x| e("quality", &x))?;
    let mut enc = b.build().map_err(|x| e("init", &x))?;

    let file = File::create(path).map_err(|x| werr(path, x))?;
    let mut out = BufWriter::new(file);
    const CHUNK: usize = 8192;
    let mut l = vec![0.0f32; CHUNK];
    let mut r = vec![0.0f32; CHUNK];
    let mut buf: Vec<u8> = Vec::with_capacity(mp3lame_encoder::max_required_buffer_size(CHUNK));
    let n = src.len();
    let mut i = 0;
    while i < n {
        let k = CHUNK.min(n - i);
        for j in 0..k {
            l[j] = src.get(0, i + j);
            r[j] = src.get(1, i + j);
        }
        buf.clear();
        enc.encode_to_vec(
            DualPcm {
                left: &l[..k],
                right: &r[..k],
            },
            &mut buf,
        )
        .map_err(|x| e("encode", &x))?;
        out.write_all(&buf).map_err(|x| werr(path, x))?;
        i += k;
    }
    buf.clear();
    enc.flush_to_vec::<FlushNoGap>(&mut buf)
        .map_err(|x| e("flush", &x))?;
    out.write_all(&buf).map_err(|x| werr(path, x))?;
    out.flush().map_err(|x| werr(path, x))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("shredlab-audio-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn sine(n: usize, f: f32, a: f32) -> Vec<f32> {
        (0..n)
            .map(|i| a * (2.0 * std::f32::consts::PI * f * i as f32 / 44_100.0).sin())
            .collect()
    }

    fn stereo(n: usize) -> [Vec<f32>; 2] {
        [sine(n, 440.0, 0.5), sine(n, 660.0, 0.3)]
    }

    fn max_err(a: &[f32], b: &[f32]) -> f32 {
        a.iter()
            .zip(b)
            .fold(0.0f32, |m, (x, y)| m.max((x - y).abs()))
    }

    #[test]
    fn wav_round_trips() {
        let x = stereo(20_000);
        for (fmt, tol) in [
            (ExportFormat::Wav16, 2.0 / 32768.0),
            (ExportFormat::Wav24, 1e-6),
        ] {
            let p = tmp(&format!("rt-{fmt:?}.wav"));
            encode(&p, &x, 44_100, fmt).unwrap();
            let a = shredlab_io::decode_file(&p).unwrap();
            assert_eq!(a.sample_rate, 44_100);
            assert_eq!(a.len(), 20_000);
            assert!(max_err(&a.channels[0], &x[0]) < tol, "{fmt:?}");
            assert!(max_err(&a.channels[1], &x[1]) < tol, "{fmt:?}");
            let spec = hound::WavReader::open(&p).unwrap().spec();
            assert_eq!(
                spec.bits_per_sample,
                if fmt == ExportFormat::Wav16 { 16 } else { 24 }
            );
        }
    }

    #[test]
    fn flac_round_trips() {
        let x = stereo(30_000);
        let p = tmp("rt.flac");
        encode(&p, &x, 44_100, ExportFormat::Flac).unwrap();
        let a = shredlab_io::decode_file(&p).unwrap();
        assert_eq!(a.sample_rate, 44_100);
        assert_eq!(a.len(), 30_000);
        assert!(max_err(&a.channels[0], &x[0]) < 1e-6);
        assert!(max_err(&a.channels[1], &x[1]) < 1e-6);
    }

    #[test]
    fn mp3_round_trips_approximately() {
        let n = 44_100 * 2;
        let x = stereo(n);
        let p = tmp("rt.mp3");
        encode(&p, &x, 44_100, ExportFormat::Mp3 { kbps: 192 }).unwrap();
        let a = shredlab_io::decode_file(&p).unwrap();
        assert_eq!(a.sample_rate, 44_100);
        // Encoder delay/padding: length within ~2 frames.
        assert!(a.len().abs_diff(n) < 3000, "len {}", a.len());
        let rms = |v: &[f32]| (v.iter().map(|s| s * s).sum::<f32>() / v.len() as f32).sqrt();
        let r0 = rms(&a.channels[0][4000..n - 4000]);
        assert!((r0 / rms(&x[0]) - 1.0).abs() < 0.05, "rms {r0}");
        // ~192 kbps: 2 s ≈ 48 KB.
        let size = std::fs::metadata(&p).unwrap().len();
        assert!((40_000..60_000).contains(&size), "size {size}");
    }

    #[test]
    fn limiter_prevents_clipping_without_touching_quiet_parts() {
        let n = 44_100;
        let mut x = stereo(n);
        for v in &mut x[0][20_000..20_500] {
            *v *= 3.0; // 1.5 peak burst
        }
        let g = limiter_gain(&x, 44_100, CEILING).unwrap();
        for i in 0..n {
            assert!((x[0][i] * g[i]).abs() <= CEILING + 1e-6, "at {i}");
            assert!((x[1][i] * g[i]).abs() <= CEILING + 1e-6);
        }
        // Far from the burst: unity gain.
        assert!((g[1000] - 1.0).abs() < 1e-6);
        assert!(g[44_000] > 0.999);
        // Below the ceiling: no limiter at all.
        assert!(limiter_gain(&stereo(100), 44_100, CEILING).is_none());
        // Encoding a hot signal never wraps around.
        let p = tmp("hot.wav");
        encode(&p, &x, 44_100, ExportFormat::Wav16).unwrap();
        let a = shredlab_io::decode_file(&p).unwrap();
        assert!(a.channels[0].iter().all(|v| v.abs() <= 1.0));
        assert!(a.channels[0][20_100] != 0.0);
    }

    #[test]
    fn bitrate_snapping() {
        assert!(matches!(
            mp3_bitrate(192),
            mp3lame_encoder::Bitrate::Kbps192
        ));
        assert!(matches!(
            mp3_bitrate(300),
            mp3lame_encoder::Bitrate::Kbps320
        ));
        assert!(matches!(mp3_bitrate(0), mp3lame_encoder::Bitrate::Kbps32));
    }
}
