//! Model adapters: STFT glue + chunking around an ONNX network core.
//!
//! - `demucs-hybrid`: `tools/reference/demucs_hybrid.py`
//! - `roformer-mask`: `tools/reference/roformer_separate.py`

use ort::session::Session;
use ort::value::TensorRef;
use shredlab_dsp::chunk;
use shredlab_dsp::{hamming_symmetric, Complex64, HybridStft, Stft};

use crate::manifest::{AdapterKind, Manifest};
use crate::{Error, Result};

/// Runs a model over audio. All buffers are flat, last axis fastest:
/// inputs `(channels, samples)`, outputs `(stems, channels, samples)`.
pub trait Adapter: Send {
    /// Samples per model call.
    fn segment(&self) -> usize;
    fn n_stems(&self) -> usize;
    fn channels(&self) -> usize {
        2
    }

    /// One raw model window: `(C, segment)` -> `(N, C, segment)`.
    fn run_window(&mut self, seg: &[f32]) -> Result<Vec<f32>>;

    /// Self-test path: exactly one segment, no chunking/weights, with the
    /// adapter's song normalization (if any) computed over that segment.
    fn run_single_segment(&mut self, channels: &[&[f32]]) -> Result<Vec<f32>>;

    /// Song normalization `(mean, std)` the adapter applies around its
    /// chunked run (`(x - mean) / std` before, `y * std + mean` after), or
    /// `None` if it feeds the audio as is.
    fn song_stats(&self, _channels: &[&[f32]]) -> Option<(f32, f32)> {
        None
    }

    /// Chunked full-song run on input that is already normalized (see
    /// [`Adapter::song_stats`]); output in the same normalized domain.
    /// `before_chunk` is called before each model call with (chunks done,
    /// total) and may abort by returning an error.
    fn separate_normalized(
        &mut self,
        channels: &[&[f32]],
        before_chunk: &mut dyn FnMut(usize, usize) -> Result<()>,
    ) -> Result<Vec<f32>>;

    /// Full-song separation: normalize (if the adapter does), chunked run,
    /// denormalize. `before_chunk` as in [`Adapter::separate_normalized`].
    fn separate(
        &mut self,
        channels: &[&[f32]],
        before_chunk: &mut dyn FnMut(usize, usize) -> Result<()>,
    ) -> Result<Vec<f32>> {
        let Some((mean, std)) = self.song_stats(channels) else {
            return self.separate_normalized(channels, before_chunk);
        };
        let norm = normalized(channels, mean, std);
        let views: Vec<&[f32]> = norm.iter().map(Vec::as_slice).collect();
        let mut y = self.separate_normalized(&views, before_chunk)?;
        denormalize(&mut y, mean, std);
        Ok(y)
    }
}

/// Creates the adapter for a manifest around a session.
pub fn create(manifest: &Manifest, session: Session) -> Result<Box<dyn Adapter>> {
    Ok(match manifest.adapter {
        AdapterKind::DemucsHybrid => Box::new(DemucsHybrid::new(manifest, session)?),
        AdapterKind::RoformerMask => Box::new(RoformerMask::new(manifest, session)?),
    })
}

fn input_shape(session: &Session, name: &str) -> Result<Vec<i64>> {
    session
        .inputs()
        .iter()
        .find(|i| i.name() == name)
        .and_then(|i| i.dtype().tensor_shape())
        .map(|s| s.iter().copied().collect())
        .ok_or_else(|| Error::Model(format!("graph has no tensor input {name:?}")))
}

fn check_dim(what: &str, graph: i64, expected: usize) -> Result<()> {
    if graph >= 0 && graph as usize != expected {
        return Err(Error::Model(format!(
            "{what}: graph has {graph}, manifest implies {expected}"
        )));
    }
    Ok(())
}

fn ort_err(e: ort::Error) -> Error {
    Error::Runtime(e.to_string())
}

/// Copies `(C, segment)` and normalizes in place.
pub(crate) fn normalized(channels: &[&[f32]], mean: f32, std: f32) -> Vec<Vec<f32>> {
    channels
        .iter()
        .map(|c| c.iter().map(|v| (v - mean) / std).collect())
        .collect()
}

/// Inverse of [`normalized`], in place: `v * std + mean`.
pub(crate) fn denormalize(y: &mut [f32], mean: f32, std: f32) {
    for v in y {
        *v = *v * std + mean;
    }
}

// --- demucs-hybrid -----------------------------------------------------------

pub struct DemucsHybrid {
    session: Session,
    stft: HybridStft,
    segment: usize,
    n_stems: usize,
    overlap: f64,
}

impl DemucsHybrid {
    pub fn new(m: &Manifest, session: Session) -> Result<Self> {
        let stft = HybridStft::new(m.stft.n_fft, m.stft.hop);
        let segment = m.segment_samples;
        let mix = input_shape(&session, "mix")?;
        let mag = input_shape(&session, "mag")?;
        if mix.len() != 3 || mag.len() != 4 {
            return Err(Error::Model(
                "demucs-hybrid: expected inputs mix (1,2,S) and mag (1,4,F,T)".into(),
            ));
        }
        check_dim("mix samples", mix[2], segment)?;
        check_dim("mag channels", mag[1], 2 * m.channels)?;
        check_dim("mag bins", mag[2], stft.bins())?;
        check_dim("mag frames", mag[3], stft.frames(segment))?;
        Ok(Self {
            session,
            stft,
            segment,
            n_stems: m.stems.len(),
            overlap: m.overlap,
        })
    }

    /// Stage 1: CaC spectrogram `(2C, bins, frames)` of a `(C, S)` window,
    /// channels `[c0.re, c0.im, c1.re, c1.im]` (graph input `mag`).
    pub fn spec_input(&self, seg: &[f32]) -> Vec<f32> {
        let (s, nc) = (self.segment, 2usize);
        assert_eq!(seg.len(), nc * s);
        let plane = self.stft.bins() * self.stft.frames(s);
        let mut mag = vec![0.0f32; 2 * nc * plane];
        for c in 0..nc {
            let (re, rest) = mag[2 * c * plane..].split_at_mut(plane);
            self.stft
                .spec(&seg[c * s..(c + 1) * s], re, &mut rest[..plane]);
        }
        mag
    }

    /// Stage 2: graph run -> (`wave` (N, C, S), `spec` (N, 2C, bins, frames)).
    pub fn run_graph(&mut self, seg: &[f32], mag: &[f32]) -> Result<(Vec<f32>, Vec<f32>)> {
        let (s, nc) = (self.segment, 2usize);
        let bins = self.stft.bins();
        let frames = self.stft.frames(s);
        let plane = bins * frames;
        let mix_t = TensorRef::from_array_view(([1usize, nc, s], seg)).map_err(ort_err)?;
        let mag_t =
            TensorRef::from_array_view(([1usize, 2 * nc, bins, frames], mag)).map_err(ort_err)?;
        let outputs = self
            .session
            .run(ort::inputs!["mix" => mix_t, "mag" => mag_t])
            .map_err(ort_err)?;
        let (_, wave) = outputs["wave"]
            .try_extract_tensor::<f32>()
            .map_err(ort_err)?;
        let (_, spec) = outputs["spec"]
            .try_extract_tensor::<f32>()
            .map_err(ort_err)?;
        let n = self.n_stems;
        if wave.len() != n * nc * s || spec.len() != n * 2 * nc * plane {
            return Err(Error::Model(format!(
                "unexpected output sizes wave {} spec {} for {n} stems",
                wave.len(),
                spec.len()
            )));
        }
        Ok((wave.to_vec(), spec.to_vec()))
    }

    /// Stage 3: `wave + ispec(spec)` -> (N, C, S).
    pub fn combine(&self, mut wave: Vec<f32>, spec: &[f32]) -> Vec<f32> {
        let (s, nc) = (self.segment, 2usize);
        let plane = self.stft.bins() * self.stft.frames(s);
        for st in 0..self.n_stems {
            for c in 0..nc {
                let base = (st * 2 * nc + 2 * c) * plane;
                let y = self.stft.ispec(
                    &spec[base..base + plane],
                    &spec[base + plane..base + 2 * plane],
                    s,
                );
                for (o, v) in wave[(st * nc + c) * s..][..s].iter_mut().zip(&y) {
                    *o += v;
                }
            }
        }
        wave
    }
}

impl Adapter for DemucsHybrid {
    fn segment(&self) -> usize {
        self.segment
    }

    fn n_stems(&self) -> usize {
        self.n_stems
    }

    fn run_window(&mut self, seg: &[f32]) -> Result<Vec<f32>> {
        let mag = self.spec_input(seg);
        let (wave, spec) = self.run_graph(seg, &mag)?;
        Ok(self.combine(wave, &spec))
    }

    fn run_single_segment(&mut self, channels: &[&[f32]]) -> Result<Vec<f32>> {
        let (mean, std) = chunk::song_stats(channels);
        let x: Vec<f32> = normalized(channels, mean, std).concat();
        let mut y = self.run_window(&x)?;
        denormalize(&mut y, mean, std);
        Ok(y)
    }

    fn song_stats(&self, channels: &[&[f32]]) -> Option<(f32, f32)> {
        Some(chunk::song_stats(channels))
    }

    fn separate_normalized(
        &mut self,
        channels: &[&[f32]],
        before_chunk: &mut dyn FnMut(usize, usize) -> Result<()>,
    ) -> Result<Vec<f32>> {
        let total = chunk::chunk_offsets(channels[0].len(), self.segment, self.overlap).len();
        let mut k = 0;
        let (segment, overlap, n) = (self.segment, self.overlap, self.n_stems);
        chunk::apply_demucs(
            channels,
            segment,
            overlap,
            n,
            |w| {
                before_chunk(k, total)?;
                k += 1;
                self.run_window(w)
            },
            &mut |_, _| {},
        )
    }
}

// --- roformer-mask -----------------------------------------------------------

pub struct RoformerMask {
    session: Session,
    input_name: String,
    stft: Stft,
    chunk: usize,
    frames: usize,
    n_stems: usize,
    num_overlap: usize,
}

impl RoformerMask {
    pub fn new(m: &Manifest, session: Session) -> Result<Self> {
        let stft = Stft::new(m.stft.n_fft, m.stft.hop);
        let input = session
            .inputs()
            .first()
            .ok_or_else(|| Error::Model("roformer-mask: graph has no inputs".into()))?;
        let input_name = input.name().to_string();
        let shape = input_shape(&session, &input_name)?;
        if shape.len() != 5 {
            return Err(Error::Model(
                "roformer-mask: expected input (1, 2, F, T, 2)".into(),
            ));
        }
        let chunk = m.segment_samples;
        let frames = stft.frames(chunk);
        check_dim("stft channels", shape[1], m.channels)?;
        check_dim("stft bins", shape[2], stft.bins())?;
        check_dim("stft frames", shape[3], frames)?;
        Ok(Self {
            session,
            input_name,
            stft,
            chunk,
            frames,
            n_stems: m.stems.len(),
            num_overlap: m.overlap as usize,
        })
    }

    /// Stage 1a: complex STFT `(bins, frames)` per channel of a `(C, chunk)` window.
    pub fn stft_channels(&self, x: &[f32]) -> Vec<Vec<Complex64>> {
        let s = self.chunk;
        assert_eq!(x.len(), 2 * s);
        (0..2)
            .map(|c| self.stft.forward(&x[c * s..(c + 1) * s]))
            .collect()
    }

    /// Stage 1b: graph input `(C, bins, frames, 2)`, real/imag last.
    pub fn graph_input(specs: &[Vec<Complex64>]) -> Vec<f32> {
        specs
            .iter()
            .flat_map(|sp| sp.iter().flat_map(|z| [z.re as f32, z.im as f32]))
            .collect()
    }

    /// Stage 2: graph run -> mask `(N, bins*C, frames, 2)`.
    pub fn run_graph(&mut self, inp: &[f32]) -> Result<Vec<f32>> {
        let (nc, bins, frames) = (2usize, self.stft.bins(), self.frames);
        let t =
            TensorRef::from_array_view(([1usize, nc, bins, frames, 2], inp)).map_err(ort_err)?;
        let outputs = self
            .session
            .run(ort::inputs![self.input_name.as_str() => t])
            .map_err(ort_err)?;
        let (_, mask) = outputs[0].try_extract_tensor::<f32>().map_err(ort_err)?;
        let n = self.n_stems;
        if mask.len() != n * bins * nc * frames * 2 {
            return Err(Error::Model(format!(
                "unexpected mask size {} for {n} stems",
                mask.len()
            )));
        }
        Ok(mask.to_vec())
    }

    /// Stage 3: complex mask multiply + iSTFT -> (N, C, chunk). The mask's
    /// frequency axis is (freq, channel) interleaved, channel fastest.
    pub fn apply_mask(&self, specs: &[Vec<Complex64>], mask: &[f32]) -> Vec<f32> {
        let (s, nc, bins, frames, n) = (
            self.chunk,
            2usize,
            self.stft.bins(),
            self.frames,
            self.n_stems,
        );
        let mut out = vec![0.0f32; n * nc * s];
        let mut z = vec![Complex64::new(0.0, 0.0); bins * frames];
        for st in 0..n {
            for c in 0..nc {
                for f in 0..bins {
                    let mrow = ((st * bins + f) * nc + c) * frames;
                    for t in 0..frames {
                        let m = Complex64::new(
                            mask[(mrow + t) * 2] as f64,
                            mask[(mrow + t) * 2 + 1] as f64,
                        );
                        z[f * frames + t] = specs[c][f * frames + t] * m;
                    }
                }
                let y = self.stft.inverse(&z, frames, s);
                out[(st * nc + c) * s..][..s].copy_from_slice(&y);
            }
        }
        out
    }
}

impl Adapter for RoformerMask {
    fn segment(&self) -> usize {
        self.chunk
    }

    fn n_stems(&self) -> usize {
        self.n_stems
    }

    fn run_window(&mut self, x: &[f32]) -> Result<Vec<f32>> {
        let specs = self.stft_channels(x);
        let inp = Self::graph_input(&specs);
        let mask = self.run_graph(&inp)?;
        Ok(self.apply_mask(&specs, &mask))
    }

    fn run_single_segment(&mut self, channels: &[&[f32]]) -> Result<Vec<f32>> {
        self.run_window(&channels.concat())
    }

    fn separate_normalized(
        &mut self,
        channels: &[&[f32]],
        before_chunk: &mut dyn FnMut(usize, usize) -> Result<()>,
    ) -> Result<Vec<f32>> {
        let chunk_len = self.chunk;
        let step = chunk_len / self.num_overlap.max(1);
        let starts = chunk::roformer_chunk_starts(channels[0].len(), chunk_len, step);
        let window = hamming_symmetric(chunk_len);
        let total = starts.len();
        let mut k = 0;
        let n = self.n_stems;
        chunk::apply_windowed(
            channels,
            chunk_len,
            &starts,
            &window,
            n,
            |w| {
                before_chunk(k, total)?;
                k += 1;
                self.run_window(w)
            },
            &mut |_, _| {},
        )
    }
}
