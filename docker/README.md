# Docker Assets

Internal Docker tooling for building and debugging Tako server artifacts.

## Files

- `build.Dockerfile`: builds Linux `tako-server` artifacts (`x86_64` / `aarch64`) using musl.
- `bun-server.Dockerfile`: internal debug container (`oven/bun:alpine` + sshd) used for deploy/install debugging.
- `install-authorized-key.sh`: helper script used by debug container boot flow.

`bun-server.Dockerfile` is internal-only and not intended as a production application image.

## Common Commands

From repository root:

```bash
just run-debug::build-tako-server
just run-debug::create-bun-server
just run-debug::install-bun-server
```

Legacy top-level aliases still work:

```bash
just build-tako-server
just create-bun-server
just install-bun-server
```
