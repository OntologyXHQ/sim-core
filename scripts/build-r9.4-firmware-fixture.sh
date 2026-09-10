#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/target/r9.4-firmware-fixture}"
CLANG="${CLANG:-clang}"
LD_LLD="${LD_LLD:-ld.lld}"
mkdir -p "$OUT"
command -v "$CLANG" >/dev/null || { echo '[sim-core-r9.4] clang is required' >&2; exit 2; }
command -v "$LD_LLD" >/dev/null || { echo '[sim-core-r9.4] ld.lld is required' >&2; exit 2; }
COMMON=( --target=arm-none-eabi -mcpu=cortex-m4 -mthumb -ffreestanding -fno-builtin -fdata-sections -ffunction-sections -Os -Wall -Wextra -Werror )
# Compile stable basenames so the ELF STT_FILE entries do not depend on the
# repository/source filename. This keeps the checked fixture byte-reproducible.
cp "$ROOT/tests/fixtures/stm32f4_cosim_feedback.S" "$OUT/startup.S"
cp "$ROOT/tests/fixtures/stm32f4_cosim_feedback.c" "$OUT/main.c"
"$CLANG" "${COMMON[@]}" -c "$OUT/startup.S" -o "$OUT/startup.o"
"$CLANG" "${COMMON[@]}" -c "$OUT/main.c" -o "$OUT/main.o"
"$LD_LLD" -T "$ROOT/tests/fixtures/stm32f4_cosim_feedback.ld" --gc-sections \
  "$OUT/startup.o" "$OUT/main.o" -o "$OUT/stm32f4_cosim_feedback.elf"
printf '[sim-core-r9.4] built %s\n' "$OUT/stm32f4_cosim_feedback.elf"
