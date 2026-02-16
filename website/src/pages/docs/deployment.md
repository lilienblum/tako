---
layout: ../../layouts/DocsLayout.astro
title: Tako Docs - Deployment
heading: Deployment
current: deployment
---

# Deployment

This guide explains what `tako deploy` actually does and what your remote servers need in place.

## Overview

From an app directory, run:

```bash
tako deploy [--env <environment>]
```

What happens during deploy:

- Build happens locally.
- Deploy artifact source is `dist` in `tako.toml` (default `.tako/dist`).
- Deploy packages `dist` directly and writes `app.json` at archive root.
- A versioned tarball is created under `.tako/artifacts/`.
- Deploys run to all target servers in parallel.
- Each server is handled independently, so partial success is possible.

## Pre-Deploy Checklist

Before you ship, do a quick sanity pass:

1. Ensure target hosts exist in `~/.tako/config.toml` (or check with `tako servers ls`) and were added with SSH checks enabled so target metadata was detected.
2. Confirm `tako.toml` has route/env/server mappings for the target environment.
3. Verify secrets are present for the target env (`tako secrets sync` if needed).
4. Run your local tests/build so you do not upload a broken artifact.
5. Ensure deployable files exist in your deploy `dist` directory (build output or prebuilt files).

## Server Prerequisites

Each target server should have:

- SSH access as the configured deployment user (typically `tako`).
- `tako-server` installed and running.
- `nc` (netcat), `tar`, `base64`, and standard shell tools (`mkdir`, `find`, `stat`).
- Writable runtime paths under `/opt/tako` and socket access at `/var/run/tako/tako.sock`.
- Privileged bind capability for `tako-server` on `:80/:443` (provided by systemd service capabilities in the installer, plus `setcap` on the binary when available).

## Configuration Inputs

### Project config ([`tako.toml`](/docs/tako-toml-reference))

- `tako.toml` is required in the project root.
- Defines environments and routes.
- Every non-development environment must define `route` or `routes`.
- Empty route sets are rejected for non-development environments (no implicit catch-all mode).
- Optional `assets` directories are merged into deploy public assets (`<dist>/public`) in listed order.
- Defines server-to-environment mapping via `[servers.<name>] env = "..."`.
- Defines per-server scaling settings (`instances`, `idle_timeout`) via global and per-server overrides.

### Build artifacts (`dist`)

- `tako deploy` reads deployable files from `dist` in `tako.toml` (default `.tako/dist`).
- If your runtime build command runs, it must write deployable files into `dist`.
- If no build command runs, pre-populate `dist` before deploying.
- If `assets` is configured, those directories are merged into `<dist>/public` before packaging.
- On asset merge conflicts, later configured asset roots overwrite earlier files.
- Deploy fails before upload if `dist` is missing or empty.
- Archive payload always includes:
  - all files from `dist` at archive root
  - `app.json` (metadata: app, env, runtime/entry point, env vars, secret names)

### Global server inventory (`~/.tako/config.toml`)

- Defines named servers (`host`, `port`, optional `description`).
- Stores detected per-server build target metadata under `[server_targets.<name>]` (`arch`, `libc`).
- Managed via `tako servers ...` commands.
- Deploy requires target metadata for every selected server and fails early if it is missing/invalid.
- Deploy does not probe target metadata at deploy-time; re-add affected servers to refresh metadata.

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
- Deploy pre-validation fails when target environment is missing secret keys used by other environments.
- Deploy pre-validation warns (does not fail) when target environment has extra secret keys not present in other secret environments.
- Manage secrets with:
  - `tako secrets set`
  - `tako secrets rm`
  - `tako secrets sync`

## Operational Notes

- Use `tako servers status` to inspect deployed app state and per-server service/connectivity state.
- Use `tako logs --env <environment>` to stream remote logs.
- HTTP requests are redirected to HTTPS by default (ACME challenge and `/_tako/status` stay on HTTP).

## Post-Deploy Verification

Right after deploy:

1. Run `tako servers status` and confirm routes/instances are healthy for the target environment/app.
2. Open one or more public routes and validate response headers/body.
3. Tail logs with `tako logs --env <environment>` for startup/runtime errors.
4. If only a subset of servers succeeded, re-run deploy after fixing failed hosts.

## Running Deploy E2E Tests

Deploy E2E tests are opt-in and Docker-backed:

```bash
just e2e e2e/fixtures/js/tanstack-start
```
