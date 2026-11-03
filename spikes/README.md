# spikes/

Python prototypes that proved the ONNX approach (network core in the graph,
DSP outside) and a few listening experiments. They are kept because the Rust
code was ported from them and validated against them, and because
`tools/setup_models.py` still uses three of them to export the bundled
models.

## Boundaries

- Load-bearing: `onnx_demucs_own.py` and `onnx_beat_this.py` contain the
  export code for the two bundled models, and `onnx_demucs.py` provides the
  Demucs model loading they import. `tools/setup_models.py` runs them inside
  `.venv-models`, and CI's model cache key includes all three. Changing them
  changes what ships.
- `onnx_beat_this.py` is also the reference implementation for
  `crates/shredlab-beats` (log-mel, chunking, peak picking);
  `tools/beat_this_fixtures.py` writes fixtures from it.
- Historical: `onnx_roformer.py` and `onnx_bisect.py` are left as they
  were. The maintained copies are `tools/export/roformer.py` (export) and
  `tools/reference/roformer_separate.py` (DSP and chunking); likewise the
  Demucs DSP and chunking are maintained in
  `tools/reference/demucs_hybrid.py`.
- Experiments: `mp3_cache_test.py` (the measurements behind the MP3 stem
  cache, cited from `shredlab-library` and the app settings) and
  `wiener_post.py`. Nothing depends on them.
- Environments: the export paths run in `.venv-models` (pinned PyPI `demucs`
  and `beat-this`) or `.venv`. Everything that uses audio-separator's model
  code (`onnx_roformer.py`, `onnx_bisect.py`, the Demucs fallback) or
  DirectML needs `.venv`. `wiener_post.py` needs `norbert`, which is in
  neither requirements file.
- Upstream sources can be placed (read-only) in `spikes/third_party/`,
  which is gitignored. Without them the scripts use the PyPI packages or
  audio-separator's vendored copies.

## Layout

Start with `onnx_demucs_own.py`.

- `onnx_demucs_own.py`: htdemucs_6s export with the STFT/iSTFT outside the
  graph, the NumPy STFT/iSTFT and complex-as-channels glue, and
  ONNX-vs-PyTorch comparison and timing commands. This is the bundled
  separation model.
- `onnx_demucs.py`: Demucs model loading (upstream `demucs` package, else
  audio-separator's vendored copy) and `apply_model` chunking in NumPy:
  normalization, 25% overlap, triangular weights, short last chunk padded
  with neighboring audio. Its `compare` and `separate` commands run a
  community htdemucs_6s ONNX export (StemSplitio/htdemucs-6s-onnx, MIT) that
  must be downloaded by hand to `models/onnx/stemsplit-htdemucs-6s/`.
- `onnx_beat_this.py`: Beat This! export, NumPy preprocessing (mono
  downmix, soxr resampling to 22.05 kHz, log-mel), 1500-frame chunking and
  minimal peak picking; commands to write PyTorch references and compare
  against them.
- `onnx_roformer.py`: BS-RoFormer-SW export and NumPy STFT equivalent to
  `torch.stft(center=True)`. Basis for the `roformer-mask` adapter and the
  personal-model tool.
- `onnx_bisect.py`: exports RoFormer sub-modules one at a time and compares
  ONNX with PyTorch, to find where outputs diverge.
- `mp3_cache_test.py`: encodes stems with LAME at several settings and
  measures size, coding noise, the noise exposed in "all but guitar", and
  the error in the per-bin pan estimate.
- `wiener_post.py`: listening test of a multichannel Wiener post-filter
  (norbert) on separated stems.

## How it works

The graphs contain only the network:

| Spike | Graph input | Graph output |
|---|---|---|
| `onnx_demucs_own.py` | `mix` (1, 2, 343980), `mag` (1, 4, 2048, 336) | `wave` (1, 6, 2, 343980), `spec` (1, 6, 4, 2048, 336) |
| `onnx_beat_this.py` | `spect` (1, 1500, 128) log-mel | `beat`, `downbeat` (1, 1500) logits |
| `onnx_roformer.py` | `stft` (1, 2, 1025, 1151, 2) | `mask` (1, 6, 2050, 1151, 2) |

Both internal normalizations of `HTDemucs.forward` stay inside the Demucs
graph; the song-level normalization stays outside. To make the models trace
with the TorchScript exporter in torch 2.4, the exports replace upstream
modules with equivalent code: explicit attention with constant rotary
angles, explicit GLU and reshapes, static shapes. The `debug` and
`compare` commands (and the Beat This! export itself) check the patched
wrapper against the upstream model.

`onnx_beat_this.py ref <audio>...` writes PyTorch references to
`out/beat_this/<model>_<song>.npz` (local), which `compare` and
`tools/beat_this_fixtures.py` read. `BEAT_THIS_MODEL=small0` switches the
model and `BEAT_THIS_THREADS` sets the ONNX Runtime thread count.

## Invariants and gotchas

- `tools/setup_models.py` calls `onnx_demucs_own.export(onnx_path=...,
  ckpt_path=...)` and `onnx_beat_this.export("final0", ckpt_path=...,
  out_path=...)`. Keep those signatures. After changing either export, bump
  `EXPORT_VERSION` in `tools/setup_models.py` (see
  [tools/README.md](../tools/README.md)).
- The Demucs graph runs on DirectML only with
  `ep.dml.disable_graph_fusion` = 1; with fusion on it produces wrong output
  without an error (`onnx_demucs_own.py song|time <audio> <provider> fusion1`
  reproduces it). Run
  each DirectML configuration in a fresh process.
- Default paths are fixed under the repo: checkpoints and ONNX files in
  `models/`, outputs in `out/`, both local.
- `mp3_cache_test.py` and `wiener_post.py` only read and write files you
  pass them.

## Testing / running

Usage for each script is in its module docstring. The main commands:

```
python spikes/onnx_demucs_own.py export
python spikes/onnx_demucs_own.py debug                     # NumPy DSP + wrapper vs torch
python spikes/onnx_demucs_own.py compare <audio>           # one window: ONNX vs torch
python spikes/onnx_beat_this.py export [final0|small0]
python spikes/onnx_beat_this.py ref <audio>...
python spikes/onnx_beat_this.py compare <audio>...
python spikes/onnx_roformer.py export
python spikes/onnx_roformer.py compare <audio>
python spikes/mp3_cache_test.py MIX.mp3 STEMS_DIR OUT_DIR
python spikes/wiener_post.py MIX.mp3 STEMS_DIR OUT_DIR [--iters 0 1 3]
```

The bundled exports are verified end to end by `python tools/setup_models.py`
(step 6).

## Common changes

- Changing a bundled export: edit `onnx_demucs_own.py` or
  `onnx_beat_this.py`, check it with `debug` / `compare`, then follow
  "Changing the Demucs or Beat This! export" in
  [tools/README.md](../tools/README.md).
- Changing Beat This! preprocessing or peak picking: change it here and in
  `crates/shredlab-beats` together, rerun `onnx_beat_this.py ref` on the
  test songs and `python tools/beat_this_fixtures.py`, then
  `cargo test -p shredlab-beats --test reference`.
