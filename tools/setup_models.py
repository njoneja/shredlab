#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Navneet Joneja

"""Download the model checkpoints and build the ONNX models the app bundles.

Run from anywhere with any Python 3.9+ (or via scripts/setup-models.ps1 / .sh):

    python tools/setup_models.py [--out DIR] [--venv DIR] [--force]

What it does (each step is skipped when its output already exists with the
expected sha256; --force redoes everything):

  1. Creates a CPU-only Python 3.12 venv (default: .venv-models) with the
     pinned packages from requirements-models.txt. Uses uv if it is on PATH,
     else `python3.12 -m venv` + pip. The DirectML venv `.venv` is never used
     or modified.
  2. Downloads the official checkpoints (both MIT) and checks their sha256:
       - Demucs v4 htdemucs_6s (Meta)  -> <out>/5c90dfd2-34c22ccb.th
       - Beat This! final0 (CPJKU)     -> <out>/beat_this/beat_this-final0.ckpt
  3. Exports <out>/onnx/htdemucs_6s_own.onnx   (spikes/onnx_demucs_own.py)
  4. Exports <out>/onnx/beat_this_final0.onnx  (spikes/onnx_beat_this.py)
  5. Builds <out>/registry/htdemucs_6s/        (tools/registry.py build-htdemucs)
  6. Verifies on the CPU: the registry self-test, plus both ONNX models against
     reference outputs of the known-good models (tools/reference/data/).

<out> defaults to the repo's models/ folder, which is where the Tauri bundle
config and debug builds look. Nothing is ever deleted: a file that has to be
replaced is renamed to <name>.bak-<timestamp> first. Personal models
(models/personal, tools/personal_models.local.json) are not touched.

Top-level code is stdlib only, so the system Python can run it; the
`_worker` subcommands run inside the model venv.
"""

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
REQUIREMENTS = REPO / "requirements-models.txt"
# Installed with --no-deps (see requirements-models.txt).
NO_DEPS = ["demucs==4.0.1"]
PYTHON_VERSION = "3.12"
REFERENCE_DIR = REPO / "tools" / "reference" / "data"
STATE_FILE = ".setup_models.json"
# Bump when the export recipe changes so existing exports are rebuilt.
EXPORT_VERSION = 1

CHECKPOINTS = [
    {
        "key": "demucs",
        "name": "Demucs v4 htdemucs_6s weights (Meta, MIT)",
        "url": "https://dl.fbaipublicfiles.com/demucs/hybrid_transformer/5c90dfd2-34c22ccb.th",
        "rel": "5c90dfd2-34c22ccb.th",
        # Upstream encodes the first 8 hex digits of the sha256 in the file
        # name (34c22ccb); the full hash was recorded from the download.
        "sha256": "34c22ccb381c6f9fdbf324f04e1e2fe21aaaf293f5ded163a162697ff9a02ddd",
        "size": 54996327,
    },
    {
        "key": "beat_this",
        "name": "Beat This! final0 weights (CPJKU, MIT)",
        # CHECKPOINT_URL from beat_this/inference.py (beat-this 1.1.0).
        "url": "https://cloud.cp.jku.at/public.php/dav/files/7ik4RrBKTS273gp/final0.ckpt",
        "rel": "beat_this/beat_this-final0.ckpt",
        # Not published upstream; recorded from the download.
        "sha256": "8c328b45f59d8dd3dff219253ff6a8d6482be57d0133a29140e2febbf8eb8331",
        "size": 81058141,
    },
]

DEMUCS_ONNX = "onnx/htdemucs_6s_own.onnx"
BEAT_ONNX = "onnx/beat_this_final0.onnx"
REGISTRY = "registry/htdemucs_6s"
# Hashes of the ONNX files the app was developed against (torch 2.4.1 on
# Windows). A fresh export on another machine or torch build may differ in
# bytes; it is accepted if it passes the reference checks in step 6.
KNOWN_GOOD = {
    DEMUCS_ONNX: {"e3fcd39627cdcbee85e23b83cf4abc18eb4cd54aa4fc543ffdcde0dece220c83"},
    BEAT_ONNX: {"9df93baf2148771b3c7a3cbde1167d31747c3ff8d097f095a455845d1635072b"},
}

N_STEPS = 6


# ----------------------------------------------------------------- helpers


def step(n, text):
    print(f"\n[{n}/{N_STEPS}] {text}", flush=True)


def info(text):
    print(f"      {text}", flush=True)


def fail(text):
    print(f"\nERROR: {text}", file=sys.stderr, flush=True)
    sys.exit(1)


def sha256_file(path, bufsize=1 << 20):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while chunk := f.read(bufsize):
            h.update(chunk)
    return h.hexdigest()


def move_aside(path):
    """Rename an existing file/folder to <name>.bak-<timestamp> (never delete)."""
    path = Path(path)
    if not path.exists():
        return
    stamp = time.strftime("%Y%m%d-%H%M%S")
    dst = path.with_name(f"{path.name}.bak-{stamp}")
    i = 1
    while dst.exists():
        dst = path.with_name(f"{path.name}.bak-{stamp}-{i}")
        i += 1
    path.rename(dst)
    info(f"kept the old file as {dst.name}")


def mb(n):
    return f"{n / 1e6:.0f} MB"


def load_state(out):
    p = out / STATE_FILE
    try:
        return json.loads(p.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}


def save_state(out, state):
    (out / STATE_FILE).write_text(json.dumps(state, indent=2) + "\n", encoding="utf-8")


def run(cmd, **kw):
    printable = " ".join(f'"{c}"' if " " in str(c) else str(c) for c in cmd)
    info(f"$ {printable}")
    r = subprocess.run([str(c) for c in cmd], **kw)
    if r.returncode != 0:
        fail(f"command failed with exit code {r.returncode}: {printable}")


def venv_python(venv):
    if os.name == "nt":
        return venv / "Scripts" / "python.exe"
    return venv / "bin" / "python"


# ------------------------------------------------------- 1. Python venv


def env_fingerprint():
    h = hashlib.sha256(REQUIREMENTS.read_bytes())
    h.update("\n".join(NO_DEPS).encode())
    h.update(PYTHON_VERSION.encode())
    return h.hexdigest()


def find_python312():
    """A Python 3.12 interpreter for `-m venv` (used only when uv is missing)."""
    if sys.version_info[:2] == (3, 12):
        return [sys.executable]
    candidates = [["py", "-3.12"]] if os.name == "nt" else []
    candidates += [["python3.12"], ["python3"], ["python"]]
    for c in candidates:
        if not shutil.which(c[0]):
            continue
        try:
            v = subprocess.run(c + ["-c", "import sys; print('%d.%d' % sys.version_info[:2])"],
                               capture_output=True, text=True, timeout=30).stdout.strip()
        except (OSError, subprocess.SubprocessError):
            continue
        if v == PYTHON_VERSION:
            return c
    return None


def setup_venv(venv, force):
    if venv.resolve() == (REPO / ".venv").resolve():
        fail(".venv is the DirectML dev environment; use a separate venv (default .venv-models).")
    py = venv_python(venv)
    marker = venv / ".shredlab-requirements.sha256"
    fp = env_fingerprint()
    if not force and py.exists() and marker.exists() and marker.read_text().strip() == fp:
        info(f"up to date: {venv}")
        return py

    uv = shutil.which("uv")
    if uv:
        info(f"using uv ({uv})")
        if not py.exists():
            run([uv, "venv", "--python", PYTHON_VERSION, venv])
        info("installing packages (the first run downloads PyTorch, ~200 MB)")
        run([uv, "pip", "install", "--python", py, "-r", REQUIREMENTS])
        run([uv, "pip", "install", "--python", py, "--no-deps", *NO_DEPS])
    else:
        info("uv not found; using python -m venv + pip")
        if not py.exists():
            base = find_python312()
            if not base:
                fail(f"Python {PYTHON_VERSION} not found. Install uv (https://docs.astral.sh/uv/) "
                     f"or Python {PYTHON_VERSION} (https://www.python.org/downloads/), then run this again.")
            run(base + ["-m", "venv", venv])
        run([py, "-m", "pip", "install", "--upgrade", "pip"])
        info("installing packages (the first run downloads PyTorch, ~200 MB)")
        run([py, "-m", "pip", "install", "-r", REQUIREMENTS])
        run([py, "-m", "pip", "install", "--no-deps", *NO_DEPS])
    marker.write_text(fp + "\n")
    return py


# ---------------------------------------------------- 2. checkpoints


def download(url, dest, size_hint):
    dest.parent.mkdir(parents=True, exist_ok=True)
    part = dest.with_name(dest.name + ".part")
    req = urllib.request.Request(url, headers={"User-Agent": "shredlab-setup/1"})
    t0 = time.time()
    with urllib.request.urlopen(req, timeout=60) as r, open(part, "wb") as f:
        total = int(r.headers.get("Content-Length") or size_hint or 0)
        done, last = 0, 0.0
        while chunk := r.read(1 << 20):
            f.write(chunk)
            done += len(chunk)
            now = time.time()
            if now - last > 1 or done == total:
                last = now
                pct = f"{100 * done / total:5.1f}%" if total else ""
                print(f"\r      {pct} {done / 1e6:6.1f} / {total / 1e6:.1f} MB", end="", flush=True)
    print(f"  ({time.time() - t0:.0f} s)", flush=True)
    return part


def fetch_checkpoints(out, force):
    paths = {}
    for ck in CHECKPOINTS:
        dest = out / ck["rel"]
        paths[ck["key"]] = dest
        info(ck["name"])
        if dest.exists():
            if not force and sha256_file(dest) == ck["sha256"]:
                info(f"  ok (sha256 matches): {dest}")
                continue
            info("  existing file has a different sha256" if not force else "  --force: downloading again")
            move_aside(dest)
        info(f"  downloading {ck['url']}")
        part = download(ck["url"], dest, ck["size"])
        got = sha256_file(part)
        if got != ck["sha256"]:
            fail(f"sha256 mismatch for {ck['url']}\n  expected {ck['sha256']}\n  got      {got}\n"
                 f"  (the partial download is kept at {part})")
        part.rename(dest)
        info(f"  ok: {dest}")
    return paths


# ---------------------------------------------------- 3./4. exports


def export_is_current(out, state, rel, inputs):
    p = out / rel
    if not p.exists():
        return False
    h = sha256_file(p)
    rec = state.get(rel, {})
    if rec.get("sha256") == h and rec.get("inputs") == inputs:
        return True
    if h in KNOWN_GOOD.get(rel, ()):
        state[rel] = {"sha256": h, "inputs": inputs}
        return True
    return False


def export_step(py, out, state, rel, inputs, worker_args, force):
    dest = out / rel
    if not force and export_is_current(out, state, rel, inputs):
        info(f"ok (sha256 matches): {dest}")
        return
    if dest.exists():
        info("existing file is not from this setup recipe" if not force else "--force: exporting again")
    tmp = dest.with_name(dest.stem + ".tmp" + dest.suffix)
    if tmp.exists():
        tmp.unlink()  # our own leftover from an interrupted run
    dest.parent.mkdir(parents=True, exist_ok=True)
    t0 = time.time()
    run([py, "-I", Path(__file__).resolve(), "_worker", *worker_args, "--out-file", tmp])
    move_aside(dest)
    tmp.rename(dest)
    state[rel] = {"sha256": sha256_file(dest), "inputs": inputs}
    save_state(out, state)
    info(f"done in {time.time() - t0:.0f} s: {dest} ({mb(dest.stat().st_size)})")


# ---------------------------------------------------- 5. registry


def registry_is_current(out):
    d = out / REGISTRY
    try:
        man = json.loads((d / "model.json").read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return False
    onnx = d / man.get("file", "")
    if not onnx.is_file() or not (d / man.get("self_test", "selftest.json")).is_file():
        return False
    h = sha256_file(onnx)
    return h == man.get("sha256") and h == sha256_file(out / DEMUCS_ONNX)


def registry_step(py, out, force):
    d = out / REGISTRY
    if not force and registry_is_current(out):
        info(f"ok (model.json matches the ONNX file): {d}")
        return
    for name in ("model.json", "selftest.json", Path(DEMUCS_ONNX).name):
        move_aside(d / name)
    run([py, "-I", REPO / "tools" / "registry.py", "build-htdemucs", "--dest", d.parent, "--onnx", out / DEMUCS_ONNX])


# ------------------------------------------------------------- main


def main():
    ap = argparse.ArgumentParser(description="Download checkpoints and build the bundled ONNX models.")
    ap.add_argument("--out", type=Path, default=REPO / "models", help="models folder (default: repo models/)")
    ap.add_argument("--venv", type=Path, default=REPO / ".venv-models", help="model export venv (default: .venv-models)")
    ap.add_argument("--force", action="store_true", help="redo every step (old files are kept as *.bak-*)")
    a = ap.parse_args()
    out = a.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    t_start = time.time()
    print(f"ShredLab model setup\n  models folder: {out}\n  venv:          {a.venv.resolve()}")

    step(1, f"Python {PYTHON_VERSION} environment for exporting (CPU only)")
    py = setup_venv(a.venv.resolve(), a.force)

    step(2, "Model checkpoints")
    ck = fetch_checkpoints(out, a.force)

    state = load_state(out)
    step(3, "Export Demucs htdemucs_6s to ONNX (1-3 minutes)")
    export_step(py, out, state, DEMUCS_ONNX,
                {"checkpoint": CHECKPOINTS[0]["sha256"], "export_version": EXPORT_VERSION},
                ["export-demucs", "--ckpt", ck["demucs"]], a.force)

    step(4, "Export Beat This! final0 to ONNX (under a minute)")
    export_step(py, out, state, BEAT_ONNX,
                {"checkpoint": CHECKPOINTS[1]["sha256"], "export_version": EXPORT_VERSION},
                ["export-beat", "--ckpt", ck["beat_this"]], a.force)
    save_state(out, state)

    step(5, "Model folder registry/htdemucs_6s (model.json + self-test reference)")
    registry_step(py, out, a.force)

    step(6, "Verify on the CPU")
    run([py, "-I", Path(__file__).resolve(), "_worker", "verify", "--models", out])

    print(f"\nAll done in {time.time() - t_start:.0f} s. Models are in {out}:")
    print(f"  {out / REGISTRY}")
    print(f"  {out / BEAT_ONNX}")
    if out != (REPO / "models").resolve():
        print("\nThis is not the repo's models/ folder, which the app bundle and debug builds use.")
        print("To run the app against these models:")
        print(f"  SHREDLAB_MODELS_DIR={out / 'registry'}")
        print(f"  SHREDLAB_BEAT_MODEL={out / BEAT_ONNX}")


# ------------------------------------------------- worker (model venv)


def _worker():
    sys.path.insert(0, str(REPO))
    sys.path.insert(0, str(REPO / "spikes"))
    ap = argparse.ArgumentParser(prog="setup_models.py _worker")
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("export-demucs", "export-beat"):
        p = sub.add_parser(name)
        p.add_argument("--ckpt", required=True)
        p.add_argument("--out-file", required=True)
    p = sub.add_parser("verify")
    p.add_argument("--models", required=True, type=Path)
    p = sub.add_parser("make-beat-reference")
    p.add_argument("--onnx", required=True)
    a = ap.parse_args(sys.argv[2:])
    if a.cmd.startswith("export"):
        # torch's tracer warnings are expected for these fixed-shape exports
        # and only confuse; correctness is checked in the verify step.
        import warnings

        warnings.simplefilter("ignore")

    if a.cmd == "export-demucs":
        import onnx_demucs_own

        onnx_demucs_own.export(onnx_path=a.out_file, ckpt_path=a.ckpt)
    elif a.cmd == "export-beat":
        import onnx_beat_this

        onnx_beat_this.export("final0", ckpt_path=a.ckpt, out_path=a.out_file)
    elif a.cmd == "verify":
        sys.exit(0 if _verify(a.models) else 1)
    elif a.cmd == "make-beat-reference":
        out = REFERENCE_DIR / "beat_this_final0_check.json"
        ref = _beat_outputs(a.onnx)
        ref["source_sha256"] = sha256_file(a.onnx)
        out.write_text(json.dumps(ref, indent=1) + "\n", encoding="utf-8")
        print(f"wrote {out}")


BEAT_PROBES = 100


def _beat_outputs(onnx):
    """Beat This! logits for a deterministic chunk: log-mel of synth-v1 (mono, 22.05 kHz)."""
    import numpy as np
    import onnxruntime as ort
    import onnx_beat_this as bt

    from tools.reference.synth import synth_v1

    n = bt.CHUNK * bt.HOP
    mono = synth_v1(n, bt.SR, 2).mean(0).astype(np.float32)
    spect = bt.log_mel(mono)[: bt.CHUNK]
    so = ort.SessionOptions()
    so.intra_op_num_threads = max(1, min((os.cpu_count() or 2) // 2, 16))
    sess = ort.InferenceSession(str(onnx), so, providers=["CPUExecutionProvider"])
    beat, down = sess.run(None, {"spect": spect[None]})
    offs = [int((k + 0.5) * bt.CHUNK // BEAT_PROBES) for k in range(BEAT_PROBES)]
    res = {"input": "synth-v1 mono 22050 Hz -> log-mel, first 1500 frames", "probe_offsets": offs}
    for name, y in (("beat", beat[0]), ("downbeat", down[0])):
        res[name] = {"rms": float(np.sqrt(np.mean(y.astype(np.float64) ** 2))), "probe": [float(v) for v in y[offs]]}
    return res


def _snr_db(ref, got):
    import numpy as np

    ref, got = np.asarray(ref, np.float64), np.asarray(got, np.float64)
    return float(10 * np.log10((ref**2).sum() / (((got - ref) ** 2).sum() + 1e-30)))


def _verify(models):
    import numpy as np

    from tools.reference import manifest as mf
    from tools.reference.sessions import make_session

    ok_all = True
    reg = models / REGISTRY

    # a) self-test of the folder (CPU vs its own selftest.json, plus sha256)
    ok, res = mf.verify_model_dir(reg, "cpu")
    print(f"      registry self-test (CPU vs selftest.json): {'PASS' if ok else 'FAIL'}")
    ok_all &= ok

    # b) same input against the reference probes of the known-good model
    ref_st = json.loads((REFERENCE_DIR / "htdemucs_6s_selftest.json").read_text(encoding="utf-8"))
    man = mf.load_manifest(reg)
    sess = make_session(reg / man["file"], "cpu", man.get("session"))
    out = mf.run_selftest_input(man, sess)
    ok, res = mf.check_selftest(ref_st, man, out)
    worst = min(r["snr_db"] for r in res.values())
    print(f"      htdemucs_6s vs reference model: {'PASS' if ok else 'FAIL'} (worst stem probe SNR {worst:.1f} dB, "
          f"need {ref_st['tolerance_db']:.0f})")
    ok_all &= ok

    # c) Beat This! logits against the reference model
    ref_b = json.loads((REFERENCE_DIR / "beat_this_final0_check.json").read_text(encoding="utf-8"))
    got = _beat_outputs(models / BEAT_ONNX)
    for name in ("beat", "downbeat"):
        snr = _snr_db(ref_b[name]["probe"], got[name]["probe"])
        rms_err = abs(got[name]["rms"] - ref_b[name]["rms"]) / ref_b[name]["rms"]
        ok = bool(snr >= 30.0 and rms_err < 0.05 and np.isfinite(snr))
        print(f"      beat_this_final0 {name:8s} vs reference model: {'PASS' if ok else 'FAIL'} "
              f"(SNR {snr:.1f} dB, need 30; rms err {rms_err:.1e})")
        ok_all &= ok
    print(f"      overall: {'PASS' if ok_all else 'FAIL'}")
    return ok_all


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "_worker":
        _worker()
    else:
        main()
