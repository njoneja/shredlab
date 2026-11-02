#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Navneet Joneja

# Fails if a tracked source file lacks an SPDX license line. Vendored code
# (third_party/) keeps its own license and is skipped. Run from anywhere.
cd "$(dirname "$0")/.."
missing=$(git ls-files \
  | grep -E '\.(rs|ts|js|mjs|svelte|py|sh|css|c|cc|cpp|h|hpp)$' \
  | grep -v -E '(^|/)third_party/|^private/' \
  | while read -r f; do
      head -n 5 "$f" | grep -q 'SPDX-License-Identifier' || echo "$f"
    done)
if [ -n "$missing" ]; then
  echo "Missing SPDX-License-Identifier (add the two SPDX lines at the top):" >&2
  echo "$missing" >&2
  exit 1
fi
echo "SPDX headers: ok"
