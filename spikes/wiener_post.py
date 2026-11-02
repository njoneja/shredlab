# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Navneet Joneja

"""Listening test: multichannel Wiener post-filter on separated stems.

Uses norbert (MIT, sigsep) the way Open-Unmix does: the stems' magnitudes
are the source models, the original mix is what gets filtered, and the
filtering runs on batches of STFT frames. With `iterations = 0` it is a soft
mask (stem magnitude / sum of stem magnitudes, applied to the mix); with
`iterations >= 1` EM also estimates each stem's stereo spatial covariance,
which can move bleed to the right stem by position as well as level.

Every variant's stems sum to the mix (the masks sum to 1), so nothing is
invented: energy can only move between stems.

Needs `norbert` (not in requirements.txt): `uv pip install norbert`.

Usage:
    python spikes/wiener_post.py MIX.mp3 STEMS_DIR OUT_DIR [--iters 0 1 3]

Writes OUT_DIR/iter<N>/<stem>.wav for each N.
"""

import argparse
from pathlib import Path

import norbert
import numpy as np
import soundfile as sf
from scipy.signal import correlate, istft, stft

N_FFT = 4096
HOP = 1024
BATCH = 200  # frames per Wiener batch (~4.6 s), as in Open-Unmix


def align(mix, est, max_lag=8192):
    """Shift `mix` so it lines up with `est` (the sum of the stems)."""
    n = min(len(mix), len(est), 30 * 44100)
    a = mix[:n].mean(axis=1)
    b = est[:n].mean(axis=1)
    c = correlate(a, b, mode="full", method="fft")
    mid = n - 1
    lag = int(np.argmax(c[mid - max_lag : mid + max_lag + 1])) - max_lag
    if lag > 0:
        mix = mix[lag:]
    elif lag < 0:
        mix = np.concatenate([np.zeros((-lag, mix.shape[1]), mix.dtype), mix])
    out = np.zeros_like(est)
    m = min(len(mix), len(est))
    out[:m] = mix[:m]
    return out, lag


def spec(x):
    # (channels, bins, frames) -> (frames, bins, channels)
    _, _, z = stft(x.T, nperseg=N_FFT, noverlap=N_FFT - HOP, window="hann")
    return np.transpose(z, (2, 1, 0)).astype(np.complex64)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("mix")
    ap.add_argument("stems_dir")
    ap.add_argument("out_dir")
    ap.add_argument("--iters", type=int, nargs="+", default=[0, 1, 3])
    args = ap.parse_args()

    names = sorted(p.stem for p in Path(args.stems_dir).glob("*.wav"))
    stems = [sf.read(Path(args.stems_dir) / f"{n}.wav", dtype="float32")[0] for n in names]
    sr = sf.info(str(Path(args.stems_dir) / f"{names[0]}.wav")).samplerate
    est = np.sum(stems, axis=0)
    mix, msr = sf.read(args.mix, dtype="float32", always_2d=True)
    assert msr == sr, (msr, sr)
    mix, lag = align(mix, est)
    resid = mix - est
    print(f"{len(names)} stems {names}; mix lag {lag} samples; "
          f"stem-sum vs mix residual {10 * np.log10(np.sum(resid**2) / np.sum(mix**2)):.1f} dB")

    x = spec(mix)
    v = np.stack([np.abs(spec(s)) for s in stems], axis=-1)

    for it in args.iters:
        y = np.zeros(v.shape, np.complex64)
        for t in range(0, x.shape[0], BATCH):
            sl = slice(t, t + BATCH)
            # float64: norbert's EM regularization is sqrt(eps) of the input
            # dtype; float32's (~3e-4) swamps quiet bins and the stems no
            # longer sum to the mix (-4 dB error instead of -35 dB).
            y[sl] = norbert.wiener(v[sl].astype(np.float64), x[sl].astype(np.complex128),
                                   iterations=it, use_softmask=True)
        out = Path(args.out_dir) / f"iter{it}"
        out.mkdir(parents=True, exist_ok=True)
        for k, n in enumerate(names):
            # (frames, bins, channels) -> (channels, bins, frames)
            _, w = istft(np.transpose(y[..., k], (2, 1, 0)), nperseg=N_FFT,
                         noverlap=N_FFT - HOP, window="hann")
            w = w.T[: len(mix)]
            moved = 10 * np.log10(np.sum((w - stems[k][: len(w)]) ** 2) / np.sum(stems[k] ** 2) + 1e-12)
            # Float, not normalized: levels stay comparable with the original.
            sf.write(out / f"{n}.wav", w, sr, subtype="FLOAT")
            print(f"  iter{it} {n:7s} change vs original stem {moved:6.1f} dB")


if __name__ == "__main__":
    main()
