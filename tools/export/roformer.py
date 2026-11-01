"""BS-RoFormer checkpoint -> ONNX export (network core only).

Cleaned-up, parameterized copy of the export code in spikes/onnx_roformer.py
(the spike is left untouched as the historical reference). The graph is:

    stft (1, 2, n_fft/2+1, F, 2) float32  ->  mask (1, N, (n_fft/2+1)*2, F, 2)

with F = chunk_size / hop + 1 (static). STFT, mask multiply and iSTFT run
outside the graph (tools/reference/roformer_separate.py).

Export-friendly rewrites (same math, verified by the accuracy check):
explicit GLU, explicit attention with constant rotary angles, static shapes,
positive axes, no einops pack/unpack. Root cause: the TorchScript exporter in
torch 2.4 loses tensor rank with dynamically traced shapes and then
misresolves negative axes.

Supported: BSRoformer (audio-separator's vendored implementation), stereo.
Mel-Band RoFormer needs its own core wrapper (not implemented yet).
"""

import time
from pathlib import Path

import torch
import yaml


class _Loader(yaml.SafeLoader):
    pass


_Loader.add_constructor("tag:yaml.org,2002:python/tuple", lambda l, n: tuple(l.construct_sequence(n)))


def load_config(cfg_path):
    return yaml.load(Path(cfg_path).read_text(encoding="utf-8"), Loader=_Loader)


def detect_family(cfg):
    m = cfg["model"]
    if "num_bands" in m and "freqs_per_bands" not in m:
        return "mel_band_roformer"
    if "freqs_per_bands" in m:
        return "bs_roformer"
    return "unknown"


def load_torch_model(ckpt_path, cfg):
    from audio_separator.separator.uvr_lib_v5.roformer.bs_roformer import BSRoformer

    family = detect_family(cfg)
    if family != "bs_roformer":
        raise NotImplementedError(f"model family {family!r} is not supported by the exporter yet")
    model = BSRoformer(**cfg["model"])
    state = torch.load(str(ckpt_path), map_location="cpu", weights_only=False)
    state = state.get("state_dict", state)
    model.load_state_dict(state)
    model.eval()
    return model


def model_params(cfg):
    """STFT/chunk parameters from the MSST-style yaml."""
    m, a = cfg["model"], cfg["audio"]
    n_fft = int(m.get("stft_n_fft", a.get("n_fft", 2048)))
    hop = int(m.get("stft_hop_length", 512))
    chunk = int(a["chunk_size"])
    if chunk % hop:
        raise ValueError(f"chunk_size {chunk} is not a multiple of the STFT hop {hop}")
    stems = cfg["training"]["instruments"]
    if cfg["training"].get("target_instrument"):
        stems = [cfg["training"]["target_instrument"]]
    return {
        "n_fft": n_fft,
        "hop": hop,
        "chunk": chunk,
        "frames": chunk // hop + 1,
        "stems": list(stems),
        "num_overlap": int(cfg.get("inference", {}).get("num_overlap", 2)),
        "sample_rate": int(a.get("sample_rate", 44100)),
        "stereo": bool(m.get("stereo", True)),
    }


# --- export-friendly patches -------------------------------------------------


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
    # x: (b, t, F). Positive axes and explicit slicing.
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
    """bs_roformer.Attention.forward with explicit reshapes, constant rotary
    angles and explicit softmax attention."""
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


def patch_rotary_module():
    """Global patches of the rotary helper module (call only after any eager
    PyTorch reference run: they change the module's code path)."""
    from audio_separator.separator.uvr_lib_v5.roformer import rotary

    # The upstream rotary helper concatenates empty edge tensors, which ONNX
    # Runtime rejects at load time; force the concat-free DirectML formula.
    rotary._is_dml_device = lambda device: True

    def rotate_half(x):
        pairs = x.reshape(*x.shape[:-1], x.shape[-1] // 2, 2)
        x1, x2 = pairs[..., 0], pairs[..., 1]
        return torch.stack((-x2, x1), dim=-1).reshape(x.shape)

    rotary.rotate_half = rotate_half


class RoformerCore(torch.nn.Module):
    """stft (b, s, f, t, 2) real -> mask (b, n, f*s, t, 2) real. Patches `m` in place."""

    def __init__(self, m):
        super().__init__()
        if any(len(block) != 2 for block in m.layers):
            raise NotImplementedError("linear transformer layers are not supported by the exporter")
        if getattr(m, "skip_connection", False):
            raise NotImplementedError("skip_connection models are not supported by the exporter")
        replace_glu(m)
        patch_attention(m)
        m.band_split.forward = _band_split_forward.__get__(m.band_split)
        for est in m.mask_estimators:
            est.forward = _mask_estimator_forward.__get__(est)
        self.m = m

    def forward(self, stft):
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


def export(model, params, onnx_path):
    """Patch `model` in place and export the core to `onnx_path`. Returns seconds taken."""
    patch_rotary_module()
    core = RoformerCore(model).eval()
    dummy = torch.randn(1, 2, params["n_fft"] // 2 + 1, params["frames"], 2)
    Path(onnx_path).parent.mkdir(parents=True, exist_ok=True)
    t0 = time.perf_counter()
    with torch.no_grad():
        torch.onnx.export(core, (dummy,), str(onnx_path), input_names=["stft"], output_names=["mask"],
                          opset_version=17, do_constant_folding=True)
    return time.perf_counter() - t0
