//! Beat This! (CPJKU, MIT) beat/downbeat tracking glue for Stem Extractor.
//!
//! The ONNX graph (`models/onnx/beat_this_final0.onnx`) contains only the
//! network: a log-mel chunk `(1, 1500, 128)` in, beat and downbeat logits
//! `(1, 1500)` each out. Everything around it lives here, ported 1:1 from the
//! validated Python reference `spikes/onnx_beat_this.py`:
//!
//! 1. [`preprocess`]: downmix to mono, resample to 22050 Hz (rubato instead of
//!    soxr), log-mel spectrogram (torchaudio `MelSpectrogram` settings).
//! 2. [`split`] / [`aggregate`] (or [`infer`] for both): 1500-frame chunks
//!    with a 6-frame border, `keep_first` overlap.
//! 3. [`postprocess`]: "minimal" peak picking (7-frame max-pool, logit > 0,
//!    merge adjacent peaks, snap downbeats to the nearest beat).
//! 4. UI helpers: [`BeatGrid::tempo_bpm`], [`BeatGrid::beats_per_bar`],
//!    [`BeatGrid::most_common_beats_per_bar`].
//!
//! ONNX Runtime is deliberately not a dependency: inference is a closure
//! `FnMut(&[f32], [usize; 3]) -> Result<(Vec<f32>, Vec<f32>), E>` that the
//! caller backs with an ORT session (see `examples/beat_e2e.rs`).
//!
//! ## Validation
//!
//! `tests/reference.rs` (log-mel vs NumPy: max abs diff ~1e-5; postprocessing
//! bit-identical to upstream on four songs) and `examples/beat_e2e.rs`
//! (full pipeline with ORT CPU vs the PyTorch reference on four songs: same
//! beat counts, 100% of beats within ±20 ms, one borderline downbeat
//! missing out of 591). Differences come from rubato vs soxr and symphonia
//! vs libsndfile MP3 decoding (spectrogram SNR ~56 dB).
//!
//! ## Workspace note (temporary)
//!
//! `Cargo.toml` carries an empty `[workspace]` table so the crate builds
//! standalone (`cargo test --manifest-path crates/stemx-beats/Cargo.toml`),
//! which creates `crates/stemx-beats/{Cargo.lock,target/}` (gitignored). To
//! wire it in: remove that table and add `crates/stemx-beats` to the root
//! workspace `members` (and `stemx-beats = { path = ... }` to
//! `[workspace.dependencies]`).

use std::sync::OnceLock;

use realfft::{RealFftPlanner, RealToComplex};
use rubato::{FftFixedIn, Resampler};

/// Model sample rate.
pub const SAMPLE_RATE: u32 = 22050;
/// STFT size.
pub const N_FFT: usize = 1024;
/// STFT hop (50 frames per second at 22050 Hz).
pub const HOP: usize = 441;
/// Spectrogram frames per second.
pub const FPS: f64 = 50.0;
/// Mel bands.
pub const N_MELS: usize = 128;
const F_MIN: f64 = 30.0;
const F_MAX: f64 = 11000.0;
/// Frames per network input chunk.
pub const CHUNK: usize = 1500;
/// Frames discarded at each chunk edge when stitching.
pub const BORDER: usize = 6;

/// Errors from this crate (inference errors are the caller's type `E`).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid input: {0}")]
    Input(String),
    #[error("resampling failed: {0}")]
    Resample(String),
    #[error("model returned {got} values, expected {expected}")]
    BadOutput { got: usize, expected: usize },
}

/// Log-mel spectrogram, row-major `(frames, N_MELS)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Spectrogram {
    pub frames: usize,
    pub data: Vec<f32>,
}

impl Spectrogram {
    pub fn frame(&self, i: usize) -> &[f32] {
        &self.data[i * N_MELS..(i + 1) * N_MELS]
    }
}

/// One network input: `CHUNK * N_MELS` values (shape `[1, CHUNK, N_MELS]`).
/// `start` is the spectrogram frame of row 0 (negative for the first chunk).
#[derive(Debug, Clone)]
pub struct Chunk {
    pub start: isize,
    pub data: Vec<f32>,
}

/// Frame-wise logits for a whole song (`FPS` frames per second).
#[derive(Debug, Clone, PartialEq)]
pub struct Logits {
    pub beat: Vec<f32>,
    pub downbeat: Vec<f32>,
}

/// Beat and downbeat times in seconds. Every downbeat is also a beat.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BeatGrid {
    pub beats: Vec<f64>,
    pub downbeats: Vec<f64>,
}

// ------------------------------------------------------------ preprocessing

/// Mono or multichannel planar audio at `sample_rate` -> log-mel spectrogram
/// with `len_at_22050 / 441 + 1` frames.
///
/// Channels are averaged (in f64, like upstream), resampled to
/// [`SAMPLE_RATE`] and fed to [`log_mel`].
pub fn preprocess(channels: &[&[f32]], sample_rate: u32) -> Result<Spectrogram, Error> {
    let mono = downmix(channels)?;
    let mono = resample_mono(&mono, sample_rate)?;
    let mono: Vec<f32> = mono.into_iter().map(|v| v as f32).collect();
    Ok(log_mel(&mono))
}

/// Interleaved variant of [`preprocess`].
pub fn preprocess_interleaved(
    samples: &[f32],
    n_channels: usize,
    sample_rate: u32,
) -> Result<Spectrogram, Error> {
    if n_channels == 0 || !samples.len().is_multiple_of(n_channels) {
        return Err(Error::Input(format!(
            "{} samples is not a multiple of {n_channels} channels",
            samples.len()
        )));
    }
    let planar: Vec<Vec<f32>> = (0..n_channels)
        .map(|c| {
            samples
                .iter()
                .skip(c)
                .step_by(n_channels)
                .copied()
                .collect()
        })
        .collect();
    let refs: Vec<&[f32]> = planar.iter().map(Vec::as_slice).collect();
    preprocess(&refs, sample_rate)
}

fn downmix(channels: &[&[f32]]) -> Result<Vec<f64>, Error> {
    let first = channels
        .first()
        .ok_or_else(|| Error::Input("no channels".into()))?;
    let n = first.len();
    if channels.iter().any(|c| c.len() != n) {
        return Err(Error::Input("channels differ in length".into()));
    }
    let k = channels.len() as f64;
    Ok((0..n)
        .map(|i| channels.iter().map(|c| c[i] as f64).sum::<f64>() / k)
        .collect())
}

/// Resamples mono f64 audio to [`SAMPLE_RATE`] with rubato's FFT resampler.
/// Output length is `round(len * 22050 / sample_rate)` (same as soxr),
/// delay-compensated.
pub fn resample_mono(x: &[f64], sample_rate: u32) -> Result<Vec<f64>, Error> {
    if sample_rate == 0 {
        return Err(Error::Input("sample rate is 0".into()));
    }
    let out_rate = SAMPLE_RATE as usize;
    let in_rate = sample_rate as usize;
    if in_rate == out_rate || x.is_empty() {
        return Ok(x.to_vec());
    }
    let len = x.len();
    let expected =
        ((len as u128 * out_rate as u128 + in_rate as u128 / 2) / in_rate as u128) as usize;
    let err = |e: &dyn std::fmt::Display| Error::Resample(e.to_string());
    // FFT length chosen so the anti-alias roll-off (~320 output samples per
    // FFT) is close to soxr HQ, which the reference uses (44.1 kHz input:
    // soxr -1.2/-9/-32 dB at 10.4/10.6/10.8 kHz, this -0.6/-9/-40 dB). The
    // top mel bands (up to 11 kHz) are sensitive to it. The output FFT length
    // must be even, else rubato's integer delay is off by half a sample.
    let g = gcd(in_rate, out_rate);
    let (in_unit, out_unit) = (in_rate / g, out_rate / g);
    let mut k = ((320 + out_unit / 2) / out_unit).max(1);
    if (out_unit * k) % 2 == 1 {
        k += 1;
    }
    let chunk = in_unit * k;
    let mut rs = FftFixedIn::<f64>::new(in_rate, out_rate, chunk, 1, 1).map_err(|e| err(&e))?;
    let delay = rs.output_delay();
    let mut out: Vec<f64> = Vec::with_capacity(expected + delay + 4096);
    let mut obuf = rs.output_buffer_allocate(true);
    let mut pos = 0;
    while pos < len {
        let need = rs.input_frames_next();
        let (_, written) = if len - pos >= need {
            let r = rs.process_into_buffer(&[&x[pos..pos + need]], &mut obuf, None);
            pos += need;
            r.map_err(|e| err(&e))?
        } else {
            let r = rs.process_partial_into_buffer(Some(&[&x[pos..]]), &mut obuf, None);
            pos = len;
            r.map_err(|e| err(&e))?
        };
        out.extend_from_slice(&obuf[0][..written]);
    }
    while out.len() < expected + delay {
        let (_, written) = rs
            .process_partial_into_buffer(None::<&[&[f64]]>, &mut obuf, None)
            .map_err(|e| err(&e))?;
        out.extend_from_slice(&obuf[0][..written]);
    }
    out.truncate(delay + expected);
    out.drain(..delay);
    Ok(out)
}

fn gcd(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn hz_to_mel(f: f64) -> f64 {
    let (f_sp, min_log_hz, min_log_mel) = (200.0 / 3.0, 1000.0, 15.0);
    let logstep = 6.4f64.ln() / 27.0;
    if f >= min_log_hz {
        min_log_mel + (f.max(1e-10) / min_log_hz).ln() / logstep
    } else {
        f / f_sp
    }
}

fn mel_to_hz(m: f64) -> f64 {
    let (f_sp, min_log_hz, min_log_mel) = (200.0 / 3.0, 1000.0, 15.0);
    let logstep = 6.4f64.ln() / 27.0;
    if m >= min_log_mel {
        min_log_hz * (logstep * (m - min_log_mel)).exp()
    } else {
        f_sp * m
    }
}

fn linspace(a: f64, b: f64, n: usize) -> Vec<f64> {
    // numpy.linspace: a + i * step, last element exactly b.
    let step = (b - a) / (n - 1) as f64;
    (0..n)
        .map(|i| if i == n - 1 { b } else { a + i as f64 * step })
        .collect()
}

/// Sparse mel filterbank: per mel band, the first FFT bin and its weights.
struct MelBank {
    bands: Vec<(usize, Vec<f32>)>,
}

/// Slaney mel filterbank without normalization
/// (`torchaudio.functional.melscale_fbanks(norm=None, mel_scale="slaney")`).
fn mel_bank() -> &'static MelBank {
    static BANK: OnceLock<MelBank> = OnceLock::new();
    BANK.get_or_init(|| {
        let n_freqs = N_FFT / 2 + 1;
        let all_freqs = linspace(0.0, (SAMPLE_RATE / 2) as f64, n_freqs);
        let m_pts = linspace(hz_to_mel(F_MIN), hz_to_mel(F_MAX), N_MELS + 2);
        let f_pts: Vec<f64> = m_pts.iter().map(|&m| mel_to_hz(m)).collect();
        let f_diff: Vec<f64> = f_pts.windows(2).map(|w| w[1] - w[0]).collect();
        let bands = (0..N_MELS)
            .map(|m| {
                let w: Vec<f32> = all_freqs
                    .iter()
                    .map(|&f| {
                        let down = -(f_pts[m] - f) / f_diff[m];
                        let up = (f_pts[m + 2] - f) / f_diff[m + 1];
                        down.min(up).max(0.0) as f32
                    })
                    .collect();
                let lo = w.iter().position(|&v| v > 0.0).unwrap_or(0);
                let hi = w.iter().rposition(|&v| v > 0.0).map_or(lo, |i| i + 1);
                (lo, w[lo..hi].to_vec())
            })
            .collect();
        MelBank { bands }
    })
}

/// numpy `pad(mode="reflect")` index (repeats the reflection for long pads).
fn reflect(i: isize, n: usize) -> usize {
    if n == 1 {
        return 0;
    }
    let period = 2 * (n as isize - 1);
    let mut j = i.rem_euclid(period);
    if j >= n as isize {
        j = period - j;
    }
    j as usize
}

/// Mono f32 at 22050 Hz -> `(len / 441 + 1, 128)` log-mel spectrogram.
///
/// `torch.stft(center=True, pad_mode="reflect", normalized=True)` with a
/// periodic Hann window, magnitude, Slaney mel without norm,
/// `log1p(1000 * x)`.
pub fn log_mel(mono: &[f32]) -> Spectrogram {
    if mono.is_empty() {
        return Spectrogram {
            frames: 0,
            data: Vec::new(),
        };
    }
    let pad = (N_FFT / 2) as isize;
    let n = mono.len();
    let frames = n / HOP + 1;
    let window: Vec<f32> = (0..N_FFT)
        .map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / N_FFT as f64).cos()) as f32)
        .collect();
    let norm = (N_FFT as f64).powf(-0.5) as f32;
    let fft: std::sync::Arc<dyn RealToComplex<f32>> =
        RealFftPlanner::<f32>::new().plan_fft_forward(N_FFT);
    let mut input = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let mut scratch = fft.make_scratch_vec();
    let mut mag = vec![0f32; N_FFT / 2 + 1];
    let bank = mel_bank();
    let mut data = vec![0f32; frames * N_MELS];
    for t in 0..frames {
        let s = (t * HOP) as isize - pad;
        if s >= 0 && s as usize + N_FFT <= n {
            for (k, v) in input.iter_mut().enumerate() {
                *v = mono[s as usize + k] * window[k];
            }
        } else {
            for (k, v) in input.iter_mut().enumerate() {
                *v = mono[reflect(s + k as isize, n)] * window[k];
            }
        }
        fft.process_with_scratch(&mut input, &mut spectrum, &mut scratch)
            .expect("fft buffer sizes");
        for (m, c) in mag.iter_mut().zip(&spectrum) {
            *m = c.norm() * norm;
        }
        let row = &mut data[t * N_MELS..(t + 1) * N_MELS];
        for (out, (lo, w)) in row.iter_mut().zip(&bank.bands) {
            let acc: f32 = w.iter().zip(&mag[*lo..]).map(|(a, b)| a * b).sum();
            *out = (1000.0 * acc).ln_1p();
        }
    }
    Spectrogram { frames, data }
}

// ------------------------------------------------------------- chunking

/// Upstream `split_piece(chunk_size=1500, border_size=6, avoid_short_end=True)`.
/// Every chunk is `CHUNK` frames, zero-padded where it runs past the piece
/// (left border of the first chunk, end of short pieces).
pub fn split(spec: &Spectrogram) -> Vec<Chunk> {
    let n = spec.frames as isize;
    let (chunk, border) = (CHUNK as isize, BORDER as isize);
    let mut starts: Vec<isize> = (-border..n - border)
        .step_by((chunk - 2 * border) as usize)
        .collect();
    if n > chunk - 2 * border {
        if let Some(last) = starts.last_mut() {
            *last = n - (chunk - border);
        }
    }
    starts
        .into_iter()
        .map(|s| {
            let mut data = vec![0f32; CHUNK * N_MELS];
            let lo = s.max(0);
            let hi = (s + chunk).min(n);
            for f in lo..hi {
                let r = (f - s) as usize;
                data[r * N_MELS..(r + 1) * N_MELS].copy_from_slice(spec.frame(f as usize));
            }
            Chunk { start: s, data }
        })
        .collect()
}

/// Upstream `aggregate_prediction(overlap_mode="keep_first")`. `preds[i]` are
/// the `(beat, downbeat)` logits (`CHUNK` each) for `chunks[i]`. Frames not
/// covered by any chunk get -1000.
pub fn aggregate(chunks: &[Chunk], preds: &[(Vec<f32>, Vec<f32>)], frames: usize) -> Logits {
    let mut beat = vec![-1000f32; frames];
    let mut downbeat = vec![-1000f32; frames];
    for (c, (b, d)) in chunks.iter().zip(preds).rev() {
        let lo = (c.start + BORDER as isize).max(0) as usize;
        let hi = ((c.start + (CHUNK - BORDER) as isize).max(0) as usize).min(frames);
        if lo >= hi {
            continue;
        }
        let off = (lo as isize - c.start) as usize;
        beat[lo..hi].copy_from_slice(&b[off..off + hi - lo]);
        downbeat[lo..hi].copy_from_slice(&d[off..off + hi - lo]);
    }
    Logits { beat, downbeat }
}

/// Network input shape for one chunk.
pub const INPUT_SHAPE: [usize; 3] = [1, CHUNK, N_MELS];

/// Splits, runs `model` on each chunk and stitches the logits.
///
/// `model(input, shape)` gets `CHUNK * N_MELS` values with shape
/// [`INPUT_SHAPE`] and must return `(beat, downbeat)` logits of `CHUNK`
/// values each (ONNX outputs `"beat"`, `"downbeat"`).
pub fn infer<E, F>(spec: &Spectrogram, mut model: F) -> Result<Logits, E>
where
    E: From<Error>,
    F: FnMut(&[f32], [usize; 3]) -> Result<(Vec<f32>, Vec<f32>), E>,
{
    let chunks = split(spec);
    let mut preds = Vec::with_capacity(chunks.len());
    for c in &chunks {
        let (b, d) = model(&c.data, INPUT_SHAPE)?;
        for got in [b.len(), d.len()] {
            if got != CHUNK {
                return Err(Error::BadOutput {
                    got,
                    expected: CHUNK,
                }
                .into());
            }
        }
        preds.push((b, d));
    }
    Ok(aggregate(&chunks, &preds, spec.frames))
}

// ------------------------------------------------------- postprocessing

/// Frames that are the maximum of their 7-frame neighbourhood and > 0.
fn peaks(logits: &[f32]) -> Vec<usize> {
    let n = logits.len();
    (0..n)
        .filter(|&i| {
            let v = logits[i];
            v > 0.0
                && logits[i.saturating_sub(3)..(i + 4).min(n)]
                    .iter()
                    .all(|&w| w <= v)
        })
        .collect()
}

/// Merges runs of peaks at most `width` frames apart into their running mean.
fn dedup(frames: &[usize], width: f64) -> Vec<f64> {
    let mut out = Vec::new();
    let mut it = frames.iter().map(|&f| f as f64);
    let Some(mut p) = it.next() else {
        return out;
    };
    let mut c = 1.0;
    for p2 in it {
        if p2 - p <= width {
            c += 1.0;
            p += (p2 - p) / c;
        } else {
            out.push(p);
            p = p2;
            c = 1.0;
        }
    }
    out.push(p);
    out
}

/// Upstream `Postprocessor(type="minimal")`: logits -> beat/downbeat times
/// (s). Downbeats are snapped to the nearest beat and deduplicated.
pub fn postprocess(logits: &Logits) -> BeatGrid {
    let beats: Vec<f64> = dedup(&peaks(&logits.beat), 1.0)
        .into_iter()
        .map(|f| f / FPS)
        .collect();
    let mut downbeats: Vec<f64> = dedup(&peaks(&logits.downbeat), 1.0)
        .into_iter()
        .map(|f| f / FPS)
        .collect();
    if !beats.is_empty() {
        for d in downbeats.iter_mut() {
            let mut best = beats[0];
            for &b in &beats[1..] {
                if (b - *d).abs() < (best - *d).abs() {
                    best = b;
                }
            }
            *d = best;
        }
    }
    downbeats.sort_by(f64::total_cmp);
    downbeats.dedup();
    BeatGrid { beats, downbeats }
}

/// Full pipeline: planar audio -> beat grid, with `model` as in [`infer`].
pub fn track<E, F>(channels: &[&[f32]], sample_rate: u32, model: F) -> Result<BeatGrid, E>
where
    E: From<Error>,
    F: FnMut(&[f32], [usize; 3]) -> Result<(Vec<f32>, Vec<f32>), E>,
{
    let spec = preprocess(channels, sample_rate)?;
    Ok(postprocess(&infer(&spec, model)?))
}

// ------------------------------------------------------------ UI helpers

impl BeatGrid {
    /// Median inter-beat interval in seconds (`None` with < 2 beats).
    pub fn median_interval(&self) -> Option<f64> {
        let mut d: Vec<f64> = self.beats.windows(2).map(|w| w[1] - w[0]).collect();
        if d.is_empty() {
            return None;
        }
        d.sort_by(f64::total_cmp);
        let m = d.len() / 2;
        Some(if d.len() % 2 == 1 {
            d[m]
        } else {
            0.5 * (d[m - 1] + d[m])
        })
    }

    /// Tempo estimate: 60 / median inter-beat interval.
    pub fn tempo_bpm(&self) -> Option<f64> {
        self.median_interval()
            .filter(|&d| d > 0.0)
            .map(|d| 60.0 / d)
    }

    /// Beats in each complete bar (from one downbeat up to the next). Beats
    /// before the first downbeat (pickup) and after the last are not counted.
    pub fn beats_per_bar(&self) -> Vec<usize> {
        let idx: Vec<usize> = self
            .downbeats
            .iter()
            .map(|&d| self.beats.partition_point(|&b| b < d))
            .collect();
        idx.windows(2).map(|w| w[1] - w[0]).collect()
    }

    /// Most common [`Self::beats_per_bar`] value (ties -> smaller value).
    pub fn most_common_beats_per_bar(&self) -> Option<usize> {
        let bars = self.beats_per_bar();
        let max = *bars.iter().max()?;
        let mut counts = vec![0usize; max + 1];
        for &b in &bars {
            counts[b] += 1;
        }
        counts
            .iter()
            .enumerate()
            .filter(|&(v, _)| v > 0)
            .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(&a.0)))
            .map(|(v, _)| v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflect_matches_numpy() {
        // np.pad([0,1,2,3], 5, mode="reflect") -> 1 2 3 2 1 | 0 1 2 3 | 2 1 0 1 2
        let got: Vec<usize> = (-5..9).map(|i| reflect(i, 4)).collect();
        assert_eq!(got, [1, 2, 3, 2, 1, 0, 1, 2, 3, 2, 1, 0, 1, 2]);
    }

    #[test]
    fn frame_count() {
        assert_eq!(log_mel(&vec![0.0; 22050]).frames, 51);
        assert_eq!(log_mel(&[0.1; 10]).frames, 1);
    }

    #[test]
    fn split_and_aggregate_cover_everything() {
        for n in [1, 5, 100, 1488, 1489, 1500, 1501, 2976, 4000, 15000] {
            let spec = Spectrogram {
                frames: n,
                data: vec![0.0; n * N_MELS],
            };
            let chunks = split(&spec);
            let preds: Vec<_> = chunks
                .iter()
                .map(|c| {
                    let v: Vec<f32> = (0..CHUNK as isize).map(|r| (c.start + r) as f32).collect();
                    (v.clone(), v)
                })
                .collect();
            let l = aggregate(&chunks, &preds, n);
            let want: Vec<f32> = (0..n).map(|i| i as f32).collect();
            assert_eq!(l.beat, want, "n={n}");
        }
    }

    #[test]
    fn peaks_and_dedup() {
        let mut l = vec![-1.0f32; 40];
        l[5] = 2.0;
        l[6] = 2.0; // plateau -> merged to 5.5
        l[20] = 1.0;
        l[22] = 3.0; // 20 is suppressed by 22 within the window
        l[35] = -0.5; // not > 0
        let p = peaks(&l);
        assert_eq!(p, [5, 6, 22]);
        assert_eq!(dedup(&p, 1.0), [5.5, 22.0]);
    }

    #[test]
    fn grid_helpers() {
        let beats: Vec<f64> = (0..17).map(|i| 0.5 * i as f64).collect();
        let g = BeatGrid {
            downbeats: vec![0.5, 2.5, 4.5, 6.0, 8.0],
            beats,
        };
        assert_eq!(g.beats_per_bar(), [4, 4, 3, 4]);
        assert_eq!(g.most_common_beats_per_bar(), Some(4));
        assert!((g.tempo_bpm().unwrap() - 120.0).abs() < 1e-9);
        assert_eq!(BeatGrid::default().tempo_bpm(), None);
        assert_eq!(BeatGrid::default().most_common_beats_per_bar(), None);
    }

    #[test]
    fn resample_length_matches_soxr() {
        // soxr.resample(np.zeros(44100*3+7), 44100, 22050) has 66154 samples.
        assert_eq!(
            resample_mono(&vec![0.0; 44100 * 3 + 7], 44100)
                .unwrap()
                .len(),
            66154
        );
        assert_eq!(
            resample_mono(&vec![0.0; 48000], 48000).unwrap().len(),
            22050
        );
    }
}
