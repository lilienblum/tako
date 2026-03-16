FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    systemd \
    systemd-sysv \
    openssh-server \
    bash \
    ca-certificates \
    unzip \
    curl \
    sudo \
    zstd \
  && rm -rf /var/lib/apt/lists/* \
  && rm -f /lib/systemd/system/multi-user.target.wants/* \
  && rm -f /etc/systemd/system/*.wants/* \
  && rm -f /lib/systemd/system/local-fs.target.wants/* \
  && rm -f /lib/systemd/system/sockets.target.wants/*udev* \
  && rm -f /lib/systemd/system/sockets.target.wants/*initctl* \
  && rm -f /lib/systemd/system/basic.target.wants/* \
  && rm -f /lib/systemd/system/anaconda.target.wants/*

# Pre-install tako-server (with dummy binary, installer creates user/service)
COPY scripts/install-tako-server.sh /tmp/install-tako-server.sh
RUN chmod +x /tmp/install-tako-server.sh \
    && printf '#!/bin/sh\nexit 0\n' > /tmp/tako-server \
    && chmod +x /tmp/tako-server \
    && tar -cf - -C /tmp tako-server | zstd -o /tmp/tako-server.tar.zst \
    && TAKO_SERVER_URL="file:///tmp/tako-server.tar.zst" TAKO_RESTART_SERVICE=0 sh /tmp/install-tako-server.sh \
    && rm -f /tmp/install-tako-server.sh /tmp/tako-server /tmp/tako-server.tar.zst

# Setup SSH and e2e keys
COPY e2e/docker/server/setup.sh /usr/local/bin/tako-e2e-setup.sh
RUN chmod +x /usr/local/bin/tako-e2e-setup.sh

# Create a oneshot service that runs setup before sshd
RUN printf '[Unit]\nBefore=sshd.service\n[Service]\nType=oneshot\nExecStart=/usr/local/bin/tako-e2e-setup.sh\n[Install]\nWantedBy=multi-user.target\n' > /etc/systemd/system/tako-e2e-setup.service \
    && systemctl enable tako-e2e-setup.service \
    && systemctl enable ssh

EXPOSE 22

STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
