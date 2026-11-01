"""Generate large end-to-end goldens into tests/fixtures/local/ (gitignored).

Usage:
    .venv\\Scripts\\python.exe tools/make_goldens.py --audio FILE [--skip-roformer]
    .venv\\Scripts\\python.exe tools/make_goldens.py --shifts-only   # only demucs_shifts2_stems

Needs ffmpeg on PATH, models/registry/htdemucs_6s (tools/registry.py
build-htdemucs) and, for the RoFormer goldens, models/personal/bs-roformer-sw
(tools/add_personal_model.py bs-roformer-sw). All inference on the CPU EP.

Files are raw little-endian float32 + tests/fixtures/local/index.json
(shapes, descriptions). See tests/fixtures/README.md.
"""

import argparse
import subprocess
import sys
import time
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

OUT = ROOT / "tests" / "fixtures" / "local"
SR = 44100


def decode(path, sr=SR):
    """Decode any audio file with ffmpeg -> (2, L) float32 at `sr`."""
    raw = subprocess.run(["ffmpeg", "-loglevel", "error", "-i", str(path), "-f", "f32le", "-ac", "2",
                          "-ar", str(sr), "-"], check=True, capture_output=True).stdout
    return np.ascontiguousarray(np.frombuffer(raw, np.float32).reshape(-1, 2).T)


def demucs_shifts_golden(w, dh, man, sess, x, shifts=2):
    """demucs_input separated with deterministic shifts (dh.shift_offsets)."""
    model = dh.DemucsHybrid(sess, man["overlap"])
    t0 = time.perf_counter()
    stems = dh.separate(model, x, shifts=shifts, sample_rate=man["sample_rate"])
    print(f"demucs separate shifts={shifts}: {time.perf_counter() - t0:.1f}s")
    w.bin(f"demucs_shifts{shifts}_stems", stems,
          f"dh.separate(demucs_input, shifts={shifts}): (N=6, 2, {x.shape[1]}), offsets "
          f"{dh.shift_offsets(shifts, man['sample_rate'])}")


def main():
    from tools.fixture_io import FixtureWriter
    from tools.reference import demucs_hybrid as dh
    from tools.reference import manifest as mf
    from tools.reference import roformer_separate as rs
    from tools.reference.sessions import make_session

    sys.stdout.reconfigure(line_buffering=True)
    ap = argparse.ArgumentParser()
    ap.add_argument("--audio", help="a full song (any format ffmpeg decodes); needed unless --shifts-only")
    ap.add_argument("--demucs-dir", default=str(ROOT / "models" / "registry" / "htdemucs_6s"))
    ap.add_argument("--roformer-dir", default=str(ROOT / "models" / "personal" / "bs-roformer-sw"))
    ap.add_argument("--skip-roformer", action="store_true")
    ap.add_argument("--shifts-only", action="store_true",
                    help="only (re)generate demucs_shifts2_stems from the existing demucs_input.bin")
    a = ap.parse_args()

    if a.shifts_only:
        w = FixtureWriter(OUT, "index.json", merge=True)
        man = mf.load_manifest(a.demucs_dir)
        x = np.fromfile(OUT / "demucs_input.bin", "<f4").reshape(2, -1)
        demucs_shifts_golden(w, dh, man, make_session(Path(a.demucs_dir) / man["file"], "cpu", man["session"]), x)
        w.close()
        return

    if not a.audio:
        ap.error("--audio is required (unless --shifts-only)")
    song = decode(a.audio)
    w = FixtureWriter(OUT, "index.json", {"sample_rate": SR, "source_audio": Path(a.audio).name,
                                          "decode": "ffmpeg -f f32le -ac 2 -ar 44100 (interleaved -> planar)"})

    # --- demucs-hybrid: 60-70 s excerpt, full chunked separation + first-window intermediates
    man = mf.load_manifest(a.demucs_dir)
    x = np.ascontiguousarray(song[:, 60 * SR : 70 * SR])
    w.bin("demucs_input", x, "excerpt 60-70 s of the song, (2, 441000)", start_sample=60 * SR)
    sess = make_session(Path(a.demucs_dir) / man["file"], "cpu", man["session"])
    model = dh.DemucsHybrid(sess, man["overlap"])
    t0 = time.perf_counter()
    stems = dh.separate(model, x)
    print(f"demucs separate: {time.perf_counter() - t0:.1f}s")
    w.bin("demucs_stems", stems, f"dh.separate(demucs_input): (N=6, 2, 441000), stems {man['stems']}")
    mean, std = dh.song_stats(x)
    w.value("demucs_song_normalization", {"mean": float(mean), "std": float(std)})
    w.value("demucs_chunks", [{"offset": o, "n": min(man["segment_samples"], x.shape[1] - o),
                               "window_start": o - (man["segment_samples"] - min(man["segment_samples"], x.shape[1] - o)) // 2}
                              for o in dh.chunk_offsets(x.shape[1], man["segment_samples"], man["overlap"])])
    # first window (offset 0, full segment) intermediates
    xn = (x - mean) / std
    seg = np.ascontiguousarray(xn[:, : man["segment_samples"]])
    z = dh.spec(seg)
    wave, sp = model.run_raw(seg)
    w.bin("demucs_w0_mix", seg, "window 0 graph input `mix` without batch axis: normalized, (2, 343980)")
    w.bin("demucs_w0_mag", dh.cac(z)[0], "window 0 graph input `mag` without batch axis: (4, 2048, 336)")
    w.bin("demucs_w0_wave", wave, "window 0 graph output `wave`: (6, 2, 343980)")
    w.bin("demucs_w0_spec", sp, "window 0 graph output `spec`: (6, 4, 2048, 336)")
    w.bin("demucs_w0_out", wave + dh.ispec(dh.uncac(sp), seg.shape[1]),
          "window 0 combined output wave + ispec(uncac(spec)) (normalized domain, before weights): (6, 2, 343980)")
    demucs_shifts_golden(w, dh, man, sess, x)
    del sess, model

    # --- roformer-mask: 60-90 s excerpt (4 full chunks incl. the end-aligned tail)
    if not a.skip_roformer:
        rdir = Path(a.roformer_dir)
        if not (rdir / "model.json").exists():
            print(f"skipping RoFormer goldens: {rdir} missing (run tools/add_personal_model.py bs-roformer-sw)")
        else:
            rman = mf.load_manifest(rdir)
            xr = np.ascontiguousarray(song[:, 60 * SR : 90 * SR])
            w.bin("roformer_input", xr, "excerpt 60-90 s of the song, (2, 1323000)", start_sample=60 * SR)
            rsess = make_session(rdir / rman["file"], "cpu", rman["session"])
            rm = rs.RoformerMask(rsess, rman["stft"]["n_fft"], rman["stft"]["hop"], rman["overlap"])
            t0 = time.perf_counter()
            rstems = rs.separate(rm, xr)
            print(f"roformer separate: {time.perf_counter() - t0:.1f}s")
            w.bin("roformer_stems", rstems, f"rs.separate(roformer_input): (N=6, 2, 1323000), stems {rman['stems']}")
            w.value("roformer_chunk_starts", rs.chunk_starts(xr.shape[1], rm.chunk, rm.chunk // rm.num_overlap))
            c0 = np.ascontiguousarray(xr[:, : rm.chunk])
            zc = rs.stft(c0, rm.n_fft, rm.hop)
            mask = rsess.run(None, {"stft": rs.stft_to_input(zc)})[0][0]
            w.bin("roformer_c0_stft", rs.stft_to_input(zc)[0], "chunk 0 graph input `stft` without batch axis: (2, 1025, 1151, 2)")
            w.bin("roformer_c0_mask", mask, "chunk 0 graph output `mask` without batch axis: (6, 2050, 1151, 2)")
            w.bin("roformer_c0_out", rs.istft(rs.apply_masks(zc, mask), rm.chunk, rm.n_fft, rm.hop),
                  "chunk 0 output before window/overlap-add: (6, 2, 588800)")
    w.close()
    total = sum(p.stat().st_size for p in OUT.iterdir())
    print(f"wrote {OUT} ({total / 1e6:.0f} MB)")


if __name__ == "__main__":
    main()
