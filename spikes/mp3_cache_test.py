"""Would MP3 be good enough for the stem cache? Encodes the stems with LAME
(via ffmpeg) at several settings and measures what the app would suffer:

- size of the six stems;
- the guitar stem's own coding noise (SNR);
- "All but guitar" = original - guitar: the guitar's coding noise is no
  longer masked by the guitar there. Reports the SNR against the backing,
  overall and in the worst 5 % of one-second segments;
- the per-bin pan estimate the Refine pan window uses (energy-weighted mean
  absolute error, pan units -1..1).

The original mix is used as decoded from its source file (the cache could
keep the source file's bytes, so it never needs re-encoding).

Writes listening excerpts (OUT/listen/) and decoded stem dirs per setting
(OUT/<setting>/stems/, WAV) for rendering slowed playback with
`cargo run -p stemx-audio --example play -- DIR --only guitar --speed 0.6 --render X.wav`.

Usage:
    python spikes/mp3_cache_test.py MIX.mp3 STEMS_DIR OUT_DIR
"""

import argparse
import subprocess
from pathlib import Path

import numpy as np
import soundfile as sf
from scipy.signal import correlate, stft

SETTINGS = {
    "cbr320": ["-b:a", "320k"],
    "cbr256": ["-b:a", "256k"],
    "v0": ["-q:a", "0"],
    "cbr192": ["-b:a", "192k"],
    "v2": ["-q:a", "2"],
}
EXCERPT = (60.0, 120.0)


def ffmpeg(*args):
    subprocess.run(["ffmpeg", "-v", "error", "-y", *args], check=True)


def align_to(ref, x, max_lag=4096):
    n = min(len(ref), len(x), 20 * 44100)
    c = correlate(x[:n].mean(1), ref[:n].mean(1), mode="full", method="fft")
    mid = n - 1
    lag = int(np.argmax(c[mid - max_lag : mid + max_lag + 1])) - max_lag
    x = x[lag:] if lag >= 0 else np.concatenate([np.zeros((-lag, 2), x.dtype), x])
    out = np.zeros_like(ref)
    m = min(len(ref), len(x))
    out[:m] = x[:m]
    return out, lag


def snr(ref, err):
    return 10 * np.log10(np.sum(ref**2) / max(np.sum(err**2), 1e-20))


def worst_segments(ref, err, sr, pct=5):
    seg = sr
    vals = []
    for t in range(0, len(ref) - seg, seg):
        r = np.sum(ref[t : t + seg] ** 2)
        if r > 1e-6 * seg:  # skip silence
            vals.append(10 * np.log10(r / max(np.sum(err[t : t + seg] ** 2), 1e-20)))
    return np.percentile(vals, pct)


def pan_error(a, b):
    def pan(x):
        _, _, z = stft(x.T, nperseg=4096, noverlap=3072)
        ml, mr = np.abs(z[0]), np.abs(z[1])
        return (mr - ml) / (ml + mr + 1e-12), ml + mr

    pa, ea = pan(a)
    pb, _ = pan(b)
    w = ea * (ea > ea.max() * 1e-3)
    return np.sum(np.abs(pa - pb) * w) / np.sum(w)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("mix")
    ap.add_argument("stems_dir")
    ap.add_argument("out_dir")
    args = ap.parse_args()
    out = Path(args.out_dir)
    (out / "listen").mkdir(parents=True, exist_ok=True)

    stems_dir = Path(args.stems_dir)
    names = sorted(p.stem for p in stems_dir.glob("*.wav"))
    stems = {n: sf.read(stems_dir / f"{n}.wav", dtype="float64")[0] for n in names}
    sr = 44100
    est = sum(stems.values())
    mix, _ = sf.read(args.mix, dtype="float64", always_2d=True)
    mix, lag = align_to(est, mix)
    print(f"mix lag {lag}")
    wav_mb = sum((stems_dir / f"{n}.wav").stat().st_size for n in names) / 1e6

    g = stems["guitar"]
    backing = mix - g
    a, b = (int(t * sr) for t in EXCERPT)
    sf.write(out / "listen" / "backing_lossless.wav", backing[a:b], sr, subtype="FLOAT")
    print(f"{'setting':8s} {'6 stems':>8s} {'guitar SNR':>11s} {'backing SNR':>12s} "
          f"{'worst 5%':>9s} {'pan err':>8s}")
    print(f"{'wav16':8s} {wav_mb:6.0f}MB")

    for name, opts in SETTINGS.items():
        d = out / name
        (d / "stems").mkdir(parents=True, exist_ok=True)
        size = 0
        dec = {}
        for n in names:
            mp3 = d / f"{n}.mp3"
            ffmpeg("-i", str(stems_dir / f"{n}.wav"), "-c:a", "libmp3lame", *opts, str(mp3))
            size += mp3.stat().st_size
            wav = d / "stems" / f"{n}.wav"
            ffmpeg("-i", str(mp3), "-c:a", "pcm_f32le", str(wav))
            x, _ = sf.read(wav, dtype="float64")
            x, l = align_to(stems[n], x)
            if l:
                print(f"  {name} {n}: decoder lag {l} (corrected)")
            sf.write(wav, x, sr, subtype="FLOAT")
            dec[n] = x
        err = dec["guitar"] - g
        bk = mix - dec["guitar"]
        sf.write(out / "listen" / f"backing_{name}.wav", bk[a:b], sr, subtype="FLOAT")
        print(f"{name:8s} {size / 1e6:6.0f}MB {snr(g, err):9.1f}dB {snr(backing, err):10.1f}dB "
              f"{worst_segments(backing, err, sr):7.1f}dB {pan_error(g, dec['guitar']):8.3f}")


if __name__ == "__main__":
    main()
