#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT="${HOME}/Downloads/sim-core-snapshot-${STAMP}.zip"

usage() {
  cat <<'USAGE'
Usage: cargo snapshot [--output PATH]

Creates a source-state snapshot of sim-core. Tracked files and non-ignored
untracked files are included; Git metadata, target output, logs, profiles and
other generated artifacts are excluded.
USAGE
}

while (($#)); do
  case "$1" in
    --output)
      [[ $# -ge 2 ]] || { echo '[sim-core] --output requires a path' >&2; exit 2; }
      OUT="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "[sim-core] unknown snapshot argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

command -v zip >/dev/null || { echo '[sim-core] zip is required' >&2; exit 1; }
command -v sha256sum >/dev/null || { echo '[sim-core] sha256sum is required' >&2; exit 1; }

mkdir -p "$(dirname "$OUT")"
OUT="$(cd "$(dirname "$OUT")" && pwd)/$(basename "$OUT")"
LIST="$(mktemp)"
trap 'rm -f "$LIST"' EXIT

cd "$ROOT"

if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  git ls-files --cached --others --exclude-standard | LC_ALL=C sort -u >"$LIST"
  SOURCE_MODE="git tracked + non-ignored untracked"
else
  find . -type f \
    ! -path './.git/*' \
    ! -path './target/*' \
    ! -name '.DS_Store' \
    ! -name '*.tmp' \
    ! -name '*.log' \
    ! -name '*.profraw' \
    ! -name '*.profdata' \
    ! -name '*.zip' \
    -print | sed 's#^./##' | LC_ALL=C sort -u >"$LIST"
  SOURCE_MODE="portable filesystem scan"
fi

# Defense in depth even when Git ignore rules drift.
FILTERED="$(mktemp)"
trap 'rm -f "$LIST" "$FILTERED"' EXIT
while IFS= read -r path; do
  case "/$path/" in
    */.git/*|*/target/*) continue ;;
  esac
  case "$path" in
    .DS_Store|*.tmp|*.log|*.profraw|*.profdata|*.zip) continue ;;
  esac
  printf '%s\n' "$path" >>"$FILTERED"
done <"$LIST"
mv "$FILTERED" "$LIST"

COUNT="$(wc -l <"$LIST" | tr -d ' ')"
[[ "$COUNT" -gt 0 ]] || { echo '[sim-core] snapshot file set is empty' >&2; exit 1; }

rm -f "$OUT"
zip -q "$OUT" -@ <"$LIST"

printf '== Sim Core snapshot ==\n'
printf 'source: %s\n' "$ROOT"
printf 'mode:   %s\n' "$SOURCE_MODE"
printf 'files:  %s\n' "$COUNT"
printf 'output: %s\n' "$OUT"
ls -lh "$OUT"
printf '\nsha256:\n'
sha256sum "$OUT"
