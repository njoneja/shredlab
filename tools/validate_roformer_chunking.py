"""Validate tools/reference/roformer_separate.py against audio-separator (PyTorch).

Runs the full song through
  (a) audio-separator 0.48 MDXCSeparator.demix (PyTorch, CPU) with the same
      chunk size (segment_size = chunk/hop + 1, override on) and num_overlap,
  (b) our ONNX + numpy reference separate(),
and prints the per-stem SNR of (b) against (a). Peak normalization done by
audio-separator's `separate()` is skipped on both sides (we call demix()
directly on the same decoded array).

Usage:
    python tools/validate_roformer_chunking.py <audio> [--model-dir models/personal/bs-roformer-sw]
        [--ep dml|cpu] [--seconds N]
"""

import argparse
import logging
import sys
import time
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from tools.make_goldens import decode  # noqa: E402
from tools.reference import manifest as mf  # noqa: E402
from tools.reference import roformer_separate as rs  # noqa: E402
from tools.reference.sessions import make_session  # noqa: E402


def snr_db(ref, got):
    ref, got = ref.astype(np.float64), got.astype(np.float64)
    return 10 * np.log10((ref**2).sum() / (((got - ref) ** 2).sum() + 1e-30))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("audio")
    ap.add_argument("--model-dir", default=str(ROOT / "models" / "personal" / "bs-roformer-sw"))
    ap.add_argument("--ckpt", default="BS-Roformer-SW.ckpt", help="audio-separator model filename in models/")
    ap.add_argument("--ep", default="dml", choices=["cpu", "dml"])
    ap.add_argument("--seconds", type=float, default=0, help="only the first N seconds (0 = full song)")
    a = ap.parse_args()

    man = mf.load_manifest(a.model_dir)
    mix = decode(a.audio)
    if a.seconds:
        mix = np.ascontiguousarray(mix[:, : int(a.seconds * man["sample_rate"])])
    print(f"song: {mix.shape[1] / man['sample_rate']:.1f}s; chunk {man['segment_samples']}, overlap {man['overlap']}")

    cache = Path(a.audio).with_name(Path(a.audio).stem + ".audiosep_roformer.npy")
    cache = ROOT / "out" / "validation" / cache.name
    if cache.exists() and np.load(cache, mmap_mode="r").shape[-1] == mix.shape[1]:
        ref = np.load(cache)
        print(f"PyTorch reference loaded from {cache}")
    else:
        from audio_separator.separator import Separator

        dim_t = man["segment_samples"] // man["stft"]["hop"] + 1
        sep = Separator(log_level=logging.WARNING, model_file_dir=str(ROOT / "models"),
                        output_dir=str(ROOT / "out" / "validation"), use_directml=False,
                        mdxc_params={"segment_size": dim_t, "override_model_segment_size": True,
                                     "batch_size": 1, "overlap": man["overlap"], "pitch_shift": 0})
        sep.load_model(a.ckpt)
        inst = sep.model_instance
        t0 = time.perf_counter()
        src = inst.demix(mix.copy(), override_model_segment_size=True)
        print(f"audio-separator demix (PyTorch CPU): {time.perf_counter() - t0:.0f}s")
        ref = np.stack([src[k] for k in man["stems"]]).astype(np.float32)
        cache.parent.mkdir(parents=True, exist_ok=True)
        np.save(cache, ref)

    sess = make_session(Path(a.model_dir) / man["file"], a.ep, man.get("session"),
                        {"disable_graph_fusion": True} if a.ep == "dml" else None)
    t0 = time.perf_counter()
    got = rs.separate(sess, mix, man["overlap"], man["stft"]["n_fft"], man["stft"]["hop"])
    print(f"reference ONNX ({a.ep}): {time.perf_counter() - t0:.0f}s")
    for i, name in enumerate(man["stems"]):
        print(f"  {name:7s} SNR vs audio-separator {snr_db(ref[i], got[i]):6.1f} dB")


if __name__ == "__main__":
    main()
