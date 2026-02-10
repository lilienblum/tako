# syntax=docker/dockerfile:1.6

FROM ubuntu:24.04

ENV container=docker
ENV DEBIAN_FRONTEND=noninteractive

# Minimal systemd + ssh image for VPS-like installer behavior.
RUN apt-get update && apt-get install -y --no-install-recommends \
    systemd \
    openssh-server \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN mkdir -p /var/run/sshd /run/sshd /root/.ssh && \
    chmod 700 /root/.ssh && \
    sed -ri 's/^#?PermitRootLogin .*/PermitRootLogin prohibit-password/' /etc/ssh/sshd_config && \
    sed -ri 's/^#?PubkeyAuthentication .*/PubkeyAuthentication yes/' /etc/ssh/sshd_config && \
    sed -ri 's/^#?PasswordAuthentication .*/PasswordAuthentication no/' /etc/ssh/sshd_config && \
    sed -ri 's|^#?AuthorizedKeysFile .*|AuthorizedKeysFile .ssh/authorized_keys|' /etc/ssh/sshd_config

COPY docker/install-authorized-key.sh /usr/local/bin/install-authorized-key
RUN chmod +x /usr/local/bin/install-authorized-key

RUN cat > /etc/systemd/system/install-authorized-key.service <<'EOF'
[Unit]
Description=Install root authorized_keys from bind mount
After=local-fs.target

[Service]
Type=oneshot
ExecStart=/usr/local/bin/install-authorized-key
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
EOF

# Enable services without requiring systemd to run during build.
RUN ln -sf /lib/systemd/system/ssh.service /etc/systemd/system/multi-user.target.wants/ssh.service && \
    ln -sf /etc/systemd/system/install-authorized-key.service /etc/systemd/system/multi-user.target.wants/install-authorized-key.service

VOLUME ["/sys/fs/cgroup"]
STOPSIGNAL SIGRTMIN+3

EXPOSE 22
CMD ["/lib/systemd/systemd"]
