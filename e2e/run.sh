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

# If no pre-built binaries, build locally (dev mode)
if [[ ! -f "$E2E_BIN_DIR/glibc/tako" ]]; then
  echo "No pre-built binaries at $E2E_BIN_DIR, building locally..."
  mkdir -p "$E2E_BIN_DIR/glibc"
  cargo build -p tako --bin tako
  cargo build -p tako-server --bin tako-server
  cp target/debug/tako target/debug/tako-dev-server target/debug/tako-loopback-proxy "$E2E_BIN_DIR/glibc/"
  cp target/debug/tako-server "$E2E_BIN_DIR/glibc/"
fi

docker compose -p "$PROJECT_NAME" -f "$COMPOSE_FILE" down --volumes --remove-orphans >/dev/null 2>&1 || true
docker compose -p "$PROJECT_NAME" -f "$COMPOSE_FILE" build server-ubuntu server-alma server-alpine runner
docker compose -p "$PROJECT_NAME" -f "$COMPOSE_FILE" run --rm --no-deps --entrypoint sh runner \
  -c "rm -f /opt/e2e/keys/id_ed25519 /opt/e2e/keys/id_ed25519.pub && ssh-keygen -t ed25519 -N '' -f /opt/e2e/keys/id_ed25519 -q"
docker compose -p "$PROJECT_NAME" -f "$COMPOSE_FILE" up -d --force-recreate server-ubuntu server-alma server-alpine
docker compose -p "$PROJECT_NAME" -f "$COMPOSE_FILE" run --rm runner "$FIXTURE"
