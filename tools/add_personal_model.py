"""Convert a model checkpoint to ONNX and install it as a personal model.

Usage (from the repo root):
    .venv\\Scripts\\python.exe tools/add_personal_model.py <name-or-path> [--dest DIR]
        [--config YAML] [--id ID] [--name "Display name"] [--audio FILE] [--check-dml]

<name-or-path>
    A short name from tools/personal_models.local.json (gitignored; format in
    tools/personal_models.example.json), or a path to a checkpoint file
    (the MSST-style .yaml config is looked up next to it unless --config).

Steps: show the license (or "unknown") -> load the PyTorch model -> export
the network core to ONNX -> accuracy check of ONNX + numpy DSP against the
PyTorch model (synth-v1 input, plus an excerpt of --audio if given) ->
write model.json + selftest.json -> install into <dest>/<id>/.

Default dest is models/personal/ in the repo. The app's real user models
folder (%APPDATA%\\<app>\\models on Windows) is resolved by the app; note
that processes launched from a packaged (MSIX) desktop app see a virtualized
AppData, so pass an explicit --dest when testing against the app.

Supported families: BS-RoFormer (adapter `roformer-mask`). Personal models
are for the user's own use; licensing is the user's responsibility.
"""

import argparse
import json
import re
import shutil
import subprocess
import sys
import time
import urllib.parse
import urllib.request
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from tools.reference import manifest as mf  # noqa: E402
from tools.reference import roformer_separate as rs  # noqa: E402
from tools.reference.sessions import make_session  # noqa: E402
from tools.reference.synth import synth_v1  # noqa: E402

LOCAL_SOURCES = ROOT / "tools" / "personal_models.local.json"
DEFAULT_DEST = ROOT / "models" / "personal"
DOWNLOAD_DIR = ROOT / "models" / "downloads"
ACCURACY_MIN_DB = 40.0

ROFORMER_SESSION = {
    "dml": {"disable_graph_fusion": False, "enable_mem_pattern": False, "sequential": True},
    "cpu": {"intra_op_threads": 0},
}


def snr_db(ref, got, floor_energy=0.0):
    """SNR of got vs ref; the reference energy is floored at floor_energy (see check_selftest)."""
    ref, got = ref.astype(np.float64), got.astype(np.float64)
    sig = max((ref**2).sum(), floor_energy)
    return float(10 * np.log10((sig + 1e-30) / (((got - ref) ** 2).sum() + 1e-30)))


def sanitize_id(s):
    s = re.sub(r"[^a-z0-9_-]+", "-", s.lower()).strip("-")
    if not s:
        raise SystemExit("could not derive a model id; pass --id")
    return s


def fetch(src, url_key, entry, base):
    """Resolve a local path (relative to the repo root) or download `url`."""
    if entry.get(src):
        p = Path(entry[src])
        p = p if p.is_absolute() else base / p
        if p.exists():
            return p
        if not entry.get(url_key):
            raise SystemExit(f"{src}: {p} does not exist and no {url_key} given")
    url = entry.get(url_key)
    if not url:
        return None
    DOWNLOAD_DIR.mkdir(parents=True, exist_ok=True)
    dst = DOWNLOAD_DIR / Path(urllib.parse.urlparse(url).path).name
    if not dst.exists():
        print(f"downloading {url} -> {dst}")
        tmp = dst.with_suffix(dst.suffix + ".part")
        urllib.request.urlretrieve(url, tmp)
        tmp.replace(dst)
    want = entry.get(f"{src}_sha256")
    if want and mf.sha256_file(dst) != want.lower():
        raise SystemExit(f"sha256 mismatch for {dst}")
    return dst


def resolve_source(arg, config_arg):
    """-> (id, entry dict with absolute 'checkpoint'/'config' Paths)."""
    p = Path(arg)
    if p.suffix and p.exists():
        cfg = Path(config_arg) if config_arg else p.with_suffix(".yaml")
        if not cfg.exists():
            raise SystemExit(f"config not found: {cfg} (pass --config)")
        return sanitize_id(p.stem), {"checkpoint": p.resolve(), "config": cfg.resolve(),
                                     "license": {"spdx": "unknown", "source": "", "notes": ""}}
    if not LOCAL_SOURCES.exists():
        raise SystemExit(f"{arg!r} is not a file and {LOCAL_SOURCES} does not exist "
                         f"(see tools/personal_models.example.json)")
    sources = json.loads(LOCAL_SOURCES.read_text(encoding="utf-8"))["models"]
    if arg not in sources:
        raise SystemExit(f"unknown model {arg!r}; known: {', '.join(sources)}")
    e = dict(sources[arg])
    e["checkpoint"] = fetch("checkpoint", "checkpoint_url", e, ROOT)
    e["config"] = Path(config_arg) if config_arg else fetch("config", "config_url", e, ROOT)
    if e["checkpoint"] is None or e["config"] is None:
        raise SystemExit(f"{arg}: needs checkpoint/checkpoint_url and config/config_url")
    return sanitize_id(e.get("id", arg)), e


def show_license(lic):
    print("License:")
    print(f"  SPDX:   {lic.get('spdx') or 'unknown'}")
    if lic.get("source"):
        print(f"  Source: {lic['source']}")
    if lic.get("notes"):
        print(f"  Notes:  {lic['notes']}")
    if (lic.get("spdx") or "unknown") == "unknown":
        print("  -> License unknown. For personal use only; do not redistribute the converted model.")


def load_audio_excerpt(path, length, sr=44100):
    with subprocess.Popen(["ffmpeg", "-loglevel", "error", "-i", str(path), "-f", "f32le", "-ac", "2",
                           "-ar", str(sr), "-"], stdout=subprocess.PIPE) as proc:
        raw = proc.stdout.read()
    audio = np.frombuffer(raw, np.float32).reshape(-1, 2).T
    start = max(0, audio.shape[1] // 3)
    seg = audio[:, start : start + length]
    if seg.shape[1] < length:
        seg = np.pad(seg, ((0, 0), (0, length - seg.shape[1])))
    return np.ascontiguousarray(seg)


def main():
    sys.stdout.reconfigure(line_buffering=True)
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("source", help="short name from personal_models.local.json, or a checkpoint path")
    ap.add_argument("--dest", default=str(DEFAULT_DEST), help=f"models folder (default {DEFAULT_DEST})")
    ap.add_argument("--config", help="model yaml (default: next to the checkpoint)")
    ap.add_argument("--id", help="model id (default: short name or checkpoint stem)")
    ap.add_argument("--name", help="display name")
    ap.add_argument("--audio", help="also check accuracy on an excerpt of this audio file (needs ffmpeg)")
    ap.add_argument("--check-dml", action="store_true", help="run the self-test on DirectML afterwards")
    a = ap.parse_args()

    model_id, entry = resolve_source(a.source, a.config)
    model_id = sanitize_id(a.id) if a.id else model_id
    lic = entry.get("license") or {"spdx": "unknown", "source": "", "notes": ""}
    lic = {"spdx": lic.get("spdx") or "unknown", "source": lic.get("source", ""), "notes": lic.get("notes", "")}
    print(f"Model:      {model_id}")
    print(f"Checkpoint: {entry['checkpoint']}")
    print(f"Config:     {entry['config']}")
    show_license(lic)

    import torch

    from tools.export import roformer as rx

    torch.set_num_threads(max(1, min(16, (torch.get_num_threads() or 8))))
    cfg = rx.load_config(entry["config"])
    family = rx.detect_family(cfg)
    if family != "bs_roformer":
        raise SystemExit(f"unsupported model family {family!r} (supported: bs_roformer)")
    params = rx.model_params(cfg)
    if not params["stereo"]:
        raise SystemExit("mono models are not supported")
    print(f"Family: {family}; stems {params['stems']}; chunk {params['chunk']} samples "
          f"({params['chunk'] / params['sample_rate']:.2f} s), n_fft {params['n_fft']}, hop {params['hop']}, "
          f"num_overlap {params['num_overlap']}")

    model = rx.load_torch_model(entry["checkpoint"], cfg)
    chunk = params["chunk"]
    tests = {"synth-v1": synth_v1(chunk, params["sample_rate"])}
    if a.audio:
        tests[Path(a.audio).name] = load_audio_excerpt(a.audio, chunk, params["sample_rate"])
    refs = {}
    with torch.no_grad():
        for label, x in tests.items():
            t0 = time.perf_counter()
            refs[label] = model(torch.from_numpy(x)[None])[0].numpy()
            print(f"PyTorch reference ({label}): {time.perf_counter() - t0:.1f}s")

    dest = Path(a.dest)
    final_dir = dest / model_id
    work = dest / f".{model_id}.partial"
    if work.exists():
        shutil.rmtree(work)
    work.mkdir(parents=True)
    onnx_name = f"{model_id}.onnx"
    print("Exporting to ONNX (a few minutes, needs several GB of RAM)...")
    el = rx.export(model, params, work / onnx_name)
    del model
    print(f"exported {onnx_name} ({(work / onnx_name).stat().st_size / 1e6:.0f} MB) in {el:.0f}s")

    sess = make_session(work / onnx_name, "cpu", ROFORMER_SESSION)
    rm = rs.RoformerMask(sess, params["n_fft"], params["hop"], params["num_overlap"])
    worst = float("inf")
    for label, x in tests.items():
        got = rm.run_chunk(x)
        # Near-silent stems are numerically noisy relative to their own level, so the
        # pass criterion floors the reference energy at -20 dB re the input mix.
        floor = (mf.FLOOR_REL**2) * float((x.astype(np.float64) ** 2).sum())
        snrs = [snr_db(refs[label][i], got[i]) for i in range(len(params["stems"]))]
        fsnrs = [snr_db(refs[label][i], got[i], floor) for i in range(len(params["stems"]))]
        worst = min(worst, min(fsnrs))
        print(f"Accuracy ONNX (CPU) vs PyTorch, {label}: "
              + " ".join(f"{n}={s:.1f}dB" for n, s in zip(params["stems"], snrs))
              + f" | floored (pass >= {ACCURACY_MIN_DB:.0f}): " + " ".join(f"{s:.1f}" for s in fsnrs))
    if worst < ACCURACY_MIN_DB:
        raise SystemExit(f"accuracy check FAILED (worst stem {worst:.1f} dB < {ACCURACY_MIN_DB} dB); "
                         f"partial output left in {work}")

    notes = lic["notes"]
    manifest = mf.build_manifest(
        id=model_id,
        name=a.name or entry.get("name") or model_id,
        adapter="roformer-mask",
        file=onnx_name,
        onnx_path=work / onnx_name,
        stems=params["stems"],
        segment_samples=chunk,
        overlap=params["num_overlap"],
        stft={"n_fft": params["n_fft"], "hop": params["hop"]},
        session=ROFORMER_SESSION,
        license={"spdx": lic["spdx"], "source": lic["source"], "notes": notes},
        personal=True,
        sample_rate=params["sample_rate"],
    )
    mf.write_json(work / "model.json", manifest)
    mf.write_selftest(work, manifest, ep="cpu")
    ok, _ = mf.verify_model_dir(work, "cpu")
    if not ok:
        raise SystemExit(f"self-test on CPU failed right after writing it; see {work}")

    if final_dir.exists():
        shutil.rmtree(final_dir)
    work.replace(final_dir)
    print(f"Installed {final_dir}")

    if a.check_dml:
        r = subprocess.run([sys.executable, str(ROOT / "tools" / "registry.py"), "verify", str(final_dir), "--ep", "dml"])
        print("DirectML self-test:", "PASS" if r.returncode == 0 else "FAIL (the app will fall back to CPU)")


if __name__ == "__main__":
    main()
