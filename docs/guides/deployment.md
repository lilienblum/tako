# Deployment

This document describes how `tako deploy` works in practice and what remote servers must provide.

## Related Docs

- `quickstart.md`: install and first deployment setup.
- `operations.md`: day-2 deploy verification and incident triage commands.
- `../architecture/overview.md`: runtime data/control plane context.

## Overview

From an app directory, run:

```bash
tako deploy [--env <environment>]
```

Deploy behavior:

- Build happens locally.
- A versioned tarball is created under `.tako/build/`.
- Deploys run to all target servers in parallel.
- Each server is handled independently (partial success across servers is possible).

## Pre-Deploy Checklist

Before running deploy from a project directory:

1. Ensure target hosts exist in `~/.tako/config.toml` (or `tako servers ls`).
2. Confirm `tako.toml` has route/env/server mappings for intended environment.
3. Verify secrets are present for the target env (`tako secrets sync` if needed).
4. Run local tests/build to avoid pushing a broken artifact.

## Server Prerequisites

Each target server should have:

- SSH access as the configured deployment user (typically `tako`).
- `tako-server` installed and running.
- `nc` (netcat), `tar`, `base64`, and standard shell tools (`mkdir`, `find`, `stat`).
- Writable runtime paths under `/opt/tako` and socket access at `/var/run/tako/tako.sock`.
- Privileged bind capability for `tako-server` on `:80/:443` (provided by systemd service capabilities in the installer, plus `setcap` on the binary when available).

## Configuration Inputs

### Project config (`tako.toml`)

- `tako.toml` is required in the project root.
- Defines environments and routes.
- Every non-development environment must define `route` or `routes`.
- Empty route sets are rejected for non-development environments (no implicit catch-all mode).
- Defines server-to-environment mapping via `[servers.<name>] env = "..."`.
- Defines per-server scaling settings (`instances`, `idle_timeout`) via global and per-server overrides.

### Global server inventory (`~/.tako/config.toml`)

- Defines named servers (`host`, `port`, optional `description`).
- Managed via `tako servers ...` commands.

## Deploy Flow (Per Server)

1. Connect via SSH.
2. Acquire per-app deploy lock at `/opt/tako/apps/<app>/.deploy_lock`.
3. Run disk-space preflight under `/opt/tako`.
4. Ensure `tako-server` is installed and active.
5. Validate route conflicts against current server routing state.
6. Create release and shared directories.
7. Upload and extract archive into `/opt/tako/apps/<app>/releases/<version>/`.
8. Link shared directories (for example `logs`).
9. Write release `.env` including `TAKO_BUILD` and environment secrets.
10. Send deploy command to `tako-server`.
11. Update `current` symlink after server accepts deploy.
12. Clean old release directories.

## Remote Layout

```text
/opt/tako/apps/<app>/
  current -> releases/<version>
  .deploy_lock/
  releases/
    <version>/
      ...app files...
      .env
      logs -> /opt/tako/apps/<app>/shared/logs
  shared/
    logs/
```

## Environment and Secrets

- Deploy writes `TAKO_BUILD="<version>"` into release `.env`.
- Local encrypted secrets are decrypted during deploy and written into release `.env` for the target environment.
- Manage secrets with:
  - `tako secrets set`
  - `tako secrets rm`
  - `tako secrets sync`

## Operational Notes

- Use `tako status` to inspect deployed app state by environment.
- Use `tako logs --env <environment>` to stream remote logs.
- Use `tako servers status <name>` to inspect remote `tako-server` install/service state.
- HTTP requests are redirected to HTTPS by default (ACME challenge and `/_tako/status` remain on HTTP).

## Post-Deploy Verification

Immediately after deploy:

1. Run `tako status` and confirm routes/instances are healthy for the target environment/app.
2. Open one or more public routes and validate response headers/body.
3. Tail logs with `tako logs --env <environment>` for startup/runtime errors.
4. If only a subset of servers succeeded, re-run deploy after correcting failed hosts.

## Running Deploy E2E Tests

Deploy E2E tests are opt-in and Docker-backed:

```bash
TAKO_E2E=1 cargo test -p tako --test deploy_e2e -- --nocapture
```
