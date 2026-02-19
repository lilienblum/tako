# syntax=docker/dockerfile:1.6

FROM debian:bookworm-slim

ARG MISE_INSTALL_PATH=/usr/local/bin/mise

ENV MISE_INSTALL_PATH=${MISE_INSTALL_PATH}
ENV PATH="/root/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends \
        bash \
        ca-certificates \
        curl \
        git \
        gzip \
        unzip \
        xz-utils; \
    rm -rf /var/lib/apt/lists/*; \
    if ! command -v mise >/dev/null 2>&1; then \
        apt-get update; \
        if apt-get install -y --no-install-recommends mise >/dev/null 2>&1; then \
            rm -rf /var/lib/apt/lists/*; \
        else \
            rm -rf /var/lib/apt/lists/*; \
            installer="$(mktemp)"; \
            curl -fsSL https://mise.run -o "$installer"; \
            chmod +x "$installer"; \
            MISE_INSTALL_PATH="$MISE_INSTALL_PATH" sh "$installer"; \
            rm -f "$installer"; \
        fi; \
    fi; \
    mise --version

WORKDIR /workspace
