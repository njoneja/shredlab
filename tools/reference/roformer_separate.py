"""`roformer-mask` adapter reference (BS/Mel-Band RoFormer core, DSP outside).

STFT glue ported from spikes/onnx_roformer.py. Chunking replicates
audio-separator 0.48 `MDXCSeparator.demix` (roformer branch), which is what
the PyTorch output we validate against uses:

    chunk  = segment_samples (= stft_hop * (dim_t - 1); 588800 for SW)
    step   = chunk // num_overlap                 (integer division)
    starts = 0, step, 2*step, ... while start + chunk < L; then a final
             "tail" start = max(L - chunk, 0) (not repeated if equal to the
             previous start). So every chunk is full length when L >= chunk.
    window = symmetric Hamming of length chunk (scipy.signal.windows.hamming)
    result[:, :, s:s+n] += model(x[:, s:s+n]) * window[:n]
    counter[s:s+n]      += window[:n]
    out = result / max(counter, 1e-10)

No song normalization (BS-Roformer-SW `inference.normalize: false`).
Inputs shorter than `chunk` are zero-padded to `chunk` (the ONNX graph has a
static frame count); PyTorch runs them at their true length, so outputs for
songs shorter than one chunk (13.35 s) differ slightly from audio-separator.

ONNX graph I/O:
    stft (1, 2, n_fft/2+1, F, 2)  F = chunk/hop + 1, real/imag last
 -> mask (1, N, (n_fft/2+1)*2, F, 2) complex mask, freq axis (freq, stereo)
                                     interleaved with stereo fastest.
"""

import numpy as np
from scipy.signal import get_window
from scipy.signal.windows import hamming

# --- STFT glue (== torch.stft/istft, center=True, reflect, periodic Hann) ---


def hann_periodic(n_fft):
    return get_window("hann", n_fft, fftbins=True).astype(np.float32)


def stft(audio, n_fft=2048, hop=512):
    """audio (C, L) -> (C, n_fft/2+1, 1 + L//hop) complex (complex128).

    Reflect-pad n_fft/2 on both sides, frames at multiples of hop, periodic
    Hann, not normalized, rfft."""
    window = hann_periodic(n_fft)
    pad = n_fft // 2
    x = np.pad(audio, ((0, 0), (pad, pad)), mode="reflect")
    n_frames = 1 + (x.shape[1] - n_fft) // hop
    idx = np.arange(n_fft)[None, :] + hop * np.arange(n_frames)[:, None]
    frames = x[:, idx] * window
    return np.fft.rfft(frames, axis=-1).transpose(0, 2, 1)


def istft(spec, length, n_fft=2048, hop=512):
    """spec (..., n_fft/2+1, T) complex -> (..., length) float32.

    irfft, multiply by window, overlap-add, divide by summed squared window
    (floor 1e-8), drop n_fft/2 at the start."""
    window = hann_periodic(n_fft)
    frames = np.fft.irfft(spec.swapaxes(-1, -2), n=n_fft, axis=-1) * window
    n_frames = frames.shape[-2]
    out_len = n_fft + hop * (n_frames - 1)
    out = np.zeros(frames.shape[:-2] + (out_len,), dtype=np.float32)
    norm = np.zeros(out_len, dtype=np.float32)
    for i in range(n_frames):
        out[..., i * hop : i * hop + n_fft] += frames[..., i, :]
        norm[i * hop : i * hop + n_fft] += window**2
    out /= np.maximum(norm, 1e-8)
    pad = n_fft // 2
    return out[..., pad : pad + length]


def stft_to_input(spec):
    """(C, F, T) complex -> (1, C, F, T, 2) float32 graph input."""
    return np.stack([spec.real, spec.imag], -1)[None].astype(np.float32)


def apply_masks(spec, mask):
    """spec (C, F, T) complex, mask (N, F*C, T, 2) -> (N, C, F, T) complex."""
    c = spec.shape[0]
    m = mask[..., 0] + 1j * mask[..., 1]
    m = m.reshape(m.shape[0], -1, c, m.shape[-1]).transpose(0, 2, 1, 3)
    return spec[None] * m


# --- model wrapper -----------------------------------------------------------


class RoformerMask:
    def __init__(self, sess, n_fft=2048, hop=512, num_overlap=2):
        self.sess = sess
        shape = sess.get_inputs()[0].shape  # (1, 2, n_fft/2+1, F, 2)
        assert int(shape[2]) == n_fft // 2 + 1, shape
        self.n_fft, self.hop = n_fft, hop
        self.frames = int(shape[3])
        self.chunk = hop * (self.frames - 1)
        self.num_overlap = num_overlap
        self.n_stems = int(sess.get_outputs()[0].shape[1])

    def run_chunk(self, x):
        """x (2, chunk) -> (N, 2, chunk)."""
        assert x.shape[1] == self.chunk
        z = stft(x, self.n_fft, self.hop)
        mask = self.sess.run(None, {self.sess.get_inputs()[0].name: stft_to_input(z)})[0][0]
        return istft(apply_masks(z, mask), self.chunk, self.n_fft, self.hop)


def chunk_starts(length, chunk, step):
    """== MDXCSeparator._roformer_chunk_starts."""
    starts = []
    for offset in range(0, length, step):
        if offset + chunk >= length:
            tail = max(length - chunk, 0)
            if not starts or starts[-1] != tail:
                starts.append(tail)
            break
        starts.append(offset)
    return starts


def chunk_window(chunk):
    return hamming(chunk).astype(np.float32)  # symmetric (sym=True)


def separate(sess, mix, num_overlap=2, n_fft=2048, hop=512, progress=None):
    """mix (2, L) float32 -> (N, 2, L) float32."""
    model = sess if hasattr(sess, "run_chunk") else RoformerMask(sess, n_fft, hop, num_overlap)
    chunk = model.chunk
    step = chunk // model.num_overlap
    length = mix.shape[1]
    window = chunk_window(chunk)
    result = np.zeros((model.n_stems, mix.shape[0], length), dtype=np.float32)
    counter = np.zeros(length, dtype=np.float32)
    starts = chunk_starts(length, chunk, step)
    for k, s in enumerate(starts):
        part = mix[:, s : s + chunk]
        n = part.shape[1]
        if n < chunk:
            part = np.pad(part, ((0, 0), (0, chunk - n)))
        x = model.run_chunk(part.astype(np.float32))
        result[:, :, s : s + n] += x[:, :, :n] * window[:n]
        counter[s : s + n] += window[:n]
        if progress:
            progress(k + 1, len(starts))
    return result / np.maximum(counter, 1e-10)


def separate_single_segment(sess, seg, n_fft=2048, hop=512):
    """Self-test path: exactly one chunk, no window/overlap. seg (2, chunk) -> (N, 2, chunk)."""
    model = sess if hasattr(sess, "run_chunk") else RoformerMask(sess, n_fft, hop)
    return model.run_chunk(seg)
