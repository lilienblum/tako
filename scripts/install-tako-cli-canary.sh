#!/bin/sh
set -eu

# Tako CLI canary installer (POSIX sh)
#
# Usage:
#   curl -fsSL https://tako.sh/install-canary | sh
#
# What it does:
# - downloads the hosted CLI installer
# - forces canary artifact source
# - runs installer as-is

INSTALLER_URL="https://tako.sh/install"
CANARY_DOWNLOAD_BASE_URL="https://github.com/lilienblum/tako/releases/download/canary-latest"

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
