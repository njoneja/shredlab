# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Navneet Joneja

"""Split a stereo stem into left / center / right parts by stereo position.

Each time-frequency bin of the STFT gets a pan estimate in [-1, 1]
(-1 = hard left, +1 = hard right) from the relative magnitudes of the two
channels. Bins are then softly assigned to pan regions. The region masks sum
to 1, so the parts add back up to the original stem.

Works best on an already-isolated stem (e.g. the guitar stem), where the
center isn't crowded by vocals, bass and drums.

Usage:
    python scripts/pan_split.py guitar.wav                 # L/C/R split
    python scripts/pan_split.py guitar.wav --range -1 -0.4 # one custom region
"""

import argparse
from pathlib import Path

import numpy as np
import soundfile as sf
from scipy.signal import istft, stft

N_FFT = 4096
HOP = 1024


def _smoothstep(x, lo, hi):
    t = np.clip((x - lo) / (hi - lo), 0.0, 1.0)
    return t * t * (3 - 2 * t)


def pan_map(left_spec, right_spec):
    """Per-bin pan estimate in [-1, 1]."""
    ml, mr = np.abs(left_spec), np.abs(right_spec)
    return (mr - ml) / (ml + mr + 1e-10)


def region_mask(pan, lo, hi, softness=0.1):
    """Soft mask that is ~1 for lo <= pan <= hi and fades out over `softness`."""
    rise = 1.0 if lo <= -1 else _smoothstep(pan, lo - softness / 2, lo + softness / 2)
    fall = 1.0 if hi >= 1 else 1 - _smoothstep(pan, hi - softness / 2, hi + softness / 2)
    return rise * fall


def lcr_masks(pan, center_width=0.33, softness=0.15):
    """Left/center/right masks that sum to exactly 1."""
    left = 1 - _smoothstep(pan, -center_width - softness / 2, -center_width + softness / 2)
    right = _smoothstep(pan, center_width - softness / 2, center_width + softness / 2)
    return {"left": left, "center": 1 - left - right, "right": right}


def _stft(audio, sr):
    _, _, spec = stft(audio.T, fs=sr, nperseg=N_FFT, noverlap=N_FFT - HOP)
    return spec  # shape: (channels, freqs, frames)


def _istft(spec, sr, length):
    _, out = istft(spec, fs=sr, nperseg=N_FFT, noverlap=N_FFT - HOP)
    return out.T[:length]


def split(audio, sr, center_width=0.33, softness=0.15):
    """Return {"left": ..., "center": ..., "right": ...} stereo arrays."""
    if audio.ndim != 2 or audio.shape[1] != 2:
        raise ValueError("pan split needs a stereo signal")
    spec = _stft(audio, sr)
    masks = lcr_masks(pan_map(spec[0], spec[1]), center_width, softness)
    return {name: _istft(spec * m, sr, len(audio)) for name, m in masks.items()}


def extract_range(audio, sr, lo, hi, softness=0.1):
    spec = _stft(audio, sr)
    mask = region_mask(pan_map(spec[0], spec[1]), lo, hi, softness)
    return _istft(spec * mask, sr, len(audio))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("input", type=Path)
    ap.add_argument("--out", type=Path, help="output directory (default: next to input)")
    ap.add_argument("--center-width", type=float, default=0.33)
    ap.add_argument("--range", type=float, nargs=2, metavar=("LO", "HI"),
                    help="extract a single pan range instead of L/C/R")
    args = ap.parse_args()

    audio, sr = sf.read(args.input, dtype="float32", always_2d=True)
    out_dir = args.out or args.input.parent
    out_dir.mkdir(parents=True, exist_ok=True)
    stem = args.input.stem

    if args.range:
        lo, hi = args.range
        path = out_dir / f"{stem}_pan[{lo:+.2f},{hi:+.2f}].wav"
        sf.write(path, extract_range(audio, sr, lo, hi), sr)
        print(path)
    else:
        for name, part in split(audio, sr, args.center_width).items():
            path = out_dir / f"{stem}_{name}.wav"
            sf.write(path, part, sr)
            print(path)


if __name__ == "__main__":
    main()
