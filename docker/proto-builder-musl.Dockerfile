# syntax=docker/dockerfile:1.6

FROM alpine:3.20

ARG PROTO_HOME=/usr/local/lib/proto

ENV PROTO_HOME=${PROTO_HOME}
ENV PATH="${PROTO_HOME}/bin:${PROTO_HOME}/shims:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

RUN set -eux; \
    apk add --no-cache bash ca-certificates curl git gzip unzip xz; \
    if ! command -v proto >/dev/null 2>&1; then \
        if apk add --no-cache proto >/dev/null 2>&1; then \
            :; \
        else \
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
