FROM alpine:3.20

RUN apk add --no-cache \
    openssh-server \
    netcat-openbsd \
    python3 \
    bash \
    curl \
    coreutils \
    tar

RUN ssh-keygen -A

RUN mkdir -p /var/run/sshd /root/.ssh /var/run/tako /opt/tako /usr/local/bin /opt/artifacts

COPY entrypoint.sh /entrypoint.sh
COPY fake_tako_server.py /fake_tako_server.py

RUN chmod +x /entrypoint.sh

EXPOSE 22

ENTRYPOINT ["/entrypoint.sh"]
