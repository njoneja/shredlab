"""model.json / selftest.json helpers (format: docs/MODEL_MANIFEST.md)."""

import hashlib
import json
from pathlib import Path

import numpy as np

from . import demucs_hybrid, roformer_separate
from .sessions import make_session
from .synth import NAME as SYNTH_NAME
from .synth import synth_v1

SCHEMA = 1
N_PROBES = 64
TOLERANCE_DB = 30.0
RMS_TOLERANCE = 0.05
FLOOR_REL = 0.1  # error is measured against max(stem level, FLOOR_REL * input level)


def sha256_file(path, bufsize=1 << 20):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while chunk := f.read(bufsize):
            h.update(chunk)
    return h.hexdigest()


def probe_offsets(length, n=N_PROBES):
    """Evenly spaced sample indices: floor((k + 0.5) * length / n), k = 0..n-1."""
    return [int((k + 0.5) * length // n) for k in range(n)]


def load_manifest(model_dir):
    model_dir = Path(model_dir)
    return json.loads((model_dir / "model.json").read_text(encoding="utf-8"))


def write_json(path, obj):
    Path(path).write_text(json.dumps(obj, indent=2) + "\n", encoding="utf-8")


def selftest_input(manifest):
    return synth_v1(manifest["segment_samples"], manifest["sample_rate"], manifest["channels"])


def rms(a):
    a = np.asarray(a, np.float64)
    return float(np.sqrt(np.mean(a**2)))


def run_selftest_input(manifest, sess):
    """Run the synth-v1 input through the adapter's single-segment path -> (N, 2, S)."""
    x = selftest_input(manifest)
    adapter = manifest["adapter"]
    if adapter == "demucs-hybrid":
        return demucs_hybrid.separate_single_segment(sess, x)
    if adapter == "roformer-mask":
        st = manifest["stft"]
        return roformer_separate.separate_single_segment(sess, x, st["n_fft"], st["hop"])
    raise ValueError(f"unknown adapter {adapter!r}")


def make_selftest(manifest, out):
    """Build the selftest.json dict from the reference output (N, 2, S)."""
    S = manifest["segment_samples"]
    offs = probe_offsets(S)
    stems = {}
    for i, name in enumerate(manifest["stems"]):
        y = out[i].astype(np.float64)
        stems[name] = {
            "rms": rms(y),
            "probe": [float(v) for v in out[i, 0, offs]],
        }
    return {
        "schema": SCHEMA,
        "input": SYNTH_NAME,
        "length": S,
        "stems": stems,
        "probe_offsets": offs,
        "input_rms": rms(selftest_input(manifest)),
        "floor_rel": FLOOR_REL,
        "tolerance_db": TOLERANCE_DB,
        "rms_tolerance": RMS_TOLERANCE,
    }


def check_selftest(selftest, manifest, out):
    """Compare an output (N, 2, S) against selftest.json. Returns (passed, per-stem results).

    Per stem, with P = len(probe_offsets), ref = expected probe, got = probe of `out`
    (channel 0), level = max(ref_rms_stem, floor_rel * input_rms):
        snr_db  = 10 log10( max(sum(ref^2), P * (floor_rel * input_rms)^2) / sum((got - ref)^2) )
        rms_err = |rms(out stem) - rms_expected| / level
        pass    = snr_db >= tolerance_db and rms_err < rms_tolerance
    The floor keeps near-silent stems (tiny but numerically noisy) from failing
    a healthy backend; broken backends are off by orders of magnitude anyway.
    """
    offs = selftest["probe_offsets"]
    tol = selftest["tolerance_db"]
    rtol = selftest["rms_tolerance"]
    floor = selftest["floor_rel"] * selftest["input_rms"]
    results, ok = {}, True
    for i, name in enumerate(manifest["stems"]):
        exp = selftest["stems"][name]
        ref = np.asarray(exp["probe"], np.float64)
        got = out[i, 0, offs].astype(np.float64)
        err = ((got - ref) ** 2).sum()
        sig = max((ref**2).sum(), len(offs) * floor**2)
        snr = 10 * np.log10(sig / (err + 1e-30))
        r = rms(out[i])
        rms_err = abs(r - exp["rms"]) / max(exp["rms"], floor)
        passed = bool(snr >= tol and rms_err < rtol)
        ok &= passed
        results[name] = {"snr_db": float(snr), "rms_err": rms_err, "rms_ratio": r / exp["rms"] if exp["rms"] else 0.0,
                         "pass": passed}
    return ok, results


def write_selftest(model_dir, manifest=None, ep="cpu"):
    """Run the self-test on `ep` (CPU for the reference) and write selftest.json."""
    model_dir = Path(model_dir)
    manifest = manifest or load_manifest(model_dir)
    sess = make_session(model_dir / manifest["file"], ep, manifest.get("session"))
    out = run_selftest_input(manifest, sess)
    st = make_selftest(manifest, out)
    write_json(model_dir / manifest.get("self_test", "selftest.json"), st)
    return st


def verify_model_dir(model_dir, ep="cpu", override=None):
    """Load a model folder, check sha256, run the self-test on `ep`. Returns (ok, results)."""
    model_dir = Path(model_dir)
    manifest = load_manifest(model_dir)
    onnx = model_dir / manifest["file"]
    if sha256_file(onnx) != manifest["sha256"]:
        raise ValueError(f"sha256 mismatch for {onnx}")
    selftest = json.loads((model_dir / manifest.get("self_test", "selftest.json")).read_text(encoding="utf-8"))
    sess = make_session(onnx, ep, manifest.get("session"), override)
    out = run_selftest_input(manifest, sess)
    return check_selftest(selftest, manifest, out)


def build_manifest(*, id, name, adapter, file, onnx_path, stems, segment_samples, overlap, stft,
                   session, license, personal, sample_rate=44100, channels=2):
    return {
        "schema": SCHEMA,
        "id": id,
        "name": name,
        "adapter": adapter,
        "file": file,
        "sha256": sha256_file(onnx_path),
        "sample_rate": sample_rate,
        "channels": channels,
        "stems": list(stems),
        "segment_samples": int(segment_samples),
        "overlap": overlap,
        "stft": stft,
        "session": session,
        "self_test": "selftest.json",
        "license": license,
        "personal": bool(personal),
    }


def install_onnx(src, dst, link=True):
    """Hardlink (same volume) or copy src -> dst."""
    import os
    import shutil

    src, dst = Path(src), Path(dst)
    dst.parent.mkdir(parents=True, exist_ok=True)
    if dst.exists():
        if dst.stat().st_size == src.stat().st_size and os.path.samefile(src, dst):
            return "exists"
        dst.unlink()
    if link:
        try:
            os.link(src, dst)
            return "hardlink"
        except OSError:
            pass
    shutil.copy2(src, dst)
    return "copy"
