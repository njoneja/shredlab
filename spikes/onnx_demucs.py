"""Spike: our own ONNX inference for htdemucs_6s (community export).

Model file: StemSplitio/htdemucs-6s-onnx (MIT), downloaded to
models/onnx/stemsplit-htdemucs-6s/. Only the .onnx file is used; the
chunking/normalization below follows demucs' reference apply_model.

Usage:
    python spikes/onnx_demucs.py compare <audio>        # one window vs PyTorch
    python spikes/onnx_demucs.py separate <audio>...    # full songs on DirectML
"""

import functools
import sys
import time
from pathlib import Path

import numpy as np
import soundfile as sf

ROOT = Path(__file__).resolve().parent.parent
ONNX_PATH = ROOT / "models" / "onnx" / "stemsplit-htdemucs-6s" / "htdemucs_6s.onnx"
TORCH_PATH = ROOT / "models" / "5c90dfd2-34c22ccb.th"

SOURCES = ("drums", "bass", "other", "vocals", "guitar", "piano")
SR = 44100
SEGMENT = 343980  # 7.8 s, fixed by the export
OVERLAP = 0.25


def session(provider="DmlExecutionProvider"):
    import onnxruntime as ort

    opts = ort.SessionOptions()
    if provider == "DmlExecutionProvider":
        opts.enable_mem_pattern = False
        opts.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
    return ort.InferenceSession(str(ONNX_PATH), opts, providers=[provider])


def run_segment(sess, mix, offset=0, n=None):
    """Separate mix[:, offset:offset+n] -> (6, 2, n). Like demucs'
    TensorChunk.padded, a short chunk is centered in a full segment filled
    with the surrounding audio (zeros only beyond the ends of the track)."""
    total = mix.shape[1]
    n = min(n or SEGMENT, total - offset)
    start = offset - (SEGMENT - n) // 2
    padded = np.zeros((2, SEGMENT), dtype=np.float32)
    lo, hi = max(0, start), min(total, start + SEGMENT)
    padded[:, lo - start : hi - start] = mix[:, lo:hi]
    out = sess.run(None, {"mix": padded[None]})[0][0]
    left = offset - start
    return out[:, :, left : left + n]


def separate(sess, mix):
    """mix (2, length) float32 -> (6, 2, length). Mirrors demucs apply_model
    with split=True, overlap=0.25, transition_power=1, shifts=0."""
    ref = mix.mean(0)
    mean, std = ref.mean(), max(ref.std(), np.finfo(np.float32).eps)
    mix = (mix - mean) / std

    length = mix.shape[1]
    stride = int((1 - OVERLAP) * SEGMENT)
    weight = np.concatenate([np.arange(1, SEGMENT // 2 + 1), np.arange(SEGMENT - SEGMENT // 2, 0, -1)])
    weight = (weight / weight.max()).astype(np.float32)

    out = np.zeros((len(SOURCES), 2, length), dtype=np.float32)
    total = np.zeros(length, dtype=np.float32)
    for offset in range(0, length, stride):
        n = min(SEGMENT, length - offset)
        out[:, :, offset : offset + n] += run_segment(sess, mix, offset, n) * weight[:n]
        total[offset : offset + n] += weight[:n]
    out /= total
    return out * std + mean


@functools.lru_cache(maxsize=None)
def demucs_modules():
    """Return (states, transformer) modules of the Demucs model code.

    Prefers upstream `demucs` (pip `demucs==4.0.1 --no-deps`, used by
    tools/setup_models.py) and falls back to the copy vendored in
    audio-separator (the original .venv). Upstream `demucs.states` imports
    `omegaconf` and `dora.log` at module level, but neither is used when
    loading a model, so missing ones are replaced by inert stubs instead of
    pulling in dora-search (and its training dependencies)."""
    import importlib.util
    import types

    if importlib.util.find_spec("demucs") is not None and importlib.util.find_spec("demucs.htdemucs") is not None:
        try:
            import omegaconf  # noqa: F401
        except ImportError:
            sys.modules["omegaconf"] = types.SimpleNamespace(OmegaConf=None)
        try:
            import dora.log  # noqa: F401
        except ImportError:
            def fatal(*args):
                raise RuntimeError(" ".join(map(str, args)))

            sys.modules["dora"] = types.ModuleType("dora")
            sys.modules["dora.log"] = types.SimpleNamespace(fatal=fatal)
        from demucs import states, transformer

        return states, transformer

    from audio_separator.separator.uvr_lib_v5 import demucs as vendored_demucs

    # The checkpoint pickles references to the `demucs` package; audio-separator
    # vendors it under another name.
    sys.modules.setdefault("demucs", vendored_demucs)
    for sub in ("htdemucs", "hdemucs", "demucs", "states", "spec", "transformer", "utils", "states"):
        try:
            sys.modules.setdefault(f"demucs.{sub}", __import__(f"audio_separator.separator.uvr_lib_v5.demucs.{sub}", fromlist=["_"]))
        except ImportError:
            pass
    from audio_separator.separator.uvr_lib_v5.demucs import states, transformer

    return states, transformer


def load_torch_model(path=None):
    import torch

    states, _ = demucs_modules()
    model = states.load_model(torch.load(path or TORCH_PATH, map_location="cpu", weights_only=False))
    model.eval()
    return model


def compare(audio_path):
    import torch

    audio, sr = sf.read(audio_path, dtype="float32", always_2d=True)
    assert sr == SR, sr
    start = len(audio) // 3
    window = audio[start : start + SEGMENT].T.copy()
    window = (window - window.mean()) / window.std()

    model = load_torch_model()
    with torch.no_grad():
        t0 = time.perf_counter()
        ref = model(torch.from_numpy(window)[None])[0].numpy()
        print(f"pytorch cpu: {time.perf_counter() - t0:.2f}s for {SEGMENT / SR:.1f}s window")

    for provider in ["CPUExecutionProvider", "DmlExecutionProvider"]:
        sess = session(provider)
        run_segment(sess, window)  # warm-up
        t0 = time.perf_counter()
        got = run_segment(sess, window)
        print(f"{provider}: {time.perf_counter() - t0:.2f}s")
        for i, name in enumerate(SOURCES):
            snr = 10 * np.log10((ref[i] ** 2).sum() / (((got[i] - ref[i]) ** 2).sum() + 1e-20))
            print(f"  {name:7s} max abs diff {np.abs(got[i] - ref[i]).max():.2e}  SNR vs torch {snr:5.1f} dB")


def separate_files(paths):
    import subprocess
    import tempfile

    # This export crashes the DirectML device on an RX 7900 XT (see spec), so CPU.
    sess = session("CPUExecutionProvider")
    for path in map(Path, paths):
        with tempfile.TemporaryDirectory() as tmp:
            wav = Path(tmp) / "in.wav"
            subprocess.run(["ffmpeg", "-loglevel", "error", "-y", "-i", str(path), "-ar", str(SR), "-ac", "2", str(wav)], check=True)
            audio, _ = sf.read(wav, dtype="float32", always_2d=True)
        t0 = time.perf_counter()
        stems = separate(sess, audio.T.copy())
        elapsed = time.perf_counter() - t0
        out_dir = ROOT / "out" / path.stem / "htdemucs_6s_onnx" / "stems"
        out_dir.mkdir(parents=True, exist_ok=True)
        for i, name in enumerate(SOURCES):
            sf.write(out_dir / f"{name}.wav", stems[i].T, SR)
        print(f"{elapsed:6.1f}s  ({len(audio) / SR:.0f}s audio)  {path.name}")

        # Rough agreement with audio-separator's PyTorch run (which used
        # random shifts=2, so not expected to be bit-identical).
        ref_dir = ROOT / "out" / path.stem / "htdemucs_6s" / "stems"
        for i, name in enumerate(SOURCES):
            ref_file = ref_dir / f"{name}.wav"
            if ref_file.exists():
                ref, _ = sf.read(ref_file, dtype="float32", always_2d=True)
                n = min(len(ref), stems.shape[2])
                diff = stems[i].T[:n] - ref[:n]
                snr = 10 * np.log10((ref[:n] ** 2).sum() / ((diff**2).sum() + 1e-20))
                print(f"    {name:7s} SNR vs PyTorch (shifts=2) run: {snr:5.1f} dB")


if __name__ == "__main__":
    cmd = sys.argv[1]
    if cmd == "compare":
        compare(sys.argv[2])
    elif cmd == "separate":
        separate_files(sys.argv[2:])
