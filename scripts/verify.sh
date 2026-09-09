#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
command -v cargo >/dev/null || { echo '[sim-core] cargo is required' >&2; exit 1; }
command -v ngspice >/dev/null || { echo '[sim-core] ngspice is required for full verification' >&2; exit 1; }
command -v unzip >/dev/null || { echo '[sim-core] unzip is required for snapshot verification' >&2; exit 1; }

printf '\n== Pure Rust boundary ==\n'
bash scripts/check-boundary.sh
printf '\n== Rust format ==\n'
cargo fmt --all -- --check
printf '\n== Rust clippy ==\n'
cargo clippy --all-targets -- -D warnings
printf '\n== Rust tests ==\n'
cargo test --all-targets
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
printf '\n== Runnable example ==\n'
cargo run --quiet --example voltage_divider
cargo run --quiet --example digital_and
cargo run --quiet --example digital_counter
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
