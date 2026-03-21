#!/usr/bin/env bash
set -euo pipefail

FIXTURE=${1:-e2e/fixtures/javascript/tanstack-start}
REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
COMPOSE_FILE="$REPO_ROOT/e2e/docker/compose.yml"
PROJECT_NAME="tako-e2e"
E2E_BIN_DIR="${E2E_BIN_DIR:-$REPO_ROOT/.e2e-bin}"

cleanup() {
  local exit_code=$?
  if [[ $exit_code -ne 0 ]]; then
    docker compose -p "$PROJECT_NAME" -f "$COMPOSE_FILE" logs --no-color --tail=200 server-ubuntu server-alma server-alpine runner || true
  fi
  docker compose -p "$PROJECT_NAME" -f "$COMPOSE_FILE" down --volumes --remove-orphans >/dev/null 2>&1 || true
}
trap cleanup EXIT

export E2E_BIN_DIR

cd "$REPO_ROOT"

# If no pre-built binaries, cross-compile Linux binaries with cargo-zigbuild
if [[ ! -f "$E2E_BIN_DIR/glibc/tako" ]]; then
  echo "No pre-built binaries at $E2E_BIN_DIR, building with cargo-zigbuild..."
  mkdir -p "$E2E_BIN_DIR/glibc" "$E2E_BIN_DIR/musl"

  # Detect host arch → pick matching Linux target
  ARCH_RAW=$(uname -m)
  if [[ "$ARCH_RAW" == "arm64" || "$ARCH_RAW" == "aarch64" ]]; then
    GLIBC_TARGET="aarch64-unknown-linux-gnu"
    MUSL_TARGET="aarch64-unknown-linux-musl"
  else
    GLIBC_TARGET="x86_64-unknown-linux-gnu"
    MUSL_TARGET="x86_64-unknown-linux-musl"
  fi

  cargo zigbuild -p tako-server -p tako \
    --bin tako --bin tako-dev-server --bin tako-loopback-proxy --bin tako-server \
    --release --target "$GLIBC_TARGET"
  cp target/"$GLIBC_TARGET"/release/tako \
     target/"$GLIBC_TARGET"/release/tako-dev-server \
     target/"$GLIBC_TARGET"/release/tako-loopback-proxy \
     target/"$GLIBC_TARGET"/release/tako-server \
     "$E2E_BIN_DIR/glibc/"

  # musl build (used for Alpine)
  if cargo zigbuild -p tako-server --release --target "$MUSL_TARGET" 2>"$E2E_BIN_DIR/musl-build.log"; then
    cp target/"$MUSL_TARGET"/release/tako-server "$E2E_BIN_DIR/musl/"
    rm -f "$E2E_BIN_DIR/musl-build.log"
  else
    echo "musl build skipped (see .e2e-bin/musl-build.log for details)"
  fi

  chmod +x "$E2E_BIN_DIR/glibc/"* "$E2E_BIN_DIR/musl/"* 2>/dev/null || true
fi

docker compose -p "$PROJECT_NAME" -f "$COMPOSE_FILE" down --volumes --remove-orphans >/dev/null 2>&1 || true
docker compose -p "$PROJECT_NAME" -f "$COMPOSE_FILE" build server-ubuntu server-alma server-alpine runner
docker compose -p "$PROJECT_NAME" -f "$COMPOSE_FILE" run --rm --no-deps --entrypoint sh runner \
  -c "rm -f /opt/e2e/keys/id_ed25519 /opt/e2e/keys/id_ed25519.pub && ssh-keygen -t ed25519 -N '' -f /opt/e2e/keys/id_ed25519 -q"
docker compose -p "$PROJECT_NAME" -f "$COMPOSE_FILE" up -d --force-recreate server-ubuntu server-alma server-alpine
docker compose -p "$PROJECT_NAME" -f "$COMPOSE_FILE" run --rm runner "$FIXTURE"
