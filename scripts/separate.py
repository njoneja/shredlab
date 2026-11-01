"""Model comparison harness: separate songs with several models and build
practice mixes.

For every input song and model, writes to out/<song>/<model>/:
    stems/<stem>.wav          isolated stems (e.g. guitar.wav, bass.wav)
    minus/no_<stem>.wav       play-along mix: everything except that stem
    guitar_pan/{left,center,right}.wav   guitar stem split by stereo position

Usage:
    python scripts/separate.py input/                       # all audio in folder
    python scripts/separate.py song.mp3 --models htdemucs_6s
    python scripts/separate.py input/ --drumsep             # also split drums
    python scripts/separate.py input/ --cpu                 # skip DirectML GPU
"""

import argparse
import logging
import time
from pathlib import Path

import numpy as np
import soundfile as sf

import pan_split

ROOT = Path(__file__).resolve().parent.parent

# Short name -> audio-separator model filename.
MODELS = {
    "htdemucs_6s": "htdemucs_6s.yaml",        # Demucs v4, 6 stems incl. guitar + piano
    "htdemucs_ft": "htdemucs_ft.yaml",        # Demucs v4 fine-tuned, 4 stems (best Demucs bass/drums)
    "bs_roformer_sw": "BS-Roformer-SW.ckpt",  # RoFormer, 6 stems incl. guitar + piano
}
DEFAULT_MODELS = ["htdemucs_6s", "bs_roformer_sw"]
DRUMSEP_MODEL = "MDX23C-DrumSep-aufr33-jarredou.ckpt"

AUDIO_EXTS = {".mp3", ".wav", ".flac", ".m4a", ".ogg", ".aiff", ".aif"}


def find_inputs(paths):
    for p in paths:
        if p.is_dir():
            yield from sorted(f for f in p.iterdir() if f.suffix.lower() in AUDIO_EXTS)
        else:
            yield p


def stem_name(path):
    """'Song (2002 Version)_(Guitar)_htdemucs_6s.wav' -> 'guitar'.

    Uses the last parenthesized group, since song titles can contain their own.
    """
    name = Path(path).name
    if "_(" in name and ")" in name:
        start = name.rindex("_(") + 2
        return name[start : name.index(")", start)].lower()
    return Path(path).stem.lower()


def make_separator(work_dir, use_gpu):
    from audio_separator.separator import Separator

    return Separator(
        log_level=logging.WARNING,
        model_file_dir=str(ROOT / "models"),
        output_dir=str(work_dir),
        output_format="WAV",
        use_directml=use_gpu,
    )


def run_model(separator, song, stems_dir):
    """Separate one song with one model; returns {stem: path}."""
    separator.output_dir = str(stems_dir)
    if separator.model_instance is not None:
        separator.model_instance.output_dir = str(stems_dir)
    outputs = separator.separate(str(song))
    stems = {}
    for out in outputs:
        out = Path(out)
        if not out.is_absolute():
            out = stems_dir / out
        target = stems_dir / f"{stem_name(out)}.wav"
        out.replace(target)
        stems[stem_name(out)] = target
    return stems


def write_minus_mixes(stems, minus_dir):
    """For each stem, sum all the other stems into a play-along mix."""
    minus_dir.mkdir(parents=True, exist_ok=True)
    audio = {name: sf.read(p, always_2d=True) for name, p in stems.items()}
    sr = next(iter(audio.values()))[1]
    length = min(a.shape[0] for a, _ in audio.values())
    total = sum(a[:length] for a, _ in audio.values())
    for name, (a, _) in audio.items():
        mix = total - a[:length]
        peak = np.abs(mix).max()
        if peak > 1.0:
            mix /= peak
        sf.write(minus_dir / f"no_{name}.wav", mix, sr)


def write_pan_split(stem_path, pan_dir):
    pan_dir.mkdir(parents=True, exist_ok=True)
    audio, sr = sf.read(stem_path, dtype="float32", always_2d=True)
    for name, part in pan_split.split(audio, sr).items():
        sf.write(pan_dir / f"{name}.wav", part, sr)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("inputs", type=Path, nargs="+", help="audio files or folders")
    ap.add_argument("--models", nargs="+", default=DEFAULT_MODELS, choices=list(MODELS))
    ap.add_argument("--out", type=Path, default=ROOT / "out")
    ap.add_argument("--cpu", action="store_true", help="disable DirectML GPU acceleration")
    ap.add_argument("--drumsep", action="store_true", help="split drum stems into kit pieces")
    args = ap.parse_args()

    songs = list(find_inputs(args.inputs))
    if not songs:
        ap.error("no audio files found")

    separator = make_separator(args.out, use_gpu=not args.cpu)
    timings = []

    # Loop models on the outside so each model loads once.
    for model in args.models:
        separator.load_model(model_filename=MODELS[model])
        for song in songs:
            song_dir = args.out / song.stem / model
            stems_dir = song_dir / "stems"
            stems_dir.mkdir(parents=True, exist_ok=True)

            print(f"[{model}] {song.name} ...", flush=True)
            t0 = time.perf_counter()
            stems = run_model(separator, song, stems_dir)
            elapsed = time.perf_counter() - t0
            timings.append((model, song.name, elapsed))
            print(f"  stems: {', '.join(sorted(stems))}  ({elapsed:.1f}s)")

            write_minus_mixes(stems, song_dir / "minus")
            if "guitar" in stems:
                write_pan_split(stems["guitar"], song_dir / "guitar_pan")

    if args.drumsep:
        separator.load_model(model_filename=DRUMSEP_MODEL)
        for song in songs:
            for model in args.models:
                drums = args.out / song.stem / model / "stems" / "drums.wav"
                if drums.exists():
                    print(f"[drumsep] {song.name} ({model} drums) ...", flush=True)
                    run_model(separator, drums, drums.parent.parent / "drum_kit")

    print("\nTimings:")
    for model, name, elapsed in timings:
        print(f"  {elapsed:7.1f}s  {model:16s} {name}")


if __name__ == "__main__":
    main()
