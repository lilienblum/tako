# Backlog

Items discovered during CI/release workflow simplification (2026-03-17).

## tako-server

- **Install script `su` fails in Docker build** — `ensure_proto` uses `su -s /bin/sh "tako"` which fails during `docker build` (no tty, permission denied). E2E Dockerfiles work around this with `USER tako` directive and `TAKO_INSTALL_PROTO=0`. The install script could detect non-interactive Docker builds and skip `su`.
