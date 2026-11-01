#!/bin/sh
# Download checkpoints and build the bundled ONNX models (macOS / Linux).
# All arguments are passed to tools/setup_models.py (--out DIR, --force, ...).
set -e
cd "$(dirname "$0")/.."
if command -v uv >/dev/null 2>&1; then
  # uv provides Python 3.12 itself if it is not installed.
  exec uv run --no-project --python 3.12 tools/setup_models.py "$@"
fi
for py in python3.12 python3 python; do
  if command -v "$py" >/dev/null 2>&1; then
    exec "$py" tools/setup_models.py "$@"
  fi
done
echo "Neither uv nor Python found. Install uv (https://docs.astral.sh/uv/) or Python 3.12." >&2
exit 1
