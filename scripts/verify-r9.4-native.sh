#!/usr/bin/env bash
set -euo pipefail
PATCH_ID="sim-core-r9.4-native-closed-loop"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RENODE_BIN="${RENODE_BIN:-$(command -v renode || true)}"

fail() { printf '[%s] ERROR: %s\n' "$PATCH_ID" "$*" >&2; exit 1; }

[[ -n "$RENODE_BIN" ]] || fail "renode is required"
command -v cargo >/dev/null || fail "cargo is required"
command -v verilator >/dev/null || fail "verilator is required"
command -v cmake >/dev/null || fail "cmake is required to build Renode External Control client"
command -v cc >/dev/null || fail "a C compiler is required to build native helpers"
command -v python3 >/dev/null || fail "python3 is required"

if [[ -z "${RENODE_SOURCE_DIR:-}" ]]; then
  for candidate in \
    "$HOME/Workspace/renode" \
    "$HOME/Workspace/Renode" \
    "/opt/renode-source"; do
    if [[ -f "$candidate/tools/external_control_client/include/renode_api.h" ]]; then
      RENODE_SOURCE_DIR="$candidate"
      break
    fi
  done
fi
[[ -n "${RENODE_SOURCE_DIR:-}" ]] || fail \
  "RENODE_SOURCE_DIR must point to the Renode source tree containing tools/external_control_client"
export RENODE_SOURCE_DIR

TMP="$(mktemp -d)"
export RENODE_EXTERNAL_CONTROL_BUILD_DIR="$TMP/renode-external-control-build"
HELPERS="$TMP/helpers"
FIFO="$TMP/renode.stdin"
LOG="$TMP/renode.log"
SCRIPT="$TMP/r9.4.resc"
RENODE_PID=""
exec 9>&-
cleanup() {
  set +e
  if [[ -n "$RENODE_PID" ]] && kill -0 "$RENODE_PID" 2>/dev/null; then
    printf 'quit\n' >&9 2>/dev/null || true
    for _ in $(seq 1 20); do
      kill -0 "$RENODE_PID" 2>/dev/null || break
      sleep 0.1
    done
    kill "$RENODE_PID" 2>/dev/null || true
    wait "$RENODE_PID" 2>/dev/null || true
  fi
  exec 9>&- 2>/dev/null || true
  rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

printf '[%s] building official Renode External Control + SharedSpice helpers\n' "$PATCH_ID"
bash "$ROOT/scripts/build-r9.3-native-helpers.sh" "$HELPERS" >/dev/null
RENODE_HELPER="$HELPERS/ontologyx-renode-cosim-helper"
NGSPICE_HELPER="$HELPERS/ontologyx-ngspice-cosim-helper"
[[ -x "$RENODE_HELPER" ]] || fail "Renode helper was not built"
[[ -x "$NGSPICE_HELPER" ]] || fail "ngspice helper was not built"

PORT="$(python3 - <<'PY'
import socket
s = socket.socket()
s.bind(('127.0.0.1', 0))
print(s.getsockname()[1])
s.close()
PY
)"
FIRMWARE="$ROOT/tests/fixtures/stm32f4_cosim_feedback.elf"
[[ -f "$FIRMWARE" ]] || fail "missing R9.4 firmware fixture: $FIRMWARE"

cat > "$SCRIPT" <<RESC
using sysbus
mach create "oxsim"
machine LoadPlatformDescription @platforms/boards/stm32f4_discovery-kit.repl
cpu PerformanceInMips 125
sysbus LoadELF @$FIRMWARE
emulation CreateExternalControlServer "oxsim_control" $PORT
log "OX_SIM_R9_4_EXTERNAL_CONTROL_READY"
RESC

mkfifo "$FIFO"
exec 9<>"$FIFO"
printf '[%s] starting isolated Renode External Control server on port %s\n' "$PATCH_ID" "$PORT"
"$RENODE_BIN" --disable-gui --console "$SCRIPT" <"$FIFO" >"$LOG" 2>&1 &
RENODE_PID=$!

ready=0
for _ in $(seq 1 200); do
  if ! kill -0 "$RENODE_PID" 2>/dev/null; then
    printf '[%s] Renode exited before External Control became ready:\n' "$PATCH_ID" >&2
    tail -n 120 "$LOG" >&2 || true
    exit 1
  fi
  if python3 - "$PORT" <<'PY' >/dev/null 2>&1
import socket, sys
s = socket.socket()
s.settimeout(0.1)
try:
    s.connect(('127.0.0.1', int(sys.argv[1])))
except OSError:
    raise SystemExit(1)
finally:
    s.close()
PY
  then
    ready=1
    break
  fi
  sleep 0.05
done
if [[ "$ready" != 1 ]]; then
  printf '[%s] Renode External Control did not become ready:\n' "$PATCH_ID" >&2
  tail -n 120 "$LOG" >&2 || true
  exit 1
fi

printf '[%s] running real Renode -> Verilator -> SharedSpice -> ADC -> firmware feedback proof\n' "$PATCH_ID"
ONTOLOGYX_SIM_REQUIRE_R9_NATIVE_CLOSED_LOOP=1 \
ONTOLOGYX_SIM_NGSPICE_DIAGNOSTICS=1 \
ONTOLOGYX_SIM_R9_RENODE_PORT="$PORT" \
ONTOLOGYX_SIM_R9_RENODE_HELPER="$RENODE_HELPER" \
ONTOLOGYX_SIM_R9_NGSPICE_HELPER="$NGSPICE_HELPER" \
  cargo test --manifest-path "$ROOT/Cargo.toml" --test r9_native_closed_loop -- --nocapture

printf '[%s] PASS: real native closed-loop co-simulation completed.\n' "$PATCH_ID"
