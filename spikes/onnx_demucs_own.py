"""Spike: export htdemucs_6s to ONNX ourselves, STFT/iSTFT outside the graph.

The ONNX graph is the network core only:
    mix  (1, 2, 343980)     raw (already song-normalized) waveform segment
    mag  (1, 4, 2048, 336)  complex-as-channels spectrogram of `mix`
 -> wave (1, 6, 2, 343980)  time-branch output (de-normalized)
    spec (1, 6, 4, 2048, 336) freq-branch output, complex-as-channels (de-normalized)
The STFT, the CaC <-> complex reshapes, the iSTFT and the final sum
`wave + istft(spec)` are done in NumPy here (any DSP library in the app).
Both internal normalizations of HTDemucs.forward (spectrogram and waveform
mean/std) stay inside the graph.

Usage:
    python spikes/onnx_demucs_own.py export
    python spikes/onnx_demucs_own.py debug              # numpy DSP + wrapper vs torch (eager)
    python spikes/onnx_demucs_own.py compare <audio>    # one window: ONNX (CPU/DML) vs torch
    python spikes/onnx_demucs_own.py song <audio> <provider> [fusion1]  # full song vs community ONNX
    python spikes/onnx_demucs_own.py time <audio> <provider> [fusion1]  # timing only (fresh process per provider)

DirectML: the graph runs only with "ep.dml.disable_graph_fusion"=1 (default here;
`fusion1` re-enables it to reproduce the wrong output). Use a fresh process per
provider configuration.
"""

import math
import sys
import time
from pathlib import Path

import numpy as np
import soundfile as sf
import torch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import onnx_demucs  # noqa: E402
from onnx_demucs import SEGMENT, SOURCES, SR, load_torch_model  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
ONNX_PATH = ROOT / "models" / "onnx" / "htdemucs_6s_own.onnx"

NFFT, HOP = 4096, 1024
FRAMES = math.ceil(SEGMENT / HOP)  # 336
BINS = NFFT // 2  # 2048 (last bin dropped by demucs)


# --- network core -----------------------------------------------------------


class DemucsCore(torch.nn.Module):
    """HTDemucs.forward between `_magnitude` and `_mask`, fixed shapes."""

    def __init__(self, m):
        super().__init__()
        self.m = m

    def forward(self, mix, mag):
        m = self.m
        B, C, Fq, T = (int(s) for s in mag.shape)
        length = int(mix.shape[2])
        S = len(m.sources)

        x = mag
        mean = x.mean(dim=(1, 2, 3), keepdim=True)
        std = x.std(dim=(1, 2, 3), keepdim=True)
        x = (x - mean) / (1e-5 + std)

        xt = mix
        meant = xt.mean(dim=(1, 2), keepdim=True)
        stdt = xt.std(dim=(1, 2), keepdim=True)
        xt = (xt - meant) / (1e-5 + stdt)

        saved, saved_t, lengths, lengths_t = [], [], [], []
        for idx, encode in enumerate(m.encoder):
            lengths.append(int(x.shape[3]))
            inject = None
            if idx < len(m.tencoder):
                lengths_t.append(int(xt.shape[2]))
                tenc = m.tencoder[idx]
                xt = tenc(xt)
                if not tenc.empty:
                    saved_t.append(xt)
                else:
                    inject = xt
            x = encode(x, inject)
            if idx == 0 and m.freq_emb is not None:
                frs = torch.arange(int(x.shape[2]))
                emb = m.freq_emb(frs).t()[None, :, :, None].expand_as(x)
                x = x + m.freq_emb_scale * emb
            saved.append(x)

        x, xt = m.crosstransformer(x, xt)

        for idx, decode in enumerate(m.decoder):
            skip = saved.pop(-1)
            x, pre = decode(x, skip, lengths.pop(-1))
            offset = m.depth - len(m.tdecoder)
            if idx >= offset:
                tdec = m.tdecoder[idx - offset]
                length_t = lengths_t.pop(-1)
                if tdec.empty:
                    pre = pre[:, :, 0]
                    xt, _ = tdec(pre, None, length_t)
                else:
                    skip = saved_t.pop(-1)
                    xt, _ = tdec(xt, skip, length_t)

        x = x.reshape(B, S, C, Fq, T) * std.reshape(B, 1, 1, 1, 1) + mean.reshape(B, 1, 1, 1, 1)
        xt = xt.reshape(B, S, 2, length) * stdt.reshape(B, 1, 1, 1) + meant.reshape(B, 1, 1, 1)
        return xt, x


# --- export-friendly patches (each verified equal in eager by `debug`) -------


def _xformer_forward(self, x, xt):
    """CrossTransformerEncoder.forward with explicit permute/reshape instead of
    einops and positional embeddings computed eagerly (traced as constants)."""
    from audio_separator.separator.uvr_lib_v5.demucs.transformer import create_2d_sin_embedding

    B, C, Fr, T1 = (int(s) for s in x.shape)
    with torch.no_grad():
        pos_2d = create_2d_sin_embedding(C, Fr, T1, "cpu", self.max_period)  # 1 C Fr T1
        pos_2d = pos_2d.permute(0, 3, 2, 1).reshape(1, T1 * Fr, C).clone()
    x = x.permute(0, 3, 2, 1).reshape(B, T1 * Fr, C)
    x = self.norm_in(x)
    x = x + self.weight_pos_embed * pos_2d

    B, C, T2 = (int(s) for s in xt.shape)
    xt = xt.permute(0, 2, 1)
    with torch.no_grad():
        pos = self._get_pos_embedding(T2, B, C, "cpu").permute(1, 0, 2).clone()  # b t2 c
    xt = self.norm_in_t(xt)
    xt = xt + self.weight_pos_embed * pos

    for idx in range(self.num_layers):
        if idx % 2 == self.classic_parity:
            x = self.layers[idx](x)
            xt = self.layers_t[idx](xt)
        else:
            old_x = x
            x = self.layers[idx](x, xt)
            xt = self.layers_t[idx](xt, old_x)

    x = x.reshape(B, T1, Fr, C).permute(0, 3, 2, 1)
    xt = xt.permute(0, 2, 1)
    return x, xt


def _mha_forward(self, query, key, value, key_padding_mask=None, need_weights=True, attn_mask=None, average_attn_weights=True, is_causal=False):
    """nn.MultiheadAttention (batch_first, no masks) as explicit matmul+softmax."""
    assert attn_mask is None and key_padding_mask is None and self.batch_first
    b, n, e = (int(s) for s in query.shape)
    s = int(key.shape[1])
    h = self.num_heads
    d = e // h
    w, bias = self.in_proj_weight, self.in_proj_bias
    q = torch.nn.functional.linear(query, w[:e], bias[:e])
    k = torch.nn.functional.linear(key, w[e : 2 * e], bias[e : 2 * e])
    v = torch.nn.functional.linear(value, w[2 * e :], bias[2 * e :])
    q = q.reshape(b, n, h, d).permute(0, 2, 1, 3)
    k = k.reshape(b, s, h, d).permute(0, 2, 3, 1)
    v = v.reshape(b, s, h, d).permute(0, 2, 1, 3)
    att = torch.matmul(q * (d**-0.5), k).softmax(dim=3)
    out = torch.matmul(att, v).permute(0, 2, 1, 3).reshape(b, n, e)
    return self.out_proj(out), None


def _my_groupnorm_forward(self, x):
    # MyGroupNorm: (B, T, C) -> group norm over C -> (B, T, C), positive axes.
    x = x.permute(0, 2, 1)
    return torch.nn.functional.group_norm(x, self.num_groups, self.weight, self.bias, self.eps).permute(0, 2, 1)


def patch(model, mha=True):
    from audio_separator.separator.uvr_lib_v5.demucs import transformer as tr

    ct = model.crosstransformer
    ct.forward = _xformer_forward.__get__(ct)
    for mod in model.modules():
        if mha and isinstance(mod, torch.nn.MultiheadAttention):
            mod.forward = _mha_forward.__get__(mod)
        if isinstance(mod, tr.MyGroupNorm):
            mod.forward = _my_groupnorm_forward.__get__(mod)
        # Torch's fused fast path for TransformerEncoderLayer is skipped in
        # tracing anyway; make sure the plain Python path is used.
        if isinstance(mod, torch.nn.TransformerEncoderLayer):
            mod.activation_relu_or_gelu = 0
    return model


# --- DSP outside the graph (matches demucs spec.py + HTDemucs._spec/_ispec) ---

WINDOW = (0.5 - 0.5 * np.cos(2 * np.pi * np.arange(NFFT) / NFFT)).astype(np.float64)  # periodic hann


def spec(x):
    """x (C, L) -> (C, 2048, ceil(L/1024)) complex64, == HTDemucs._spec."""
    L = x.shape[-1]
    le = math.ceil(L / HOP)
    pad = HOP // 2 * 3
    import scipy.fft

    x = np.pad(x.astype(np.float32), ((0, 0), (pad, pad + le * HOP - L)), mode="reflect")
    # torch.stft(center=True, pad_mode=reflect, normalized=True)
    x = np.pad(x, ((0, 0), (NFFT // 2, NFFT // 2)), mode="reflect")
    n_frames = 1 + (x.shape[1] - NFFT) // HOP
    idx = np.arange(NFFT)[None, :] + HOP * np.arange(n_frames)[:, None]
    frames = x[:, idx] * (WINDOW / np.sqrt(NFFT)).astype(np.float32)
    z = scipy.fft.rfft(frames, axis=-1, workers=-1)  # (C, frames, 2049)
    z = z.transpose(0, 2, 1)[:, :-1, 2 : 2 + le]
    return z.astype(np.complex64)


def ispec(z, length):
    """z (..., 2048, T) complex -> (..., length), == HTDemucs._ispec."""
    z = np.concatenate([z, np.zeros(z.shape[:-2] + (1, z.shape[-1]), z.dtype)], axis=-2)
    z = np.pad(z, [(0, 0)] * (z.ndim - 1) + [(2, 2)])
    pad = HOP // 2 * 3
    le = HOP * math.ceil(length / HOP) + 2 * pad
    import scipy.fft

    win = WINDOW.astype(np.float32)
    frames = scipy.fft.irfft(z.swapaxes(-1, -2).astype(np.complex64), n=NFFT, axis=-1, workers=-1)
    frames *= np.float32(np.sqrt(NFFT)) * win
    # Overlap-add: NFFT == 4 * HOP, so each output hop block is the sum of 4 frame quarters.
    n = frames.shape[-2]
    q = frames.reshape(frames.shape[:-1] + (4, HOP))
    out = np.zeros(frames.shape[:-2] + (n + 3, HOP), np.float32)
    env = np.zeros((n + 3, HOP), np.float32)
    wq = (win**2).reshape(4, HOP)
    for k in range(4):
        out[..., k : k + n, :] += q[..., :, k, :]
        env[k : k + n] += wq[k]
    out = out.reshape(out.shape[:-2] + (-1,))
    env = env.reshape(-1)
    start = NFFT // 2  # center=True
    out = out[..., start : start + le] / np.maximum(env[start : start + le], 1e-11)
    return out[..., pad : pad + length]


def cac(z):
    """(C, F, T) complex -> (1, 2C, F, T) real, == HTDemucs._magnitude."""
    return np.stack([z.real, z.imag], 1).reshape(1, -1, *z.shape[-2:]).astype(np.float32)


def uncac(x):
    """(S, 2C, F, T) real -> (S, C, F, T) complex, == HTDemucs._mask (cac)."""
    S, C2, F, T = x.shape
    x = x.reshape(S, C2 // 2, 2, F, T)
    return x[:, :, 0] + 1j * x[:, :, 1]


class OwnSession:
    """Drop-in for onnx_demucs.run_segment's `sess`: run(None, {"mix": (1,2,N)})."""

    def __init__(self, provider="CPUExecutionProvider", fusion=False, path=ONNX_PATH):
        # DirectML graph fusion produces garbage output for this graph (verified
        # on RX 7900 XT, ORT-DML 1.24.4); with it disabled DML matches torch to ~120 dB.
        import onnxruntime as ort

        opts = ort.SessionOptions()
        if provider == "DmlExecutionProvider":
            opts.enable_mem_pattern = False
            opts.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
            if not fusion:
                opts.add_session_config_entry("ep.dml.disable_graph_fusion", "1")
        self.sess = ort.InferenceSession(str(path), opts, providers=[provider])
        self.t_net = 0.0

    def run(self, _names, feeds):
        mix = feeds["mix"][0]
        z = spec(mix)
        t0 = time.perf_counter()
        wave, sp = self.sess.run(None, {"mix": mix[None], "mag": cac(z)})
        self.t_net += time.perf_counter() - t0
        return [(wave[0] + ispec(uncac(sp[0]), mix.shape[-1]))[None]]


# --- commands ----------------------------------------------------------------


def _snr(ref, got):
    return 10 * np.log10((ref**2).sum() / (((got - ref) ** 2).sum() + 1e-20))


def _window(audio_path):
    audio, sr = sf.read(audio_path, dtype="float32", always_2d=True)
    assert sr == SR, sr
    start = len(audio) // 3
    w = audio[start : start + SEGMENT].T.copy()
    return (w - w.mean()) / w.std()


def export():
    model = patch(load_torch_model())
    core = DemucsCore(model).eval()
    mix = torch.randn(1, 2, SEGMENT)
    mag = torch.from_numpy(cac(spec(mix[0].numpy())))
    t0 = time.perf_counter()
    with torch.no_grad():
        torch.onnx.export(core, (mix, mag), str(ONNX_PATH), input_names=["mix", "mag"],
                          output_names=["wave", "spec"], opset_version=17, do_constant_folding=True)
    print(f"exported {ONNX_PATH} ({ONNX_PATH.stat().st_size / 1e6:.0f} MB) in {time.perf_counter() - t0:.0f}s")


def debug(audio_path=None):
    torch.manual_seed(0)
    mix = _window(audio_path) if audio_path else (torch.randn(2, SEGMENT) * 0.3).numpy()
    model = load_torch_model()
    with torch.no_grad():
        zt = model._spec(torch.from_numpy(mix)[None])[0].numpy()
        z = spec(mix)
        print("spec vs torch: rel max diff", np.abs(z - zt).max() / np.abs(zt).max())
        back = model._ispec(torch.from_numpy(zt)[None, None], SEGMENT)[0, 0].numpy()
        print("ispec vs torch: rel max diff", np.abs(ispec(zt, SEGMENT) - back).max() / np.abs(back).max())
        ref = model(torch.from_numpy(mix)[None])[0].numpy()
        for label, m in [("unpatched", model), ("patched", patch(model))]:
            wave, sp = DemucsCore(m)(torch.from_numpy(mix)[None], torch.from_numpy(cac(z)))
            got = wave[0].numpy() + ispec(uncac(sp[0].numpy()), SEGMENT)
            print(f"wrapper ({label}) vs model, SNR per stem:", " ".join(f"{_snr(ref[i], got[i]):.1f}" for i in range(6)))


def bisect():
    """Export sub-modules one at a time and compare ONNX vs torch (CPU)."""
    import io

    import onnxruntime as ort

    model = patch(load_torch_model())
    torch.manual_seed(0)

    def check(name, module, *inputs):
        buf = io.BytesIO()
        with torch.no_grad():
            torch.onnx.export(module, inputs, buf, opset_version=17, do_constant_folding=True)
            ref = module(*inputs)
        ref = ref[0] if isinstance(ref, tuple) else ref
        sess = ort.InferenceSession(buf.getvalue(), providers=["CPUExecutionProvider"])
        got = sess.run(None, {i.name: x.numpy() for i, x in zip(sess.get_inputs(), inputs)})[0]
        rel = np.abs(got - ref.numpy()).max() / (np.abs(ref.numpy()).max() + 1e-12)
        print(f"{name:24s} rel err {rel:.2e}  shape {tuple(ref.shape)}")

    ct = model.crosstransformer
    check("encoder[0]", model.encoder[0], torch.randn(1, 4, 2048, FRAMES))
    check("tencoder[0]", model.tencoder[0], torch.randn(1, 2, SEGMENT))
    check("crosstransformer", ct, torch.randn(1, 512, 8, FRAMES), torch.randn(1, 512, FRAMES))
    check("ct.layers[0]", ct.layers[0], torch.randn(1, 8 * FRAMES, 512))
    check("ct.layers[1]", ct.layers[1], torch.randn(1, 8 * FRAMES, 512), torch.randn(1, FRAMES, 512))


def compare(audio_path):
    mix = _window(audio_path)
    model = load_torch_model()
    with torch.no_grad():
        t0 = time.perf_counter()
        ref = model(torch.from_numpy(mix)[None])[0].numpy()
        print(f"pytorch cpu: {time.perf_counter() - t0:.2f}s")
    provider = sys.argv[3] if len(sys.argv) > 3 else "CPUExecutionProvider"
    fusion = "fusion1" in sys.argv
    sess = OwnSession(provider, fusion)
    sess.run(None, {"mix": mix[None]})
    t0 = time.perf_counter()
    sess.t_net = 0
    got = sess.run(None, {"mix": mix[None]})[0][0]
    print(f"{provider} fusion={fusion}: {time.perf_counter() - t0:.2f}s total, {sess.t_net:.2f}s in ORT")
    for i, name in enumerate(SOURCES):
        print(f"  {name:7s} max abs diff {np.abs(got[i] - ref[i]).max():.2e}  SNR vs torch {_snr(ref[i], got[i]):5.1f} dB")


def song(audio_path, provider, fusion, check=True):
    audio, sr = sf.read(audio_path, dtype="float32", always_2d=True)
    assert sr == SR, sr
    mix = audio.T.copy()
    sess = OwnSession(provider, fusion)
    sess.run(None, {"mix": mix[:, :SEGMENT][None]})  # warm-up
    sess.t_net = 0
    t0 = time.perf_counter()
    got = onnx_demucs.separate(sess, mix)
    el = time.perf_counter() - t0
    n_win = len(range(0, mix.shape[1], int(0.75 * SEGMENT)))
    print(f"own {provider} fusion={fusion}: {el:.1f}s for {mix.shape[1] / SR:.0f}s audio "
          f"({n_win} windows, {el / n_win:.2f}s/window, {sess.t_net / n_win:.2f}s/window in ORT)")
    if not check:
        return
    ref_path = Path(audio_path).with_suffix(".community.npy")
    if ref_path.exists():
        ref = np.load(ref_path)
    else:
        csess = onnx_demucs.session("CPUExecutionProvider")
        onnx_demucs.run_segment(csess, mix[:, :SEGMENT])
        t0 = time.perf_counter()
        ref = onnx_demucs.separate(csess, mix)
        el = time.perf_counter() - t0
        print(f"community CPU: {el:.1f}s ({el / n_win:.2f}s/window)")
        np.save(ref_path, ref)
    for i, name in enumerate(SOURCES):
        print(f"  {name:7s} SNR vs community ONNX (CPU) {_snr(ref[i], got[i]):5.1f} dB")


if __name__ == "__main__":
    cmd = sys.argv[1]
    if cmd == "export":
        export()
    elif cmd == "debug":
        debug(sys.argv[2] if len(sys.argv) > 2 else None)
    elif cmd == "bisect":
        bisect()
    elif cmd == "compare":
        compare(sys.argv[2])
    elif cmd in ("song", "time"):
        song(sys.argv[2], sys.argv[3], "fusion1" in sys.argv, check=cmd == "song")
