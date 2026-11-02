//! Compressed cache files: 16-bit FLAC (flacenc) and MP3 VBR V0 (LAME via
//! mp3lame-encoder), both written with the same per-file `scale` as the WAV
//! cache, and decoded with symphonia (shredlab-io).
//!
//! MP3 timing: the file starts with a LAME tag holding the encoder delay and
//! padding, and symphonia's gapless mode trims both, so a decoded stem lines
//! up sample for sample with the original. The decoded length is then fitted
//! to the length in `song.json` (tested in `mp3_is_sample_aligned`).

use std::fs;
use std::path::Path;

use crate::{Error, Result};

fn err(path: &Path, msg: impl std::fmt::Display) -> Error {
    Error::Wav {
        path: path.display().to_string(),
        msg: msg.to_string(),
    }
}

/// Writes `bytes` to a temp file next to `path`, then renames it into place.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    {
        use std::io::Write;
        let mut f = fs::File::create(&tmp).map_err(|e| Error::io(&tmp, e))?;
        f.write_all(bytes)
            .and_then(|_| f.sync_all())
            .map_err(|e| Error::io(&tmp, e))?;
    }
    fs::rename(&tmp, path).map_err(|e| Error::io(path, e))
}

/// Sample `i` of channel `c`, divided by `scale`, NaN/inf as 0.
#[inline]
fn sample(ch: [&[f32]; 2], c: usize, i: usize, scale: f32) -> f32 {
    let v = ch[c][i];
    if v.is_finite() {
        v / scale
    } else {
        0.0
    }
}

/// 16-bit stereo FLAC of `ch / scale`; returns the scale to store (`scale`
/// widened by 32768/32767, so the peak maps to 32767 and symphonia's
/// `i16 / 32768` decoding times the returned scale gives the input back).
pub(crate) fn write_flac(
    path: &Path,
    sample_rate: u32,
    ch: [&[f32]; 2],
    scale: f32,
) -> Result<f32> {
    use flacenc::component::BitRepr;
    use flacenc::error::Verify;

    let scale = scale * (32768.0 / 32767.0);
    let len = ch[0].len();
    let mut interleaved = Vec::with_capacity(len * 2);
    for i in 0..len {
        for c in 0..2 {
            // 32768 with the widened scale: the peak lands on 32767.
            let q = (sample(ch, c, i, scale) * 32768.0)
                .round()
                .clamp(-32768.0, 32767.0);
            interleaved.push(q as i32);
        }
    }
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|(_, e)| err(path, format!("FLAC config: {e:?}")))?;
    let source =
        flacenc::source::MemSource::from_samples(&interleaved, 2, 16, sample_rate as usize);
    let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|e| err(path, format!("FLAC: {e:?}")))?;
    drop(interleaved);
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| err(path, format!("FLAC: {e:?}")))?;
    let mut bytes = sink.as_slice().to_vec();
    fix_streaminfo_min_block(&mut bytes);
    write_atomic(path, &bytes)?;
    Ok(scale)
}

/// flacenc stores the (shorter) last block's size as the minimum block size;
/// symphonia then rejects the fixed-blocksize frames. Set min = max (same
/// fix as the export encoder in shredlab-audio).
fn fix_streaminfo_min_block(bytes: &mut [u8]) {
    if bytes.len() >= 12 && &bytes[..4] == b"fLaC" && bytes[4] & 0x7f == 0 {
        let (max0, max1) = (bytes[10], bytes[11]);
        bytes[8] = max0;
        bytes[9] = max1;
    }
}

/// MP3 VBR V0 (LAME `-V0`, quality 2) of `ch / scale`, with a LAME tag so
/// decoders can trim the encoder delay and padding.
pub(crate) fn write_mp3(path: &Path, sample_rate: u32, ch: [&[f32]; 2], scale: f32) -> Result<()> {
    use mp3lame_encoder::{Builder, DualPcm, FlushGap, Quality, VbrMode};

    let e = |what: &str, x: &dyn std::fmt::Debug| err(path, format!("MP3 {what}: {x:?}"));
    let mut b = Builder::new().ok_or_else(|| err(path, "cannot create LAME encoder"))?;
    b.set_num_channels(2).map_err(|x| e("channels", &x))?;
    b.set_sample_rate(sample_rate)
        .map_err(|x| e("sample rate", &x))?;
    b.set_vbr_mode(VbrMode::Mtrh).map_err(|x| e("VBR", &x))?;
    b.set_vbr_quality(Quality::Best)
        .map_err(|x| e("VBR quality", &x))?;
    b.set_quality(Quality::NearBest)
        .map_err(|x| e("quality", &x))?;
    b.set_to_write_vbr_tag(true)
        .map_err(|x| e("LAME tag", &x))?;
    let mut enc = b.build().map_err(|x| e("init", &x))?;

    const CHUNK: usize = 8192;
    let len = ch[0].len();
    let mut out: Vec<u8> = Vec::with_capacity(len / 4 + 8192);
    let mut l = vec![0.0f32; CHUNK];
    let mut r = vec![0.0f32; CHUNK];
    let mut i = 0;
    while i < len {
        let k = CHUNK.min(len - i);
        for j in 0..k {
            l[j] = sample(ch, 0, i + j, scale);
            r[j] = sample(ch, 1, i + j, scale);
        }
        out.reserve(mp3lame_encoder::max_required_buffer_size(k));
        enc.encode_to_vec(
            DualPcm {
                left: &l[..k],
                right: &r[..k],
            },
            &mut out,
        )
        .map_err(|x| e("encode", &x))?;
        i += k;
    }
    out.reserve(8192);
    enc.flush_to_vec::<FlushGap>(&mut out)
        .map_err(|x| e("flush", &x))?;
    // The stream starts with a placeholder frame for the LAME tag (no ID3
    // tag is written); fill it in now that delay and length are known.
    let mut tag = Vec::with_capacity(enc.lame_tag_size());
    enc.lame_tag_encode_to_vec(&mut tag)
        .ok_or_else(|| err(path, "MP3: no LAME tag"))?;
    if tag.len() > out.len() {
        return Err(err(path, "MP3: LAME tag longer than the stream"));
    }
    out[..tag.len()].copy_from_slice(&tag);
    write_atomic(path, &out)
}

/// Decodes a cache file (FLAC or MP3) or a copied source file to stereo at
/// `sample_rate`, multiplied by `scale` and fitted to `len` frames (cut or
/// zero-padded). With `resample` false a different sample rate is an error.
pub(crate) fn read(
    path: &Path,
    sample_rate: u32,
    len: usize,
    scale: f32,
    resample: bool,
) -> Result<[Vec<f32>; 2]> {
    let mut audio = shredlab_io::decode_file(path).map_err(|e| err(path, e))?;
    if audio.sample_rate != sample_rate {
        if !resample {
            return Err(err(
                path,
                format!("sample rate {}, expected {sample_rate}", audio.sample_rate),
            ));
        }
        audio = shredlab_io::resample(&audio, sample_rate).map_err(|e| err(path, e))?;
    }
    let mut ch = audio.channels.into_iter();
    let mut l = ch.next().unwrap_or_default();
    let mut r = ch.next().unwrap_or_else(|| l.clone());
    for c in [&mut l, &mut r] {
        c.resize(len, 0.0);
        if scale != 1.0 {
            c.iter_mut().for_each(|v| *v *= scale);
        }
    }
    Ok([l, r])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("shredlab-codec-test-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    /// Noise bursts with sharp onsets (a misalignment of even a few samples
    /// shows up as a large error) at up to 1.6x full scale.
    fn signal(n: usize) -> [Vec<f32>; 2] {
        let mut s = 0x2545_F491_4F6C_DD1Du64;
        let mut rnd = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 40) as f32 / (1u64 << 23) as f32 - 1.0
        };
        let mut l = vec![0.0; n];
        let mut r = vec![0.0; n];
        for i in 0..n {
            let env = if (i / 4410).is_multiple_of(2) {
                0.8
            } else {
                0.05
            };
            let tone = (2.0 * std::f32::consts::PI * 220.0 * i as f32 / 44_100.0).sin();
            l[i] = env * (rnd() + tone);
            r[i] = env * (0.5 * rnd() + tone);
        }
        [l, r]
    }

    fn snr_db(a: &[f32], b: &[f32]) -> f64 {
        let (mut s, mut e) = (0.0f64, 0.0f64);
        for (x, y) in a.iter().zip(b) {
            s += (*x as f64).powi(2);
            e += (*x as f64 - *y as f64).powi(2);
        }
        10.0 * (s / e.max(1e-30)).log10()
    }

    #[test]
    fn flac_roundtrip_is_16_bit_exact() {
        let [l, r] = signal(44_100);
        let scale = 1.6;
        let p = tmp("a.flac");
        let stored = write_flac(&p, 44_100, [&l, &r], scale).unwrap();
        let [dl, dr] = read(&p, 44_100, l.len(), stored, false).unwrap();
        let q = stored / 32768.0;
        for (a, b) in l.iter().zip(&dl).chain(r.iter().zip(&dr)) {
            assert!((a - b).abs() <= q * 0.51, "{a} vs {b}");
        }
    }

    /// Gated tones up to 5.3 kHz (MP3 codes them well, and a one-sample
    /// shift already costs a lot at 5 kHz), at up to 1.6x full scale.
    fn tones(n: usize) -> [Vec<f32>; 2] {
        let w = |f: f32, i: usize| (2.0 * std::f32::consts::PI * f * i as f32 / 44_100.0).sin();
        let gate = |i: usize| {
            if (i / 4410).is_multiple_of(2) {
                0.4
            } else {
                0.1
            }
        };
        let l = (0..n)
            .map(|i| gate(i) * (w(440.0, i) + w(1250.0, i) + w(3100.0, i) + w(5300.0, i)))
            .collect();
        let r = (0..n)
            .map(|i| gate(i) * (w(330.0, i) + w(2200.0, i) + w(4700.0, i)))
            .collect();
        [l, r]
    }

    #[test]
    fn mp3_is_sample_aligned() {
        let [l, r] = tones(5 * 44_100 + 123);
        let scale = 1.6;
        let p = tmp("a.mp3");
        write_mp3(&p, 44_100, [&l, &r], scale).unwrap();
        let [dl, dr] = read(&p, 44_100, l.len(), scale, false).unwrap();
        assert_eq!(dl.len(), l.len());
        // Best lag over ±2000 samples (LAME's delay is ~1100) must be 0.
        let (a, n) = (2 * 44_100, 20_000);
        let corr = |lag: isize| -> f64 {
            (0..n)
                .map(|i| l[a + i] as f64 * dl[(a as isize + i as isize + lag) as usize] as f64)
                .sum()
        };
        let best = (-2000..=2000)
            .max_by(|&x, &y| corr(x).total_cmp(&corr(y)))
            .unwrap();
        assert_eq!(best, 0);
        let snr = snr_db(&l, &dl).min(snr_db(&r, &dr));
        assert!(snr > 20.0, "SNR {snr:.1} dB");
    }

    #[test]
    fn mp3_rejects_other_sample_rate_without_resampling() {
        let [l, r] = signal(4410);
        let p = tmp("b.mp3");
        write_mp3(&p, 44_100, [&l, &r], 1.0).unwrap();
        assert!(read(&p, 48_000, l.len(), 1.0, false).is_err());
        assert_eq!(read(&p, 48_000, 100, 1.0, true).unwrap()[0].len(), 100);
    }
}
