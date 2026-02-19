---
layout: ../../layouts/DocsLayout.astro
title: Tako Docs - Troubleshooting
heading: Troubleshooting
current: troubleshooting
---

# Troubleshooting

Day-2 checks for local and remote Tako environments.

Use this when things feel weird and you want a clean, repeatable response path.

## Fast Triage (Do This First)

1. Run `tako servers status` (remote state snapshot).
2. Run `tako logs --env <environment>` (live logs across mapped servers).
3. Reproduce once and note if failure is local-only, one-host-only, or all hosts.

This quickly separates config errors from host/runtime failures.

## Local Development Issues (`tako dev`)

Baseline checks:

1. `tako doctor`
2. `tako dev`
3. Open:
   - macOS: `https://{app}.tako.local/`
   - non-macOS: `https://{app}.tako.local:47831/`

If local URL fails:

- Ensure `tako dev` is currently running.
- Re-run `tako doctor` and fix preflight issues it reports.
- On macOS, verify `/etc/resolver/tako.local` points to `127.0.0.1:53535`.

Important local behavior:

- `tako dev` uses fixed HTTPS port `127.0.0.1:47831`.
- On macOS, Tako configures local forwarding and split DNS so you can use `https://{app}.tako.local/` without port suffix.
- On first trust/setup flow, elevated access may be required for local CA trust + forwarding setup.
- If `tako dev` says local forwarding looks inactive, read the diagnostics printed right before sudo: they call out missing pf rules, runtime pf reset (common after reboot), or local listeners on `127.0.0.1:80/443`.

Config-related local failures:

- `[envs.development]` routes must be `{app}.tako.local` or subdomains.
- Dev routing uses exact hostnames; wildcard host entries are ignored.
- If configured dev routes contain no exact hostnames, `tako dev` fails validation.

## Deploy Failures and Partial Success

When `tako deploy` reports mixed success:

1. Identify failed hosts from deploy output.
2. Run `tako servers status` and inspect only failing host blocks.
3. Confirm host prerequisites:
   - `tako-server` installed and running
   - writable `/opt/tako`
   - management socket reachable (`/var/run/tako/tako.sock`)
4. Re-run deploy after fixing host-level issues.

Expected deploy behavior:

- Partial failures are possible: some servers can succeed while others fail.
- Rolling updates are health-gated and auto-rollback on failed health transition.
- Failed partial release directories are auto-cleaned on deploy failure.

## High-Value Failure Modes

- `Deploy lock left behind`:
  - Symptom: deploy fails immediately due to existing lock.
  - Fix: remove stale lock directory on affected host:
    - `/opt/tako/apps/{app}/.deploy_lock`
- `Low disk space under /opt/tako`:
  - Symptom: deploy fails before upload with required vs available sizes.
  - Fix: free space, then redeploy.
- `Local artifact build failed`:
  - Symptom: deploy fails during artifact build before upload.
  - Fix: check preset resolution (explicit top-level runtime-local `preset` or adapter default from top-level `runtime`/detection), app `[[build.stages]]`, and target build logs. Namespaced aliases in `tako.toml` (for example `js/tanstack-start`) are rejected, and `github:` refs are not supported. Build order is preset stage first (`[build].install`/`[build].build`), then app `[[build.stages]]`. JS runtime base presets (`bun`, `node`, `deno`) default to local build mode (`container = false`) unless explicitly set to `true`; if preset build mode resolves to container (`[build].container`), also confirm Docker is available locally.
  - If preset parsing fails, ensure preset artifact filters use `[build].exclude` and runtime/dev commands use top-level `main`/`install`/`start`/`dev` (legacy `[deploy]`, `[dev]`, `include`, `[artifact]`, top-level `dev_cmd`, and `[build].docker` are rejected).
  - Note: containerized builds cache dependencies in Docker volumes prefixed `tako-build-cache-`; if needed, remove stale volumes and redeploy.
- `Deploy entrypoint missing after build`:
  - Symptom: deploy fails during artifact preparation with a message that the deploy entrypoint (`main`) was not found after build.
  - Fix: ensure your build output creates the configured `main` path (from `tako.toml` or preset), or update `main` to the actual generated file.
- `Installer reports unsupported libc`:
  - Symptom: `install-server` exits with `unsupported libc`.
  - Fix: run on Linux host with `glibc` or `musl`; for custom base images, set `TAKO_SERVER_URL` to a known matching artifact URL.
- `Installer failed to install mise`:
  - Symptom: `install-server` exits after reporting mise install failure.
  - Fix: install `mise` manually on the host ([mise install docs](https://mise.jdx.dev/getting-started.html)), ensure `mise` is on `PATH`, then rerun installer (or set `TAKO_INSTALL_MISE=0` to skip installer-managed mise setup).
- `Bun dependency install failed`:
  - Symptom: server responds with `Invalid app release: Bun dependency install failed ...`.
  - Fix: ensure release dependencies are resolvable in production, and Bun lockfile (if present) matches packaged dependency specs.
- `Unexpected local artifact cache behavior`:
  - Symptom: repeated deploy unexpectedly rebuilds or cache warning appears before rebuild.
  - Expected: Tako verifies cached artifact checksum/size and automatically rebuilds if cache is invalid.
  - Expected: each deploy also prunes local `.tako/artifacts/` cache (best-effort), keeping 30 newest source archives and 90 newest target artifacts, and removing orphan target metadata files.
  - Fix: if needed, remove local cache directory `.tako/artifacts/` and redeploy.
- `504 App startup timed out`:
  - Symptom: on-demand app (`instances = 0`) was scaled to zero and did not become healthy within startup timeout (30s default).
  - Fix: check startup logs and health probe readiness.
- `502 App failed to start`:
  - Symptom: cold start failed before the app reached ready/healthy state.
  - Fix: check runtime command, startup errors, and app dependencies.
- `Route mismatch / wrong app`:
  - Verify env route config in [`tako.toml` reference](/docs/tako-toml).
  - Ensure environment has valid `route` or `routes` values.
- `HTTPS 502 / TLS handshake failure on private domains (for example `\*.local`)`:
  - Verify deploy completed after upgrading `tako-server` (private-domain cert generation happens at deploy time).
  - Check cert files exist on host under `/opt/tako/certs/<route-host>/fullchain.pem` and `privkey.pem`.
  - Re-run deploy to regenerate self-signed certs for private/local routes.

## Config and State Edge Cases

From spec-defined behavior:

- `~/.tako/` deleted: auto-recreated on next command.
- `.tako/` deleted: auto-recreated on next deploy.
- `tako.toml` deleted: config-requiring commands fail with guidance to run `tako init`.
- `.tako/secrets` deleted: warning is shown; restore secrets before deploy.
- `~/.tako/config.toml` corrupted: parse error with line context.

## Files and Paths Worth Inspecting

- Local:
  - `{TAKO_HOME}/dev-server.sock`
  - `{TAKO_HOME}/ca/ca.crt`
- Remote:
  - `/var/run/tako/tako.sock`
  - `/opt/tako/apps/<app>/current`
  - `/opt/tako/apps/<app>/releases/<version>/`
  - `/opt/tako/apps/<app>/.deploy_lock`

## Escalation Bundle

If issue remains unresolved, capture:

1. `tako servers status` output
2. `tako logs --env <environment>` output
3. host scope (`one host` vs `all hosts`)
4. route/env/server mapping from [`tako.toml` reference](/docs/tako-toml)
