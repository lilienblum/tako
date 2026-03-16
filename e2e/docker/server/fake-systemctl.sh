#!/bin/sh
# Fake systemctl for E2E containers — satisfies install script checks
# without requiring real systemd.
case "$1" in
  show-environment) exit 0 ;;
  is-active)        exit 1 ;;  # nothing is "active"
  *)                exit 0 ;;  # daemon-reload, enable, start, reload, status
esac
