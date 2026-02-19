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

- Deploy packages source files into a versioned source archive, then builds target-specific artifacts locally.
- Source bundle root is resolved in this order: git root, current app directory.
- Source filtering uses `.gitignore`.
- Non-overridable excludes: `.git/`, `.tako/`, `.env*`, `node_modules/`, `target/`.
- A versioned source tarball is created under `.tako/artifacts/`.
- Deploy version format: clean git tree => `{commit}`; dirty git tree => `{commit}_{source_hash8}`; no git commit => `nogit_{source_hash8}`.
- Build preset is resolved from top-level `preset` (runtime-local alias or GitHub ref; namespaced aliases like `bun/tanstack-start` are rejected) or adapter base default from top-level `runtime`/detection when omitted, and locked in `.tako/build.lock.json`.
- For each required server target (`arch`/`libc`), Tako runs preset install/build locally and packages a target artifact tarball (Docker/local based on preset `[build].container`; default derived from `[build].targets`).
- Before packaging each target artifact, Tako verifies the resolved deploy `main` file exists in the post-build app directory.
- Docker build containers stay ephemeral, but dependency downloads are reused from per-target Docker cache volumes keyed by cache kind + target label + builder image.
- Target artifacts are cached locally in `.tako/artifacts/` using a deterministic build-input key.
- On cache hit, deploy reuses the verified artifact; on cache mismatch/corruption, deploy rebuilds that target artifact automatically.
- On every deploy, Tako prunes local `.tako/artifacts/` cache (best-effort): keeps 30 newest source archives, keeps 90 newest target artifacts, and removes orphan target metadata files.
- Deploys run to all target servers in parallel.
- On each server, Tako writes final `app.json`, sends merged env/secrets in deploy command payload, performs runtime prep (Bun dependency install), and performs rolling update from the uploaded target artifact.
- Each server is handled independently, so partial success is possible.

## Pre-Deploy Checklist

Before you ship, do a quick sanity pass:

1. Ensure target hosts exist in `~/.tako/config.toml` (or check with `tako servers ls`) and were added with SSH checks enabled so target metadata was detected.
2. Confirm `tako.toml` has route/env/server mappings for the target environment.
3. Verify secrets are present for the target env (`tako secrets sync` if needed).
4. Run your local tests before deploy.
5. Ensure deploy entrypoint is set either in `tako.toml` (`main = "..."`) or preset top-level `main`.
6. Ensure your build output includes that entrypoint path (deploy validates this before artifact packaging).
7. If preset build mode resolves to container (`[build].container = true`), ensure Docker is available locally.

## Server Prerequisites

Each target server should have:

- SSH access as the configured deployment user (typically `tako`).
- `tako-server` installed and running.
- `tako-server` installed via the hosted installer (or equivalent) for the host target; installer resolves `arch` + `libc` and downloads matching `tako-server-linux-<arch>-<libc>`.
- `proto` installed on host (`install-server` attempts distro package manager first, then upstream installer fallback).
- `nc` (netcat), `tar`, `base64`, and standard shell tools (`mkdir`, `find`, `stat`).
- Writable runtime paths under `/opt/tako` and socket access at `/var/run/tako/tako.sock`.
- Privileged bind capability for `tako-server` on `:80/:443` (provided by systemd service capabilities in the installer, plus `setcap` on the binary when available).

## Configuration Inputs

### Project config ([`tako.toml`](/docs/tako-toml))

- `tako.toml` is required in the project root.
- App identity resolves from top-level `name` when set, otherwise sanitized project directory name.
- Set `name` explicitly for stable identity and uniqueness per server; renaming identity later creates a new app path and requires manual cleanup of the old deployment.
- Defines environments and routes.
- Every non-development environment must define `route` or `routes`.
- Empty route sets are rejected for non-development environments (no implicit catch-all mode).
- Optional `main` overrides runtime entrypoint in deployed `app.json`.
- Optional `[build]` controls artifact generation:
  - `include` / `exclude` artifact globs
  - `assets` directories merged into app `public/` after container build in listed order
- Optional top-level preset selection controls runtime/build defaults:
  - `runtime` (optional override: `bun`, `node`, `deno`)
  - `preset` (optional runtime-local override such as `tanstack-start`; defaults to adapter base preset from top-level `runtime` or detection)
- Defines server-to-environment mapping via `[servers.<name>] env = "..."`.
- Defines per-server scaling settings (`instances`, `idle_timeout`) via global and per-server overrides.

### Source bundle and runtime manifest

- Archive payload is source-based and includes filtered files from the resolved source bundle root.
- Archive includes a fallback `app.json` at app path inside the archive.
- Build preset resolves from official alias/GitHub ref and is locked to a commit in `.tako/build.lock.json`.
- Preset runtime fields are top-level `main`/`install`/`start` (legacy preset `[deploy]` is not supported).
- Runtime base presets provide defaults for `dev`/`install`/`start`, `[build].install`/`[build].build`, and `[build].exclude`/`[build].targets`/`[build].container`.
- Preset `[build].exclude` appends to runtime-base excludes (base-first, deduplicated), while preset `[build].targets` and `[build].container` override when set.
- Artifact include precedence is `build.include` then `**/*`; artifact excludes are effective preset `[build].exclude` plus `build.exclude`.
- For each server target label, Tako runs install/build from preset `[build].install` and `[build].build` (Docker/local mode from preset `[build].container`; unset defaults to Docker when `[build].targets` is non-empty).
- Containerized deploy builds reuse per-target dependency cache volumes (proto + runtime cache mounts) while still creating fresh build containers.
- Runtime version is resolved proto-first (`proto run <tool> -- --version`), with fallback to `.prototools`, then `latest`.
- Local artifact cache key includes source hash, target label, resolved preset source/commit, runtime tool/version, Docker/local mode, build commands, include/exclude patterns, asset roots, and app subdirectory.
- `assets` are copied into app `public/` after container build (later entries overwrite earlier ones).
- Final `app.json` is written in app directory after resolving runtime `main`.
- Runtime `main` resolution order:
  1. `main` from `tako.toml`
  2. top-level `main` from preset
- Before artifact packaging, deploy checks that the resolved `main` exists in the built app directory and fails if it is missing.

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
9. Resolve runtime `main`, write final app `app.json`.
10. Send deploy command to `tako-server` including merged environment (`TAKO_BUILD`, runtime vars, user vars, decrypted secrets); `tako-server` runs runtime prep (Bun dependency install) before rolling update.
11. Update `current` symlink after server accepts deploy.
12. Clean old release directories.

## Remote Layout

```text
/opt/tako/apps/<app>/
  current -> releases/<version>
  .deploy_lock/
  releases/
    <version>/
      <app-subdir>/        # "." when deploying from app root
        ...app files...
        app.json
      logs -> /opt/tako/apps/<app>/shared/logs
  shared/
    logs/
```

## Environment and Secrets

- Deploy sends `TAKO_BUILD="<version>"` in the deploy command payload and `tako-server` injects it into app process environment.
- Local encrypted secrets are decrypted during deploy and sent in the deploy command payload for the target environment.
- Bun runtime dependency install runs on server from the uploaded release (`bun install --production`, and `--frozen-lockfile` when lockfile exists).
- Deploy pre-validation fails when target environment is missing secret keys used by other environments.
- Deploy pre-validation warns (does not fail) when target environment has extra secret keys not present in other secret environments.
- Manage secrets with:
  - `tako secrets set`
  - `tako secrets rm`
  - `tako secrets sync`

## Operational Notes

- Use `tako servers status` to inspect deployed app state and per-server service/connectivity state.
- Use `tako logs --env <environment>` to stream remote logs.
- HTTP requests are redirected to HTTPS by default (307 with `Cache-Control: no-store`).
- Exceptions on HTTP: `/.well-known/acme-challenge/*` and internal `Host: tako.internal` + `/status`.
- Forwarded private/local hosts (`localhost`, `*.localhost`, single-label hosts, and reserved suffixes like `*.local`) are treated as already HTTPS when proxy proto metadata is missing to avoid local redirect loops.
- Requests without internal host are routed to apps normally.
- Private/local route hostnames (`localhost`, `*.localhost`, single-label hosts, and reserved suffixes like `*.local`) get self-signed certs during deploy; public hostnames use ACME.

## Post-Deploy Verification

Right after deploy:

1. Run `tako servers status` and confirm routes/instances are healthy for the target environment/app.
2. Open one or more public routes and validate response headers/body.
3. Tail logs with `tako logs --env <environment>` for startup/runtime errors.
4. If only a subset of servers succeeded, re-run deploy after fixing failed hosts.

## Running Deploy E2E Tests

Deploy E2E tests are opt-in and Docker-backed:

```bash
just e2e e2e/fixtures/js/bun
just e2e e2e/fixtures/js/tanstack-start
```
