# syntax=docker/dockerfile:1.6

FROM alpine:3.20

ARG MISE_INSTALL_PATH=/usr/local/bin/mise

ENV MISE_INSTALL_PATH=${MISE_INSTALL_PATH}
ENV PATH="/root/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

RUN set -eux; \
    apk add --no-cache \
        bash \
        ca-certificates \
        curl \
        git \
        gzip \
        libgcc \
        libstdc++ \
        unzip \
        xz; \
    if ! command -v mise >/dev/null 2>&1; then \
        if apk add --no-cache mise >/dev/null 2>&1; then \
            :; \
        else \
            installer="$(mktemp)"; \
            curl -fsSL https://mise.run -o "$installer"; \
            chmod +x "$installer"; \
            MISE_INSTALL_PATH="$MISE_INSTALL_PATH" sh "$installer"; \
            rm -f "$installer"; \
        fi; \
    fi; \
    mise --version

WORKDIR /workspace
