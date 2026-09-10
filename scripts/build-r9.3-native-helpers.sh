#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/target/r9.3-helpers}"
mkdir -p "$OUT"

CC="${CC:-cc}"
CFLAGS=( -std=c11 -O2 -Wall -Wextra -Werror )

if [[ -z "${RENODE_SOURCE_DIR:-}" ]]; then
  echo "[sim-core-r9.3] RENODE_SOURCE_DIR is required to build the Renode External Control helper" >&2
  echo "Point it at the Renode source tree containing tools/external_control_client." >&2
  exit 2
fi
RENODE_API_DIR="$RENODE_SOURCE_DIR/tools/external_control_client"
RENODE_BUILD_DIR="${RENODE_EXTERNAL_CONTROL_BUILD_DIR:-$RENODE_SOURCE_DIR/build-external-control}"
cmake -S "$RENODE_API_DIR/lib" -B "$RENODE_BUILD_DIR"
cmake --build "$RENODE_BUILD_DIR" --parallel 1
RENODE_LIB="$(find "$RENODE_BUILD_DIR" -type f -name 'librenode_api.a' -print -quit)"
if [[ -z "$RENODE_LIB" ]]; then
  echo "[sim-core-r9.3] librenode_api.a not found after External Control client build" >&2
  exit 2
fi
"$CC" "${CFLAGS[@]}" \
  -I"$RENODE_API_DIR/include" \
  "$ROOT/native/renode-cosim-helper.c" "$RENODE_LIB" \
  -pthread -o "$OUT/ontologyx-renode-cosim-helper"

if pkg-config --exists ngspice 2>/dev/null; then
  read -r -a NG_CFLAGS <<<"$(pkg-config --cflags ngspice)"
  read -r -a NG_LIBS <<<"$(pkg-config --libs ngspice)"
else
  NG_CFLAGS=()
  NG_LIBS=( -lngspice )
fi
"$CC" "${CFLAGS[@]}" "${NG_CFLAGS[@]}" \
  "$ROOT/native/ngspice-cosim-helper.c" "${NG_LIBS[@]}" -lm -pthread \
  -o "$OUT/ontologyx-ngspice-cosim-helper"

printf '%s\n' "$OUT/ontologyx-renode-cosim-helper" "$OUT/ontologyx-ngspice-cosim-helper"
