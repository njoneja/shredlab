"""Deterministic self-test input "synth-v1" (see docs/MODEL_MANIFEST.md).

All arithmetic is float64, evaluated in the documented order, then cast to
float32. A Rust port using f64 `sin` produces the same float32 values except
for rare last-ulp differences in libm (well below any test tolerance).
"""

import math

import numpy as np

NAME = "synth-v1"


def synth_v1(length, sample_rate=44100, channels=2):
    """Return (channels, length) float32."""
    t = np.arange(length, dtype=np.float64) / float(sample_rate)
    two_pi = 2.0 * math.pi
    out = np.empty((channels, length), dtype=np.float32)
    for c in range(channels):
        x = (
            0.30 * np.sin(two_pi * 110.0 * t)
            + 0.20 * np.sin(two_pi * (440.0 + 3.0 * c) * t)
            + 0.15 * np.sin(two_pi * 1250.0 * t) * (0.5 + 0.5 * np.sin(two_pi * 2.0 * t))
            + 0.05 * np.sin(two_pi * 3.0 * (1.0 + c) * t + 7.0 * np.sin(two_pi * 0.5 * t))
        )
        out[c] = x.astype(np.float32)
    return out
