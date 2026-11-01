# Download checkpoints and build the bundled ONNX models (Windows).
# All arguments are passed to tools/setup_models.py (--out DIR, --force, ...).
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$script = Join-Path $repo 'tools\setup_models.py'
if (Get-Command uv -ErrorAction SilentlyContinue) {
    # uv provides Python 3.12 itself if it is not installed.
    & uv run --no-project --python 3.12 $script @args
} elseif (Get-Command py -ErrorAction SilentlyContinue) {
    & py -3 $script @args
} elseif (Get-Command python -ErrorAction SilentlyContinue) {
    & python $script @args
} else {
    Write-Error 'Neither uv nor Python found. Install uv (https://docs.astral.sh/uv/) or Python 3.12.'
}
exit $LASTEXITCODE
