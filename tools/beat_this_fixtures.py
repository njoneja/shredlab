"""Reference fixtures for the Rust port of the Beat This! glue (crates/shredlab-beats).

Writes raw little-endian arrays plus a meta.json to tests/fixtures/local/beat_this/
(gitignored), using the NumPy reference in spikes/onnx_beat_this.py:

  synth_*      short synthetic signals: input audio, soxr-resampled mono and the
               log-mel spectrogram (preprocessing tests)
  song_*       per test song (from out/beat_this/<model>_<song>.npz, written by
               `spikes/onnx_beat_this.py ref`): torch logits, beats/downbeats and the
               reference spectrogram (postprocessing + end-to-end comparison)

Usage:
    .venv/Scripts/python.exe tools/beat_this_fixtures.py [--model final0]
"""

import argparse
import json
import sys
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "spikes"))
import onnx_beat_this as bt  # noqa: E402

OUT = ROOT / "tests" / "fixtures" / "local" / "beat_this"


def save(meta, name, arr, dtype):
    arr = np.ascontiguousarray(arr, dtype=dtype)
    path = OUT / f"{name}.{np.dtype(dtype).name}"
    arr.astype(arr.dtype.newbyteorder("<")).tofile(path)
    meta[name] = {"file": path.name, "dtype": np.dtype(dtype).name, "shape": list(arr.shape)}


def synth(seconds, sr, channels, seed):
    """Clicks at 120 BPM + two sines + a little noise; distinct per channel."""
    rng = np.random.default_rng(seed)
    t = np.arange(int(seconds * sr)) / sr
    out = []
    for c in range(channels):
        x = 0.3 * np.sin(2 * np.pi * (220 + 110 * c) * t) + 0.1 * np.sin(2 * np.pi * 3520 * t)
        x += 0.02 * rng.standard_normal(len(t))
        for beat in np.arange(0, seconds, 0.5):
            i = int(beat * sr)
            n = min(int(0.02 * sr), len(t) - i)
            x[i : i + n] += 0.8 * np.exp(-np.arange(n) / (0.003 * sr)) * rng.standard_normal(n)
        out.append(x)
    return np.stack(out, 1)  # (n, channels) float64


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", default="final0")
    args = ap.parse_args()
    OUT.mkdir(parents=True, exist_ok=True)
    meta = {"synth": [], "songs": []}

    # odd lengths on purpose (exercise reflect padding and the last frame)
    for name, seconds, sr, ch in [("synth_44k_stereo", 4.0123, 44100, 2), ("synth_48k_mono", 3.3, 48000, 1),
                                  ("synth_22k_mono", 2.71, 22050, 1)]:
        audio = synth(seconds, sr, ch, seed=len(meta["synth"]))
        audio = audio.astype(np.float32)  # the Rust side gets f32 samples
        mono = audio.astype(np.float64).mean(1)
        if sr != bt.SR:
            import soxr

            mono = soxr.resample(mono, in_rate=sr, out_rate=bt.SR)
        mono = mono.astype(np.float32)
        spect = bt.log_mel(mono)
        save(meta, f"{name}_audio", audio, np.float32)  # interleaved (n, ch)
        save(meta, f"{name}_mono22k", mono, np.float32)
        save(meta, f"{name}_spect", spect, np.float32)
        meta["synth"].append({"name": name, "sample_rate": sr, "channels": ch})
        print(f"{name}: {len(audio)} samples -> {len(mono)} @22050 -> {spect.shape}")

    for npz in sorted((ROOT / "out" / "beat_this").glob(f"{args.model}_*.npz")):
        r = np.load(npz)
        key = "song_" + "".join(ch if ch.isalnum() else "_" for ch in npz.stem[len(args.model) + 1 :]).strip("_")
        beats, downs = bt.postprocess(r["beat"], r["down"])
        assert np.array_equal(beats, r["beats"]) and np.array_equal(downs, r["downs"])
        save(meta, f"{key}_beat_logits", r["beat"], np.float32)
        save(meta, f"{key}_down_logits", r["down"], np.float32)
        save(meta, f"{key}_beats", r["beats"], np.float64)
        save(meta, f"{key}_downs", r["downs"], np.float64)
        save(meta, f"{key}_spect", r["spect"], np.float32)
        meta["songs"].append({"key": key, "npz": npz.name, "song": npz.stem[len(args.model) + 1 :]})
        print(f"{key}: {len(r['spect'])} frames, {len(r['beats'])} beats, {len(r['downs'])} downbeats")

    (OUT / "meta.json").write_text(json.dumps(meta, indent=1))
    print(f"wrote {OUT}")


if __name__ == "__main__":
    main()
