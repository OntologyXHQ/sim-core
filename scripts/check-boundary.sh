#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
for forbidden in package.json pnpm-workspace.yaml node_modules packages apps; do
  if [[ -e "$forbidden" ]]; then
    echo "[sim-core] forbidden non-Rust boundary path: $forbidden" >&2
    exit 1
  fi
done
if [[ ! -f Cargo.toml || ! -d src || ! -d tests ]]; then
  echo "[sim-core] root Rust crate contract is incomplete" >&2
  exit 1
fi
grep -q '^name = "ontologyx-sim-core"$' Cargo.toml || {
  echo '[sim-core] crate identity drift' >&2
  exit 1
}
if grep -RIn --exclude-dir=.git --exclude-dir=target -E '@ontologyx/sim|napi|Node\.js API' src tests Cargo.toml >/dev/null 2>&1; then
  echo '[sim-core] language-binding ownership leaked into Rust core' >&2
  exit 1
fi
echo '[sim-core] pure-Rust repository boundary passed'
