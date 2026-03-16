FROM almalinux:9

RUN dnf install -y \
      systemd \
      openssh-server \
      openssh-clients \
      sudo \
      xz \
      zstd \
    && dnf clean all \
    && rm -rf /var/cache/dnf \
    && rm -f /lib/systemd/system/multi-user.target.wants/* \
    && rm -f /etc/systemd/system/*.wants/* \
    && rm -f /lib/systemd/system/local-fs.target.wants/* \
    && rm -f /lib/systemd/system/sockets.target.wants/*udev* \
    && rm -f /lib/systemd/system/sockets.target.wants/*initctl* \
    && rm -f /lib/systemd/system/basic.target.wants/*

# Pre-install tako-server (with dummy binary, installer creates user/service)
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

# Setup SSH and e2e keys
COPY e2e/docker/server/setup.sh /usr/local/bin/tako-e2e-setup.sh
RUN chmod +x /usr/local/bin/tako-e2e-setup.sh

# Create a oneshot service that runs setup before sshd
RUN printf '[Unit]\nBefore=sshd.service\n[Service]\nType=oneshot\nExecStart=/usr/local/bin/tako-e2e-setup.sh\n[Install]\nWantedBy=multi-user.target\n' > /etc/systemd/system/tako-e2e-setup.service \
    && systemctl enable tako-e2e-setup.service \
    && systemctl enable sshd

EXPOSE 22

STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
