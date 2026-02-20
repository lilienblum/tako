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
#   TAKO_INSTALL_MISE       default: 1 (set 0/false to skip mise install)
#   TAKO_MISE_VERSION       optional mise version for installer (default: latest)
#   TAKO_MISE_BIN           default: /usr/local/bin/mise
#   TAKO_RESTART_SERVICE    default: 1 (set 0/false for install-only refresh; no service restart)

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
TAKO_INSTALL_MISE="${TAKO_INSTALL_MISE:-1}"
TAKO_MISE_VERSION="${TAKO_MISE_VERSION:-}"
TAKO_MISE_BIN="${TAKO_MISE_BIN:-/usr/local/bin/mise}"
TAKO_RESTART_SERVICE="${TAKO_RESTART_SERVICE:-1}"
PATH="/root/.local/bin:$PATH"

need_cmd() { command -v "$1" >/dev/null 2>&1; }

is_enabled() {
  case "${1:-}" in
    1|true|TRUE|yes|YES|on|ON)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

systemd_is_usable() {
  if ! need_cmd systemctl; then
    return 1
  fi

  # Containers can have systemctl installed without systemd as PID 1.
  if [ ! -d /run/systemd/system ]; then
    return 1
  fi

  if ! systemctl show-environment >/dev/null 2>&1; then
    return 1
  fi

  return 0
}

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

install_upgrade_helper() {
  cat > /usr/local/bin/tako-server-upgrade <<EOF
#!/bin/sh
set -eu

if [ "\$(id -u)" -ne 0 ]; then
  echo "error: run as root (use sudo)" >&2
  exit 1
fi

run_installer() {
  TAKO_USER='$TAKO_USER' \\
  TAKO_HOME='$TAKO_HOME' \\
  TAKO_SOCKET='$TAKO_SOCKET' \\
  TAKO_INSTALL_MISE='$TAKO_INSTALL_MISE' \\
  TAKO_MISE_VERSION='$TAKO_MISE_VERSION' \\
  TAKO_MISE_BIN='$TAKO_MISE_BIN' \\
  TAKO_DOWNLOAD_BASE_URL='$TAKO_DOWNLOAD_BASE_URL' \\
  TAKO_SERVER_URL='${TAKO_SERVER_URL:-}' \\
  TAKO_RESTART_SERVICE=0 \\
  sh
}

if command -v curl >/dev/null 2>&1; then
  curl -fsSL https://tako.sh/install-server | run_installer
elif command -v wget >/dev/null 2>&1; then
  wget -qO- https://tako.sh/install-server | run_installer
else
  echo "error: missing downloader (need curl or wget)" >&2
  exit 1
fi
EOF
  chmod 0755 /usr/local/bin/tako-server-upgrade
}

install_sudoers_rules() {
  if ! need_cmd sudo; then
    return
  fi

  systemctl_path="$(command -v systemctl || true)"
  journalctl_path="$(command -v journalctl || true)"

  if [ -n "$systemctl_path" ] && [ -n "$journalctl_path" ]; then
    cat > /etc/sudoers.d/tako <<EOF
$TAKO_USER ALL=(root) NOPASSWD: $systemctl_path reload tako-server, $systemctl_path restart tako-server, $systemctl_path is-active tako-server, $systemctl_path status tako-server, $journalctl_path -u tako-server *, /usr/local/bin/tako-server-upgrade
EOF
  else
    cat > /etc/sudoers.d/tako <<EOF
$TAKO_USER ALL=(root) NOPASSWD: /usr/local/bin/tako-server-upgrade
EOF
  fi

  chmod 440 /etc/sudoers.d/tako
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

install_sqlite_runtime() {
  if need_cmd apt-get; then
    apt-get update -y
    apt-get install -y libsqlite3-0
  elif need_cmd dnf; then
    dnf install -y sqlite-libs
  elif need_cmd yum; then
    yum install -y sqlite-libs
  elif need_cmd pacman; then
    pacman -Sy --noconfirm sqlite
  elif need_cmd apk; then
    apk add --no-cache sqlite-libs
  elif need_cmd zypper; then
    zypper --non-interactive install sqlite3
  else
    echo "warning: unsupported package manager; install libsqlite3 runtime manually if needed." >&2
  fi
}

install_mise_prerequisites() {
  if need_cmd apt-get; then
    apt-get update -y
    apt-get install -y bash git unzip gzip xz-utils
  elif need_cmd dnf; then
    dnf install -y bash git unzip gzip xz
  elif need_cmd yum; then
    yum install -y bash git unzip gzip xz
  elif need_cmd pacman; then
    pacman -Sy --noconfirm bash git unzip gzip xz
  elif need_cmd apk; then
    apk add --no-cache bash git unzip gzip xz
  elif need_cmd zypper; then
    zypper --non-interactive install bash git unzip gzip xz
  else
    echo "warning: unsupported package manager; mise prerequisites may be missing." >&2
  fi
}

try_install_mise_package_manager() {
  if need_cmd mise; then
    return 0
  fi

  if need_cmd apt-get; then
    apt-get update -y
    if apt-get install -y mise >/dev/null 2>&1; then
      return 0
    fi
  elif need_cmd dnf; then
    if dnf install -y mise >/dev/null 2>&1; then
      return 0
    fi
  elif need_cmd yum; then
    if yum install -y mise >/dev/null 2>&1; then
      return 0
    fi
  elif need_cmd pacman; then
    if pacman -Sy --noconfirm mise >/dev/null 2>&1; then
      return 0
    fi
  elif need_cmd apk; then
    if apk add --no-cache mise >/dev/null 2>&1; then
      return 0
    fi
  elif need_cmd zypper; then
    if zypper --non-interactive install mise >/dev/null 2>&1; then
      return 0
    fi
  fi

  need_cmd mise
}

install_mise_via_script() {
  install_mise_prerequisites

  installer_url="https://mise.run"
  installer="$(mktemp)"

  if need_cmd curl; then
    curl -fsSL "$installer_url" -o "$installer"
  else
    wget -qO "$installer" "$installer_url"
  fi

  chmod +x "$installer"
  if [ -n "$TAKO_MISE_VERSION" ]; then
    MISE_INSTALL_PATH="$TAKO_MISE_BIN" MISE_VERSION="$TAKO_MISE_VERSION" sh "$installer"
  else
    MISE_INSTALL_PATH="$TAKO_MISE_BIN" sh "$installer"
  fi
  rm -f "$installer"

  if [ -x "$TAKO_MISE_BIN" ]; then
    ln -sf "$TAKO_MISE_BIN" /usr/local/bin/mise
  elif [ -x "/root/.local/bin/mise" ]; then
    ln -sf "/root/.local/bin/mise" /usr/local/bin/mise
  fi
}

ensure_mise_toolchain() {
  if ! is_enabled "$TAKO_INSTALL_MISE"; then
    return
  fi

  if need_cmd mise; then
    echo "OK mise is already installed"
    return
  fi

  if try_install_mise_package_manager && need_cmd mise; then
    echo "OK installed mise via package manager"
    return
  fi

  echo "info: package manager install for mise is unavailable; falling back to upstream installer." >&2
  install_mise_via_script

  if ! need_cmd mise; then
    echo "error: failed to install mise. Install it manually (https://mise.jdx.dev/getting-started.html) and re-run installer." >&2
    exit 1
  fi
  echo "OK installed mise at $(command -v mise)"
}

detect_libc() {
  if need_cmd ldd; then
    ldd_out="$(ldd --version 2>&1 || true)"
    ldd_lower="$(printf "%s" "$ldd_out" | tr '[:upper:]' '[:lower:]')"
    if printf "%s" "$ldd_lower" | grep -q "musl"; then
      echo "musl"
      return
    fi
    if printf "%s" "$ldd_lower" | grep -Eq "glibc|gnu libc|gnu c library"; then
      echo "glibc"
      return
    fi
  fi

  if need_cmd getconf && getconf GNU_LIBC_VERSION >/dev/null 2>&1; then
    echo "glibc"
    return
  fi

  if ls /lib/ld-musl-*.so.1 /usr/lib/ld-musl-*.so.1 >/dev/null 2>&1; then
    echo "musl"
    return
  fi

  if ls /lib/*-linux-gnu/libc.so.6 /usr/lib/*-linux-gnu/libc.so.6 >/dev/null 2>&1; then
    echo "glibc"
    return
  fi

  echo "unknown"
}

ensure_nc() {
  nc_supports_unix_socket() {
    if ! need_cmd nc; then
      return 1
    fi

    # Preferred check: implementation advertises -U in help output.
    if nc -h 2>&1 | grep -Eq '(^|[[:space:][:punct:]])-U([[:space:][:punct:]]|$)'; then
      return 0
    fi

    # Fallback probe: detect option-parser errors for -U.
    nc_err="$(nc -U /var/run/tako/nonexistent.sock 2>&1 >/dev/null || true)"
    if printf "%s" "$nc_err" | grep -Eqi 'unrecognized option|illegal option|invalid option'; then
      return 1
    fi

    # If parser accepted -U, treat as supported even if connect failed.
    return 0
  }

  if nc_supports_unix_socket; then
    return
  fi

  if need_cmd nc; then
    echo "warning: installed netcat ('nc') does not support Unix sockets (-U); installing a compatible netcat implementation." >&2
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
    apk add --no-cache netcat-openbsd
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

  if ! nc_supports_unix_socket; then
    echo "error: netcat ('nc') does not support Unix sockets (-U)." >&2
    echo "Install a compatible implementation (for example: netcat-openbsd or nmap-ncat), then retry." >&2
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
ensure_mise_toolchain
ensure_nc
install_sqlite_runtime

arch="$(uname -m)"
case "$arch" in
  x86_64|amd64) arch="x86_64" ;;
  aarch64|arm64) arch="aarch64" ;;
  *)
    echo "error: unsupported architecture: $arch (supported: x86_64, aarch64)" >&2
    exit 1
    ;;
esac

libc="$(detect_libc)"
case "$libc" in
  glibc|musl) ;;
  *)
    echo "error: unsupported libc: $libc (supported: glibc, musl)" >&2
    exit 1
    ;;
esac

bin_url="${TAKO_SERVER_URL:-$TAKO_DOWNLOAD_BASE_URL/tako-server-linux-$arch-$libc}"
sha_url="${bin_url}.sha256"

tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

echo "Downloading tako-server: $bin_url"
case "$bin_url" in
  file://*)
    cp "${bin_url#file://}" "$tmp"
    ;;
  *)
    if need_cmd curl; then
      curl -fL "$bin_url" -o "$tmp"
    else
      wget -O "$tmp" "$bin_url"
    fi
    ;;
esac

expected_sha=""
case "$sha_url" in
  file://*)
    sha_file="${sha_url#file://}"
    if [ -f "$sha_file" ]; then
      expected_sha="$(awk '{print $1}' "$sha_file" 2>/dev/null || true)"
    fi
    ;;
  *)
    if need_cmd curl; then
      expected_sha="$(curl -fsSL "$sha_url" 2>/dev/null | awk '{print $1}' || true)"
    else
      expected_sha="$(wget -qO- "$sha_url" 2>/dev/null | awk '{print $1}' || true)"
    fi
    ;;
esac

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
install_upgrade_helper
install_sudoers_rules

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

if systemd_is_usable; then
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
KillMode=control-group
TimeoutStopSec=30min
RuntimeDirectory=tako
RuntimeDirectoryMode=0755

[Install]
WantedBy=multi-user.target
EOF

  systemctl daemon-reload
  if is_enabled "$TAKO_RESTART_SERVICE"; then
    systemctl enable --now tako-server
    systemctl --no-pager status tako-server || true
    if ! systemctl is-active --quiet tako-server; then
      echo "error: tako-server failed to start. Recent logs:" >&2
      journalctl -u tako-server --no-pager -n 60 >&2 || true
      exit 1
    fi
  else
    systemctl enable tako-server >/dev/null 2>&1 || true
    echo "OK install refreshed without restarting tako-server (TAKO_RESTART_SERVICE=0)"
  fi
else
  echo "warning: systemd not found; start tako-server manually:" >&2
  echo "  sudo -u $TAKO_USER /usr/local/bin/tako-server --socket $TAKO_SOCKET --data-dir $TAKO_HOME" >&2
  echo "  (if bind permission fails on :80/:443, install setcap/libcap or run as root)" >&2
  echo "  /usr/local/bin/tako-server --socket $TAKO_SOCKET --data-dir $TAKO_HOME" >&2
fi

echo "OK installed tako-server"
echo "OK configured user: $TAKO_USER"
