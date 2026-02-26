#!/bin/sh
set -eu

# Tako server canary installer (POSIX sh)
#
# Usage:
#   sudo sh -c "$(curl -fsSL https://tako.sh/server-install-canary)"
#
# What it does:
# - downloads the hosted server installer
# - forces canary artifact source
# - runs installer as-is

INSTALLER_URL="https://tako.sh/install-server"
CANARY_DOWNLOAD_BASE_URL="https://github.com/lilienblum/tako/releases/download/canary"

installer="$(mktemp)"
trap 'rm -f "$installer"' EXIT

if command -v curl >/dev/null 2>&1; then
  curl -fsSL "$INSTALLER_URL" -o "$installer"
elif command -v wget >/dev/null 2>&1; then
  wget -qO "$installer" "$INSTALLER_URL"
else
  echo "error: missing required downloader (curl or wget)" >&2
  exit 1
fi

TAKO_DOWNLOAD_BASE_URL="$CANARY_DOWNLOAD_BASE_URL" sh "$installer"
