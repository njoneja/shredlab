# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Navneet Joneja

"""onnxruntime session construction from a model.json `session` block."""

import os

PROVIDERS = {"cpu": "CPUExecutionProvider", "dml": "DmlExecutionProvider"}


def default_cpu_threads():
    # The ORT default thread count was ~5x slow on a 14900K (P/E cores), so
    # always pick explicitly. physical-core count isn't in the stdlib; use
    # logical/2 as an approximation.
    return max(1, min((os.cpu_count() or 2) // 2, 16))


def make_session(onnx_path, ep="cpu", session_cfg=None, override=None):
    """Create an InferenceSession for `ep` ("cpu" | "dml").

    session_cfg: the manifest's `session` dict (per-EP options).
    override: dict merged over the EP's options (e.g. {"disable_graph_fusion": False}
              to reproduce the DirectML fusion bug).
    """
    import onnxruntime as ort

    opts_cfg = dict((session_cfg or {}).get(ep, {}))
    opts_cfg.update(override or {})
    so = ort.SessionOptions()
    if ep == "cpu":
        n = opts_cfg.get("intra_op_threads", 0) or default_cpu_threads()
        so.intra_op_num_threads = int(n)
    elif ep == "dml":
        if not opts_cfg.get("enable_mem_pattern", True):
            so.enable_mem_pattern = False
        if opts_cfg.get("sequential", False):
            so.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
        if opts_cfg.get("disable_graph_fusion", False):
            so.add_session_config_entry("ep.dml.disable_graph_fusion", "1")
    else:
        raise ValueError(f"unknown ep {ep!r}")
    return ort.InferenceSession(str(onnx_path), so, providers=[PROVIDERS[ep]])
