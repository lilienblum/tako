export TAKO_HOME := "debug/.tako"

# Docker-based deploy debugging (systemd-based VPS simulation).

CONTAINER_NAME := "tako-debug-server"
IMAGE_NAME := "tako-debug-server:local"
DOCKER_SSH_PORT := "2222"

# Docker platform for the local debug container.

DOCKER_PLATFORM := "linux/arm64"

# For local debug installs, we serve a built `tako-server` binary from the host
# and have the Docker container download it.

ARTIFACTS_DIR := "debug/artifacts"
ARTIFACTS_PORT := "8000"
DEBUG_TMP_DIR := "debug/tmp"
DOCKER_SSH_KEY_FILE := "debug/tmp/docker_ssh_key"

# Docker Linux containers can reach the macOS host via this hostname.
# If this doesn't resolve for you, try `host.docker.internal`.

tako *arguments:
    # Build/run the current CLI source without installing.
    TAKO_HOME="$(pwd)/{{ TAKO_HOME }}" cargo run -p tako -- {{ arguments }}

clean:
    cargo clean
    rm -rf {{ TAKO_HOME }}
    # On macOS, remove the local dev CA from the keychain (best-effort).
    if [ "$(uname -s)" = "Darwin" ]; then \
    security delete-generic-password -s "tako-local-ca" -a "tako" >/dev/null 2>&1 || true; \
    security find-certificate -c "Tako Local Development CA" /Library/Keychains/System.keychain >/dev/null 2>&1 && \
        sudo security delete-certificate -c "Tako Local Development CA" /Library/Keychains/System.keychain >/dev/null 2>&1 || true; \
    fi
    # Remove any leftover containers and images.
    docker rm -f {{ CONTAINER_NAME }} 2>/dev/null || true
    docker rmi -f {{ IMAGE_NAME }} 2>/dev/null || true
    # Kill the local dev server process if it's still around.
    pkill -x tako-dev-server 2>/dev/null || true

test:
    cargo test --workspace

build-tako-server:
    # Build both architectures for unknown deployment targets.
    mkdir -p {{ ARTIFACTS_DIR }}
    # Docker/buildx expects amd64/arm64 (not x86_64/aarch64).
    for arch in amd64 arm64; do \
        docker buildx build -f docker/build.Dockerfile \
            --platform linux/$arch \
            --target tako-server-artifact \
            --output type=local,dest={{ ARTIFACTS_DIR }}/linux-$arch \
            . || exit 1; \
        if [ "$arch" = "amd64" ]; then outarch="x86_64"; else outarch="aarch64"; fi; \
        cp {{ ARTIFACTS_DIR }}/linux-$arch/tako-server {{ ARTIFACTS_DIR }}/tako-server-linux-$outarch || exit 1; \
        cp {{ ARTIFACTS_DIR }}/linux-$arch/tako-server.sha256 {{ ARTIFACTS_DIR }}/tako-server-linux-$outarch.sha256 || exit 1; \
        rm -rf {{ ARTIFACTS_DIR }}/linux-$arch; \
    done
    echo "Built binaries:"
    ls -la {{ ARTIFACTS_DIR }}/tako-server-linux-*

create-debug-server:
    # Create a named debug container with systemd + sshd for VPS-like installer behavior.
    docker rm -f {{ CONTAINER_NAME }} 2>/dev/null || true
    docker build --platform {{ DOCKER_PLATFORM }} -f docker/debug-server.Dockerfile -t {{ IMAGE_NAME }} .
    mkdir -p {{ DEBUG_TMP_DIR }}
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
            -v /sys/fs/cgroup:/sys/fs/cgroup:rw \
            --cgroupns=host \
            --privileged \
            --platform {{ DOCKER_PLATFORM }} \
            {{ IMAGE_NAME }} sh -lc '\
                /usr/local/bin/install-authorized-key && \
                exec /lib/systemd/systemd \
            '

install-server:
    # Install tako-server in the temporary Docker debug server container.
    # Starts a temporary local artifacts server for this install run.
    test -f {{ DOCKER_SSH_KEY_FILE }} || (echo "Missing {{ DOCKER_SSH_KEY_FILE }}. Run 'just create-debug-server' first." >&2; exit 1)
    mkdir -p {{ ARTIFACTS_DIR }}
    mkdir -p {{ DEBUG_TMP_DIR }}
    sh -eu -c '\
        python3 -m http.server {{ ARTIFACTS_PORT }} --directory {{ ARTIFACTS_DIR }} >/dev/null 2>&1 & \
        artifacts_pid=$!; \
        trap "kill $artifacts_pid >/dev/null 2>&1 || true" EXIT INT TERM; \
        ssh -F /dev/null \
            -o StrictHostKeyChecking=no \
            -o UserKnownHostsFile=/dev/null \
            -o GlobalKnownHostsFile=/dev/null \
            -o IdentitiesOnly=yes \
            -i "$(cat {{ DOCKER_SSH_KEY_FILE }})" \
            -p {{ DOCKER_SSH_PORT }} \
            root@localhost \
            '\''TAKO_DOWNLOAD_BASE_URL=http://host.docker.internal:{{ ARTIFACTS_PORT }} TAKO_SSH_PUBKEY="$(cat /root/.ssh/authorized_keys)" sh -s'\'' \
            < ./scripts/setup-tako-server.sh \
    '
