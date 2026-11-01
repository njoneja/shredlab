# spikes/

Python prototypes that proved the ONNX approach. The Rust code is ported
from them and validated against them, and `tools/setup_models.py` still uses
them to export the bundled models, so they are kept and maintained.

| File | What |
|---|---|
| `onnx_demucs_own.py` | Export of htdemucs_6s with the STFT moved outside the graph, plus the NumPy STFT/iSTFT glue. This is the bundled separation model. |
| `onnx_demucs.py` | `separate()`: demucs `apply_model` chunking in NumPy (normalization, 25 % overlap, triangular weights, short last chunk padded with neighbouring audio). |
| `onnx_beat_this.py` | Beat This! export, NumPy log-mel preprocessing, chunking and minimal peak picking. |
| `onnx_roformer.py` | BS-RoFormer export and NumPy STFT (equivalent to `torch.stft(center=True)`). Basis for the `roformer-mask` adapter and the personal-model tool. |
| `onnx_bisect.py` | Bisects a model sub-module by sub-module to find where ONNX and PyTorch outputs diverge. |

Related scripts in `scripts/`: `separate.py` (the original audio-separator
harness) and `pan_split.py` (STFT-based stereo pan split).

Usage for each script is in its module docstring. Upstream sources can be
vendored (read-only) in `spikes/third_party/`, which is gitignored; without
them the scripts use the PyPI packages.
