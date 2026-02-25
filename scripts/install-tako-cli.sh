#!/bin/sh
set -eu

# Tako CLI installer (POSIX sh)
#
# Usage:
#   curl -fsSL https://tako.sh/install | sh
#
# What it does:
# - downloads and installs `tako` CLI for your OS/architecture
# - installs to ~/.local/bin/tako by default
#
# Optional env vars:
#   TAKO_INSTALL_DIR        default: $HOME/.local/bin
#   TAKO_URL                override archive URL (.tar.gz; optional)
#   TAKO_DOWNLOAD_BASE_URL  override release download base URL (optional)
#   TAKO_REPO_OWNER         default: lilienblum
#   TAKO_REPO_NAME          default: tako
#   TAKO_TAG_PREFIX         default: tako-v
#   TAKO_TAGS_API           override tags API URL (optional)

need_cmd() { command -v "$1" >/dev/null 2>&1; }

download_file() {
  src="$1"
  dest="$2"
  if need_cmd curl; then
    curl -fsSL "$src" -o "$dest"
  else
    wget -qO "$dest" "$src"
  fi
}

download_stdout() {
  url="$1"
  if need_cmd curl; then
    curl -fsSL "$url"
  else
    wget -qO- "$url"
  fi
}

resolve_latest_tag() {
  prefix="$1"
  tags_api="$2"
  tags_json="$(download_stdout "$tags_api" 2>/dev/null || true)"
  if [ -z "$tags_json" ]; then
    return 1
  fi
  tag="$(printf '%s' "$tags_json" | grep -o "\"name\": \"${prefix}[^\"]*\"" | head -n 1 | sed -E 's/"name": "([^"]+)"/\1/' || true)"
  if [ -z "$tag" ]; then
    return 1
  fi
  printf '%s\n' "$tag"
}

if [ -z "${HOME:-}" ]; then
  echo "error: HOME is not set" >&2
  exit 1
fi

if ! need_cmd install; then
  echo "error: missing required command: install" >&2
  exit 1
fi

if ! need_cmd curl && ! need_cmd wget; then
  echo "error: missing downloader (need curl or wget)" >&2
  exit 1
fi
if ! need_cmd tar; then
  echo "error: missing required command: tar" >&2
  exit 1
fi

TAKO_INSTALL_DIR="${TAKO_INSTALL_DIR:-$HOME/.local/bin}"
TAKO_DOWNLOAD_BASE_URL="${TAKO_DOWNLOAD_BASE_URL:-}"
TAKO_REPO_OWNER="${TAKO_REPO_OWNER:-lilienblum}"
TAKO_REPO_NAME="${TAKO_REPO_NAME:-tako}"
TAKO_TAG_PREFIX="${TAKO_TAG_PREFIX:-tako-v}"
TAKO_TAGS_API="${TAKO_TAGS_API:-https://api.github.com/repos/$TAKO_REPO_OWNER/$TAKO_REPO_NAME/tags?per_page=100}"

os_raw="$(uname -s)"
case "$os_raw" in
  Linux) os="linux" ;;
  Darwin) os="darwin" ;;
  *)
    echo "error: unsupported OS: $os_raw (supported: Linux, Darwin)" >&2
    exit 1
    ;;
esac

arch_raw="$(uname -m)"
case "$arch_raw" in
  x86_64|amd64) arch="x86_64" ;;
  aarch64|arm64) arch="aarch64" ;;
  *)
    echo "error: unsupported architecture: $arch_raw (supported: x86_64, aarch64)" >&2
    exit 1
    ;;
esac

download_url="${TAKO_URL:-}"
if [ -z "$download_url" ]; then
  download_base="$TAKO_DOWNLOAD_BASE_URL"
  if [ -z "$download_base" ]; then
    tag="$(resolve_latest_tag "$TAKO_TAG_PREFIX" "$TAKO_TAGS_API" || true)"
    if [ -z "$tag" ]; then
      echo "error: could not resolve latest tag for prefix '$TAKO_TAG_PREFIX'" >&2
      exit 1
    fi
    download_base="https://github.com/$TAKO_REPO_OWNER/$TAKO_REPO_NAME/releases/download/$tag"
  fi
  download_url="$download_base/tako-$os-$arch.tar.gz"
fi
case "$download_url" in
  *.tar.gz|file://*.tar.gz) ;;
  *)
    echo "error: TAKO_URL must point to a .tar.gz archive" >&2
    exit 1
    ;;
esac
sha_url="${download_url}.sha256"

tmp_payload="$(mktemp)"
tmp_extract="$(mktemp -d)"
trap 'rm -f "$tmp_payload"; rm -rf "$tmp_extract"' EXIT

echo "Downloading tako CLI: $download_url"
download_file "$download_url" "$tmp_payload"

expected_sha=""
expected_sha="$(download_stdout "$sha_url" 2>/dev/null | awk '{print $1}' || true)"

if [ -n "$expected_sha" ]; then
  if need_cmd sha256sum; then
    actual="$(sha256sum "$tmp_payload" | awk '{print $1}')"
  elif need_cmd shasum; then
    actual="$(shasum -a 256 "$tmp_payload" | awk '{print $1}')"
  else
    echo "warning: sha256 tool not found; skipping integrity check" >&2
    actual=""
  fi

  if [ -n "$actual" ] && [ "$actual" != "$expected_sha" ]; then
    echo "error: sha256 mismatch (expected=$expected_sha actual=$actual)" >&2
    exit 1
  fi
else
  echo "warning: could not fetch SHA256 ($sha_url); skipping integrity check" >&2
fi

tar -xzf "$tmp_payload" -C "$tmp_extract"
tmp_bin="$(find "$tmp_extract" -type f -name tako | head -n 1 || true)"
if [ -z "$tmp_bin" ]; then
  echo "error: archive did not contain a tako binary" >&2
  exit 1
fi

mkdir -p "$TAKO_INSTALL_DIR"
target="$TAKO_INSTALL_DIR/tako"
install -m 0755 "$tmp_bin" "$target"

echo "OK installed tako to $target"

case ":$PATH:" in
  *":$TAKO_INSTALL_DIR:"*)
    echo "OK '$TAKO_INSTALL_DIR' is on PATH"
    ;;
  *)
    echo "warning: '$TAKO_INSTALL_DIR' is not on PATH." >&2
    echo "warning: add it to your shell profile and restart your shell." >&2
    ;;
esac

echo "Run: tako --version"
