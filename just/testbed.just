set working-directory := ".."

# Docker-based deploy testbed (Alpine + sshd VPS simulation).

CONTAINER_NAME := "tako-testbed"
IMAGE_NAME := "tako-testbed:local"
DOCKER_SSH_PORT := "2222"

# Docker platform for the local testbed container.

DOCKER_PLATFORM := "linux/arm64"

# For local testbed installs, we serve a built `tako-server` binary from the host
# and have the Docker container download it.

ARTIFACTS_DIR := "dist"
ARTIFACTS_PORT := "8000"
TESTBED_TMP_DIR := "local-dev/tmp"
DOCKER_SSH_KEY_FILE := "local-dev/tmp/docker_ssh_key"

# Docker Linux containers can reach the macOS host via this hostname.
# If this doesn't resolve for you, try `host.docker.internal`.

create-container:
    # Create a named testbed container with sshd for VPS-like installer behavior.
    docker rm -f {{ CONTAINER_NAME }} 2>/dev/null || true
    docker build --platform {{ DOCKER_PLATFORM }} -f docker/bun.Dockerfile -t {{ IMAGE_NAME }} .
    mkdir -p {{ TESTBED_TMP_DIR }}
    PUBKEY_FILE="${SSH_PUBKEY_FILE:-}"; \
        if [ -z "$PUBKEY_FILE" ]; then \
            for f in "$HOME/.ssh/id_ed25519.pub" "$HOME/.ssh/id_rsa.pub" "$HOME/.ssh/id_ecdsa.pub" "$HOME/.ssh/id_dsa.pub"; do \
                if [ -f "$f" ]; then PUBKEY_FILE="$f"; break; fi; \
            done; \
        fi; \
        if [ -z "$PUBKEY_FILE" ]; then \
            echo "No SSH public key found. Set SSH_PUBKEY_FILE=/path/to/key.pub" >&2; \
            exit 1; \
        fi; \
        KEY_FILE="${SSH_PRIVATE_KEY_FILE:-${PUBKEY_FILE%.pub}}"; \
        if [ ! -f "$KEY_FILE" ]; then \
            echo "No matching private key found: $KEY_FILE (set SSH_PRIVATE_KEY_FILE=/path/to/key)" >&2; \
            exit 1; \
        fi; \
        printf "%s\n" "$KEY_FILE" > {{ DOCKER_SSH_KEY_FILE }}; \
        docker run -d \
            --name {{ CONTAINER_NAME }} \
            -p {{ DOCKER_SSH_PORT }}:22 \
            -v "$PUBKEY_FILE:/run/authorized_keys:ro" \
            --platform {{ DOCKER_PLATFORM }} \
            {{ IMAGE_NAME }} sh -lc '\
                /usr/local/bin/install-authorized-key && \
                exec /usr/sbin/sshd -D -e \
            '

install:
    # Install tako-server in the temporary Docker testbed server container.
    # Starts a temporary local artifacts server for this install run.
    test -f {{ DOCKER_SSH_KEY_FILE }} || (echo "Missing {{ DOCKER_SSH_KEY_FILE }}. Run 'just testbed::create-container' first." >&2; exit 1)
    mkdir -p {{ ARTIFACTS_DIR }}
    mkdir -p {{ TESTBED_TMP_DIR }}
    sh -eu -c '\
        python3 -m http.server {{ ARTIFACTS_PORT }} --directory {{ ARTIFACTS_DIR }} >/dev/null 2>&1 & \
        artifacts_pid=$!; \
        trap "kill $artifacts_pid >/dev/null 2>&1 || true" EXIT INT TERM; \
        sleep 1; \
        ssh -F /dev/null \
            -o StrictHostKeyChecking=no \
            -o UserKnownHostsFile=/dev/null \
            -o GlobalKnownHostsFile=/dev/null \
            -o IdentitiesOnly=yes \
            -i "$(cat {{ DOCKER_SSH_KEY_FILE }})" \
            -p {{ DOCKER_SSH_PORT }} \
            root@localhost \
            '\''TAKO_DOWNLOAD_BASE_URL=http://host.docker.internal:{{ ARTIFACTS_PORT }} TAKO_SSH_PUBKEY="$(cat /root/.ssh/authorized_keys)" sh -s'\'' \
            < ./scripts/install-tako-server.sh; \
        ssh -F /dev/null \
            -o StrictHostKeyChecking=no \
            -o UserKnownHostsFile=/dev/null \
            -o GlobalKnownHostsFile=/dev/null \
            -o IdentitiesOnly=yes \
            -i "$(cat {{ DOCKER_SSH_KEY_FILE }})" \
            -p {{ DOCKER_SSH_PORT }} \
            root@localhost \
            '\''if ! command -v systemctl >/dev/null 2>&1; then \
                list_tako_pids() { \
                    if command -v pidof >/dev/null 2>&1; then \
                        pidof tako-server 2>/dev/null || true; \
                        return; \
                    fi; \
                    if command -v pgrep >/dev/null 2>&1; then \
                        pgrep -f "/usr/local/bin/tako-server" 2>/dev/null || true; \
                        return; \
                    fi; \
                    for proc in /proc/[0-9]*; do \
                        [ -r "$proc/cmdline" ] || continue; \
                        cmd="$(tr "\\000" " " < "$proc/cmdline" 2>/dev/null || true)"; \
                        case "$cmd" in \
                            *"/usr/local/bin/tako-server"*) printf "%s " "${proc##*/}" ;; \
                        esac; \
                    done; \
                }; \
                pids="$(list_tako_pids)"; \
                if [ -n "$pids" ]; then \
                    for pid in $pids; do kill "$pid" >/dev/null 2>&1 || true; done; \
                    sleep 1; \
                    for pid in $(list_tako_pids); do kill -9 "$pid" >/dev/null 2>&1 || true; done; \
                fi; \
                if id -u tako >/dev/null 2>&1; then \
                    if command -v usermod >/dev/null 2>&1; then usermod -U tako >/dev/null 2>&1 || true; fi; \
                    if command -v passwd >/dev/null 2>&1; then passwd -u tako >/dev/null 2>&1 || true; passwd -d tako >/dev/null 2>&1 || true; fi; \
                fi; \
                if id -u tako >/dev/null 2>&1; then \
                    su -s /bin/sh -c "nohup /usr/local/bin/tako-server --socket /var/run/tako/tako.sock --data-dir /opt/tako >/opt/tako/tako-server.log 2>&1 &" tako; \
                else \
                    nohup /usr/local/bin/tako-server --socket /var/run/tako/tako.sock --data-dir /opt/tako >/opt/tako/tako-server.log 2>&1 & \
                fi; \
            fi'\'' \
    '
