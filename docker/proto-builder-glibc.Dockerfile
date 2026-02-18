# syntax=docker/dockerfile:1.6

FROM debian:bookworm-slim

ARG PROTO_HOME=/usr/local/lib/proto

ENV PROTO_HOME=${PROTO_HOME}
ENV PATH="${PROTO_HOME}/bin:${PROTO_HOME}/shims:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

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
    if ! command -v proto >/dev/null 2>&1; then \
        apt-get update; \
        if apt-get install -y --no-install-recommends proto >/dev/null 2>&1; then \
            rm -rf /var/lib/apt/lists/*; \
        else \
            rm -rf /var/lib/apt/lists/*; \
            installer="$(mktemp)"; \
            curl -fsSL https://moonrepo.dev/install/proto.sh -o "$installer"; \
            chmod +x "$installer"; \
            PROTO_HOME="$PROTO_HOME" bash "$installer" --yes --no-profile; \
            ln -sf "$PROTO_HOME/bin/proto" /usr/local/bin/proto; \
            rm -f "$installer"; \
        fi; \
    fi; \
    proto --version

WORKDIR /workspace
