# Docker Assets

Internal Docker tooling for building and debugging Tako server artifacts.

## Files

- `build.Dockerfile`: builds Linux `tako-server` artifacts for `x86_64` / `aarch64` across both libc families (`musl`, `glibc`).
- `bun.Dockerfile`: internal debug container (`oven/bun:alpine` + sshd) used for deploy/install debugging.
  - Bootstraps server-side dependencies through `scripts/install-tako-server.sh` so debug image deps stay aligned with installer behavior.
- `install-authorized-key.sh`: helper script used by debug container boot flow.

`bun.Dockerfile` is internal-only and not intended as a production application image.

## Common Commands

From repository root:

```bash
just build::tako-server
just testbed::create-container
just testbed::install
```

`just build::tako-server` builds release artifacts for all Linux target/libc combinations:

- `dist/tako-server-linux-x86_64-musl`
- `dist/tako-server-linux-aarch64-musl`
- `dist/tako-server-linux-x86_64-glibc`
- `dist/tako-server-linux-aarch64-glibc`

Use the `testbed::` namespace for these recipes.
