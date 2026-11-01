"""Rust-friendly fixture files: raw little-endian float32 `.bin` + JSON index."""

import json
from pathlib import Path

import numpy as np


class FixtureWriter:
    def __init__(self, out_dir, index_name="index.json", meta=None, merge=False):
        """merge=True keeps the entries of an existing index (adds/replaces files)."""
        self.dir = Path(out_dir)
        self.dir.mkdir(parents=True, exist_ok=True)
        self.index_name = index_name
        self.index = {"format": "raw little-endian, C order (last axis fastest)", **(meta or {}), "files": {}}
        existing = self.dir / index_name
        if merge and existing.is_file():
            self.index = json.loads(existing.read_text(encoding="utf-8"))

    def bin(self, name, arr, desc, dtype="<f4", **extra):
        """Write arr as `<name>.bin`. Complex arrays get a trailing axis of 2 (re, im)."""
        arr = np.asarray(arr)
        if np.iscomplexobj(arr):
            arr = np.stack([arr.real, arr.imag], -1)
        arr = np.ascontiguousarray(arr, dtype=dtype)
        path = self.dir / f"{name}.bin"
        arr.tofile(path)
        self.index["files"][name] = {
            "file": path.name,
            "dtype": {"<f4": "f32", "<i4": "i32", "<i8": "i64"}[dtype],
            "shape": list(arr.shape),
            "bytes": path.stat().st_size,
            "desc": desc,
            **extra,
        }
        return path

    def value(self, name, v):
        self.index[name] = v

    def close(self):
        (self.dir / self.index_name).write_text(json.dumps(self.index, indent=2) + "\n", encoding="utf-8")
