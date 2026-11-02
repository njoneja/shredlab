# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Navneet Joneja

"""Generate the small, committed golden fixtures in tests/fixtures/.

Usage: .venv\\Scripts\\python.exe tools/make_fixtures.py

Everything here is model-free (no ONNX), deterministic and small (< 2 MB).
See tests/fixtures/README.md for the description of every file.
"""

import json
import sys
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from tools.fixture_io import FixtureWriter  # noqa: E402
from tools.reference import demucs_hybrid as dh  # noqa: E402
from tools.reference import roformer_separate as rs  # noqa: E402
from tools.reference.synth import synth_v1  # noqa: E402

OUT = ROOT / "tests" / "fixtures"
SR = 44100
N = SR // 2  # 0.5 s: 22050 samples, not a multiple of either hop (edge padding is exercised)


# --- fake "models" for chunking tests: output depends on position in the window ---


class FakeDemucs:
    """run_window(seg)[0] = seg * (1 + j / S) for sample j of the window."""

    def __init__(self, segment, overlap=0.25):
        self.segment, self.overlap, self.n_stems = segment, overlap, 1

    def run_window(self, seg):
        ramp = (1 + np.arange(self.segment, dtype=np.float32) / self.segment).astype(np.float32)
        return (seg * ramp)[None]


class FakeRoformer:
    def __init__(self, chunk, num_overlap):
        self.chunk, self.num_overlap, self.n_stems = chunk, num_overlap, 1

    def run_chunk(self, x):
        ramp = (1 + np.arange(self.chunk, dtype=np.float32) / self.chunk).astype(np.float32)
        return (x * ramp)[None]


def ramp_input(length, channels=2):
    """Deterministic small input: x[c][i] = (i + 1) * (1 if c == 0 else -0.5) / length."""
    i = np.arange(length, dtype=np.float32)
    return np.stack([(i + 1) / length * (1.0 if c == 0 else -0.5) for c in range(channels)]).astype(np.float32)


def f32list(a):
    return [float(v) for v in np.asarray(a, np.float32).ravel()]


def chunking_vectors():
    out = {"demucs": {}, "roformer": {}}
    d = out["demucs"]
    d["triangular_weights"] = {str(s): f32list(dh.triangular_weights(s)) for s in (8, 9, 10, 11)}
    d["cases"] = []
    for S, L in [(8, 21), (8, 5), (12, 40), (8, 8)]:
        m = FakeDemucs(S)
        x = ramp_input(L)
        offsets = dh.chunk_offsets(L, S, 0.25)
        win = []
        for o in offsets:
            n = min(S, L - o)
            win.append({"offset": o, "n": n, "window_start": o - (S - n) // 2})
        y = dh.chunked_apply(m, x)[0]
        d["cases"].append({"segment": S, "overlap": 0.25, "length": L, "stride": int(0.75 * S),
                           "chunks": win, "input": [f32list(x[0]), f32list(x[1])],
                           "output": [f32list(y[0]), f32list(y[1])]})
    r = out["roformer"]
    r["hamming"] = {str(c): f32list(rs.chunk_window(c)) for c in (8, 9, 10, 11)}
    r["cases"] = []
    for C, K, L in [(10, 2, 25), (10, 2, 30), (10, 4, 23), (10, 2, 7), (10, 2, 10)]:
        m = FakeRoformer(C, K)
        x = ramp_input(L)
        y = rs.separate(m, x)[0]
        r["cases"].append({"chunk": C, "num_overlap": K, "step": C // K, "length": L,
                           "starts": rs.chunk_starts(L, C, C // K),
                           "input": [f32list(x[0]), f32list(x[1])],
                           "output": [f32list(y[0]), f32list(y[1])]})
    out["notes"] = {
        "fake_model": "output[c][j] = window[c][j] * (1 + j / S) with j the sample index inside the "
                      "model window (S = segment/chunk), single stem; computed in float32.",
        "input": "x[c][i] = (i + 1) / L * (1 if c == 0 else -0.5), float32",
        "demucs": "chunked_apply only (no song normalization). Short chunks are centered in a full "
                  "window starting at window_start (may be negative; zeros outside [0, L)).",
        "roformer": "separate(): chunks shorter than `chunk` (only when L < chunk) are zero-padded "
                    "at the end; window = symmetric Hamming; out = sum(y*w) / max(sum(w), 1e-10).",
    }
    return out


def main():
    import torch

    w = FixtureWriter(OUT, "index.json", {"sample_rate": SR})
    x = synth_v1(N, SR)
    w.bin("synth_input", x, "synth-v1 input, first 22050 samples (0.5 s), stereo", )

    # demucs-style STFT (HTDemucs._spec) and its inverse
    z = dh.spec(x)
    w.bin("demucs_stft", z, "dh.spec(synth_input): (C, n_fft/2=2048 bins, ceil(L/1024)=22 frames, re/im)",
          n_fft=4096, hop=1024)
    w.bin("demucs_istft", dh.ispec(z, N), "dh.ispec(demucs_stft, 22050): roundtrip back to (C, L)")

    # torch cross-check of the demucs STFT (same formula as HTDemucs._spec)
    le = -(-N // 1024)
    pad = 1024 // 2 * 3
    xt = torch.nn.functional.pad(torch.from_numpy(x)[None], (pad, pad + le * 1024 - N), mode="reflect")[0]
    zt = torch.stft(xt, 4096, 1024, window=torch.hann_window(4096), normalized=True, center=True,
                    return_complex=True, pad_mode="reflect")[:, :-1, 2 : 2 + le].numpy()
    err_d = np.abs(z - zt).max() / np.abs(zt).max()
    # torch cross-check of the demucs iSTFT (HTDemucs._ispec)
    zi = torch.nn.functional.pad(torch.nn.functional.pad(torch.from_numpy(z), (0, 0, 0, 1)), (2, 2))
    yi_t = torch.istft(zi, 4096, 1024, window=torch.hann_window(4096), win_length=4096, normalized=True,
                       length=1024 * le + 2 * pad, center=True)[..., pad : pad + N].numpy()

    # roformer STFT (torch.stft center=True, n_fft 2048, hop 512, periodic Hann, not normalized)
    win = torch.hann_window(2048)
    zr_t = torch.stft(torch.from_numpy(x), 2048, 512, window=win, center=True, return_complex=True,
                      pad_mode="reflect")
    zr = rs.stft(x)
    err_r = np.abs(zr - zr_t.numpy()).max() / np.abs(zr_t.numpy()).max()
    w.bin("roformer_stft", zr_t.numpy(), "torch.stft(synth_input, 2048, 512, hann, center=True): "
          "(C, 1025 bins, 1 + L//512 = 44 frames, re/im)", n_fft=2048, hop=512)
    yr = rs.istft(zr_t.numpy(), N)
    yr_t = torch.istft(zr_t, 2048, 512, window=win, center=True, length=N).numpy()
    w.bin("roformer_istft", yr, "rs.istft(roformer_stft, 22050) (numpy reference; matches torch.istft(length=L))")

    # song normalization stats (float32 numpy semantics)
    stats = {}
    for label, L in [("synth_input_22050", N), ("synth_v1_343980", 343980), ("synth_v1_588800", 588800)]:
        mean, std = dh.song_stats(synth_v1(L, SR))
        stats[label] = {"mean": float(mean), "std": float(std)}
    w.value("song_normalization", stats)
    w.value("checks", {
        "demucs_stft_vs_torch_rel_max_err": float(err_d),
        "roformer_stft_numpy_vs_torch_rel_max_err": float(err_r),
        "demucs_istft_vs_torch_max_abs_err": float(np.abs(dh.ispec(z, N) - yi_t).max()),
        "demucs_istft_roundtrip_max_abs_err (not ~0 by design: Nyquist bin and edge frames dropped)":
            float(np.abs(dh.ispec(z, N) - x).max()),
        "roformer_istft_numpy_vs_torch_max_abs_err": float(np.abs(yr - yr_t).max()),
        "roformer_istft_roundtrip_max_abs_err": float(np.abs(yr - x).max()),
    })
    w.close()

    (OUT / "chunking.json").write_text(json.dumps(chunking_vectors(), separators=(",", ":")) + "\n", encoding="utf-8")
    total = sum(p.stat().st_size for p in OUT.iterdir() if p.is_file())
    print(json.dumps(w.index["checks"], indent=2))
    print(f"wrote {OUT}: {total / 1e6:.2f} MB")


if __name__ == "__main__":
    main()
