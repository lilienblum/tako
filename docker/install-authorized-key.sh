#!/bin/sh
set -eu

if [ ! -f /run/authorized_keys ]; then
  exit 0
fi

install -d -m 700 /root/.ssh
install -m 600 /run/authorized_keys /root/.ssh/authorized_keys
chown root:root /root/.ssh/authorized_keys
