FROM alpine:3.22

RUN apk add --no-cache \
      bash \
      ca-certificates \
      curl \
      openrc \
      openssh \
      shadow \
      sudo \
      unzip \
      zstd \
    && ssh-keygen -A

# Pre-install tako-server (with dummy binary, installer creates user/service artifacts)
COPY scripts/install-tako-server.sh /tmp/install-tako-server.sh
RUN chmod +x /tmp/install-tako-server.sh \
    && printf '#!/bin/sh\nexit 0\n' > /tmp/tako-server \
    && chmod +x /tmp/tako-server \
    && tar -cf - -C /tmp tako-server | zstd -o /tmp/tako-server.tar.zst \
    && sha256sum /tmp/tako-server.tar.zst | awk '{print $1}' > /tmp/tako-server.tar.zst.sha256 \
    && TAKO_SERVER_URL="file:///tmp/tako-server.tar.zst" TAKO_RESTART_SERVICE=0 sh /tmp/install-tako-server.sh \
    && rm -f /tmp/install-tako-server.sh /tmp/tako-server /tmp/tako-server.tar.zst /tmp/tako-server.tar.zst.sha256

# Setup SSH and e2e keys at container boot.
COPY e2e/docker/server/entrypoint.sh /usr/local/bin/tako-e2e-entrypoint.sh
RUN chmod +x /usr/local/bin/tako-e2e-entrypoint.sh

EXPOSE 22

CMD ["/usr/local/bin/tako-e2e-entrypoint.sh"]
