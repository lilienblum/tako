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
#   TAKO_URL                override binary URL (optional)
#   TAKO_DOWNLOAD_BASE_URL  default: https://github.com/lilienblum/tako/releases/latest/download

need_cmd() { command -v "$1" >/dev/null 2>&1; }

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

TAKO_INSTALL_DIR="${TAKO_INSTALL_DIR:-$HOME/.local/bin}"
TAKO_DOWNLOAD_BASE_URL="${TAKO_DOWNLOAD_BASE_URL:-https://github.com/lilienblum/tako/releases/latest/download}"

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

bin_url="${TAKO_URL:-$TAKO_DOWNLOAD_BASE_URL/tako-$os-$arch}"
sha_url="${bin_url}.sha256"

tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

echo "Downloading tako CLI: $bin_url"
if need_cmd curl; then
  curl -fL "$bin_url" -o "$tmp"
else
  wget -O "$tmp" "$bin_url"
fi

expected_sha=""
if need_cmd curl; then
  expected_sha="$(curl -fsSL "$sha_url" 2>/dev/null | awk '{print $1}' || true)"
else
  expected_sha="$(wget -qO- "$sha_url" 2>/dev/null | awk '{print $1}' || true)"
fi

if [ -n "$expected_sha" ]; then
  if need_cmd sha256sum; then
    actual="$(sha256sum "$tmp" | awk '{print $1}')"
  elif need_cmd shasum; then
    actual="$(shasum -a 256 "$tmp" | awk '{print $1}')"
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

mkdir -p "$TAKO_INSTALL_DIR"
target="$TAKO_INSTALL_DIR/tako"
install -m 0755 "$tmp" "$target"

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
