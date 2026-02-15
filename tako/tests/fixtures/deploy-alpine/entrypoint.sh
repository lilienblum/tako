#!/bin/sh
set -eu

addgroup -S tako 2>/dev/null || true
adduser -S -D -h /home/tako -s /bin/sh -G tako tako 2>/dev/null || true

# Alpine can create the user in a locked state; unlock to allow pubkey SSH auth.
if command -v passwd >/dev/null 2>&1; then
  passwd -u tako >/dev/null 2>&1 || true
  passwd -d tako >/dev/null 2>&1 || true
fi

mkdir -p /home/tako/.ssh /var/run/tako /opt/tako /opt/artifacts /usr/local/bin
chmod 700 /home/tako/.ssh

if [ -n "${AUTHORIZED_KEY:-}" ]; then
  echo "$AUTHORIZED_KEY" > /home/tako/.ssh/authorized_keys
fi

chmod 600 /home/tako/.ssh/authorized_keys || true
chown -R tako:tako /home/tako/.ssh /var/run/tako /opt/tako || true

cat > /etc/ssh/sshd_config <<'EOF'
Port 22
Protocol 2
HostKey /etc/ssh/ssh_host_ed25519_key
HostKey /etc/ssh/ssh_host_rsa_key
PermitRootLogin no
PasswordAuthentication no
ChallengeResponseAuthentication no
PubkeyAuthentication yes
AuthorizedKeysFile .ssh/authorized_keys
AllowUsers tako
Subsystem sftp /usr/lib/ssh/sftp-server
PidFile /var/run/sshd.pid
EOF

cat > /usr/local/bin/tako-server <<'EOF'
#!/bin/sh
if [ "${1:-}" = "--version" ]; then
  echo "tako-server 0.0.0"
  exit 0
fi
echo "stub tako-server"
EOF
chmod +x /usr/local/bin/tako-server

# Serve the binary for remote-download install tests.
python3 -m http.server 8000 --directory /opt/artifacts >/opt/tako/artifacts.log 2>&1 &

su -s /bin/sh -c "python3 /fake_tako_server.py" tako &

exec /usr/sbin/sshd -D -e -f /etc/ssh/sshd_config
