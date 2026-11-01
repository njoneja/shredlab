"""Spike: export the Beat This! beat/downbeat tracker (CPJKU, MIT) to ONNX.

The ONNX graph contains only the network: log-mel spectrogram chunk in
(1, 1500, 128), beat + downbeat logits out (1, 1500) each. Everything else
stays outside the graph and is reimplemented here in NumPy so it can be ported
to Rust 1:1:
  - downmix to mono, resample to 22050 Hz (soxr, like upstream)
  - log-mel spectrogram (torchaudio MelSpectrogram settings, numpy STFT)
  - chunking: 1500-frame chunks, 6-frame border, keep_first overlap
  - "minimal" postprocessing: 7-frame max-pool peak picking, logit > 0,
    merge adjacent peaks, snap downbeats to nearest beat (no madmom/DBN).

Upstream source is vendored (read-only) in spikes/third_party/beat_this/ and is
only imported for the PyTorch reference / export. Without it, the installed
`beat-this` package (pip beat-this==1.1.0, same code) is used.

Usage:
    python spikes/onnx_beat_this.py export [final0|small0]
    python spikes/onnx_beat_this.py ref <audio>...            # torch reference -> out/beat_this/*.npz
    python spikes/onnx_beat_this.py compare <audio>...        # numpy preproc + ONNX (CPU) vs torch reference
    python spikes/onnx_beat_this.py time <CPU|DML> <audio>... # timing; run each provider in a fresh process
Set BEAT_THIS_MODEL=small0 to use the small model in ref/compare/time, and
BEAT_THIS_THREADS=<n> for the ORT intra-op thread count (default 8, 0 = ORT default).
"""

import os
import sys
import time
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent
UPSTREAM = ROOT / "spikes" / "third_party" / "beat_this" / "beat_this-main"
MODEL_NAME = os.environ.get("BEAT_THIS_MODEL", "final0")
CKPT_DIR = ROOT / "models" / "beat_this"
OUT_DIR = ROOT / "out" / "beat_this"


def onnx_path(name=MODEL_NAME):
    return ROOT / "models" / "onnx" / f"beat_this_{name}.onnx"


SR = 22050
N_FFT, HOP = 1024, 441  # 50 frames per second
F_MIN, F_MAX, N_MELS = 30.0, 11000.0, 128
FPS = 50
CHUNK, BORDER = 1500, 6


# ---------------------------------------------------------------- audio / DSP


def load_mono(path):
    """Decode, downmix to mono float64 (upstream averages channels), resample to 22050 Hz with soxr (HQ)."""
    import soundfile as sf
    import soxr

    audio, sr = sf.read(str(path), dtype="float64", always_2d=True)
    mono = audio.mean(1)
    if sr != SR:
        mono = soxr.resample(mono, in_rate=sr, out_rate=SR)
    return mono.astype(np.float32), audio, sr


def _hz_to_mel_slaney(f):
    f = np.asarray(f, dtype=np.float64)
    f_sp, min_log_hz, min_log_mel, logstep = 200.0 / 3, 1000.0, 15.0, np.log(6.4) / 27.0
    return np.where(f >= min_log_hz, min_log_mel + np.log(np.maximum(f, 1e-10) / min_log_hz) / logstep, f / f_sp)


def _mel_to_hz_slaney(m):
    m = np.asarray(m, dtype=np.float64)
    f_sp, min_log_hz, min_log_mel, logstep = 200.0 / 3, 1000.0, 15.0, np.log(6.4) / 27.0
    return np.where(m >= min_log_mel, min_log_hz * np.exp(logstep * (m - min_log_mel)), f_sp * m)


def mel_filterbank():
    """(n_freqs, n_mels); same formula as torchaudio.functional.melscale_fbanks(norm=None, mel_scale='slaney')."""
    n_freqs = N_FFT // 2 + 1
    all_freqs = np.linspace(0, SR // 2, n_freqs)
    m_pts = np.linspace(_hz_to_mel_slaney(F_MIN), _hz_to_mel_slaney(F_MAX), N_MELS + 2)
    f_pts = _mel_to_hz_slaney(m_pts)
    f_diff = f_pts[1:] - f_pts[:-1]
    slopes = f_pts[None, :] - all_freqs[:, None]
    down = -slopes[:, :-2] / f_diff[:-1]
    up = slopes[:, 2:] / f_diff[1:]
    return np.maximum(0.0, np.minimum(down, up)).astype(np.float32)


MEL_FB = mel_filterbank()
WINDOW = (0.5 - 0.5 * np.cos(2 * np.pi * np.arange(N_FFT) / N_FFT)).astype(np.float32)  # periodic hann


def log_mel(mono):
    """mono float32 (n,) at 22050 Hz -> (frames, 128) float32, frames = n // 441 + 1.

    torch.stft(center=True, pad_mode='reflect', normalized=True -> * n_fft**-0.5),
    magnitude (power=1), slaney mel without norm, log1p(1000 * x)."""
    pad = N_FFT // 2
    x = np.pad(mono, (pad, pad), mode="reflect")
    n_frames = 1 + (len(x) - N_FFT) // HOP
    idx = np.arange(N_FFT)[None, :] + HOP * np.arange(n_frames)[:, None]
    mag = np.abs(np.fft.rfft(x[idx] * WINDOW, axis=1)).astype(np.float32) * np.float32(N_FFT**-0.5)
    return np.log1p(1000.0 * (mag @ MEL_FB)).astype(np.float32)


# ------------------------------------------------- chunking (mirrors upstream)


def split_piece(spect):
    """Upstream split_piece(chunk_size=1500, border_size=6, avoid_short_end=True).
    Returns fixed-size (CHUNK, 128) chunks; pieces shorter than CHUNK are zero-padded at the end
    (upstream instead feeds a shorter chunk -- see caveat in the report)."""
    n = len(spect)
    starts = np.arange(-BORDER, n - BORDER, CHUNK - 2 * BORDER)
    if n > CHUNK - 2 * BORDER:
        starts[-1] = n - (CHUNK - BORDER)
    chunks = []
    for s in starts:
        c = spect[max(s, 0) : min(s + CHUNK, n)]
        left = max(0, -s)
        right = CHUNK - left - len(c)  # upstream pads right by at most BORDER; rest is only for short pieces
        chunks.append(np.pad(c, ((left, right), (0, 0))))
    return chunks, starts


def aggregate(preds, starts, n):
    """Upstream aggregate_prediction(overlap_mode='keep_first'): preds list of (beat, downbeat) (CHUNK,) logits."""
    beat = np.full(n, -1000.0, np.float32)
    down = np.full(n, -1000.0, np.float32)
    for s, (b, d) in reversed(list(zip(starts, preds))):
        lo, hi = s + BORDER, min(s + CHUNK - BORDER, n)
        beat[lo:hi] = b[BORDER : BORDER + hi - lo]
        down[lo:hi] = d[BORDER : BORDER + hi - lo]
    return beat, down


# ---------------------------------------------------- minimal postprocessing


def _dedup(frames, width=1):
    out, it = [], iter(int(f) for f in frames)
    p = next(it, None)
    if p is None:
        return np.array([])
    c = 1
    for p2 in it:
        if p2 - p <= width:
            c += 1
            p += (p2 - p) / c
        else:
            out.append(p)
            p, c = p2, 1
    out.append(p)
    return np.array(out, dtype=np.float64)


def _peaks(logits):
    pad = np.pad(logits, 3, constant_values=-np.inf)
    win = np.lib.stride_tricks.sliding_window_view(pad, 7).max(1)
    return np.nonzero((logits == win) & (logits > 0))[0]


def postprocess(beat_logits, down_logits):
    """Upstream Postprocessor(type='minimal'). Returns (beat_times, downbeat_times) in seconds."""
    beats = _dedup(_peaks(beat_logits)) / FPS
    downs = _dedup(_peaks(down_logits)) / FPS
    if len(beats):
        downs = np.array([beats[np.argmin(np.abs(beats - d))] for d in downs])
    return beats, np.unique(downs)


def tempo(beats):
    return 60.0 / np.median(np.diff(beats)) if len(beats) > 1 else 0.0


# ----------------------------------------------------------- torch / export


def load_torch_model(name=MODEL_NAME, ckpt_path=None):
    # Model code: the vendored upstream source if present, else the installed
    # `beat-this` package (pip beat-this==1.1.0, used by tools/setup_models.py).
    if UPSTREAM.is_dir():
        sys.path.insert(0, str(UPSTREAM))
    from beat_this.inference import load_model

    ckpt = ckpt_path or CKPT_DIR / f"beat_this-{name}.ckpt"
    return load_model(str(ckpt), "cpu")  # torch.load(weights_only=True)


def _rotate_half(x):
    b, h, n, d = (int(s) for s in x.shape)
    pairs = x.reshape(b, h, n, d // 2, 2)
    return torch.stack((-pairs[:, :, :, :, 1], pairs[:, :, :, :, 0]), dim=4).reshape(b, h, n, d)


def _attention_forward(self, x):
    """Same math as beat_this.model.roformer.Attention.forward with static shapes, constant
    rotary angles (no empty-tensor concat) and explicit softmax attention instead of SDPA."""
    b, n, _ = (int(s) for s in x.shape)
    h = self.heads
    x = self.norm(x)
    qkv = self.to_qkv(x)
    d = int(qkv.shape[2]) // (3 * h)
    qkv = qkv.reshape(b, n, 3, h, d).permute(2, 0, 3, 1, 4)
    q, k, v = qkv[0], qkv[1], qkv[2]
    with torch.no_grad():
        ang = torch.arange(n, dtype=torch.float32)[:, None] * self.rotary_embed.freqs.float()[None, :]
        ang = ang.repeat_interleave(2, dim=1)
    cos, sin = ang.cos(), ang.sin()
    q = q * cos + _rotate_half(q) * sin
    k = k * cos + _rotate_half(k) * sin
    scores = torch.matmul(q, k.transpose(2, 3)) * (d**-0.5)
    out = torch.matmul(scores.softmax(dim=3), v)  # b h n d
    gates = self.to_gates(x).permute(0, 2, 1).unsqueeze(3).sigmoid()
    out = (out * gates).permute(0, 2, 1, 3).reshape(b, n, h * d)
    return self.to_out(out)


def _rmsnorm_forward(self, x):
    # F.normalize(x, dim=-1) with a positive axis
    ax = x.dim() - 1
    return x / x.norm(p=2, dim=ax, keepdim=True).clamp_min(1e-12) * self.scale * self.gamma


def _partial_ft_forward(self, x):
    b, c, f, t = (int(s) for s in x.shape)
    x = x.permute(0, 3, 2, 1).reshape(b * t, f, c)
    x = x + self.attnF(x)
    x = x + self.ffF(x)
    x = x.reshape(b, t, f, c).permute(0, 2, 1, 3).reshape(b * f, t, c)
    x = x + self.attnT(x)
    x = x + self.ffT(x)
    return x.reshape(b, f, t, c).permute(0, 3, 1, 2)


def make_core(model):
    from beat_this.model import roformer
    from beat_this.model.beat_tracker import PartialFTTransformer

    for m in model.modules():
        if isinstance(m, roformer.Attention):
            m.forward = _attention_forward.__get__(m)
        elif isinstance(m, roformer.RMSNorm):
            m.forward = _rmsnorm_forward.__get__(m)
        elif isinstance(m, PartialFTTransformer):
            m.forward = _partial_ft_forward.__get__(m)

    class Core(torch.nn.Module):
        """spect (1, T, 128) -> beat (1, T), downbeat (1, T) logits."""

        def __init__(self, m):
            super().__init__()
            self.m = m

        def forward(self, spect):
            fe = self.m.frontend
            x = spect.permute(0, 2, 1)  # b f t
            x = fe.stem.bn1d(x).unsqueeze(1)
            x = fe.stem.activation(fe.stem.bn2d(fe.stem.conv2d(x)))
            x = fe.blocks(x)  # b c f t
            b, c, f, t = (int(s) for s in x.shape)
            x = x.permute(0, 3, 1, 2).reshape(b, t, c * f)
            x = fe.linear(x)
            x = self.m.transformer_blocks(x)
            y = self.m.task_heads.beat_downbeat_lin(x)  # b t 2
            beat, down = y[:, :, 0], y[:, :, 1]
            return beat + down, down  # SumHead

    return Core(model).eval()


def export(name="final0", ckpt_path=None, out_path=None):
    global torch
    import torch

    model = load_torch_model(name, ckpt_path)
    assert hasattr(model.task_heads, "beat_downbeat_lin")
    core = make_core(model)
    dummy = torch.from_numpy(np.random.default_rng(0).standard_normal((1, CHUNK, N_MELS)).astype(np.float32))
    path = Path(out_path) if out_path else onnx_path(name)
    path.parent.mkdir(parents=True, exist_ok=True)
    t0 = time.perf_counter()
    torch.onnx.export(core, (dummy,), str(path), input_names=["spect"], output_names=["beat", "downbeat"],
                      opset_version=17, do_constant_folding=True)
    print(f"exported {path} ({path.stat().st_size / 1e6:.1f} MB) in {time.perf_counter() - t0:.0f}s")
    # sanity: wrapper vs upstream forward on the dummy chunk (fresh, unpatched model)
    with torch.no_grad():
        ref = load_torch_model(name, ckpt_path)(dummy)
        b, d = core(dummy)
    print("wrapper vs upstream max abs diff: beat %.2e downbeat %.2e"
          % ((b - ref["beat"]).abs().max(), (d - ref["downbeat"]).abs().max()))


# ------------------------------------------------------------ commands


def _key(path):
    return OUT_DIR / f"{MODEL_NAME}_{Path(path).stem}.npz"


def ref(paths):
    """Upstream pipeline end-to-end in PyTorch (Audio2Beats, minimal postprocessing)."""
    if UPSTREAM.is_dir():
        sys.path.insert(0, str(UPSTREAM))
    from beat_this.inference import Audio2Beats

    OUT_DIR.mkdir(parents=True, exist_ok=True)
    a2b = Audio2Beats(str(CKPT_DIR / f"beat_this-{MODEL_NAME}.ckpt"), device="cpu", dbn=False)
    for p in paths:
        _, audio, sr = load_mono(p)
        t0 = time.perf_counter()
        spect = a2b.signal2spect(audio, sr)  # upstream: mean(1), soxr, torchaudio LogMelSpect
        t1 = time.perf_counter()
        beat_l, down_l = a2b.spect2frames(spect)
        t2 = time.perf_counter()
        beats, downs = a2b.frames2beats(beat_l, down_l)
        t3 = time.perf_counter()
        np.savez(_key(p), spect=spect.numpy(), beat=beat_l.numpy(), down=down_l.numpy(), beats=beats, downs=downs)
        print(f"{Path(p).stem}: {len(audio) / sr:.0f}s audio, {len(beats)} beats, {len(downs)} downbeats, "
              f"~{tempo(beats):.1f} BPM | torch preproc {t1 - t0:.2f}s net {t2 - t1:.2f}s post {t3 - t2:.3f}s")


def make_session(provider):
    import onnxruntime as ort

    opts = ort.SessionOptions()
    # ORT's default (one thread per logical core) is ~5x slower on this hybrid P/E-core CPU;
    # 8-16 intra-op threads is the sweet spot. BEAT_THIS_THREADS=0 restores the default.
    opts.intra_op_num_threads = int(os.environ.get("BEAT_THIS_THREADS", "8"))
    if provider == "DmlExecutionProvider":
        opts.enable_mem_pattern = False
        opts.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
    return ort.InferenceSession(str(onnx_path()), opts, providers=[provider])


def run_onnx(sess, spect):
    chunks, starts = split_piece(spect)
    preds = []
    for c in chunks:
        b, d = sess.run(None, {"spect": c[None]})
        preds.append((b[0], d[0]))
    return aggregate(preds, starts, len(spect))


def _snr(ref_, x):
    return 10 * np.log10((ref_**2).sum() / (((x - ref_) ** 2).sum() + 1e-30))


def _match(a, b):
    """count diff and max |offset| (s) after nearest-neighbour matching."""
    if len(a) == 0 or len(b) == 0:
        return abs(len(a) - len(b)), float("nan")
    off = np.abs(b[:, None] - a[None, :]).min(1).max()
    return len(b) - len(a), off


def compare(paths):
    sess = make_session("CPUExecutionProvider")
    for p in paths:
        r = np.load(_key(p))
        mono, _, _ = load_mono(p)
        spect = log_mel(mono)
        valid = r["beat"] > -999
        print(f"== {Path(p).stem}")
        print(f"  spect: frames {len(spect)} vs {len(r['spect'])}, max abs diff {np.abs(spect - r['spect']).max():.2e}, "
              f"SNR {_snr(r['spect'], spect):.1f} dB")
        # network only: torch spect through ONNX vs torch logits
        b, d = run_onnx(sess, r["spect"])
        print(f"  logits (torch spect -> onnx): beat max abs {np.abs(b - r['beat']).max():.2e} SNR {_snr(r['beat'][valid], b[valid]):.1f} dB"
              f" | downbeat max abs {np.abs(d - r['down']).max():.2e} SNR {_snr(r['down'][valid], d[valid]):.1f} dB")
        # full numpy pipeline
        b, d = run_onnx(sess, spect)
        print(f"  logits (numpy spect -> onnx): beat max abs {np.abs(b - r['beat']).max():.2e} | downbeat max abs {np.abs(d - r['down']).max():.2e}")
        beats, downs = postprocess(b, d)
        rb, rd = postprocess(r["beat"], r["down"])
        assert np.array_equal(rb, r["beats"]) and np.array_equal(rd, r["downs"]), "numpy postprocess != upstream"
        nb, ob = _match(r["beats"], beats)
        nd, od = _match(r["downs"], downs)
        print(f"  beats {len(beats)} (ref {len(r['beats'])}, diff {nb:+d}, max offset {ob * 1000:.0f} ms, identical {np.array_equal(beats, r['beats'])})"
              f" | downbeats {len(downs)} (ref {len(r['downs'])}, diff {nd:+d}, max offset {od * 1000:.0f} ms, identical {np.array_equal(downs, r['downs'])})"
              f" | ~{tempo(beats):.1f} BPM")


def timing(provider, paths):
    prov = {"CPU": "CPUExecutionProvider", "DML": "DmlExecutionProvider"}[provider]
    t0 = time.perf_counter()
    sess = make_session(prov)
    sess.run(None, {"spect": np.zeros((1, CHUNK, N_MELS), np.float32)})  # warm-up
    print(f"{provider} ({MODEL_NAME}): session create + warm-up {time.perf_counter() - t0:.2f}s, providers {sess.get_providers()}")
    for p in paths:
        t0 = time.perf_counter()
        mono, audio, sr = load_mono(p)
        t1 = time.perf_counter()
        spect = log_mel(mono)
        t2 = time.perf_counter()
        b, d = run_onnx(sess, spect)
        t3 = time.perf_counter()
        beats, downs = postprocess(b, d)
        t4 = time.perf_counter()
        r = np.load(_key(p))
        same = np.array_equal(beats, r["beats"]) and np.array_equal(downs, r["downs"])
        print(f"  {Path(p).stem[:28]:28s} {len(audio) / sr:5.0f}s  decode+resample {t1 - t0:.2f}s  mel {t2 - t1:.2f}s  "
              f"net {t3 - t2:.2f}s ({len(split_piece(spect)[0])} chunks)  post {(t4 - t3) * 1000:.0f}ms  "
              f"beats {len(beats)} downbeats {len(downs)}  same-as-torch {same}")


if __name__ == "__main__":
    cmd = sys.argv[1]
    if cmd in ("export", "ref"):
        import torch
    if cmd == "export":
        export(sys.argv[2] if len(sys.argv) > 2 else "final0")
    elif cmd == "ref":
        ref(sys.argv[2:])
    elif cmd == "compare":
        compare(sys.argv[2:])
    elif cmd == "time":
        timing(sys.argv[2], sys.argv[3:])
