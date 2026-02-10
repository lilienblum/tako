# Docker Assets

Internal Docker tooling for building and debugging Tako server artifacts.

## Files

- `build.Dockerfile`: builds Linux `tako-server` artifacts (`x86_64` / `aarch64`) using musl.
- `debug-server.Dockerfile`: internal debug container (systemd + sshd) used for deploy/install debugging.
- `install-authorized-key.sh`: helper script used by debug container boot flow.

`debug-server.Dockerfile` is internal-only and not intended as a production application image.

## Common Commands

From repository root:

```bash
just build-tako-server
just create-debug-server
just install-server
```
