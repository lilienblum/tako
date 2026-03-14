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
3. Run `tako releases ls --env <environment>` (confirm what is currently deployed and what rollback targets exist).
4. Reproduce once and note if failure is local-only, one-host-only, or all hosts.
5. Re-run the failing command with `--verbose` to capture technical detail when needed.

This quickly separates config errors from host/runtime failures.

## Local Development Issues (`tako dev`)

Baseline checks:

1. `tako doctor`
2. `tako dev`
3. Open:
   - macOS: `https://{app}.tako.test/`
   - non-macOS: `https://{app}.tako.test:47831/`

If local URL fails:

- Ensure `tako dev` is currently running.
- Re-run `tako doctor` and fix preflight issues it reports.
- On macOS, verify `/etc/resolver/tako.test` points to `127.0.0.1:53535`.

Important local behavior:

- `tako dev` uses fixed HTTPS port `127.0.0.1:47831`.
- On macOS, Tako configures local forwarding and split DNS so you can use `https://{app}.tako.test/` without port suffix.
- On first trust/setup flow, elevated access may be required for local CA trust + forwarding setup.
- If `tako dev` says local forwarding looks inactive, read the diagnostics printed right before sudo: they call out missing pf rules, runtime pf reset (common after reboot), or local listeners on `127.0.0.1:80/443`.

Config-related local failures:

- `[envs.development]` routes must be `{app}.tako.test` or subdomains.
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
5. If you need fast mitigation, run `tako releases rollback <release-id> --env <environment>` after confirming target release history.

Expected deploy behavior:

- Partial failures are possible: some servers can succeed while others fail.
- Rolling updates are health-gated and auto-rollback on failed health transition.
- Failed partial release directories are auto-cleaned on deploy failure.

## High-Value Failure Modes

- `Deploy lock left behind`:
  - Symptom: deploy fails immediately due to existing lock.
  - Fix: remove stale lock directory on affected host:
    - `/opt/tako/apps/{app}/{env}/.deploy_lock`
- `Low disk space under /opt/tako`:
  - Symptom: deploy fails before upload with required vs available sizes.
  - Fix: free space, then redeploy.
- `Local artifact build failed`:
  - Symptom: deploy fails during artifact build before upload.
  - Fix: check preset resolution (explicit top-level runtime-local `preset` or adapter default from top-level `runtime`/detection), app `[[build.stages]]`, and target build logs. Namespaced aliases in `tako.toml` (for example `js/tanstack-start`) are rejected, and `github:` refs are not supported. Unpinned official aliases fetch from `master` on each resolve; fetch failures fail resolution, while runtime base aliases (`bun`, `node`, `deno`) fall back to embedded defaults when missing from fetched family manifests. Build order is preset stage first (`[build].install`/`[build].build`), then app `[[build.stages]]`. JS runtime base presets (`bun`, `node`, `deno`) default to local build mode (`container = false`) unless explicitly set to `true`; if preset build mode resolves to container (`[build].container`), also confirm Docker is available locally.
  - If using container builds, confirm Docker can pull default builder images for your targets: `ghcr.io/lilienblum/tako-builder-musl:v1` (`*-musl`) and `ghcr.io/lilienblum/tako-builder-glibc:v1` (`*-glibc`).
  - If preset parsing fails, ensure preset artifact filters use `[build].exclude` and runtime commands use top-level `main`/`install`/`start`. For `tako dev`, explicit top-level `preset` uses preset top-level `dev`; omitted top-level `preset` ignores preset top-level `dev` and runs runtime-default command with resolved `main`.
  - Note: containerized builds cache dependencies in Docker volumes prefixed `tako-build-cache-`; if needed, remove stale volumes and redeploy.
- `Deploy entrypoint missing after build`:
  - Symptom: deploy fails during artifact preparation with a message that the deploy entrypoint (`main`) was not found after build.
  - Fix: ensure your build output creates the configured `main` path (from `tako.toml` or preset), or update `main` to the actual generated file. For JS runtimes (`bun`, `node`, `deno`) with preset `main` set to `index.<ext>` or `src/index.<ext>` (`ts`/`tsx`/`js`/`jsx`), Tako automatically checks `index.<ext>` first, then `src/index.<ext>`.
- `Installer reports unsupported libc`:
  - Symptom: `install-server` exits with `unsupported libc`.
  - Fix: run on Linux host with `glibc` or `musl`; for custom base images, set `TAKO_SERVER_URL` to a known matching artifact URL.
- `Installer reports missing service manager`:
  - Symptom: `install-server` exits with an error that a supported service manager is required.
  - Fix: run on a host with active systemd or OpenRC (`rc-service` + `rc-update`) for normal installs, or rerun with `TAKO_RESTART_SERVICE=0` for install-refresh workflows where init is not active yet.
- `Installer failed to install mise`:
  - Symptom: `install-server` exits after reporting mise install failure.
  - Fix: install `mise` manually on the host ([mise install docs](https://mise.jdx.dev/getting-started.html)), ensure `mise` is on `PATH`, then rerun installer (or set `TAKO_INSTALL_MISE=0` to skip installer-managed mise setup).
- `SSH host key verification failed`:
  - Symptom: server commands fail with host key verification error.
  - Fix: verify the host fingerprint out-of-band, then add/update the entry in local `~/.ssh/known_hosts` for that host/port.
- `Bun dependency install failed`:
  - Symptom: server responds with `Invalid app release: Bun dependency install failed ...`.
  - Fix: ensure release dependencies are resolvable in production, and Bun lockfile (if present) matches packaged dependency specs.
- `Unexpected local artifact cache behavior`:
  - Symptom: repeated deploy unexpectedly rebuilds or cache warning appears before rebuild.
  - Expected: Tako verifies cached artifact checksum/size and automatically rebuilds if cache is invalid.
  - Expected: each deploy also prunes local `.tako/artifacts/` cache (best-effort), keeping 30 newest source archives (`*-source.tar.zst`) and 90 newest target artifacts (`artifact-cache-*.tar.zst`), and removing orphan target metadata files.
  - Fix: if needed, remove local cache directory `.tako/artifacts/` and redeploy.
- `Unexpected edge response cache behavior`:
  - Symptom: stale app response appears on repeated `GET`/`HEAD` requests.
  - Expected: edge proxy caches only when response `Cache-Control` / `Expires` headers explicitly allow it.
  - Fix: for dynamic/user-specific responses, send `Cache-Control: no-store` (or stricter private/no-cache directives) from the app.
- `504 App startup timed out`:
  - Symptom: the app's desired instance count is `0`, it scaled to zero, and cold start did not become healthy within startup timeout (30s default).
  - Fix: check startup logs and health probe readiness.
- `502 App failed to start`:
  - Symptom: cold start failed before the app reached ready/healthy state.
  - Fix: check runtime command, startup errors, and app dependencies.
- `503 App startup queue is full`:
  - Symptom: an app with desired instance count `0` is currently cold-starting and concurrent startup waiters exceeded the queue limit (100 per app by default).
  - Fix: retry shortly (proxy sends `Retry-After: 1`), or keep at least one warm instance with `tako scale 1` for bursty traffic.
- `Prometheus metrics endpoint not responding`:
  - Symptom: scraping `http://127.0.0.1:9898/` returns connection refused or no data.
  - Fix: confirm `tako-server` is running. Check if `--metrics-port 0` was set (disables metrics). Verify you are scraping from localhost (the endpoint is not publicly accessible). Default port is 9898.
- `Route mismatch / wrong app`:
  - Verify env route config in [`tako.toml` reference](/docs/tako-toml).
  - Ensure environment has valid `route` or `routes` values.
- `Path-route static asset 404`:
  - Confirm the asset file exists under the deployed app `public/` directory.
  - For path-prefixed routes (for example `example.com/app/*`), use URLs under that prefix; Tako will also try prefix-stripped static lookup.
- `HTTPS 502 / TLS handshake failure`:
  - Expected behavior: if no certificate matches an SNI hostname yet, Tako serves a fallback self-signed default cert and unmatched hosts/routes can still return `404`.
  - Check cert files on host under `/opt/tako/certs/default/fullchain.pem` and `privkey.pem`.
  - If route certs are missing for deployed private/local hosts, re-run deploy to regenerate self-signed route certs under `/opt/tako/certs/<route-host>/`.

## Config and State Edge Cases

From spec-defined behavior:

- Config/data directory deleted: auto-recreated on next command.
- `.tako/` deleted: auto-recreated on next deploy.
- `tako.toml` deleted: config-requiring commands fail with guidance to run `tako init`.
- `.tako/secrets` deleted: warning is shown; restore secrets before deploy.
- `config.toml` corrupted: parse error with line context.

## Files and Paths Worth Inspecting

- Local:
  - `{TAKO_HOME}/dev-server.sock`
  - `{TAKO_HOME}/ca/ca.crt`
- Remote:
  - `/var/run/tako/tako.sock`
  - `/opt/tako/apps/<app>/<env>/current`
  - `/opt/tako/apps/<app>/<env>/releases/<version>/`
  - `/opt/tako/apps/<app>/<env>/.deploy_lock`

## Escalation Bundle

If issue remains unresolved, capture:

1. `tako servers status` output
2. `tako logs --env <environment>` output
3. host scope (`one host` vs `all hosts`)
4. route/env/server mapping from [`tako.toml` reference](/docs/tako-toml)
