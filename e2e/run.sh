#!/usr/bin/env bash
set -euo pipefail

FIXTURE=${1:-e2e/js/tanstack-start}
REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
COMPOSE_FILE="$REPO_ROOT/e2e/docker/compose.yml"
PROJECT_NAME="tako-e2e"
CACHE_ROOT="${E2E_CACHE_ROOT:-${XDG_CACHE_HOME:-$HOME/.cache}/tako/e2e}"
E2E_CARGO_HOME_DIR="${E2E_CARGO_HOME_DIR:-$CACHE_ROOT/cargo-home}"
E2E_CARGO_TARGET_DIR="${E2E_CARGO_TARGET_DIR:-$CACHE_ROOT/target}"

cleanup() {
  local exit_code=$?
  if [[ $exit_code -ne 0 ]]; then
    docker compose -p "$PROJECT_NAME" -f "$COMPOSE_FILE" logs --no-color --tail=200 server-ubuntu server-alma runner || true
  fi
  docker compose -p "$PROJECT_NAME" -f "$COMPOSE_FILE" down --volumes --remove-orphans >/dev/null 2>&1 || true
}
trap cleanup EXIT

mkdir -p "$E2E_CARGO_HOME_DIR" "$E2E_CARGO_TARGET_DIR"

export E2E_CARGO_HOME_DIR
export E2E_CARGO_TARGET_DIR

cd "$REPO_ROOT"

docker compose -p "$PROJECT_NAME" -f "$COMPOSE_FILE" down --volumes --remove-orphans >/dev/null 2>&1 || true
docker compose -p "$PROJECT_NAME" -f "$COMPOSE_FILE" build server-ubuntu server-alma runner
docker compose -p "$PROJECT_NAME" -f "$COMPOSE_FILE" run --rm --no-deps --entrypoint sh runner \
  -c "rm -f /opt/e2e/keys/id_ed25519 /opt/e2e/keys/id_ed25519.pub && ssh-keygen -t ed25519 -N '' -f /opt/e2e/keys/id_ed25519 -q"
docker compose -p "$PROJECT_NAME" -f "$COMPOSE_FILE" up -d --force-recreate server-ubuntu server-alma
docker compose -p "$PROJECT_NAME" -f "$COMPOSE_FILE" run --rm runner "$FIXTURE"
