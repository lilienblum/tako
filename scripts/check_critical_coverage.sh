#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COV_DIR="$ROOT_DIR/target/covtarget"
RAW_DIR="$COV_DIR/raw"
PROFDATA="$COV_DIR/coverage.profdata"
REPORT_FILE="$COV_DIR/coverage-critical.txt"
THRESHOLD="${1:-80.0}"

SOURCE_FILES=(
  "$ROOT_DIR/tako/src/config/secrets.rs"
  "$ROOT_DIR/tako/src/config/servers_toml.rs"
  "$ROOT_DIR/tako/src/config/tako_toml.rs"
  "$ROOT_DIR/tako/src/runtime/bun.rs"
  "$ROOT_DIR/tako/src/validation/config.rs"
  "$ROOT_DIR/tako-server/src/proxy/static_files.rs"
  "$ROOT_DIR/tako-server/src/routing.rs"
  "$ROOT_DIR/tako-server/src/scaling/cold_start.rs"
)

mkdir -p "$RAW_DIR"
rm -f "$RAW_DIR"/*.profraw 2>/dev/null || true

export CARGO_TARGET_DIR="$COV_DIR"
export CARGO_INCREMENTAL=0
export RUSTFLAGS="-Cinstrument-coverage"
export RUSTDOCFLAGS="-Cinstrument-coverage"
export LLVM_PROFILE_FILE="$RAW_DIR/tako-%p-%m.profraw"

cargo test --workspace >/dev/null

SYSROOT="$(rustc --print sysroot)"
HOST="$(rustc -vV | awk '/host:/ {print $2}')"
LLVM_BIN="$SYSROOT/lib/rustlib/$HOST/bin"

"$LLVM_BIN/llvm-profdata" merge -sparse "$RAW_DIR"/*.profraw -o "$PROFDATA"

objects=()
for file in "$ROOT_DIR"/target/covtarget/debug/deps/*; do
  if [[ -f "$file" && -x "$file" ]]; then
    objects+=("$file")
  fi
done
for file in "$ROOT_DIR"/target/covtarget/debug/tako "$ROOT_DIR"/target/covtarget/debug/tako-server "$ROOT_DIR"/target/covtarget/debug/tako-dev-server; do
  if [[ -f "$file" && -x "$file" ]]; then
    objects+=("$file")
  fi
done

cov_args=()
for obj in "${objects[@]}"; do
  cov_args+=(--object "$obj")
done

source_args=()
for src in "${SOURCE_FILES[@]}"; do
  source_args+=(--sources "$src")
done

"$LLVM_BIN/llvm-cov" report \
  "${cov_args[@]}" \
  --instr-profile "$PROFDATA" \
  --ignore-filename-regex='/.cargo/registry|/rustc/' \
  "${source_args[@]}" \
  > "$REPORT_FILE"

cat "$REPORT_FILE"

total_line_cov="$(awk '$1=="TOTAL"{gsub("%","",$10); print $10}' "$REPORT_FILE")"
if [[ -z "$total_line_cov" ]]; then
  echo "Failed to parse TOTAL line coverage from $REPORT_FILE" >&2
  exit 1
fi

if awk -v coverage="$total_line_cov" -v threshold="$THRESHOLD" 'BEGIN { exit !(coverage + 0 >= threshold + 0) }'; then
  echo "Critical coverage check passed: ${total_line_cov}% >= ${THRESHOLD}%"
else
  echo "Critical coverage check failed: ${total_line_cov}% < ${THRESHOLD}%" >&2
  exit 1
fi
