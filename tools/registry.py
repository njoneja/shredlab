# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Navneet Joneja

"""Build and verify model folders (model.json + onnx + selftest.json).

Usage (from the repo root, venv python):
    python tools/registry.py build-htdemucs [--dest models/registry] [--onnx models/onnx/htdemucs_6s_own.onnx]
    python tools/registry.py verify <model dir> [--ep cpu|dml] [--dml-fusion]

`build-htdemucs` packages models/onnx/htdemucs_6s_own.onnx (exported by
spikes/onnx_demucs_own.py) as the bundled-model candidate. `verify` checks the
sha256 and runs the self-test; `--dml-fusion` re-enables DirectML graph
fusion to demonstrate that the self-test catches the bad output. Run each
DirectML configuration in a fresh process (a DML device crash poisons the
process).
"""

import argparse
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from tools.reference import manifest as mf  # noqa: E402

HTDEMUCS_ONNX = ROOT / "models" / "onnx" / "htdemucs_6s_own.onnx"

DEMUCS_SESSION = {
    "dml": {"disable_graph_fusion": True, "enable_mem_pattern": False, "sequential": True},
    "cpu": {"intra_op_threads": 0},
}


def build_htdemucs(dest, onnx=HTDEMUCS_ONNX):
    onnx = Path(onnx)
    model_dir = Path(dest) / "htdemucs_6s"
    how = mf.install_onnx(onnx, model_dir / HTDEMUCS_ONNX.name)
    manifest = mf.build_manifest(
        id="htdemucs_6s",
        name="Demucs v4 (6 stems)",
        adapter="demucs-hybrid",
        file=HTDEMUCS_ONNX.name,
        onnx_path=model_dir / HTDEMUCS_ONNX.name,
        stems=["drums", "bass", "other", "vocals", "guitar", "piano"],
        segment_samples=343980,
        overlap=0.25,
        stft={"n_fft": 4096, "hop": 1024},
        session=DEMUCS_SESSION,
        license={
            "spdx": "MIT",
            "source": "https://github.com/facebookresearch/demucs",
            "notes": "Weights: htdemucs_6s (5c90dfd2-34c22ccb.th), Meta, MIT. ONNX export by this project "
                     "(spikes/onnx_demucs_own.py).",
        },
        personal=False,
    )
    mf.write_json(model_dir / "model.json", manifest)
    st = mf.write_selftest(model_dir, manifest, ep="cpu")
    print(f"built {model_dir} (onnx: {how}); selftest rms: "
          + ", ".join(f"{k}={v['rms']:.4f}" for k, v in st["stems"].items()))


def verify(model_dir, ep, fusion):
    override = {"disable_graph_fusion": False} if fusion else None
    ok, res = mf.verify_model_dir(model_dir, ep, override)
    label = f"{ep}" + (" (DML graph fusion ON)" if fusion else "")
    print(f"self-test {label}: {'PASS' if ok else 'FAIL'}")
    for name, r in res.items():
        print(f"  {name:8s} probe SNR {r['snr_db']:7.1f} dB  rms ratio {r['rms_ratio']:.4f} (err {r['rms_err']:.2e})  {'ok' if r['pass'] else 'FAIL'}")
    return ok


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    b = sub.add_parser("build-htdemucs")
    b.add_argument("--dest", default=str(ROOT / "models" / "registry"))
    b.add_argument("--onnx", default=str(HTDEMUCS_ONNX), help="exported htdemucs_6s_own.onnx")
    v = sub.add_parser("verify")
    v.add_argument("model_dir")
    v.add_argument("--ep", default="cpu", choices=["cpu", "dml"])
    v.add_argument("--dml-fusion", action="store_true")
    a = ap.parse_args()
    if a.cmd == "build-htdemucs":
        build_htdemucs(a.dest, a.onnx)
    else:
        sys.exit(0 if verify(a.model_dir, a.ep, a.dml_fusion) else 1)


if __name__ == "__main__":
    main()
