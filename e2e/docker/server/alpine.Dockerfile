FROM alpine:3.22

RUN apk add --no-cache \
      bash \
      ca-certificates \
      curl \
      git \
      netcat-openbsd \
      openssh \
      shadow \
      sudo \
      xz \
      zstd \
    && ssh-keygen -A

# Pre-install tako-server (with dummy binary, installer creates user/service)
COPY scripts/install-tako-server.sh /tmp/install-tako-server.sh
RUN chmod +x /tmp/install-tako-server.sh \
    && printf '#!/bin/sh\nexit 0\n' > /tmp/tako-server \
    && chmod +x /tmp/tako-server \
    && tar -cf - -C /tmp tako-server | zstd -o /tmp/tako-server.tar.zst \
    && sha256sum /tmp/tako-server.tar.zst | awk '{print $1}' > /tmp/tako-server.tar.zst.sha256 \
    && TAKO_SERVER_URL="file:///tmp/tako-server.tar.zst" TAKO_RESTART_SERVICE=0 TAKO_SERVER_NAME=e2e sh /tmp/install-tako-server.sh \
    && rm -f /tmp/install-tako-server.sh /tmp/tako-server /tmp/tako-server.tar.zst /tmp/tako-server.tar.zst.sha256

# Pre-install bun for e2e tests (production servers use the download engine)
USER tako
RUN curl -fsSL https://bun.sh/install | bash
USER root
RUN ln -sf /home/tako/.bun/bin/bun /usr/local/bin/bun

# Setup SSH and e2e keys at container boot.
COPY e2e/docker/server/entrypoint.sh /usr/local/bin/tako-e2e-entrypoint.sh
RUN chmod +x /usr/local/bin/tako-e2e-entrypoint.sh

EXPOSE 22

CMD ["/usr/local/bin/tako-e2e-entrypoint.sh"]
