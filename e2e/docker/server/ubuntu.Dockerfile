FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    openssh-server \
    bash \
    ca-certificates \
    curl \
    sudo \
    xz-utils \
    zstd \
  && rm -rf /var/lib/apt/lists/*

# Fake systemctl so the install script's systemd path works without real systemd
COPY e2e/docker/server/fake-systemctl.sh /usr/local/bin/systemctl
RUN chmod +x /usr/local/bin/systemctl && mkdir -p /run/systemd/system

# Pre-install tako-server (with dummy binary, installer creates user/service artifacts)
COPY scripts/install-tako-server.sh /tmp/install-tako-server.sh
RUN chmod +x /tmp/install-tako-server.sh \
    && printf '#!/bin/sh\nexit 0\n' > /tmp/tako-server \
    && chmod +x /tmp/tako-server \
    && tar -cf - -C /tmp tako-server | zstd -o /tmp/tako-server.tar.zst \
    && sha256sum /tmp/tako-server.tar.zst | awk '{print $1}' > /tmp/tako-server.tar.zst.sha256 \
    && TAKO_SERVER_URL="file:///tmp/tako-server.tar.zst" TAKO_RESTART_SERVICE=0 TAKO_INSTALL_PROTO=0 TAKO_SERVER_NAME=e2e sh /tmp/install-tako-server.sh \
    && rm -f /tmp/install-tako-server.sh /tmp/tako-server /tmp/tako-server.tar.zst /tmp/tako-server.tar.zst.sha256

# Install proto as the tako user
USER tako
RUN curl -fsSL https://moonrepo.dev/install/proto.sh | bash -s -- --yes
USER root
RUN ln -sf /home/tako/.proto/bin/proto /usr/local/bin/proto \
    && chmod -R g+rX /home/tako/.proto 2>/dev/null || true

# Generate SSH host keys
RUN ssh-keygen -A

COPY e2e/docker/server/entrypoint.sh /usr/local/bin/tako-e2e-entrypoint.sh
RUN chmod +x /usr/local/bin/tako-e2e-entrypoint.sh

EXPOSE 22

CMD ["/usr/local/bin/tako-e2e-entrypoint.sh"]
