# syntax=docker/dockerfile:1.6

FROM oven/bun:alpine

# Lightweight SSH-accessible debug server image for installer/deploy testing.
RUN apk add --no-cache \
    openssh \
    bash \
    curl \
    sudo \
    shadow \
    coreutils \
    procps \
    libcap-utils \
    netcat-openbsd

RUN ssh-keygen -A && \
    mkdir -p /var/run/sshd /run/sshd /root/.ssh && \
    chmod 700 /root/.ssh && \
    sed -i 's/^#\?PermitRootLogin .*/PermitRootLogin prohibit-password/' /etc/ssh/sshd_config && \
    sed -i 's/^#\?PubkeyAuthentication .*/PubkeyAuthentication yes/' /etc/ssh/sshd_config && \
    sed -i 's/^#\?PasswordAuthentication .*/PasswordAuthentication no/' /etc/ssh/sshd_config && \
    sed -i 's|^#\?AuthorizedKeysFile .*|AuthorizedKeysFile .ssh/authorized_keys|' /etc/ssh/sshd_config

COPY docker/install-authorized-key.sh /usr/local/bin/install-authorized-key
RUN chmod +x /usr/local/bin/install-authorized-key

EXPOSE 22
CMD ["/usr/sbin/sshd", "-D", "-e"]
