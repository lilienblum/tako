# Docker Assets

Internal Docker tooling for building and debugging Tako release artifacts.

## Files

- `build.Dockerfile`: builds Linux release artifacts for `tako` and `tako-server` across `x86_64` / `aarch64`.
  - `tako` artifacts are built from the glibc image path.
  - `tako-server` artifacts are built for both libc families (`musl`, `glibc`).
- `tako-builder-musl.Dockerfile`: base build image (`alpine:3.23`) with `mise` preinstalled (package-manager first, installer fallback).
- `tako-builder-glibc.Dockerfile`: base build image (`debian:bookworm-slim`) with `mise` preinstalled (package-manager first, installer fallback).
- `testbed.Dockerfile`: internal debug container (`oven/bun:alpine` + sshd) used for deploy/install debugging.
  - Bootstraps server-side dependencies through `scripts/install-tako-server.sh` so debug image deps stay aligned with installer behavior.
- `install-authorized-key.sh`: helper script used by debug container boot flow.

`testbed.Dockerfile` is internal-only and not intended as a production application image.

## Common Commands

From repository root:

```bash
just build::tako-server-all
just build::tako-server arm64 musl   # default testbed.Dockerfile container (Alpine on linux/arm64)
just build::tako-server amd64 glibc
just build::tako-linux
just testbed::create
just testbed::install
just release::builder-images v1
```

`just build::tako-server` builds a single target and requires args:

- `arch`: `amd64` or `arm64`
- `libc`: `musl` or `glibc`

Use `just build::tako-server-all` to build all Linux target/libc combinations:

- `dist/tako-server-linux-x86_64-musl`
- `dist/tako-server-linux-aarch64-musl`
- `dist/tako-server-linux-x86_64-glibc`
- `dist/tako-server-linux-aarch64-glibc`

`just build::tako-linux` builds Linux CLI artifacts:

- `dist/tako-linux-x86_64`
- `dist/tako-linux-aarch64`

Use the `testbed::` namespace for these recipes.

Build local mise-enabled builder images:

```bash
docker build -f docker/tako-builder-musl.Dockerfile -t tako-builder:musl-v1 .
docker build -f docker/tako-builder-glibc.Dockerfile -t tako-builder:glibc-v1 .
```

Example multi-arch publish (adjust registry/tag):

```bash
docker buildx build -f docker/tako-builder-musl.Dockerfile \
  --platform linux/amd64,linux/arm64 \
  -t ghcr.io/lilienblum/tako-builder-musl:v1 \
  -t ghcr.io/lilienblum/tako-builder-musl:latest \
  --push .

docker buildx build -f docker/tako-builder-glibc.Dockerfile \
  --platform linux/amd64,linux/arm64 \
  -t ghcr.io/lilienblum/tako-builder-glibc:v1 \
  -t ghcr.io/lilienblum/tako-builder-glibc:latest \
  --push .
```
