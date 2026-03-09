#!/bin/sh
set -eu

# Tako installer (POSIX sh)
#
# Usage:
#   sudo sh -c "$(curl -fsSL https://tako.sh/install-server)"
#
# What it does:
# - downloads and installs `tako-server`
# - creates OS user `tako`
# - configures a service manager (systemd or OpenRC) for `tako-server`
# - installs maintenance helpers and sudoers for the tako service user
#
# Optional env vars:
#   TAKO_USER               default: tako
#   TAKO_HOME               default: /opt/tako
#   TAKO_SOCKET             default: /var/run/tako/tako.sock
#   TAKO_SSH_PUBKEY         public key line to authorize for TAKO_USER (optional)
#                           if unset, installer prompts in interactive terminals
#
#   TAKO_SERVER_URL         override archive URL (.tar.gz; optional)
#   TAKO_DOWNLOAD_BASE_URL  override release download base URL (optional)
#   TAKO_REPO_OWNER         default: lilienblum
#   TAKO_REPO_NAME          default: tako
#   TAKO_TAG_PREFIX         default: tako-server-v
#   TAKO_TAGS_API           override tags API URL (optional)
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
TAKO_DOWNLOAD_BASE_URL="${TAKO_DOWNLOAD_BASE_URL:-}"
TAKO_REPO_OWNER="${TAKO_REPO_OWNER:-lilienblum}"
TAKO_REPO_NAME="${TAKO_REPO_NAME:-tako}"
TAKO_TAG_PREFIX="${TAKO_TAG_PREFIX:-tako-server-v}"
TAKO_TAGS_API="${TAKO_TAGS_API:-https://api.github.com/repos/$TAKO_REPO_OWNER/$TAKO_REPO_NAME/tags?per_page=100}"
TAKO_INSTALL_MISE="${TAKO_INSTALL_MISE:-1}"
TAKO_MISE_VERSION="${TAKO_MISE_VERSION:-}"
TAKO_MISE_BIN="${TAKO_MISE_BIN:-/usr/local/bin/mise}"
TAKO_RESTART_SERVICE="${TAKO_RESTART_SERVICE:-1}"
TAKO_SERVER_INSTALL_REFRESH_HELPER="/usr/local/bin/tako-server-install-refresh"
TAKO_SERVER_SERVICE_HELPER="/usr/local/bin/tako-server-service"
PATH="/root/.local/bin:$PATH"

need_cmd() { command -v "$1" >/dev/null 2>&1; }

download_file() {
  src="$1"
  dest="$2"
  case "$src" in
    file://*)
      cp "${src#file://}" "$dest"
      ;;
    *)
      if need_cmd curl; then
        curl -fsSL "$src" -o "$dest"
      else
        wget -qO "$dest" "$src"
      fi
      ;;
  esac
}

download_stdout() {
  url="$1"
  case "$url" in
    file://*)
      cat "${url#file://}"
      ;;
    *)
      if need_cmd curl; then
        curl -fsSL "$url"
      else
        wget -qO- "$url"
      fi
      ;;
  esac
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

openrc_is_usable() {
  if ! need_cmd rc-service; then
    return 1
  fi

  if ! need_cmd rc-update; then
    return 1
  fi

  # OpenRC creates this runtime directory when it is the active init system.
  if [ ! -d /run/openrc ]; then
    return 1
  fi

  return 0
}

detect_service_manager() {
  if systemd_is_usable; then
    echo "systemd"
    return
  fi

  if openrc_is_usable; then
    echo "openrc"
    return
  fi

  echo "none"
}

SERVICE_MANAGER="$(detect_service_manager)"

install_upgrade_helpers() {
  cat > "$TAKO_SERVER_INSTALL_REFRESH_HELPER" <<'EOF'
#!/bin/sh
set -eu

channel="${1:-stable}"
case "$channel" in
  stable)
    download_base=""
    ;;
  canary)
    download_base="https://github.com/lilienblum/tako/releases/download/canary-latest"
    ;;
  *)
    echo "error: expected channel 'stable' or 'canary'" >&2
    exit 1
    ;;
esac

installer_url="https://tako.sh/install-server"
installer="$(mktemp)"
trap 'rm -f "$installer"' EXIT

if command -v curl >/dev/null 2>&1; then
  curl -fsSL "$installer_url" -o "$installer"
elif command -v wget >/dev/null 2>&1; then
  wget -qO "$installer" "$installer_url"
else
  echo "error: missing required downloader (curl or wget)" >&2
  exit 1
fi

if [ -n "$download_base" ]; then
  TAKO_DOWNLOAD_BASE_URL="$download_base" TAKO_RESTART_SERVICE=0 sh "$installer"
else
  TAKO_RESTART_SERVICE=0 sh "$installer"
fi
EOF
  chmod 0755 "$TAKO_SERVER_INSTALL_REFRESH_HELPER"

  cat > "$TAKO_SERVER_SERVICE_HELPER" <<'EOF'
#!/bin/sh
set -eu

action="${1:-}"
case "$action" in
  reload|restart)
    ;;
  *)
    echo "error: expected action 'reload' or 'restart'" >&2
    exit 1
    ;;
esac

if command -v systemctl >/dev/null 2>&1; then
  systemctl "$action" tako-server
elif command -v rc-service >/dev/null 2>&1; then
  rc-service tako-server "$action"
else
  echo "error: no supported service manager found (systemctl or rc-service)" >&2
  exit 1
fi
EOF
  chmod 0755 "$TAKO_SERVER_SERVICE_HELPER"

  cat > /etc/sudoers.d/tako <<EOF
# Managed by Tako install-server.
# The tako user is a no-login service account (only accessible via SSH key).
# It needs root for upgrades (binary install + service reload) and server
# administration tasks (DNS setup, systemd drop-ins). Commands are invoked
# via sudo sh -c '...' so the rule must not be restricted to specific binaries.
$TAKO_USER ALL=(root) NOPASSWD: ALL
EOF
  chmod 0440 /etc/sudoers.d/tako

  if need_cmd visudo; then
    if ! visudo -cf /etc/sudoers.d/tako >/dev/null 2>&1; then
      echo "error: generated sudoers policy is invalid (/etc/sudoers.d/tako)" >&2
      exit 1
    fi
  fi
}

ensure_privileged_bind_capability() {
  if need_cmd setcap; then
    if setcap cap_net_bind_service=+ep /usr/local/bin/tako-server; then
      echo "OK granted CAP_NET_BIND_SERVICE to /usr/local/bin/tako-server"
      return
    fi
    if [ "$SERVICE_MANAGER" = "systemd" ]; then
      echo "warning: failed to grant CAP_NET_BIND_SERVICE via setcap; systemd service will still use AmbientCapabilities." >&2
      return
    fi
    echo "warning: failed to grant CAP_NET_BIND_SERVICE via setcap; non-root :80/:443 binds may fail." >&2
    return
  fi

  if [ "$SERVICE_MANAGER" = "systemd" ]; then
    echo "warning: setcap not found; systemd service still sets bind capability via AmbientCapabilities." >&2
    return
  fi
  echo "warning: setcap not found; non-root :80/:443 binds may fail." >&2
}

if is_enabled "$TAKO_RESTART_SERVICE" && [ "$SERVICE_MANAGER" = "none" ]; then
  echo "error: a usable service manager is required for tako-server (systemd or OpenRC)" >&2
  exit 1
fi


maybe_prompt_ssh_pubkey() {
  is_valid_ssh_public_key() {
    key_line="$1"
    key_type="$(printf '%s\n' "$key_line" | awk '{print $1}')"
    key_blob="$(printf '%s\n' "$key_line" | awk '{print $2}')"

    if [ -z "$key_type" ] || [ -z "$key_blob" ]; then
      return 1
    fi

    case "$key_type" in
      ssh-ed25519|ssh-rsa|ssh-dss|ecdsa-sha2-nistp256|ecdsa-sha2-nistp384|ecdsa-sha2-nistp521|sk-ssh-ed25519@openssh.com|sk-ecdsa-sha2-nistp256@openssh.com)
        ;;
      *)
        return 1
        ;;
    esac

    printf '%s\n' "$key_blob" | grep -Eq '^[A-Za-z0-9+/=]+$'
  }

  first_valid_authorized_key() {
    auth_file="$1"
    if [ ! -r "$auth_file" ]; then
      return 1
    fi
    awk '
      /^[[:space:]]*#/ { next }
      NF < 2 { next }
      $1 ~ /^(ssh-ed25519|ssh-rsa|ssh-dss|ecdsa-sha2-nistp256|ecdsa-sha2-nistp384|ecdsa-sha2-nistp521|sk-ssh-ed25519@openssh.com|sk-ecdsa-sha2-nistp256@openssh.com)$/ && $2 ~ /^[A-Za-z0-9+\/=]+$/ { print $1 " " $2; exit }
    ' "$auth_file"
  }

  maybe_use_invoking_user_key() {
    invoking_user="${SUDO_USER:-}"
    if [ -z "$invoking_user" ] || [ "$invoking_user" = "root" ]; then
      return 1
    fi

    invoking_home=""
    if need_cmd getent; then
      invoking_home="$(getent passwd "$invoking_user" 2>/dev/null | awk -F: '{print $6}' || true)"
    fi
    if [ -z "$invoking_home" ]; then
      invoking_home="$(awk -F: -v u="$invoking_user" '$1==u {print $6}' /etc/passwd 2>/dev/null || true)"
    fi
    if [ -z "$invoking_home" ]; then
      return 1
    fi

    fallback_key="$(first_valid_authorized_key "$invoking_home/.ssh/authorized_keys" || true)"
    if ! is_valid_ssh_public_key "$fallback_key"; then
      return 1
    fi

    TAKO_SSH_PUBKEY="$fallback_key"
    echo "OK using SSH key from '$invoking_user' authorized_keys for '$TAKO_USER'"
    return 0
  }

  if [ -n "${TAKO_SSH_PUBKEY:-}" ]; then
    if ! is_valid_ssh_public_key "$TAKO_SSH_PUBKEY"; then
      echo "error: TAKO_SSH_PUBKEY must be a single SSH public key line (for example: ssh-ed25519 AAAA...)." >&2
      exit 1
    fi
    return
  fi

  echo "SSH setup:"
  echo "  To allow SSH login as '$TAKO_USER', paste your public key."
  echo "  Get one from your local machine with: cat ~/.ssh/id_ed25519.pub"
  echo "  If needed, create one with: ssh-keygen -t ed25519"

  if [ -t 0 ] && [ -t 1 ]; then
    while :; do
      printf "Public key for '$TAKO_USER': "
      if ! IFS= read -r TAKO_SSH_PUBKEY; then
        if ! maybe_use_invoking_user_key; then
          echo "warning: could not read SSH key input; skipping SSH key setup." >&2
          echo "warning: re-run with TAKO_SSH_PUBKEY='ssh-ed25519 ...' to install a key." >&2
          TAKO_SSH_PUBKEY=""
        fi
        break
      fi
      if is_valid_ssh_public_key "$TAKO_SSH_PUBKEY"; then
        break
      fi
      echo "warning: invalid SSH public key format. Paste the full key line (for example: ssh-ed25519 AAAA...)." >&2
    done
  elif [ -r /dev/tty ] && [ -w /dev/tty ] && (printf '' > /dev/tty) 2>/dev/null; then
    # Support common piped installs (curl ... | sudo sh) by prompting on the controlling tty.
    while :; do
      printf "Public key for '$TAKO_USER': " > /dev/tty
      if ! IFS= read -r TAKO_SSH_PUBKEY < /dev/tty; then
        if ! maybe_use_invoking_user_key; then
          echo "warning: could not read SSH key input from terminal; skipping SSH key setup." > /dev/tty
          echo "warning: re-run with TAKO_SSH_PUBKEY='ssh-ed25519 ...' to install a key." > /dev/tty
          TAKO_SSH_PUBKEY=""
        fi
        break
      fi
      if is_valid_ssh_public_key "$TAKO_SSH_PUBKEY"; then
        break
      fi
      echo "warning: invalid SSH public key format. Paste the full key line (for example: ssh-ed25519 AAAA...)." > /dev/tty
    done
  else
    if ! maybe_use_invoking_user_key; then
      echo "warning: non-interactive install; skipping SSH key prompt." >&2
      echo "warning: re-run with TAKO_SSH_PUBKEY='ssh-ed25519 ...' to install a key." >&2
    fi
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

  if [ -x "$TAKO_MISE_BIN" ] && [ "$TAKO_MISE_BIN" != "/usr/local/bin/mise" ]; then
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

download_url="${TAKO_SERVER_URL:-}"
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
  download_url="$download_base/tako-server-linux-$arch-$libc.tar.gz"
fi
case "$download_url" in
  *.tar.gz|file://*.tar.gz) ;;
  *)
    echo "error: TAKO_SERVER_URL must point to a .tar.gz archive" >&2
    exit 1
    ;;
esac
sha_url="${download_url}.sha256"

tmp_payload="$(mktemp)"
tmp_extract="$(mktemp -d)"
trap 'rm -f "$tmp_payload"; rm -rf "$tmp_extract"' EXIT

echo "Downloading tako-server: $download_url"
download_file "$download_url" "$tmp_payload"

expected_sha=""
expected_sha="$(download_stdout "$sha_url" 2>/dev/null | awk '{print $1}' || true)"

if [ -n "$expected_sha" ]; then
  if need_cmd sha256sum; then
    actual="$(sha256sum "$tmp_payload" | awk '{print $1}')"
  else
    actual="$(shasum -a 256 "$tmp_payload" | awk '{print $1}')"
  fi
  if [ "$actual" != "$expected_sha" ]; then
    echo "error: sha256 mismatch (expected=$expected_sha actual=$actual)" >&2
    exit 1
  fi
else
  echo "error: could not fetch SHA256 ($sha_url); aborting install" >&2
  exit 1
fi

tar -xzf "$tmp_payload" -C "$tmp_extract"
tmp_bin="$(find "$tmp_extract" -type f -name tako-server | head -n 1 || true)"
if [ -z "$tmp_bin" ]; then
  echo "error: archive did not contain a tako-server binary" >&2
  exit 1
fi

install -m 0755 "$tmp_bin" /usr/local/bin/tako-server
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

# Create optional `tako-app` user (used by privileged process-separation setups).
if ! id -u "tako-app" >/dev/null 2>&1; then
  if need_cmd useradd; then
    useradd --system --no-create-home --shell /usr/sbin/nologin --gid "$TAKO_USER" "tako-app" 2>/dev/null || \
      useradd --system --no-create-home --shell /sbin/nologin --gid "$TAKO_USER" "tako-app"
  elif need_cmd adduser; then
    adduser -S -D -H -s /sbin/nologin -G "$TAKO_USER" "tako-app"
  fi
fi

# Remove deprecated helper path if present.
rm -f /usr/local/bin/tako-server-upgrade
install_upgrade_helpers

mkdir -p "$TAKO_HOME" "$(dirname "$TAKO_SOCKET")"
chown -R "$TAKO_USER":"$TAKO_USER" "$TAKO_HOME" "$(dirname "$TAKO_SOCKET")" 2>/dev/null || true
chmod 0700 "$TAKO_HOME"
chmod 0700 "$(dirname "$TAKO_SOCKET")"

# App socket directory writable by both tako and tako-app (group-shared).
mkdir -p /var/run/tako-app
chown "$TAKO_USER":"$TAKO_USER" /var/run/tako-app
chmod 0770 /var/run/tako-app

maybe_prompt_ssh_pubkey

# Install authorized_keys for SSH (optional).
tako_home_dir() {
  _home=""
  if need_cmd getent; then
    _home="$(getent passwd "$TAKO_USER" 2>/dev/null | awk -F: '{print $6}' || true)"
  fi
  if [ -z "$_home" ]; then
    _home="$(awk -F: -v u="$TAKO_USER" '$1==u {print $6}' /etc/passwd 2>/dev/null || true)"
  fi
  if [ -z "$_home" ]; then
    _home="/home/$TAKO_USER"
  fi
  printf '%s' "$_home"
}

home_dir="$(tako_home_dir)"
auth_keys="$home_dir/.ssh/authorized_keys"

if [ -n "${TAKO_SSH_PUBKEY:-}" ]; then
  mkdir -p "$home_dir/.ssh"
  chmod 700 "$home_dir/.ssh"

  # Check if key already exists in authorized_keys to avoid duplicates
  if [ -f "$auth_keys" ] && grep -qF "$TAKO_SSH_PUBKEY" "$auth_keys" 2>/dev/null; then
    echo "OK SSH key already present in authorized_keys"
  elif [ -f "$auth_keys" ] && [ -s "$auth_keys" ]; then
    # File exists and is non-empty — append instead of overwriting
    printf '%s\n' "$TAKO_SSH_PUBKEY" >> "$auth_keys"
    echo "OK appended SSH key to existing authorized_keys"
  else
    printf '%s\n' "$TAKO_SSH_PUBKEY" > "$auth_keys"
    echo "OK wrote SSH key to authorized_keys"
  fi

  chmod 600 "$auth_keys"
  chown -R "$TAKO_USER":"$TAKO_USER" "$home_dir/.ssh" 2>/dev/null || true
elif [ -f "$auth_keys" ] && [ -s "$auth_keys" ]; then
  echo "OK existing SSH key retained for '$TAKO_USER'"
else
  echo "warning: no SSH key installed for '$TAKO_USER'." >&2
  echo "warning: configure ~/.ssh/authorized_keys manually or rerun installer with TAKO_SSH_PUBKEY." >&2
fi

install_systemd_service_unit() {
  mkdir -p /etc/systemd/system
  cat > /etc/systemd/system/tako-server.service <<EOF
[Unit]
Description=Tako Server
After=network.target

[Service]
Type=notify
NotifyAccess=all
User=$TAKO_USER
Group=$TAKO_USER
NoNewPrivileges=true
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE
ExecStart=/usr/local/bin/tako-server --socket $TAKO_SOCKET --data-dir $TAKO_HOME
ExecReload=/bin/kill -HUP \$MAINPID
Restart=always
RestartSec=1
KillMode=mixed
TimeoutStopSec=30min
RuntimeDirectory=tako
RuntimeDirectoryMode=0700

[Install]
WantedBy=multi-user.target
EOF
}

install_openrc_service_script() {
  cat > /etc/init.d/tako-server <<EOF
#!/sbin/openrc-run
description="Tako Server"

command="/usr/local/bin/tako-server"
command_args="--socket $TAKO_SOCKET --data-dir $TAKO_HOME"
command_user="$TAKO_USER:$TAKO_USER"
pidfile="/run/\${RC_SVCNAME}.pid"
command_background="yes"
retry="TERM/1800/KILL/5"

depend() {
  need net
}

extra_started_commands="reload"

reload() {
  ebegin "Reloading \${RC_SVCNAME}"
  if [ ! -f "\$pidfile" ]; then
    eend 1
    return 1
  fi
  start-stop-daemon --signal HUP --pidfile "\$pidfile"
  eend \$?
}
EOF
  chmod 0755 /etc/init.d/tako-server
}

if [ "$SERVICE_MANAGER" = "systemd" ]; then
  install_systemd_service_unit
elif [ "$SERVICE_MANAGER" = "openrc" ]; then
  install_openrc_service_script
fi

if [ "$SERVICE_MANAGER" = "systemd" ]; then
  systemctl daemon-reload
  if is_enabled "$TAKO_RESTART_SERVICE"; then
    systemctl enable tako-server >/dev/null 2>&1 || true
    if systemctl is-active --quiet tako-server; then
      # Service already running — graceful reload (SIGHUP) to pick up new binary
      systemctl reload tako-server
      echo "OK tako-server reloaded (SIGHUP)"
    else
      systemctl start tako-server
    fi
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
elif [ "$SERVICE_MANAGER" = "openrc" ]; then
  rc-update add tako-server default >/dev/null 2>&1 || true
  if is_enabled "$TAKO_RESTART_SERVICE"; then
    if rc-service tako-server status >/dev/null 2>&1; then
      rc-service tako-server reload || rc-service tako-server restart
    else
      rc-service tako-server start
    fi
    rc-service tako-server status || true
    if ! rc-service tako-server status >/dev/null 2>&1; then
      echo "error: tako-server failed to start via OpenRC." >&2
      exit 1
    fi
  else
    echo "OK install refreshed without restarting tako-server (TAKO_RESTART_SERVICE=0)"
  fi
else
  # Install-refresh mode can run before init is active (for example in image builds).
  # In this mode we install binaries/users only and skip service definition install.
  echo "OK install refreshed without active service manager (TAKO_RESTART_SERVICE=0); skipped service definition install"
fi

# Optional DNS provider setup for wildcard certificates
TAKO_DNS_PROVIDER_CONF="$TAKO_HOME/dns-provider.conf"
TAKO_DNS_CREDENTIALS_ENV="$TAKO_HOME/dns-credentials.env"

if [ -f "$TAKO_DNS_PROVIDER_CONF" ]; then
  existing_dns_provider="$(cat "$TAKO_DNS_PROVIDER_CONF" 2>/dev/null || true)"
  echo "OK DNS provider already configured: $existing_dns_provider"
  # Ensure systemd drop-in is in place (idempotent)
  if [ "$SERVICE_MANAGER" = "systemd" ] && [ -n "$existing_dns_provider" ]; then
    dropin_dir="/etc/systemd/system/tako-server.service.d"
    if [ ! -f "$dropin_dir/dns.conf" ]; then
      mkdir -p "$dropin_dir"
      cat > "$dropin_dir/dns.conf" <<DNSEOF
[Service]
EnvironmentFile=$TAKO_DNS_CREDENTIALS_ENV
ExecStart=
ExecStart=/usr/local/bin/tako-server --socket $TAKO_SOCKET --data-dir $TAKO_HOME --dns-provider $existing_dns_provider
DNSEOF
      systemctl daemon-reload
      echo "OK restored DNS systemd drop-in for $existing_dns_provider"
    fi
  fi
fi

echo "OK installed tako-server"
echo "OK configured user: $TAKO_USER"
