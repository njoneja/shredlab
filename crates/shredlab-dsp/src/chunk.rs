// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Chunking and overlap-add for full-song separation.
//!
//! - demucs `apply_model` (split=True, transition_power=1, shifts=0):
//!   [`chunk_offsets`], [`triangular_weights`], [`apply_demucs`]. Reference:
//!   `chunked_apply` / `run_segment` in `tools/reference/demucs_hybrid.py`.
//! - MSST-style windowed chunks (audio-separator `MDXCSeparator.demix`,
//!   roformer branch): [`roformer_chunk_starts`], [`apply_windowed`].
//!   Reference: `separate` in `tools/reference/roformer_separate.py`.
//! - Song normalization: [`song_stats`].

/// Song normalization stats: mean and population std of the mono mix
/// (channel average), std floored at f32 epsilon. Accumulated in f64.
pub fn song_stats(channels: &[&[f32]]) -> (f32, f32) {
    let len = channels.first().map_or(0, |c| c.len());
    if len == 0 {
        return (0.0, f32::EPSILON);
    }
    let nc = channels.len() as f32;
    let mono = |i: usize| -> f64 {
        // numpy: mix.mean(0) in float32
        (channels.iter().map(|c| c[i]).sum::<f32>() / nc) as f64
    };
    let mean = (0..len).map(mono).sum::<f64>() / len as f64;
    let var = (0..len).map(|i| (mono(i) - mean).powi(2)).sum::<f64>() / len as f64;
    (mean as f32, (var.sqrt() as f32).max(f32::EPSILON))
}

/// demucs `apply_model` weights (transition_power=1), max-normalized:
/// `[1, 2, ..., S/2, S - S/2, ..., 1] / max`.
pub fn triangular_weights(segment: usize) -> Vec<f32> {
    let half = segment / 2;
    let w: Vec<f64> = (1..=half)
        .map(|v| v as f64)
        .chain((1..=segment - half).rev().map(|v| v as f64))
        .collect();
    let max = w.iter().cloned().fold(0.0, f64::max);
    w.into_iter().map(|v| (v / max) as f32).collect()
}

/// Chunk offsets: `0, stride, 2*stride, ... < length` with
/// `stride = int((1 - overlap) * segment)`.
pub fn chunk_offsets(length: usize, segment: usize, overlap: f64) -> Vec<usize> {
    let stride = (((1.0 - overlap) * segment as f64) as usize).max(1);
    (0..length).step_by(stride).collect()
}

/// Builds the model input for the chunk `[offset, offset + n)`: the chunk is
/// centered in a full `segment`-long window filled with the surrounding audio
/// (zeros beyond the ends), like demucs' `TensorChunk.padded`.
/// Returns the flat `(channels, segment)` window and the chunk's start
/// inside it.
pub fn padded_window(mix: &[&[f32]], offset: usize, n: usize, segment: usize) -> (Vec<f32>, usize) {
    let total = mix[0].len() as isize;
    let start = offset as isize - ((segment - n) / 2) as isize;
    let mut out = vec![0.0f32; mix.len() * segment];
    let lo = start.max(0);
    let hi = total.min(start + segment as isize);
    for (c, ch) in mix.iter().enumerate() {
        if hi > lo {
            let dst = c * segment + (lo - start) as usize;
            out[dst..dst + (hi - lo) as usize].copy_from_slice(&ch[lo as usize..hi as usize]);
        }
    }
    (out, (offset as isize - start) as usize)
}

/// Progress of a chunked run: (chunks done, total chunks).
pub type ChunkProgress<'a> = &'a mut dyn FnMut(usize, usize);

/// demucs `apply_model` chunking (no normalization).
///
/// `run` maps a flat `(channels, segment)` window to a flat
/// `(n_stems, channels, segment)` output; it may fail (e.g. cancellation),
/// which aborts the loop. Returns flat `(n_stems, channels, length)`.
pub fn apply_demucs<E>(
    mix: &[&[f32]],
    segment: usize,
    overlap: f64,
    n_stems: usize,
    mut run: impl FnMut(&[f32]) -> Result<Vec<f32>, E>,
    progress: ChunkProgress<'_>,
) -> Result<Vec<f32>, E> {
    let nc = mix.len();
    let length = mix[0].len();
    let weight = triangular_weights(segment);
    let mut out = vec![0.0f32; n_stems * nc * length];
    let mut total = vec![0.0f32; length];
    let offsets = chunk_offsets(length, segment, overlap);
    for (k, &offset) in offsets.iter().enumerate() {
        let n = segment.min(length - offset);
        let (window, left) = padded_window(mix, offset, n, segment);
        let y = run(&window)?;
        assert_eq!(y.len(), n_stems * nc * segment, "model output size");
        for s in 0..n_stems {
            for c in 0..nc {
                let src = &y[(s * nc + c) * segment + left..][..n];
                let dst = &mut out[(s * nc + c) * length + offset..][..n];
                for ((d, v), w) in dst.iter_mut().zip(src).zip(&weight[..n]) {
                    *d += v * w;
                }
            }
        }
        for (t, w) in total[offset..offset + n].iter_mut().zip(&weight[..n]) {
            *t += w;
        }
        progress(k + 1, offsets.len());
    }
    for row in out.chunks_mut(length) {
        for (v, t) in row.iter_mut().zip(&total) {
            *v /= t;
        }
    }
    Ok(out)
}

/// Chunk starts of audio-separator's roformer demix: `0, step, 2*step, ...`
/// while `start + chunk < length`, then a final tail start
/// `max(length - chunk, 0)` (not repeated).
pub fn roformer_chunk_starts(length: usize, chunk: usize, step: usize) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut offset = 0;
    while offset < length {
        if offset + chunk >= length {
            let tail = length.saturating_sub(chunk);
            if starts.last() != Some(&tail) {
                starts.push(tail);
            }
            break;
        }
        starts.push(offset);
        offset += step.max(1);
    }
    starts
}

/// Windowed overlap-add over fixed-size chunks (MSST / audio-separator).
///
/// Each chunk `mix[:, s..s+chunk]` (zero-padded to `chunk` at the end) goes
/// through `run` (flat `(channels, chunk)` -> flat `(n_stems, channels,
/// chunk)`), is multiplied by `window` and accumulated; the result is
/// divided by the summed window (floor 1e-10). Returns flat
/// `(n_stems, channels, length)`.
pub fn apply_windowed<E>(
    mix: &[&[f32]],
    chunk: usize,
    starts: &[usize],
    window: &[f32],
    n_stems: usize,
    mut run: impl FnMut(&[f32]) -> Result<Vec<f32>, E>,
    progress: ChunkProgress<'_>,
) -> Result<Vec<f32>, E> {
    assert_eq!(window.len(), chunk);
    let nc = mix.len();
    let length = mix[0].len();
    let mut out = vec![0.0f32; n_stems * nc * length];
    let mut counter = vec![0.0f32; length];
    for (k, &s) in starts.iter().enumerate() {
        let n = chunk.min(length - s);
        let mut part = vec![0.0f32; nc * chunk];
        for (c, ch) in mix.iter().enumerate() {
            part[c * chunk..c * chunk + n].copy_from_slice(&ch[s..s + n]);
        }
        let y = run(&part)?;
        assert_eq!(y.len(), n_stems * nc * chunk, "model output size");
        for sc in 0..n_stems * nc {
            let src = &y[sc * chunk..][..n];
            let dst = &mut out[sc * length + s..][..n];
            for ((d, v), w) in dst.iter_mut().zip(src).zip(&window[..n]) {
                *d += v * w;
            }
        }
        for (t, w) in counter[s..s + n].iter_mut().zip(&window[..n]) {
            *t += w;
        }
        progress(k + 1, starts.len());
    }
    for row in out.chunks_mut(length) {
        for (v, t) in row.iter_mut().zip(&counter) {
            *v /= t.max(1e-10);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights() {
        assert_eq!(triangular_weights(4), vec![0.5, 1.0, 1.0, 0.5]);
        let w = triangular_weights(5);
        assert_eq!(w, vec![1.0 / 3.0, 2.0 / 3.0, 1.0, 2.0 / 3.0, 1.0 / 3.0]);
    }

    #[test]
    fn offsets() {
        assert_eq!(chunk_offsets(10, 4, 0.25), vec![0, 3, 6, 9]);
        assert_eq!(
            chunk_offsets(343980 * 2, 343980, 0.25),
            vec![0, 257985, 515970]
        );
    }

    #[test]
    fn roformer_starts() {
        assert_eq!(roformer_chunk_starts(10, 4, 2), vec![0, 2, 4, 6]);
        assert_eq!(roformer_chunk_starts(11, 4, 2), vec![0, 2, 4, 6, 7]);
        assert_eq!(roformer_chunk_starts(3, 4, 2), vec![0]);
        assert_eq!(roformer_chunk_starts(4, 4, 2), vec![0]);
    }

    #[test]
    fn padded_window_centers_short_chunk() {
        let a: Vec<f32> = (0..10).map(|v| v as f32).collect();
        let (w, left) = padded_window(&[&a], 8, 2, 6);
        // start = 8 - 2 = 6 -> [6, 7, 8, 9, 0, 0]
        assert_eq!(w, vec![6.0, 7.0, 8.0, 9.0, 0.0, 0.0]);
        assert_eq!(left, 2);
        let (w, left) = padded_window(&[&a], 0, 6, 6);
        assert_eq!(w, vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(left, 0);
    }

    #[test]
    fn identity_model_reconstructs_input() {
        let l: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.1).sin()).collect();
        let r: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.03).cos()).collect();
        let mix = [&l[..], &r[..]];
        // two "stems": identity and 2x
        let run = |w: &[f32]| -> Result<Vec<f32>, ()> {
            let mut o = w.to_vec();
            o.extend(w.iter().map(|v| v * 2.0));
            Ok(o)
        };
        let out = apply_demucs(&mix, 128, 0.25, 2, run, &mut |_, _| {}).unwrap();
        for i in 0..1000 {
            assert!((out[i] - l[i]).abs() < 1e-5);
            assert!((out[1000 + i] - r[i]).abs() < 1e-5);
            assert!((out[2000 + i] - 2.0 * l[i]).abs() < 1e-5);
        }
        let starts = roformer_chunk_starts(1000, 128, 64);
        let win = crate::hamming_symmetric(128);
        let out = apply_windowed(&mix, 128, &starts, &win, 2, run, &mut |_, _| {}).unwrap();
        for i in 0..1000 {
            assert!((out[i] - l[i]).abs() < 1e-5);
            assert!((out[3000 + i] - 2.0 * r[i]).abs() < 1e-5);
        }
    }

    #[test]
    fn stats() {
        let l = [1.0f32, 3.0];
        let r = [1.0f32, 3.0];
        let (m, s) = song_stats(&[&l, &r]);
        assert_eq!(m, 2.0);
        assert_eq!(s, 1.0);
    }
}
