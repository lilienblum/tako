#!/bin/sh
set -eu

# Tako installer (POSIX sh)
#
# Usage:
#   curl -fsSL https://tako.sh/install-server | sh
#
# What it does:
# - downloads and installs `tako-server`
# - creates OS user `tako`
# - configures a systemd service (when available)
#
# Optional env vars:
#   TAKO_USER               default: tako
#   TAKO_HOME               default: /opt/tako
#   TAKO_SOCKET             default: /var/run/tako/tako.sock
#   TAKO_SSH_PUBKEY         public key line to authorize for TAKO_USER (optional)
#                           if unset, installer prompts in interactive terminals
#
#   TAKO_SERVER_URL         override binary URL (optional)
#   TAKO_DOWNLOAD_BASE_URL  default: https://github.com/lilienblum/tako/releases/latest/download

if [ "$(id -u)" -ne 0 ]; then
  echo "error: run as root (use sudo)" >&2
  exit 1
fi

if [ "$(uname -s)" != "Linux" ]; then
  echo "error: this installer supports Linux only" >&2
  exit 1
fi

TAKO_USER="${TAKO_USER:-tako}"
TAKO_HOME="${TAKO_HOME:-/opt/tako}"
TAKO_SOCKET="${TAKO_SOCKET:-/var/run/tako/tako.sock}"
TAKO_DOWNLOAD_BASE_URL="${TAKO_DOWNLOAD_BASE_URL:-https://github.com/lilienblum/tako/releases/latest/download}"

need_cmd() { command -v "$1" >/dev/null 2>&1; }

ensure_privileged_bind_capability() {
  if need_cmd setcap; then
    if setcap cap_net_bind_service=+ep /usr/local/bin/tako-server; then
      echo "OK granted CAP_NET_BIND_SERVICE to /usr/local/bin/tako-server"
      return
    fi
    echo "warning: failed to grant CAP_NET_BIND_SERVICE via setcap; systemd service will still use AmbientCapabilities." >&2
    return
  fi

  echo "warning: setcap not found; non-systemd/manual runs on :80/:443 may require root." >&2
}

maybe_prompt_ssh_pubkey() {
  if [ -n "${TAKO_SSH_PUBKEY:-}" ]; then
    return
  fi

  echo "SSH setup:"
  echo "  To allow SSH login as '$TAKO_USER', paste your public key."
  echo "  Get one from your local machine with: cat ~/.ssh/id_ed25519.pub"
  echo "  If needed, create one with: ssh-keygen -t ed25519"

  if [ -t 0 ] && [ -t 1 ]; then
    printf "Public key for '$TAKO_USER' (press Enter to skip): "
    IFS= read -r TAKO_SSH_PUBKEY || true
  else
    echo "warning: non-interactive install; skipping SSH key prompt." >&2
    echo "warning: re-run with TAKO_SSH_PUBKEY='ssh-ed25519 ...' to install a key." >&2
  fi
}

install_pkgs() {
  # Avoid arrays for POSIX sh compatibility.
  if need_cmd apt-get; then
    apt-get update -y
    apt-get install -y "$@"
  elif need_cmd dnf; then
    dnf install -y "$@"
  elif need_cmd yum; then
    yum install -y "$@"
  elif need_cmd pacman; then
    pacman -Sy --noconfirm "$@"
  elif need_cmd apk; then
    apk add --no-cache "$@"
  elif need_cmd zypper; then
    zypper --non-interactive install "$@"
  else
    echo "error: unsupported package manager; install curl + ca-certificates + tar manually" >&2
    exit 1
  fi
}

ensure_nc() {
  if need_cmd nc; then
    return
  fi

  if need_cmd apt-get; then
    apt-get update -y
    apt-get install -y netcat-openbsd || apt-get install -y netcat-traditional
  elif need_cmd dnf; then
    dnf install -y nmap-ncat || dnf install -y nc
  elif need_cmd yum; then
    yum install -y nmap-ncat || yum install -y nc
  elif need_cmd pacman; then
    pacman -Sy --noconfirm openbsd-netcat || pacman -Sy --noconfirm gnu-netcat
  elif need_cmd apk; then
    apk add --no-cache netcat-openbsd || apk add --no-cache busybox-extras
  elif need_cmd zypper; then
    zypper --non-interactive install netcat-openbsd || zypper --non-interactive install netcat
  else
    echo "error: unsupported package manager; install netcat ('nc') manually" >&2
    exit 1
  fi

  if ! need_cmd nc; then
    echo "error: netcat ('nc') not found after install. Install it manually and retry." >&2
    exit 1
  fi
}

if ! need_cmd curl && ! need_cmd wget; then
  install_pkgs curl
fi
if ! need_cmd tar; then
  install_pkgs tar
fi
if ! need_cmd sha256sum && ! need_cmd shasum; then
  install_pkgs coreutils
fi
if ! need_cmd sudo; then
  install_pkgs sudo
fi
ensure_nc

arch="$(uname -m)"
case "$arch" in
  x86_64|amd64) arch="x86_64" ;;
  aarch64|arm64) arch="aarch64" ;;
  *)
    echo "error: unsupported architecture: $arch (supported: x86_64, aarch64)" >&2
    exit 1
    ;;
esac

bin_url="${TAKO_SERVER_URL:-$TAKO_DOWNLOAD_BASE_URL/tako-server-linux-$arch}"
sha_url="${bin_url}.sha256"

tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

echo "Downloading tako-server: $bin_url"
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
  else
    actual="$(shasum -a 256 "$tmp" | awk '{print $1}')"
  fi
  if [ "$actual" != "$expected_sha" ]; then
    echo "error: sha256 mismatch (expected=$expected_sha actual=$actual)" >&2
    exit 1
  fi
else
  echo "warning: could not fetch SHA256 ($sha_url); skipping integrity check" >&2
fi

install -m 0755 "$tmp" /usr/local/bin/tako-server
ensure_privileged_bind_capability

# Create `tako` user.
if ! id -u "$TAKO_USER" >/dev/null 2>&1; then
  if need_cmd useradd; then
    groupadd --system "$TAKO_USER" 2>/dev/null || true
    useradd --system --create-home --home-dir "/home/$TAKO_USER" --shell /bin/bash --gid "$TAKO_USER" "$TAKO_USER" 2>/dev/null || \
      useradd --system --create-home --home-dir "/home/$TAKO_USER" --shell /bin/sh --gid "$TAKO_USER" "$TAKO_USER"
  elif need_cmd adduser; then
    addgroup -S "$TAKO_USER" 2>/dev/null || true
    adduser -S -D -h "/home/$TAKO_USER" -s /bin/sh -G "$TAKO_USER" "$TAKO_USER"
  else
    echo "error: missing useradd/adduser" >&2
    exit 1
  fi
fi

mkdir -p "$TAKO_HOME" "$(dirname "$TAKO_SOCKET")"
chown -R "$TAKO_USER":"$TAKO_USER" "$TAKO_HOME" "$(dirname "$TAKO_SOCKET")" 2>/dev/null || true

maybe_prompt_ssh_pubkey

# Install authorized_keys for SSH (optional).
if [ -n "${TAKO_SSH_PUBKEY:-}" ]; then
  home_dir=""
  if need_cmd getent; then
    home_dir="$(getent passwd "$TAKO_USER" 2>/dev/null | awk -F: '{print $6}' || true)"
  fi
  if [ -z "$home_dir" ]; then
    home_dir="$(awk -F: -v u="$TAKO_USER" '$1==u {print $6}' /etc/passwd 2>/dev/null || true)"
  fi
  if [ -z "$home_dir" ]; then
    home_dir="/home/$TAKO_USER"
  fi

  mkdir -p "$home_dir/.ssh"
  chmod 700 "$home_dir/.ssh"
  printf '%s\n' "$TAKO_SSH_PUBKEY" > "$home_dir/.ssh/authorized_keys"
  chmod 600 "$home_dir/.ssh/authorized_keys"
  chown -R "$TAKO_USER":"$TAKO_USER" "$home_dir/.ssh" 2>/dev/null || true
else
  echo "warning: no SSH key installed for '$TAKO_USER'." >&2
  echo "warning: configure ~/.ssh/authorized_keys manually or rerun installer with TAKO_SSH_PUBKEY." >&2
fi

if need_cmd systemctl; then
  systemctl_path="$(command -v systemctl || true)"
  journalctl_path="$(command -v journalctl || true)"
  if [ -n "$systemctl_path" ] && [ -n "$journalctl_path" ]; then
    cat > /etc/sudoers.d/tako <<EOF
$TAKO_USER ALL=(root) NOPASSWD: $systemctl_path reload tako-server, $systemctl_path restart tako-server, $systemctl_path is-active tako-server, $systemctl_path status tako-server, $journalctl_path -u tako-server *
EOF
    chmod 440 /etc/sudoers.d/tako
  fi

  cat > /etc/systemd/system/tako-server.service <<EOF
[Unit]
Description=Tako Server
After=network.target

[Service]
Type=simple
User=$TAKO_USER
Group=$TAKO_USER
NoNewPrivileges=true
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE
ExecStart=/usr/local/bin/tako-server --socket $TAKO_SOCKET --data-dir $TAKO_HOME
Restart=always
RestartSec=1
RuntimeDirectory=tako
RuntimeDirectoryMode=0755

[Install]
WantedBy=multi-user.target
EOF

  systemctl daemon-reload
  systemctl enable --now tako-server
  systemctl --no-pager status tako-server || true
  if ! systemctl is-active --quiet tako-server; then
    echo "error: tako-server failed to start. Recent logs:" >&2
    journalctl -u tako-server --no-pager -n 60 >&2 || true
    exit 1
  fi
else
  echo "warning: systemd not found; start tako-server manually:" >&2
  echo "  sudo -u $TAKO_USER /usr/local/bin/tako-server --socket $TAKO_SOCKET --data-dir $TAKO_HOME" >&2
  echo "  (if bind permission fails on :80/:443, install setcap/libcap or run as root)" >&2
  echo "  /usr/local/bin/tako-server --socket $TAKO_SOCKET --data-dir $TAKO_HOME" >&2
fi

echo "OK installed tako-server"
echo "OK configured user: $TAKO_USER"
