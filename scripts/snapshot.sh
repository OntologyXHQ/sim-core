#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${1:-$HOME/Downloads}"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT="$OUT_DIR/sim-core-snapshot-$STAMP.zip"
mkdir -p "$OUT_DIR"
python3 - "$ROOT" "$OUT" <<'PY'
import os, sys, zipfile
root, out = sys.argv[1], sys.argv[2]
ignored = {'.git', 'target', '.DS_Store'}
with zipfile.ZipFile(out, 'w', zipfile.ZIP_DEFLATED, compresslevel=9) as z:
    for base, dirs, files in os.walk(root):
        dirs[:] = [d for d in dirs if d not in ignored]
        for name in files:
            if name in ignored or name.endswith(('.profraw',)):
                continue
            path = os.path.join(base, name)
            rel = os.path.relpath(path, root)
            z.write(path, os.path.join('sim-core', rel))
print(out)
PY
