#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
command -v cargo >/dev/null || { echo '[sim-core] cargo is required' >&2; exit 1; }
command -v ngspice >/dev/null || { echo '[sim-core] ngspice is required for full verification' >&2; exit 1; }

printf '\n== Pure Rust boundary ==\n'
bash scripts/check-boundary.sh
printf '\n== Rust format ==\n'
cargo fmt --all -- --check
printf '\n== Rust clippy ==\n'
cargo clippy --all-targets -- -D warnings
printf '\n== Rust tests ==\n'
cargo test --all-targets
printf '\n== Real ngspice integration ==\n'
ONTOLOGYX_SIM_REQUIRE_NGSPICE=1 cargo test --test ngspice_engine -- --nocapture
printf '\n== Runnable example ==\n'
cargo run --quiet --example voltage_divider
printf '\n== Cargo package dry-run ==\n'
cargo package --allow-dirty
printf '\n[sim-core] full verification passed.\n'
