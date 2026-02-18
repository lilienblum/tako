FROM almalinux:9

RUN dnf install -y \
      openssh-server \
      openssh-clients \
      unzip \
    && dnf clean all \
    && rm -rf /var/cache/dnf

COPY scripts/install-tako-server.sh /tmp/install-tako-server.sh
RUN chmod +x /tmp/install-tako-server.sh \
    && printf '#!/bin/sh\nexit 0\n' > /tmp/tako-server \
    && chmod +x /tmp/tako-server \
    && TAKO_SERVER_URL="file:///tmp/tako-server" sh /tmp/install-tako-server.sh \
    && rm -f /tmp/install-tako-server.sh /tmp/tako-server

RUN mkdir -p /run/sshd /var/run/tako /opt/tako /usr/local/bin /opt/e2e/keys
RUN ssh-keygen -A

COPY e2e/docker/server/entrypoint.sh /entrypoint.sh

RUN chmod +x /entrypoint.sh

EXPOSE 22

ENTRYPOINT ["/entrypoint.sh"]
