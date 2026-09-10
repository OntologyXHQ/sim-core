#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
command -v cargo >/dev/null || { echo '[sim-core] cargo is required' >&2; exit 1; }
command -v ngspice >/dev/null || { echo '[sim-core] ngspice is required for full verification' >&2; exit 1; }
command -v verilator >/dev/null || { echo '[sim-core] verilator is required for full verification' >&2; exit 1; }
command -v renode >/dev/null || { echo '[sim-core] renode is required for full verification' >&2; exit 1; }
command -v make >/dev/null || { echo '[sim-core] GNU Make is required by verilator --binary' >&2; exit 1; }
if ! command -v c++ >/dev/null && ! command -v g++ >/dev/null && ! command -v clang++ >/dev/null; then
  echo '[sim-core] a C++ compiler is required by verilator --binary' >&2
  exit 1
fi
command -v unzip >/dev/null || { echo '[sim-core] unzip is required for snapshot verification' >&2; exit 1; }

printf '\n== Pure Rust boundary ==\n'
bash scripts/check-boundary.sh
printf '\n== Rust format ==\n'
cargo fmt --all -- --check
printf '\n== Rust clippy ==\n'
cargo clippy --all-targets --all-features -- -D warnings
printf '\n== Rust tests ==\n'
cargo test --all-targets --all-features
printf '\n== Execution control proof ==\n'
cargo test --test execution_control -- --nocapture
printf '\n== Digital foundation proof ==\n'
cargo test --test digital_engine -- --nocapture
printf '\n== Complete R3 digital proof ==\n'
cargo test --test r3_complete -- --nocapture
printf '\n== Real ngspice integration ==\n'
ONTOLOGYX_SIM_REQUIRE_NGSPICE=1 cargo test --test ngspice_engine -- --nocapture
printf '\n== Real XSPICE digital parity ==\n'
ONTOLOGYX_SIM_REQUIRE_XSPICE=1 cargo test --test xspice_engine -- --nocapture
printf '\n== Complete R4 mixed-signal contract proof ==\n'
cargo test --test r4_complete -- --nocapture
printf '\n== R4 mixed-signal integration ==\n'
ONTOLOGYX_SIM_REQUIRE_MIXED_SIGNAL=1 cargo test --test mixed_signal_engine -- --nocapture
printf '\n== Complete R5 HDL contract proof ==\n'
cargo test --test r5_complete -- --nocapture
printf '\n== Real Verilator HDL integration ==\n'
ONTOLOGYX_SIM_REQUIRE_VERILATOR=1 cargo test --test verilator_engine -- --nocapture
printf '\n== Complete R6 MCU/firmware contract proof ==\n'
cargo test --test r6_complete -- --nocapture
printf '\n== Real Renode firmware integration ==\n'
ONTOLOGYX_SIM_REQUIRE_RENODE=1 cargo test --test renode_engine -- --nocapture
printf '\n== Complete R7 service API proof ==\n'
cargo test --features service --test r7_complete -- --nocapture
printf '\n== R7 service binary compile ==\n'
cargo build --features service --bin sim-service
printf '\n== Complete R8 production runtime proof ==\n'
cargo test --features production --test r8_complete -- --nocapture
printf '\n== R8 production worker binary compile ==\n'
cargo build --features production --bin sim-worker
printf '\n== R9.1 deterministic co-simulation scheduler proof ==\n'
cargo test --test r9_scheduler -- --nocapture
printf '\n== R9.2 Digital + live Verilator co-simulation adapters ==\n'
ONTOLOGYX_SIM_REQUIRE_R9_VERILATOR=1 cargo test --test r9_adapters -- --nocapture
printf '\n== R9.3 Renode External Control + SharedSpice participant contracts ==\n'
cargo test --test r9_external_backends -- --nocapture
printf '\n== R9.4 explicit bridges + portable closed-loop proof ==\n'
ONTOLOGYX_SIM_REQUIRE_R9_CLOSED_LOOP=1 cargo test --test r9_closed_loop -- --nocapture
if [[ "${ONTOLOGYX_SIM_REQUIRE_R9_NATIVE_CLOSED_LOOP:-0}" == "1" ]]; then
  printf '\n== R9.4 native Renode + Verilator + SharedSpice closed-loop proof ==\n'
  bash scripts/verify-r9.4-native.sh
else
  printf '\n== R9.4 native closeout proof skipped (set ONTOLOGYX_SIM_REQUIRE_R9_NATIVE_CLOSED_LOOP=1) ==\n'
fi
if command -v bwrap >/dev/null && command -v prlimit >/dev/null; then
  printf '\n== R8 Linux isolation proof ==\n'
  ONTOLOGYX_SIM_REQUIRE_PRODUCTION_ISOLATION=1 cargo test --features production --test r8_isolation -- --nocapture
else
  printf '\n== R8 Linux isolation proof skipped (bwrap/prlimit unavailable) ==\n'
fi
printf '\n== Runnable example ==\n'
cargo run --quiet --example voltage_divider
cargo run --quiet --example digital_and
cargo run --quiet --example digital_counter
cargo run --quiet --example mixed_signal_round_trip
cargo run --quiet --example verilator_counter
cargo run --quiet --example renode_gpio
cargo run --quiet --example cosim_chain
printf '\n== cargo snapshot contract ==\n'
SNAPSHOT_TMP="$(mktemp)"
rm -f "$SNAPSHOT_TMP"
SNAPSHOT_PROOF="${SNAPSHOT_TMP}.zip"
trap 'rm -f "$SNAPSHOT_PROOF"' EXIT
cargo snapshot --output "$SNAPSHOT_PROOF"
unzip -Z1 "$SNAPSHOT_PROOF" | grep -q '^src/lib.rs$' || { echo '[sim-core] snapshot is missing src/lib.rs' >&2; exit 1; }
if unzip -Z1 "$SNAPSHOT_PROOF" | grep -Eq '(^|/)(\.git|target)/'; then
  echo '[sim-core] snapshot contains forbidden generated/Git content' >&2
  exit 1
fi
rm -f "$SNAPSHOT_PROOF"
trap - EXIT
printf '\n== Cargo package dry-run ==\n'
cargo package --allow-dirty
printf '\n[sim-core] full verification passed.\n'
