# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Navneet Joneja

"""Reference implementations of the app's separation pipeline.

numpy + scipy + onnxruntime only (no torch). These are the ground truth the
Rust port is validated against (see tests/fixtures/README.md).

Modules:
    synth             deterministic self-test input ("synth-v1")
    demucs_hybrid     `demucs-hybrid` adapter: STFT glue + apply_model chunking
    roformer_separate `roformer-mask` adapter: STFT glue + MSST-style chunking
    manifest          model.json / selftest.json writers, self-test runner
    sessions          onnxruntime session construction from a manifest
"""
