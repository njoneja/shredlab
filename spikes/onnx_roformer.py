# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Navneet Joneja

"""Spike: export BS-RoFormer (SW, 6 stems) to ONNX and compare with PyTorch.

The ONNX graph contains only the network core: real-valued STFT in, complex
masks out. STFT, mask multiply and iSTFT stay outside the graph (NumPy here,
any DSP library in the app), because complex ops don't export cleanly.

Usage:
    python spikes/onnx_roformer.py export
    python spikes/onnx_roformer.py compare <audio file>
"""

import sys
import time
from pathlib import Path

import numpy as np
import torch
import yaml
from einops import rearrange, pack, unpack
from scipy.signal import get_window

ROOT = Path(__file__).resolve().parent.parent
CKPT = ROOT / "models" / "BS-Roformer-SW.ckpt"
CFG = ROOT / "models" / "BS-Roformer-SW.yaml"
ONNX_PATH = ROOT / "models" / "onnx" / "bs_roformer_sw.onnx"

N_FFT, HOP = 2048, 512
CHUNK = 588800  # samples per inference chunk (from the model yaml)
FRAMES = CHUNK // HOP + 1


class _Loader(yaml.SafeLoader):
    pass


_Loader.add_constructor("tag:yaml.org,2002:python/tuple", lambda l, n: tuple(l.construct_sequence(n)))


def load_torch_model():
    from audio_separator.separator.uvr_lib_v5.roformer.bs_roformer import BSRoformer

    cfg = yaml.load(CFG.read_text(), Loader=_Loader)
    model = BSRoformer(**cfg["model"])
    state = torch.load(CKPT, map_location="cpu", weights_only=False)
    state = state.get("state_dict", state)
    model.load_state_dict(state)
    model.eval()
    return model, cfg


class ExplicitGLU(torch.nn.Module):
    """nn.GLU equivalent; the TorchScript ONNX exporter mis-declares GLU's output shape."""

    def __init__(self, dim):
        super().__init__()
        self.dim = dim

    def forward(self, x):
        dim = self.dim % x.dim()
        half = x.shape[dim] // 2
        a, b = x.narrow(dim, 0, half), x.narrow(dim, half, half)
        return a * torch.sigmoid(b)


def replace_glu(module):
    for name, child in module.named_children():
        if isinstance(child, torch.nn.GLU):
            setattr(module, name, ExplicitGLU(child.dim))
        else:
            replace_glu(child)


def _band_split_forward(self, x):
    # x: (b, t, F). Positive axes and explicit slicing: the exporter resolves
    # negative axes after split/unbind against the wrong rank.
    outs, start = [], 0
    for dim_in, to_feature in zip(self.dim_inputs, self.to_features):
        outs.append(to_feature(x[:, :, start : start + dim_in]))
        start += dim_in
    return torch.stack(outs, dim=2)


def _mask_estimator_forward(self, x):
    # x: (b, t, bands, d)
    outs = [mlp(x[:, :, i, :]) for i, mlp in enumerate(self.to_freqs)]
    return torch.cat(outs, dim=2)


def _rotate_half(x):
    # x: (b, h, n, d) with static shape
    b, h, n, d = (int(s) for s in x.shape)
    pairs = x.reshape(b, h, n, d // 2, 2)
    return torch.stack((-pairs[:, :, :, :, 1], pairs[:, :, :, :, 0]), dim=4).reshape(b, h, n, d)


def _rotary_angles(rotary_embed, seq_len):
    """(seq_len, dim_head) rotary angles, computed eagerly so they trace as constants."""
    with torch.no_grad():
        pos = torch.arange(seq_len, dtype=torch.float32)
        angles = pos[:, None] * rotary_embed.freqs.float()[None, :]
        return angles.repeat_interleave(2, dim=-1)


def _attention_forward(self, x):
    """Same math as bs_roformer.Attention.forward, written for the ONNX tracer:
    explicit reshapes, constant rotary angles, explicit softmax attention."""
    # Static ints and positive axes throughout: the TorchScript exporter loses
    # tensor rank when shapes are traced dynamically, then misreads negative axes.
    b, n = int(x.shape[0]), int(x.shape[1])
    h = self.heads
    x = self.norm(x)
    qkv = self.to_qkv(x)
    d = int(qkv.shape[2]) // (3 * h)
    qkv = qkv.reshape(b, n, 3, h, d).permute(2, 0, 3, 1, 4)  # 3 b h n d
    q, k, v = qkv[0], qkv[1], qkv[2]
    angles = _rotary_angles(self.rotary_embed, n)
    cos, sin = angles.cos(), angles.sin()
    q = q * cos + _rotate_half(q) * sin
    k = k * cos + _rotate_half(k) * sin
    scores = torch.matmul(q, k.transpose(2, 3)) * (d**-0.5)
    out = torch.matmul(scores.softmax(dim=3), v)  # b h n d
    gates = self.to_gates(x).permute(0, 2, 1).unsqueeze(3).sigmoid()  # b h n 1
    out = (out * gates).permute(0, 2, 1, 3).reshape(b, n, h * d)
    return self.to_out(out)


def patch_attention(model):
    from audio_separator.separator.uvr_lib_v5.roformer.bs_roformer import Attention

    for mod in model.modules():
        if isinstance(mod, Attention):
            mod.forward = _attention_forward.__get__(mod)


class RoformerCore(torch.nn.Module):
    """stft (b, s, f, t, 2) real -> mask (b, n, f*s, t, 2) real."""

    def __init__(self, m):
        super().__init__()
        replace_glu(m)
        patch_attention(m)
        m.band_split.forward = _band_split_forward.__get__(m.band_split)
        for est in m.mask_estimators:
            est.forward = _mask_estimator_forward.__get__(est)
        self.m = m

    def forward(self, stft):
        # Explicit permute/reshape instead of einops pack/unpack, which bakes
        # wrong shapes into the traced graph.
        m = self.m
        b, s, f, t, c = (int(n) for n in stft.shape)
        x = stft.permute(0, 3, 2, 1, 4).reshape(b, t, f * s * c)  # b t (f s c)
        x = m.band_split(x)  # b t bands d
        bands, d = int(x.shape[2]), int(x.shape[3])
        for block in m.layers:
            time_t, freq_t = block[-2], block[-1]
            x = x.permute(0, 2, 1, 3).reshape(b * bands, t, d)
            x = time_t(x)
            x = x.reshape(b, bands, t, d).permute(0, 2, 1, 3).reshape(b * t, bands, d)
            x = freq_t(x)
            x = x.reshape(b, t, bands, d)
        x = m.final_norm(x)
        mask = torch.stack([fn(x) for fn in m.mask_estimators], dim=1)  # b n t (f s c)
        n = int(mask.shape[1])
        return mask.reshape(b, n, t, f * s, c).permute(0, 1, 3, 2, 4)  # b n (f s) t c


# --- DSP outside the graph (matches torch.stft/istft with center=True, hann) ---

WINDOW = get_window("hann", N_FFT, fftbins=True).astype(np.float32)


def stft(audio):
    """audio (s, n) -> (s, f, t) complex, same framing as torch.stft(center=True)."""
    pad = N_FFT // 2
    x = np.pad(audio, ((0, 0), (pad, pad)), mode="reflect")
    n_frames = 1 + (x.shape[1] - N_FFT) // HOP
    idx = np.arange(N_FFT)[None, :] + HOP * np.arange(n_frames)[:, None]
    frames = x[:, idx] * WINDOW  # (s, t, n_fft)
    return np.fft.rfft(frames, axis=-1).transpose(0, 2, 1)


def istft(spec, length):
    """spec (..., f, t) complex -> (..., length)."""
    frames = np.fft.irfft(spec.swapaxes(-1, -2), n=N_FFT, axis=-1) * WINDOW
    n_frames = frames.shape[-2]
    out_len = N_FFT + HOP * (n_frames - 1)
    out = np.zeros(frames.shape[:-2] + (out_len,), dtype=np.float32)
    norm = np.zeros(out_len, dtype=np.float32)
    for i in range(n_frames):
        out[..., i * HOP : i * HOP + N_FFT] += frames[..., i, :]
        norm[i * HOP : i * HOP + N_FFT] += WINDOW**2
    out /= np.maximum(norm, 1e-8)
    pad = N_FFT // 2
    return out[..., pad : pad + length]


def apply_masks(spec, mask):
    """spec (s, f, t) complex, mask (n, f*s, t, 2) -> stems (n, s, f, t) complex."""
    s = spec.shape[0]
    m = mask[..., 0] + 1j * mask[..., 1]
    m = m.reshape(m.shape[0], -1, s, m.shape[-1]).transpose(0, 2, 1, 3)  # (n, s, f, t)
    return spec[None] * m


def patch_for_export():
    # The upstream rotary helper concatenates empty edge tensors, which ONNX
    # Runtime rejects at load time. audio-separator's DirectML branch uses an
    # equivalent concat-free formula; force it for tracing.
    from audio_separator.separator.uvr_lib_v5.roformer import rotary
    rotary._is_dml_device = lambda device: True

    # einops-based rotate_half exports with a wrong split axis; use plain reshapes.
    def rotate_half(x):
        pairs = x.reshape(*x.shape[:-1], x.shape[-1] // 2, 2)
        x1, x2 = pairs[..., 0], pairs[..., 1]
        return torch.stack((-x2, x1), dim=-1).reshape(x.shape)

    rotary.rotate_half = rotate_half


def export():
    patch_for_export()
    model, _ = load_torch_model()
    core = RoformerCore(model).eval()
    dummy = torch.randn(1, 2, N_FFT // 2 + 1, FRAMES, 2)
    ONNX_PATH.parent.mkdir(parents=True, exist_ok=True)
    t0 = time.perf_counter()
    torch.onnx.export(
        core, (dummy,), str(ONNX_PATH),
        input_names=["stft"], output_names=["mask"],
        opset_version=17, do_constant_folding=True,
    )
    print(f"exported {ONNX_PATH} ({ONNX_PATH.stat().st_size / 1e6:.0f} MB) in {time.perf_counter() - t0:.0f}s")


def compare(audio_path):
    import onnxruntime as ort
    import soundfile as sf

    audio, sr = sf.read(audio_path, dtype="float32", always_2d=True)
    assert sr == 44100, sr
    start = len(audio) // 3
    chunk = audio[start : start + CHUNK].T.copy()  # (2, CHUNK)

    model, cfg = load_torch_model()
    with torch.no_grad():
        t0 = time.perf_counter()
        ref = model(torch.from_numpy(chunk)[None])[0].numpy()  # (n, s, t)
        t_torch = time.perf_counter() - t0

    spec = stft(chunk)
    inp = np.stack([spec.real, spec.imag], -1)[None].astype(np.float32)
    def session(provider, dml_fusion=True):
        opts = ort.SessionOptions()
        if provider == "DmlExecutionProvider":
            opts.enable_mem_pattern = False
            opts.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
            if not dml_fusion:
                opts.add_session_config_entry("ep.dml.disable_graph_fusion", "1")
        return ort.InferenceSession(str(ONNX_PATH), opts, providers=[provider])

    results = {}
    for label, provider, fusion in [
        ("CPU", "CPUExecutionProvider", True),
        ("DirectML", "DmlExecutionProvider", True),
        ("DirectML (no graph fusion)", "DmlExecutionProvider", False),
    ]:
        try:
            sess = session(provider, fusion)
            sess.run(None, {"stft": inp})  # warm-up
            t0 = time.perf_counter()
            mask = sess.run(None, {"stft": inp})[0][0]
            results[label] = (mask, time.perf_counter() - t0)
        except Exception as e:
            print(f"{label}: FAILED ({str(e).strip().splitlines()[-1][:160]})")

    instruments = cfg["training"]["instruments"]
    print(f"chunk: {CHUNK / sr:.1f}s audio")
    print(f"pytorch cpu: {t_torch:.2f}s")
    for provider, (mask, elapsed) in results.items():
        stems = istft(apply_masks(spec, mask), CHUNK)
        print(f"{provider}: {elapsed:.2f}s")
        for i, name in enumerate(instruments):
            err = np.abs(stems[i] - ref[i]).max()
            snr = 10 * np.log10((ref[i] ** 2).sum() / (((stems[i] - ref[i]) ** 2).sum() + 1e-20))
            print(f"  {name:7s} max abs diff {err:.2e}  SNR vs torch {snr:5.1f} dB")


def debug():
    """Locate mismatches: numpy STFT vs torch, wrapper vs original model."""
    torch.manual_seed(0)
    audio = (torch.randn(2, CHUNK) * 0.1).numpy()

    window = torch.hann_window(N_FFT)
    ref_spec = torch.stft(torch.from_numpy(audio), N_FFT, HOP, window=window, return_complex=True).numpy()
    spec = stft(audio)
    print("stft max diff:", np.abs(spec - ref_spec).max(), "| istft roundtrip max diff:",
          np.abs(istft(spec, CHUNK) - audio).max())

    model, _ = load_torch_model()
    with torch.no_grad():
        ref = model(torch.from_numpy(audio)[None])[0].numpy()
    patch_for_export()
    core = RoformerCore(model).eval()
    inp = torch.from_numpy(np.stack([spec.real, spec.imag], -1)[None].astype(np.float32))
    with torch.no_grad():
        mask = core(inp)[0].numpy()
    out = istft(apply_masks(spec, mask), CHUNK)
    print("wrapper vs original model, max diff per stem:", np.abs(out - ref).reshape(len(ref), -1).max(1))


if __name__ == "__main__":
    {"export": export, "debug": debug, "compare": lambda: compare(sys.argv[2])}[sys.argv[1]]()
