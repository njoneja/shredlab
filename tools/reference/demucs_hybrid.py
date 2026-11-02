# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Navneet Joneja

"""`demucs-hybrid` adapter reference (htdemucs-style ONNX core, DSP outside).

Ported from spikes/onnx_demucs_own.py (STFT glue) and spikes/onnx_demucs.py
(apply_model chunking); numerically identical to them.

ONNX graph I/O (htdemucs_6s values):
    mix  (1, 2, S)            song-normalized waveform segment, S = 343980
    mag  (1, 4, n_fft/2, F)   CaC spectrogram of `mix` (re/im as channels), F = 336
 -> wave (1, N, 2, S)         time branch
    spec (1, N, 4, n_fft/2, F) freq branch, CaC
segment output = wave + ispec(uncac(spec)).

Pipeline for a song (2, L):
    1. mean/std of the mono mix (float32), x = (mix - mean) / std
    2. apply_model chunking: stride = int(0.75 * S), offsets 0, stride, 2*stride, ... < L;
       each chunk of n = min(S, L - offset) samples is centered in a full
       S-sample window filled with neighboring audio (zeros past the ends),
       run, cropped back to n, multiplied by triangular weights[:n] and
       overlap-added; finally divided by the summed weights.
    3. out * std + mean

Shifts (demucs apply_model shifts=N > 1, `separate(..., shifts=N)`): step 1 once
over the real audio; then for each deterministic offset (`shift_offsets`) the
normalized song is padded as zeros(max_shift - offset) ++ x ++ zeros(offset)
(== demucs' TensorChunk view of the max_shift-padded mix; chunk context outside
the view is zeros either way), run through step 2, cropped by
max_shift - offset, summed and divided by N; then step 3.

CLI (debugging / ad-hoc goldens):
    python -m tools.reference.demucs_hybrid AUDIO OUT_DIR [--shifts 2] [--model-dir DIR]
"""

import math

import numpy as np
import scipy.fft

# --- STFT glue (== HTDemucs._spec / _ispec) ----------------------------------


def hann_periodic(n_fft):
    return (0.5 - 0.5 * np.cos(2 * np.pi * np.arange(n_fft) / n_fft)).astype(np.float64)


def spec(x, n_fft=4096, hop=1024):
    """x (C, L) float -> (C, n_fft/2, ceil(L/hop)) complex64.

    Padding: reflect-pad by 3*hop/2 on the left and 3*hop/2 + (le*hop - L) on the
    right (le = ceil(L/hop)), then torch.stft(center=True) = reflect-pad n_fft/2
    both sides, periodic Hann, normalized (window / sqrt(n_fft)), rfft. Drop the
    last frequency bin (Nyquist) and keep frames [2, 2+le).
    """
    L = x.shape[-1]
    le = math.ceil(L / hop)
    pad = hop // 2 * 3
    window = hann_periodic(n_fft)
    x = np.pad(x.astype(np.float32), ((0, 0), (pad, pad + le * hop - L)), mode="reflect")
    x = np.pad(x, ((0, 0), (n_fft // 2, n_fft // 2)), mode="reflect")
    n_frames = 1 + (x.shape[1] - n_fft) // hop
    idx = np.arange(n_fft)[None, :] + hop * np.arange(n_frames)[:, None]
    frames = x[:, idx] * (window / np.sqrt(n_fft)).astype(np.float32)
    z = scipy.fft.rfft(frames, axis=-1, workers=-1)  # (C, frames, n_fft/2+1)
    z = z.transpose(0, 2, 1)[:, :-1, 2 : 2 + le]
    return z.astype(np.complex64)


def ispec(z, length, n_fft=4096, hop=1024):
    """z (..., n_fft/2, T) complex -> (..., length) float32 (inverse of `spec`).

    Append a zero Nyquist bin, zero-pad 2 frames on each side, irfft, multiply
    by sqrt(n_fft) * window, overlap-add with hop, divide by the summed squared
    window (floor 1e-11), drop n_fft/2 (center) then 3*hop/2 (the spec padding).
    """
    assert n_fft == 4 * hop, "overlap-add below assumes n_fft == 4 * hop"
    z = np.concatenate([z, np.zeros(z.shape[:-2] + (1, z.shape[-1]), z.dtype)], axis=-2)
    z = np.pad(z, [(0, 0)] * (z.ndim - 1) + [(2, 2)])
    pad = hop // 2 * 3
    le = hop * math.ceil(length / hop) + 2 * pad
    win = hann_periodic(n_fft).astype(np.float32)
    frames = scipy.fft.irfft(z.swapaxes(-1, -2).astype(np.complex64), n=n_fft, axis=-1, workers=-1)
    frames *= np.float32(np.sqrt(n_fft)) * win
    n = frames.shape[-2]
    q = frames.reshape(frames.shape[:-1] + (4, hop))
    out = np.zeros(frames.shape[:-2] + (n + 3, hop), np.float32)
    env = np.zeros((n + 3, hop), np.float32)
    wq = (win**2).reshape(4, hop)
    for k in range(4):
        out[..., k : k + n, :] += q[..., :, k, :]
        env[k : k + n] += wq[k]
    out = out.reshape(out.shape[:-2] + (-1,))
    env = env.reshape(-1)
    start = n_fft // 2
    out = out[..., start : start + le] / np.maximum(env[start : start + le], 1e-11)
    return out[..., pad : pad + length]


def cac(z):
    """(C, F, T) complex -> (1, 2C, F, T) float32: channels [c0.re, c0.im, c1.re, c1.im]."""
    return np.stack([z.real, z.imag], 1).reshape(1, -1, *z.shape[-2:]).astype(np.float32)


def uncac(x):
    """(N, 2C, F, T) float -> (N, C, F, T) complex."""
    N, C2, F, T = x.shape
    x = x.reshape(N, C2 // 2, 2, F, T)
    return x[:, :, 0] + 1j * x[:, :, 1]


# --- model wrapper -----------------------------------------------------------


class DemucsHybrid:
    """Wraps an onnxruntime session of a demucs-hybrid graph.

    Shapes are read from the graph: S from `mix`, n_fft from `mag`; hop = n_fft/4.
    """

    def __init__(self, sess, overlap=0.25):
        self.sess = sess
        ins = {i.name: i.shape for i in sess.get_inputs()}
        self.segment = int(ins["mix"][2])
        self.n_fft = int(ins["mag"][2]) * 2
        self.hop = self.n_fft // 4
        self.overlap = overlap
        self.n_stems = int(sess.get_outputs()[0].shape[1])

    def run_raw(self, seg):
        """seg (2, S) normalized -> (wave (N,2,S), spec (N,4,F,T)) raw graph outputs."""
        z = spec(seg, self.n_fft, self.hop)
        wave, sp = self.sess.run(None, {"mix": seg[None].astype(np.float32), "mag": cac(z)})
        return wave[0], sp[0]

    def run_window(self, seg):
        """seg (2, S) normalized -> (N, 2, S)."""
        wave, sp = self.run_raw(seg)
        return wave + ispec(uncac(sp), seg.shape[-1], self.n_fft, self.hop)


def song_stats(mix):
    """Song normalization stats: (mean, std) of the mono mix, float32 numpy semantics."""
    ref = mix.mean(0)
    return ref.mean(), max(ref.std(), np.finfo(np.float32).eps)


def run_segment(model, mix, offset=0, n=None):
    """Separate mix[:, offset:offset+n] -> (N, 2, n). A short chunk is centered
    in a full segment filled with the surrounding audio (zeros beyond the ends),
    like demucs' TensorChunk.padded."""
    S = model.segment
    total = mix.shape[1]
    n = min(n or S, total - offset)
    start = offset - (S - n) // 2
    padded = np.zeros((mix.shape[0], S), dtype=np.float32)
    lo, hi = max(0, start), min(total, start + S)
    padded[:, lo - start : hi - start] = mix[:, lo:hi]
    out = model.run_window(padded)
    left = offset - start
    return out[:, :, left : left + n]


def triangular_weights(segment):
    """demucs apply_model weights (transition_power=1), max-normalized, float32."""
    w = np.concatenate([np.arange(1, segment // 2 + 1), np.arange(segment - segment // 2, 0, -1)])
    return (w / w.max()).astype(np.float32)


def chunk_offsets(length, segment, overlap=0.25):
    stride = int((1 - overlap) * segment)
    return list(range(0, length, stride))


def chunked_apply(model, mix, progress=None):
    """apply_model chunking only (no normalization). `model` needs .segment,
    .overlap, .n_stems and .run_window(seg (2, S)) -> (N, 2, S)."""
    S = model.segment
    length = mix.shape[1]
    weight = triangular_weights(S)
    out = np.zeros((model.n_stems, mix.shape[0], length), dtype=np.float32)
    total = np.zeros(length, dtype=np.float32)
    offsets = chunk_offsets(length, S, model.overlap)
    for k, offset in enumerate(offsets):
        n = min(S, length - offset)
        out[:, :, offset : offset + n] += run_segment(model, mix, offset, n) * weight[:n]
        total[offset : offset + n] += weight[:n]
        if progress:
            progress(k + 1, len(offsets))
    out /= total
    return out


def max_shift(sample_rate=44100):
    """demucs apply_model: int(0.5 * samplerate)."""
    return int(0.5 * sample_rate)


def shift_offsets(shifts, sample_rate=44100):
    """Deterministic offsets round(k * max_shift / shifts), half up, k in 0..shifts
    (upstream draws random.randint(0, max_shift)). shifts=2 @ 44.1 kHz: [0, 11025].
    Same as shredlab_separation::shift_offsets."""
    ms = max_shift(sample_rate)
    return [(2 * k * ms + shifts) // (2 * shifts) for k in range(shifts)]


def shifted_apply(model, x, shifts, sample_rate=44100, progress=None):
    """demucs apply_model shifts on normalized x (2, L) with deterministic offsets."""
    length = x.shape[1]
    ms = max_shift(sample_rate)
    out = np.zeros((model.n_stems, x.shape[0], length), dtype=np.float32)
    for k, offset in enumerate(shift_offsets(shifts, sample_rate)):
        lead = ms - offset
        shifted = np.zeros((x.shape[0], lead + length + offset), dtype=np.float32)
        shifted[:, lead : lead + length] = x
        pr = (lambda d, t, k=k: progress(k * t + d, shifts * t)) if progress else None
        res = chunked_apply(model, shifted, pr)
        out += res[:, :, lead : lead + length]
    out /= np.float32(shifts)
    return out


def _wrap(sess, overlap=0.25):
    return sess if hasattr(sess, "run_window") else DemucsHybrid(sess, overlap)


def separate(sess, mix, overlap=0.25, progress=None, shifts=1, sample_rate=44100):
    """mix (2, L) float32 -> (N, 2, L) float32. demucs apply_model with
    split=True, overlap=0.25, transition_power=1, shifts=0 (shifts <= 1) or
    deterministic shifts (see module docstring).

    `sess` is an onnxruntime session (or a DemucsHybrid)."""
    model = _wrap(sess, overlap)
    mean, std = song_stats(mix)
    x = (mix - mean) / std
    if shifts <= 1:
        out = chunked_apply(model, x, progress)
    else:
        out = shifted_apply(model, x, shifts, sample_rate, progress)
    return out * std + mean


def separate_single_segment(sess, seg):
    """Self-test path: exactly one segment (len == S), song normalization over
    that segment, no chunking/weights. seg (2, S) -> (N, 2, S)."""
    model = _wrap(sess)
    assert seg.shape[1] == model.segment
    mean, std = song_stats(seg)
    return model.run_window((seg - mean) / std) * std + mean


def _main():
    import argparse
    import subprocess
    import time
    from pathlib import Path

    from scipy.io import wavfile

    from tools.reference import manifest as mf
    from tools.reference.sessions import make_session

    root = Path(__file__).resolve().parents[2]
    ap = argparse.ArgumentParser(description="Separate an audio file with the demucs-hybrid reference.")
    ap.add_argument("audio")
    ap.add_argument("out_dir")
    ap.add_argument("--model-dir", default=str(root / "models" / "registry" / "htdemucs_6s"))
    ap.add_argument("--shifts", type=int, default=1, help="time-shifted passes to average (1 = plain)")
    ap.add_argument("--ep", default="cpu", choices=["cpu", "dml"])
    a = ap.parse_args()
    man = mf.load_manifest(a.model_dir)
    sr = man["sample_rate"]
    raw = subprocess.run(["ffmpeg", "-loglevel", "error", "-i", a.audio, "-f", "f32le", "-ac", "2", "-ar", str(sr), "-"],
                         check=True, capture_output=True).stdout
    mix = np.ascontiguousarray(np.frombuffer(raw, np.float32).reshape(-1, 2).T)
    sess = make_session(Path(a.model_dir) / man["file"], a.ep, man["session"])
    t0 = time.perf_counter()
    stems = separate(DemucsHybrid(sess, man["overlap"]), mix, shifts=a.shifts, sample_rate=sr)
    print(f"separated {mix.shape[1] / sr:.1f}s in {time.perf_counter() - t0:.1f}s (shifts={a.shifts})")
    out = Path(a.out_dir)
    out.mkdir(parents=True, exist_ok=True)
    for name, st in zip(man["stems"], stems):
        wavfile.write(out / f"{name}.wav", sr, st.T)


if __name__ == "__main__":
    _main()
