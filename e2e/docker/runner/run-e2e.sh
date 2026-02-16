#!/usr/bin/env bash
set -euo pipefail

WORKSPACE=${WORKSPACE:-/workspace}
FIXTURE_REL=${1:-${E2E_FIXTURE:-e2e/fixtures/js/tanstack-start}}
FIXTURE_DIR="$WORKSPACE/$FIXTURE_REL"

if [[ ! -d "$FIXTURE_DIR" ]]; then
  echo "Fixture directory not found: $FIXTURE_DIR" >&2
  exit 1
fi

TMP_ROOT=$(mktemp -d)
cleanup() {
  rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

HOME_DIR="$TMP_ROOT/home"
TAKO_HOME="$TMP_ROOT/tako-home"
JS_WORKSPACE_DIR="$TMP_ROOT/js-workspace"
PROJECT_DIR="$JS_WORKSPACE_DIR/$FIXTURE_REL"
mkdir -p "$HOME_DIR/.ssh" "$TAKO_HOME" "$JS_WORKSPACE_DIR"

cp /opt/e2e/keys/id_ed25519 "$HOME_DIR/.ssh/id_ed25519"
cp /opt/e2e/keys/id_ed25519.pub "$HOME_DIR/.ssh/id_ed25519.pub"
cat > "$HOME_DIR/.ssh/config" <<'CFG'
Host server-ubuntu server-alma
  User tako
  IdentityFile ~/.ssh/id_ed25519
  IdentitiesOnly yes
  StrictHostKeyChecking no
  UserKnownHostsFile /dev/null
CFG
chmod 700 "$HOME_DIR/.ssh"
chmod 600 "$HOME_DIR/.ssh/id_ed25519"
chmod 644 "$HOME_DIR/.ssh/id_ed25519.pub"
chmod 600 "$HOME_DIR/.ssh/config"
SSH_KEY="$HOME_DIR/.ssh/id_ed25519"
SSH_OPTS=(
  -o StrictHostKeyChecking=no
  -o UserKnownHostsFile=/dev/null
  -o BatchMode=yes
  -i "$SSH_KEY"
)

ssh_exec() {
  local host=$1
  shift
  HOME="$HOME_DIR" ssh "${SSH_OPTS[@]}" "tako@$host" "$@"
}

scp_to() {
  local source=$1
  local host=$2
  local destination=$3
  HOME="$HOME_DIR" scp "${SSH_OPTS[@]}" "$source" "tako@$host:$destination"
}

ssh_wait() {
  local host=$1
  for _ in $(seq 1 80); do
    if ssh_exec "$host" "echo ok" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.25
  done
  echo "SSH not ready: $host" >&2
  return 1
}

wait_tako_socket() {
  local host=$1
  for _ in $(seq 1 120); do
    if ssh_exec "$host" \
      "printf '%s\n' '{\"command\":\"list\"}' | nc -U /var/run/tako/tako.sock | head -n 1 | grep -q ." \
      >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.25
  done
  echo "tako-server socket not ready: $host" >&2
  ssh_exec "$host" "tail -n 120 /tmp/tako-server.log || true" >&2 || true
  return 1
}

resolve_current_release_link() {
  local host=$1
  ssh_exec "$host" '
for link in /opt/tako/apps/*/current; do
  [ -L "$link" ] || continue
  readlink "$link"
  exit 0
done
exit 1
'
}

detect_route_host() {
  local toml_path=$1
  local env_name=${2:-production}
  awk -v env_name="$env_name" '
    $0 ~ "^\\[envs\\." env_name "\\]" {
      in_env = 1
      next
    }
    in_env && $0 ~ "^\\[" {
      in_env = 0
    }
    in_env && $1 == "route" {
      line = $0
      sub(/^[^=]*=[[:space:]]*"/, "", line)
      sub(/".*$/, "", line)
      print line
      exit
    }
  ' "$toml_path"
}

fetch_route_path() {
  local route_host=$1
  local route_path=$2
  local headers_file=$3
  local body_file=$4

  curl -sS \
    -k \
    --http1.1 \
    --connect-timeout 3 \
    --max-time 10 \
    -H "Host: ${route_host}" \
    -H "Connection: close" \
    -D "$headers_file" \
    -o "$body_file" \
    -w "%{http_code}" \
    "https://server-ubuntu:8443${route_path}"
}

require_http_ok() {
  local route_host=$1
  local route_path=$2
  local description=$3
  local require_non_empty=${4:-1}
  local headers_file="$TMP_ROOT/http_headers.tmp"
  local body_file="$TMP_ROOT/http_body.tmp"
  local status

  status=$(fetch_route_path "$route_host" "$route_path" "$headers_file" "$body_file" || true)

  if [[ ! "$status" =~ ^[0-9]+$ ]] || (( status < 200 || status >= 400 )); then
    echo "$description check failed for path '$route_path' (status=$status)" >&2
    [[ -f "$headers_file" ]] && cat "$headers_file" >&2 || true
    [[ -f "$body_file" ]] && cat "$body_file" >&2 || true
    exit 1
  fi

  if (( require_non_empty )) && [[ ! -s "$body_file" ]]; then
    echo "$description check failed for path '$route_path': empty response body" >&2
    exit 1
  fi
}

check_http_ok_optional() {
  local route_host=$1
  local route_path=$2
  local description=$3
  local headers_file="$TMP_ROOT/http_headers_optional.tmp"
  local body_file="$TMP_ROOT/http_body_optional.tmp"
  local status

  status=$(fetch_route_path "$route_host" "$route_path" "$headers_file" "$body_file" || true)

  if [[ ! "$status" =~ ^[0-9]+$ ]] || (( status < 200 || status >= 400 )); then
    echo "$description check skipped for path '$route_path' (status=$status)" >&2
    [[ -f "$headers_file" ]] && cat "$headers_file" >&2 || true
    [[ -f "$body_file" ]] && cat "$body_file" >&2 || true
  fi
}

run_universal_http_checks() {
  local route_host=$1
  local release_app_dir=$2
  local root_headers="$TMP_ROOT/root_headers.tmp"
  local root_body="$TMP_ROOT/root_body.tmp"
  local root_status
  local root_content_type
  local root_ready=0
  local response_kind="text"
  local static_path=""
  local public_path=""
  local compiled_release_path=""
  local compiled_checked=0

  echo "Running universal HTTP checks for route: $route_host"

  for _ in $(seq 1 80); do
    root_status=$(fetch_route_path "$route_host" "/" "$root_headers" "$root_body" || true)
    if [[ "$root_status" =~ ^[0-9]+$ ]] && (( root_status >= 200 && root_status < 400 )) && [[ -s "$root_body" ]]; then
      root_ready=1
      break
    fi
    sleep 0.5
  done
  if (( root_ready == 0 )); then
    echo "App root check failed for '/' (status=$root_status)" >&2
    [[ -f "$root_headers" ]] && cat "$root_headers" >&2 || true
    [[ -f "$root_body" ]] && cat "$root_body" >&2 || true
    exit 1
  fi

  root_content_type=$(tr -d '\r' < "$root_headers" | awk 'tolower($1) == "content-type:" {print tolower($2)}' | tail -n 1)
  if [[ "$root_content_type" == *"text/html"* ]] || grep -Eqi '<!doctype html|<html[[:space:]>]' "$root_body"; then
    response_kind="html"
  elif [[ "$root_content_type" == *"application/json"* ]] || jq -e . "$root_body" >/dev/null 2>&1; then
    response_kind="json"
  fi

  if [[ "$response_kind" == "html" ]] && ! grep -Eqi '<!doctype html|<html[[:space:]>]' "$root_body"; then
    echo "App root was classified as HTML but did not contain HTML markup." >&2
    exit 1
  fi
  if [[ "$response_kind" == "json" ]] && ! jq -e . "$root_body" >/dev/null 2>&1; then
    echo "App root was classified as JSON but body is not valid JSON." >&2
    exit 1
  fi

  echo "Root response kind: $response_kind"

  static_path=$(ssh_exec server-ubuntu "cd '$release_app_dir' && find static -type f 2>/dev/null | head -n 1 | sed 's#^#/#'" || true)
  static_path=$(echo "$static_path" | tr -d '\r' | head -n 1)
  if [[ -n "$static_path" ]]; then
    check_http_ok_optional "$route_host" "$static_path" "Static file"
  fi

  public_path=$(ssh_exec server-ubuntu "cd '$release_app_dir' && find public -type f 2>/dev/null | head -n 1 | sed 's#^public/#/#'" || true)
  public_path=$(echo "$public_path" | tr -d '\r' | head -n 1)
  if [[ -n "$public_path" ]]; then
    check_http_ok_optional "$route_host" "$public_path" "Public file"
  fi

  if [[ "$response_kind" == "html" ]]; then
    mapfile -t html_asset_paths < <(grep -Eo "/[^\"'[:space:]>]+\\.(js|mjs|css)(\\?[^\"'[:space:]>]+)?" "$root_body" | sort -u)
    if (( ${#html_asset_paths[@]} > 0 )); then
      for asset_path in "${html_asset_paths[@]}"; do
        check_http_ok_optional "$route_host" "$asset_path" "Compiled asset"
        compiled_checked=1
      done
    fi
  fi

  if (( compiled_checked == 0 )); then
    compiled_release_path=$(ssh_exec server-ubuntu "cd '$release_app_dir' && { find static -type f \\( -name '*.js' -o -name '*.mjs' -o -name '*.css' \\) 2>/dev/null; find assets -type f \\( -name '*.js' -o -name '*.mjs' -o -name '*.css' \\) 2>/dev/null; } | head -n 1 | sed 's#^#/#'" || true)
    compiled_release_path=$(echo "$compiled_release_path" | tr -d '\r' | head -n 1)
    if [[ -n "$compiled_release_path" ]]; then
      check_http_ok_optional "$route_host" "$compiled_release_path" "Compiled asset"
      compiled_checked=1
    fi
  fi

  if (( compiled_checked == 0 )); then
    echo "No compiled static asset candidates found; skipping compiled asset check."
  fi
}

start_tako_server() {
  local host=$1
  scp_to "$WORKSPACE/target/debug/tako-server" "$host" "/home/tako/tako-server"
  ssh_exec "$host" "chmod +x /home/tako/tako-server"
  ssh_exec "$host" "pkill -x tako-server >/dev/null 2>&1 || true"
  ssh_exec "$host" "rm -f /var/run/tako/tako.sock"
  ssh_exec "$host" "nohup /usr/local/bin/tako-server --no-acme --port 8080 --tls-port 8443 --data-dir /opt/tako >/tmp/tako-server.log 2>&1 &"
  wait_tako_socket "$host"
}

ssh_wait server-ubuntu
ssh_wait server-alma

echo "Building CLI and server binaries"
cd "$WORKSPACE"
cargo build -p tako --bin tako
cargo build -p tako-server --bin tako-server
start_tako_server server-ubuntu
start_tako_server server-alma

# Stage a minimal JS monorepo copy so Bun workspace/catalog references resolve
# like local dev, without rewriting dependency declarations.
jq --arg fixture_rel "$FIXTURE_REL" '
  .workspaces.packages = ["sdk", $fixture_rel]
' "$WORKSPACE/package.json" > "$JS_WORKSPACE_DIR/package.json"
cp -R "$WORKSPACE/sdk" "$JS_WORKSPACE_DIR/sdk"
mkdir -p "$(dirname "$PROJECT_DIR")"
cp -R "$FIXTURE_DIR" "$PROJECT_DIR"
rm -rf "$JS_WORKSPACE_DIR/sdk/node_modules" "$PROJECT_DIR/node_modules"

if [[ -f "$PROJECT_DIR/package.json" ]]; then
  if ! command -v bun >/dev/null 2>&1; then
    echo "bun is required in the e2e runner image to build JS fixtures" >&2
    exit 1
  fi

  (cd "$JS_WORKSPACE_DIR" && bun install)
  (cd "$JS_WORKSPACE_DIR/sdk" && bun run build)
  (cd "$PROJECT_DIR" && bun install)
fi

ARCH_RAW=$(uname -m)
TARGET_ARCH="x86_64"
if [[ "$ARCH_RAW" == "aarch64" || "$ARCH_RAW" == "arm64" ]]; then
  TARGET_ARCH="aarch64"
fi

cat > "$TAKO_HOME/config.toml" <<CFG
[[servers]]
name = "ssh"
host = "server-ubuntu"
port = 22

[[servers]]
name = "ssh2"
host = "server-alma"
port = 22

[server_targets.ssh]
arch = "$TARGET_ARCH"
libc = "gnu"

[server_targets.ssh2]
arch = "$TARGET_ARCH"
libc = "gnu"
CFG

HOME="$HOME_DIR" TAKO_HOME="$TAKO_HOME" "$WORKSPACE/target/debug/tako" deploy --env production --yes "$PROJECT_DIR"

CURRENT_LINK=$(resolve_current_release_link server-ubuntu || true)
APP_RELEASE_DIR="$CURRENT_LINK/$FIXTURE_REL"

if [[ -z "$CURRENT_LINK" ]]; then
  echo "Failed to resolve deployed release symlink under /opt/tako/apps/*/current" >&2
  exit 1
fi

if ! ssh_exec server-ubuntu "test -d '$APP_RELEASE_DIR'" >/dev/null 2>&1; then
  APP_RELEASE_DIR="$CURRENT_LINK"
fi

ROUTE_HOST=$(detect_route_host "$PROJECT_DIR/tako.toml" "production")
if [[ -z "$ROUTE_HOST" ]]; then
  echo "Could not resolve production route host from $PROJECT_DIR/tako.toml" >&2
  exit 1
fi

if ! ssh_exec server-ubuntu "test -f '$APP_RELEASE_DIR/app.json'" >/dev/null 2>&1; then
  echo "Missing app.json under deployed app directory: $APP_RELEASE_DIR" >&2
  exit 1
fi

run_universal_http_checks "$ROUTE_HOST" "$APP_RELEASE_DIR"

echo "E2E deploy test passed for $FIXTURE_REL"
