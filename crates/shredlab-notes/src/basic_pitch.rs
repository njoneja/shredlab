// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! The model side of polyphonic note detection with Spotify's Basic Pitch
//! (`nmp.onnx`, Apache-2.0): resampling, windowing and stitching as the
//! upstream Python package's `inference.py`, and the note events of its
//! `note_creation.py` (`output_to_notes_polyphonic`). The network itself
//! runs in the caller's closure, so this crate stays free of ONNX Runtime.
//! The lead line, bends and chordal stretches are in `lead.rs`.

/// Model sample rate.
pub const SAMPLE_RATE: u32 = 22_050;
/// Samples per model window (input shape `[1, WINDOW, 1]`).
pub const WINDOW: usize = 43_844;
/// Frames per window in each model output.
pub const FRAMES_PER_WINDOW: usize = 172;
/// Note and onset bins: one per semitone from A0 (MIDI 21).
pub const N_NOTES: usize = 88;
/// Contour bins: three per semitone from A0.
pub const N_CONTOUR: usize = 264;
/// Samples per model frame.
const FFT_HOP: usize = 256;
/// Frames of overlap between windows.
const OVERLAP_FRAMES: usize = 30;
pub(crate) const MIDI_OFFSET: usize = 21;
const OVERLAP: usize = OVERLAP_FRAMES * FFT_HOP;
const HOP: usize = WINDOW - OVERLAP;
const TRIM: usize = OVERLAP_FRAMES / 2;
/// Seconds per model frame.
pub(crate) const FRAME_S: f64 = FFT_HOP as f64 / SAMPLE_RATE as f64;

/// Upstream note creation defaults: an onset peak at least this high starts
/// a note, which lasts while the note activation stays at least this high,
/// allowing this many frames under it.
const ONSET_THRESHOLD: f32 = 0.5;
const FRAME_THRESHOLD: f32 = 0.3;
const ENERGY_TOLERANCE: usize = 11;

/// The model's output for one window, each row-major `[frames][bins]`
/// (`FRAMES_PER_WINDOW` frames): `note` and `onset` with [`N_NOTES`] bins,
/// `contour` with [`N_CONTOUR`].
#[derive(Debug, Clone, Default)]
pub struct WindowOutput {
    pub note: Vec<f32>,
    pub onset: Vec<f32>,
    pub contour: Vec<f32>,
}

/// Stitched activations of a whole stem.
pub(crate) struct Activations {
    /// Frames.
    pub n: usize,
    /// `[n][N_NOTES]`.
    pub note: Vec<f32>,
    /// `[n][N_NOTES]`.
    pub onset: Vec<f32>,
    /// `[n][N_CONTOUR]`.
    pub contour: Vec<f32>,
    /// Start time of each frame in seconds (increasing; not evenly spaced
    /// across window boundaries).
    pub time: Vec<f64>,
    /// Level of the audio around each frame (dBFS, 1,024 samples).
    pub level_db: Vec<f32>,
}

impl Activations {
    pub fn note_at(&self, t: usize, k: usize) -> f32 {
        self.note[t * N_NOTES + k]
    }

    pub fn onset_at(&self, t: usize, k: usize) -> f32 {
        self.onset[t * N_NOTES + k]
    }

    pub fn contour_row(&self, t: usize) -> &[f32] {
        &self.contour[t * N_CONTOUR..(t + 1) * N_CONTOUR]
    }

    /// Time of frame `f`, extrapolated past the end.
    pub fn time_of(&self, f: usize) -> f64 {
        if f < self.n {
            self.time[f]
        } else if self.n == 0 {
            f as f64 * FRAME_S
        } else {
            self.time[self.n - 1] + (f + 1 - self.n) as f64 * FRAME_S
        }
    }
}

/// Mono `samples` at `sample_rate` to model-rate `f32`.
pub(crate) fn resample(samples: &[f32], sample_rate: u32) -> Result<Vec<f32>, String> {
    if sample_rate == SAMPLE_RATE {
        return Ok(samples.to_vec());
    }
    let x: Vec<f64> = samples.iter().map(|&s| f64::from(s)).collect();
    let y = shredlab_beats::resample_mono(&x, sample_rate).map_err(|e| e.to_string())?;
    Ok(y.into_iter().map(|v| v as f32).collect())
}

/// Runs the model over model-rate mono `audio` window by window: 3,840
/// samples of zeros first, windows of [`WINDOW`] samples that overlap by 30
/// frames, the last one zero-padded. 15 frames are trimmed from both ends
/// of each window's output and the rest concatenated (upstream
/// `unwrap_output`); frame times come from each window's real position.
pub(crate) fn infer(
    audio: &[f32],
    model: &mut dyn FnMut(&[f32]) -> Result<WindowOutput, String>,
) -> Result<Activations, String> {
    let mut padded = vec![0.0f32; OVERLAP / 2];
    padded.extend_from_slice(audio);
    // Frames upstream keeps: len * 86 / 22050 (its integer frame rate).
    let n_keep = audio.len() * (SAMPLE_RATE as usize / FFT_HOP) / SAMPLE_RATE as usize;
    let (mut note, mut onset, mut contour, mut time) = (vec![], vec![], vec![], vec![]);
    let mut buf = vec![0.0f32; WINDOW];
    let mut start = 0;
    while start < padded.len() && time.len() < n_keep {
        let end = (start + WINDOW).min(padded.len());
        buf.fill(0.0);
        buf[..end - start].copy_from_slice(&padded[start..end]);
        let out = model(&buf)?;
        let frames = out.note.len() / N_NOTES;
        if frames != FRAMES_PER_WINDOW
            || out.onset.len() != frames * N_NOTES
            || out.contour.len() != frames * N_CONTOUR
        {
            return Err(format!(
                "note model: unexpected output sizes {} / {} / {}",
                out.note.len(),
                out.onset.len(),
                out.contour.len()
            ));
        }
        for f in TRIM..frames - TRIM {
            note.extend_from_slice(&out.note[f * N_NOTES..(f + 1) * N_NOTES]);
            onset.extend_from_slice(&out.onset[f * N_NOTES..(f + 1) * N_NOTES]);
            contour.extend_from_slice(&out.contour[f * N_CONTOUR..(f + 1) * N_CONTOUR]);
            let sample = (start + f * FFT_HOP) as f64 - (OVERLAP / 2) as f64;
            time.push(sample / f64::from(SAMPLE_RATE));
        }
        start += HOP;
    }
    let n = time.len().min(n_keep);
    note.truncate(n * N_NOTES);
    onset.truncate(n * N_NOTES);
    contour.truncate(n * N_CONTOUR);
    time.truncate(n);
    let level_db = time
        .iter()
        .map(|&t| {
            let c = (t * f64::from(SAMPLE_RATE)) as isize + (FFT_HOP / 2) as isize;
            let a = (c - 512).clamp(0, audio.len() as isize) as usize;
            let b = (c + 512).clamp(0, audio.len() as isize) as usize;
            let e: f64 = audio[a..b].iter().map(|&x| f64::from(x).powi(2)).sum();
            if e > 0.0 {
                ((10.0 * (e / 1024.0).log10()) as f32).max(-120.0)
            } else {
                -120.0
            }
        })
        .collect();
    Ok(Activations {
        n,
        note,
        onset,
        contour,
        time,
        level_db,
    })
}

/// A note event in frames: start, end (exclusive), note bin, mean note
/// activation, and the model's onset activation at its start.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Event {
    pub start: usize,
    pub end: usize,
    pub bin: usize,
    pub amp: f32,
    pub onset: f32,
}

impl Event {
    pub fn midi(&self) -> usize {
        self.bin + MIDI_OFFSET
    }
}

/// Upstream `get_infered_onsets` (n_diff = 2): onsets from rises of the note
/// activation, scaled to the onset output's maximum.
fn inferred_onsets(a: &Activations) -> Vec<f32> {
    let (n, b) = (a.n, N_NOTES);
    let mut diff = vec![0.0f32; n * b];
    for t in 2..n {
        for k in 0..b {
            let f = |i: usize| a.note[i * b + k];
            diff[t * b + k] = (f(t) - f(t - 1)).min(f(t) - f(t - 2)).max(0.0);
        }
    }
    let max_on = a.onset.iter().copied().fold(0.0f32, f32::max);
    let max_d = diff.iter().copied().fold(0.0f32, f32::max);
    let scale = if max_d > 0.0 { max_on / max_d } else { 0.0 };
    a.onset
        .iter()
        .zip(&diff)
        .map(|(&o, &d)| o.max(d * scale))
        .collect()
}

/// The model's own onset activation at the start of a note: the highest
/// within a frame before to two after, on its bin and the neighbors.
fn onset_strength(a: &Activations, start: usize, k: usize) -> f32 {
    let mut best = 0.0f32;
    for t in start.saturating_sub(1)..(start + 3).min(a.n) {
        for kk in k.saturating_sub(1)..(k + 2).min(N_NOTES) {
            best = best.max(a.onset_at(t, kk));
        }
    }
    best
}

/// Upstream `output_to_notes_polyphonic` with inferred onsets and the
/// melodia trick: onset peaks (latest first) start notes that last while
/// the note activation stays above the frame threshold; then the remaining
/// energy is tracked from its maxima both ways. Events of `min_frames` or
/// fewer are dropped.
pub(crate) fn note_events(a: &Activations, min_frames: usize) -> Vec<Event> {
    let (n, b) = (a.n, N_NOTES);
    if n < 3 {
        return vec![];
    }
    let onsets = inferred_onsets(a);
    // Onset peaks over time (strictly over both neighbors), latest first.
    let mut peaks = vec![];
    for t in 1..n - 1 {
        for k in 0..b {
            let v = onsets[t * b + k];
            if v >= ONSET_THRESHOLD && v > onsets[(t - 1) * b + k] && v > onsets[(t + 1) * b + k] {
                peaks.push((t, k));
            }
        }
    }
    peaks.reverse();
    let mut energy = a.note.clone();
    let mut events = vec![];
    let clear = |e: &mut [f32], t: usize, k: usize| {
        e[t * b + k] = 0.0;
        if k + 1 < b {
            e[t * b + k + 1] = 0.0;
        }
        if k > 0 {
            e[t * b + k - 1] = 0.0;
        }
    };
    let ev = |start: usize, end: usize, k: usize| Event {
        start,
        end,
        bin: k,
        amp: (start..end).map(|t| a.note_at(t, k)).sum::<f32>() / (end - start) as f32,
        onset: onset_strength(a, start, k),
    };
    for (start, k) in peaks {
        let mut i = start + 1;
        let mut low = 0;
        while i < n - 1 && low < ENERGY_TOLERANCE {
            if energy[i * b + k] < FRAME_THRESHOLD {
                low += 1;
            } else {
                low = 0;
            }
            i += 1;
        }
        i -= low;
        if i - start <= min_frames {
            continue;
        }
        for t in start..i {
            clear(&mut energy, t, k);
        }
        events.push(ev(start, i, k));
    }
    // Melodia trick: frames by falling remaining energy (the same as
    // repeated argmax, since visiting only lowers values to zero).
    let mut order: Vec<usize> = (0..n * b)
        .filter(|&i| energy[i] > FRAME_THRESHOLD)
        .collect();
    order.sort_by(|&x, &y| energy[y].total_cmp(&energy[x]));
    for idx in order {
        if energy[idx] <= FRAME_THRESHOLD {
            continue;
        }
        let (mid, k) = (idx / b, idx % b);
        energy[idx] = 0.0;
        let mut i = mid + 1;
        let mut low = 0;
        while i < n - 1 && low < ENERGY_TOLERANCE {
            if energy[i * b + k] < FRAME_THRESHOLD {
                low += 1;
            } else {
                low = 0;
            }
            clear(&mut energy, i, k);
            i += 1;
        }
        let end = i - 1 - low;
        let mut i = mid as isize - 1;
        let mut low = 0;
        while i > 0 && low < ENERGY_TOLERANCE {
            let t = i as usize;
            if energy[t * b + k] < FRAME_THRESHOLD {
                low += 1;
            } else {
                low = 0;
            }
            clear(&mut energy, t, k);
            i -= 1;
        }
        let start = (i + 1) as usize + low;
        if end <= start || end - start <= min_frames {
            continue;
        }
        events.push(ev(start, end, k));
    }
    events.sort_by_key(|e| (e.start, e.bin));
    events
}

/// Contour bin of A0 (note bin 0's center): the middle of the three contour
/// bins the model pools into each note bin. Measured on real stems: with
/// bin 0 (upstream's `midi_pitch_to_contour_bin`), notes of songs tuned to
/// A440 read a third of a semitone sharp.
const CONTOUR_A0: f32 = 1.0;

/// Contour bin of a (fractional) MIDI pitch.
pub(crate) fn contour_bin(midi: f32) -> f32 {
    (midi - MIDI_OFFSET as f32) * 3.0 + CONTOUR_A0
}

/// (Fractional) MIDI pitch of a contour bin.
pub(crate) fn contour_midi(bin: f32) -> f32 {
    (bin - CONTOUR_A0) / 3.0 + MIDI_OFFSET as f32
}

/// The contour peak nearest `near` (a contour bin) within `radius` bins at
/// frame `t` that is a local maximum of at least `floor`: its refined bin
/// and activation.
pub(crate) fn contour_peak(
    a: &Activations,
    t: usize,
    near: f32,
    radius: f32,
    floor: f32,
) -> Option<(f32, f32)> {
    let row = a.contour_row(t);
    let lo = ((near - radius).floor().max(0.0)) as usize;
    let hi = ((near + radius).ceil() as usize).min(N_CONTOUR - 1);
    let mut best: Option<(f32, f32, f32)> = None; // (distance, bin, act)
    for c in lo..=hi {
        let v = row[c];
        if v < floor {
            continue;
        }
        let l = if c > 0 { row[c - 1] } else { 0.0 };
        let r = if c + 1 < N_CONTOUR { row[c + 1] } else { 0.0 };
        if v < l || v < r {
            continue;
        }
        let den = l - 2.0 * v + r;
        let off = if den < 0.0 {
            (0.5 * (l - r) / den).clamp(-0.5, 0.5)
        } else {
            0.0
        };
        let bin = c as f32 + off;
        let d = (bin - near).abs();
        if d <= radius && best.is_none_or(|(bd, _, _)| d < bd) {
            best = Some((d, bin, v));
        }
    }
    best.map(|(_, bin, v)| (bin, v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_stitch_to_frame_count() {
        let audio = vec![0.0f32; SAMPLE_RATE as usize * 5];
        let mut calls = 0;
        let a = infer(&audio, &mut |x: &[f32]| {
            assert_eq!(x.len(), WINDOW);
            calls += 1;
            Ok(WindowOutput {
                note: vec![0.0; FRAMES_PER_WINDOW * N_NOTES],
                onset: vec![0.0; FRAMES_PER_WINDOW * N_NOTES],
                contour: vec![0.0; FRAMES_PER_WINDOW * N_CONTOUR],
            })
        })
        .unwrap();
        assert_eq!(calls, 4);
        assert_eq!(a.n, 5 * 86);
        assert!(a.time[0].abs() < 1e-9);
        assert!(a.time.windows(2).all(|w| w[1] > w[0]));
        // Frames stay within one frame of the audio they describe.
        let last = *a.time.last().unwrap();
        assert!((last - (a.n - 1) as f64 * FRAME_S).abs() < 0.2, "{last}");
    }

    #[test]
    fn wrong_output_size_is_an_error() {
        let audio = vec![0.0f32; 1000];
        let r = infer(&audio, &mut |_: &[f32]| {
            Ok(WindowOutput {
                note: vec![0.0; 10],
                onset: vec![0.0; 10],
                contour: vec![0.0; 10],
            })
        });
        assert!(r.is_err());
    }
}
