# Developing

Practical notes for working on the code. Building from scratch is covered in
the [README](../README.md); how the pieces fit together is in
[ARCHITECTURE.md](ARCHITECTURE.md).

## Running the app

```bash
pnpm --dir app tauri dev
```

Logs go to stderr and `<app data>/shredlab.log`.

Headless end-to-end check (separates a song, detects beats and plays a few
seconds of audio):

```bash
SHREDLAB_AUTOTEST=<song> SHREDLAB_AUTOTEST_EXIT=1 pnpm --dir app tauri dev
```

It prints `AUTOTEST PASS`.

### Environment overrides

| Variable | Effect |
|---|---|
| `SHREDLAB_DATA_DIR` | Library location (song folders, stem cache) |
| `SHREDLAB_MODELS_DIR` | Extra model folders (`;`-separated on Windows, `:` elsewhere) |
| `SHREDLAB_MODEL` | Separation model id, e.g. a personal model such as `bs-roformer-sw` |
| `SHREDLAB_DEVICE` | `auto`, `cpu` or `gpu` |
| `SHREDLAB_BEAT_MODEL` | Beat model id |
| `SHREDLAB_BEATS_DEVICE` | Device for beat detection |
| `SHREDLAB_NO_PRELOAD` | Skip loading the model at startup |
| `SHREDLAB_AUTOTEST`, `SHREDLAB_AUTOTEST_EXIT` | Headless end-to-end check (above) |

## Python environments

Two separate virtual environments, both Python 3.12:

- **`.venv`** is the dev/reference environment. On Windows it uses
  torch 2.4.1 (pinned by torch-directml) and `onnxruntime-directml`. Never
  install plain `onnxruntime` into it: it replaces the DirectML build. Use
  `--no-deps` when adding packages.
- **`.venv-models`** is created by `tools/setup_models.py` from
  `requirements-models.txt` and is CPU only: torch 2.4.1, plain
  `onnxruntime`, and `demucs` / `beat-this` from PyPI. It exists only to
  export the bundled models.

Python reference implementations are described in
[spikes/README.md](../spikes/README.md) and [tools/README.md](../tools/README.md).

## ONNX Runtime and DirectML

- Session options that work: `enable_mem_pattern = false`, sequential
  execution.
- **`htdemucs_6s_own.onnx` needs `ep.dml.disable_graph_fusion = 1`.** With
  graph fusion on, DirectML silently produces garbage (no error). This is
  why every GPU session runs the device self-test
  ([MODEL_MANIFEST.md](MODEL_MANIFEST.md)) before it is used.
- A DirectML device crash poisons the whole process. When comparing
  configurations, run each one in a fresh process.
- The CPU provider's default thread count can be very slow on CPUs with
  performance and efficiency cores; set intra-op threads explicitly (8–16).
- The statically linked ORT (pyke's build) differs from Microsoft's official
  build at around −80 dB, which is inaudible. The `load-dynamic` feature of
  `shredlab-separation` can load the official DLLs for exact comparisons.

## Windows tips

- Git Bash converts arguments that start with `/` into Windows paths. Set
  `MSYS_NO_PATHCONV=1` when that gets in the way.
- Tools installed with winget (uv, ffmpeg) only appear on PATH in shells
  started after the install.
