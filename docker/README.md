# Docker Assets

Internal Docker tooling for building and debugging Tako server artifacts.

## Files

- `build.Dockerfile`: builds Linux `tako-server` artifacts (`x86_64` / `aarch64`) using musl.
- `bun.Dockerfile`: internal debug container (`oven/bun:alpine` + sshd) used for deploy/install debugging.
  - Bootstraps server-side dependencies through `scripts/install-tako-server.sh` so debug image deps stay aligned with installer behavior.
- `install-authorized-key.sh`: helper script used by debug container boot flow.

`bun.Dockerfile` is internal-only and not intended as a production application image.

## Common Commands

From repository root:

```bash
just testbed::build-tako-server
just testbed::create-bun-server
just testbed::install-bun-server
```

Use the `testbed::` namespace for these recipes.
