#!/bin/sh
set -eu

# Tako CLI canary installer (POSIX sh)
#
# Usage:
#   curl -fsSL https://tako.sh/install-canary.sh | sh
#
# What it does:
# - downloads the hosted CLI installer
# - forces canary artifact source
# - runs installer as-is
# - sets upgrade channel to canary

INSTALLER_URL="https://tako.sh/install.sh"
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

# Set upgrade channel to canary so `tako upgrade` stays on canary.
config_dir=""
if [ -n "${TAKO_HOME:-}" ]; then
  config_dir="$TAKO_HOME"
elif [ "$(uname -s)" = "Darwin" ]; then
  config_dir="$HOME/Library/Application Support/tako"
else
  config_dir="${XDG_CONFIG_HOME:-$HOME/.config}/tako"
fi
mkdir -p "$config_dir"
config_file="$config_dir/config.toml"
if [ -f "$config_file" ] && grep -q '^upgrade_channel' "$config_file"; then
  sed -i.bak 's/^upgrade_channel.*/upgrade_channel = "canary"/' "$config_file"
  rm -f "$config_file.bak"
else
  printf 'upgrade_channel = "canary"\n' >> "$config_file"
fi
