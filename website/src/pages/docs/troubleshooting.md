---
layout: ../../layouts/DocsLayout.astro
title: "Tako Docs - Troubleshooting"
heading: Troubleshooting
current: troubleshooting
---

# Troubleshooting

This guide covers common issues you may run into with Tako and how to resolve them. Start with the quick triage steps, then jump to the section that matches your situation.

## Quick Triage

Before diving into specific issues, run through these steps to narrow down the problem:

1. Run `tako doctor` for a local diagnostic report.
2. Run `tako servers status` for a snapshot of remote server state.
3. Run `tako logs --env <environment>` to check recent log output.
4. Re-run the failing command with `--verbose` to get a detailed execution transcript.

This usually tells you whether the issue is local (your machine), remote (one server), or systemic (all servers).

## Debugging Flags

### `--verbose`

Adding `-v` or `--verbose` to any Tako command switches output to an append-only execution transcript with timestamps and log levels. Each line is formatted as `HH:MM:SS LEVEL message`. Spinners are replaced with log lines, and DEBUG-level messages are included. This is the single best tool for understanding what Tako is doing under the hood.

```bash
tako deploy --verbose
tako dev --verbose
```

### `--ci`

The `--ci` flag produces deterministic, non-interactive output with no colors, spinners, or prompts. If a required prompt value is missing, the command fails with an actionable error message suggesting CLI flags or config to set. Combine it with `--verbose` for maximum detail in automated environments:

```bash
tako deploy --ci --verbose
```

### `tako doctor`

`tako doctor` prints a local diagnostic report covering:

- Dev daemon listen info and status
- macOS loopback proxy install and load status
- macOS loopback boot-helper status
- Dedicated loopback alias status
- launchd load status
- TCP reachability on the loopback address (ports 443 and 80)
- Local DNS status

If the dev daemon is not running, doctor reports `status: not running` with a hint to start `tako dev`. It exits successfully either way, so you can always run it safely.

## Local Development Issues

### Basic health check

1. Run `tako doctor` and fix any issues it reports.
2. Run `tako dev`.
3. Open your app:
   - macOS: `https://{app}.tako.test/`
   - Other platforms: `https://{app}.tako.test:47831/`

### Local URL does not load

Make sure `tako dev` is currently running. If it is, run `tako doctor` and check for preflight failures.

On macOS, verify that `/etc/resolver/tako.test` exists and points to `127.0.0.1:53535`. This file is created automatically during first setup, but macOS updates can sometimes remove it.

### Local CA and HTTPS

Tako generates a root CA on first run and stores the private key in the system keychain. Leaf certificates are generated on-the-fly for each app domain.

- The public CA cert is at `{TAKO_HOME}/ca/ca.crt` (useful for `NODE_EXTRA_CA_CERTS`).
- On first run, `tako dev` installs the root CA into the system trust store and may prompt for your password. It explains why elevated access is needed before prompting.
- Once trusted, there should be no browser security warnings.

If you see certificate errors after reinstalling Tako or changing `TAKO_HOME`, the CA key in the keychain may not match the CA cert on disk. Run `tako dev` again and it will re-establish trust.

### macOS loopback proxy

On macOS, Tako configures a launchd-managed loopback proxy so you can access apps on standard ports (443 for HTTPS, 80 for HTTP redirect) without specifying a port number. The proxy uses a dedicated loopback address (`127.77.0.1`) and forwards traffic:

- `127.77.0.1:443` to `127.0.0.1:47831`
- `127.77.0.1:80` to `127.0.0.1:47830`

If `tako dev` reports the loopback proxy looks inactive, run `tako doctor` and check the preflight results. The proxy is socket-activated and may exit after a long idle window; launchd reactivates it on the next request. If the proxy needs reinstallation, `tako dev` explains it is reloading or reinstalling the launchd helper before prompting for sudo.

After applying or repairing the loopback proxy, Tako retries reachability on ports 80 and 443 and fails startup if they remain unreachable. If the daemon is reachable on `127.0.0.1:47831` but `https://{app}.tako.test/` still fails, Tako reports a targeted hint that the loopback proxy is not forwarding correctly.

### DNS issues

Tako uses split DNS for `tako.test` on macOS by writing `/etc/resolver/tako.test` (one-time sudo), pointing to a local DNS listener on `127.0.0.1:53535`. The dev daemon answers `A` queries for active `*.tako.test` hosts.

If DNS resolution fails for `*.tako.test`:

- Check that `/etc/resolver/tako.test` exists and contains `nameserver 127.0.0.1` and `port 53535`.
- Make sure `tako dev` is running (the DNS listener runs inside the dev daemon).
- On macOS, if the file was removed by an OS update, running `tako dev` again recreates it (may prompt for sudo).

### Dev route configuration

Routes for `[envs.development]` must be `{app}.tako.test` or a subdomain of it. Dev routing matches exact hostnames only; wildcard host entries are ignored. If your configured dev routes contain no exact hostnames, `tako dev` fails with an invalid route error.

### HTTPS listen port conflict

The dev daemon performs an upfront bind-availability check for port 47831 and exits immediately with an explicit error if the port is already in use. Check for other processes using that port and stop them before running `tako dev`.

## Deploy Issues

### Deploy lock left behind

**Symptom:** Deploy fails immediately with a message about an existing lock.

**Fix:** A previous deploy crashed mid-flight and left a lock directory behind. Remove it manually on the affected server:

```
/opt/tako/apps/{app}/{env}/.deploy_lock
```

The lock is an atomic `mkdir` on the server. It prevents concurrent deploys of the same app/environment on the same server. Normally it is released at the end of deploy, but if the process crashes, it must be removed by hand.

### Low disk space

**Symptom:** Deploy fails before upload with a message showing required vs. available disk sizes.

**Fix:** Free space under `/opt/tako` on the target server and redeploy. Tako checks disk space before uploading artifacts so you get an early, clear failure.

### Failed deploy cleanup

If a deploy fails after creating a new release directory on the server, Tako automatically removes that partial release directory before returning the error. You should not need to clean up partial releases manually.

### Build failures

**Symptom:** Deploy fails during artifact build before upload.

Things to check:

- **Preset resolution:** Make sure your `preset` value is a runtime-local alias (like `tanstack-start`), not a namespaced alias (like `js/tanstack-start`, which is rejected). `github:` refs are also not supported.
- **Build order:** Preset build commands run first (`[build].install` then `[build].build`), followed by app `[[build.stages]]` in declaration order.
- **Container builds:** If your preset sets `[build].container = true`, make sure Docker is available locally and can pull the default builder images (`ghcr.io/lilienblum/tako-builder-musl:v1` for `*-musl` targets, `ghcr.io/lilienblum/tako-builder-glibc:v1` for `*-glibc` targets).
- **JS runtime builds:** JS runtime base presets (`bun`, `node`, `deno`) default to local build mode (`container = false`) unless explicitly set otherwise.
- **Preset fetch:** Unpinned official aliases are fetched from `master` on each resolve. If fetch fails, resolution fails. Runtime base aliases (`bun`, `node`, `deno`) fall back to embedded defaults when missing from fetched family manifests.
- **Stale Docker cache:** Containerized builds cache dependencies in Docker volumes prefixed `tako-build-cache-`. If needed, remove stale volumes and redeploy.

### Deploy entrypoint missing after build

**Symptom:** Deploy fails during artifact preparation saying the deploy entrypoint (`main`) was not found after build.

**Fix:** Make sure your build output creates the file specified by `main` (from `tako.toml` or the preset). For JS runtimes with preset `main` set to `index.<ext>` or `src/index.<ext>` (where ext is `ts`, `tsx`, `js`, or `jsx`), Tako checks `index.<ext>` first, then `src/index.<ext>`.

### Artifact cache issues

**Symptom:** Repeated deploys unexpectedly rebuild, or a cache warning appears before rebuild.

**Expected behavior:** Tako verifies cached artifact checksum/size and automatically rebuilds if the cache is invalid. Each deploy also prunes local `.tako/artifacts/` cache (best-effort), keeping 30 newest source archives and 90 newest target artifacts, and removing orphan target metadata files.

**Fix:** If cache behavior is unexpected, remove the local cache directory `.tako/artifacts/` and redeploy.

### Network interruption during deploy

Deploy runs against all servers in parallel. If some servers succeed while others fail due to network issues, Tako reports the partial failure at the end. You can safely retry the deploy, as it is idempotent for servers that already succeeded (the deploy lock prevents double-deploy on the same server).

### Missing server target metadata

Deploy requires valid `arch` and `libc` metadata for each server in `config.toml`. If a server was added with `--no-test`, this metadata may be missing.

**Fix:** Remove the server with `tako servers rm` and re-add it with `tako servers add` (without `--no-test`) to capture target metadata via SSH.

### Bun dependency install failed on server

**Symptom:** Server responds with `Invalid app release: Bun dependency install failed ...`.

**Fix:** Make sure your release dependencies are resolvable in production and that your Bun lockfile (if present) matches the packaged dependency specs. The server runs `bun install --production` (plus `--frozen-lockfile` when a lockfile is present) on the deployed release.

## Health Checks and Rolling Updates

### How health checks work

Tako-server probes each instance by sending `GET /status` with `Host: tako` over the instance's Unix socket. The SDK implements this endpoint automatically.

- **Probe interval:** 1 second
- **Unhealthy threshold:** 2 consecutive failures removes the instance from the load balancer
- **Dead threshold:** 5 consecutive failures kills the instance process
- **Recovery:** A single successful probe resets the failure count and restores the instance to healthy

### Rolling update failures

During a rolling update, Tako starts a new instance and waits up to 30 seconds for it to pass health checks. If the new instance does not become healthy in time, Tako automatically rolls back: it kills the new instance, keeps the old ones running, and returns an error to the CLI.

When the desired instance count is `0` (scale-to-zero mode), a deploy still starts one warm instance so the app is immediately reachable. If that warm-instance startup fails, the deploy fails.

### Scale-to-zero cold start errors

- **504 App startup timed out:** The app scaled to zero and cold start did not become healthy within the startup timeout (30 seconds by default). Check startup logs and health probe readiness.
- **502 App failed to start:** Cold start failed before the app reached a healthy state. Check the runtime command, startup errors, and app dependencies.
- **503 App startup queue is full:** A cold start is already in progress and concurrent startup waiters exceeded the queue limit (100 per app by default). Retry shortly (the response includes `Retry-After: 1`), or keep at least one warm instance with `tako scale 1` for bursty traffic.

### Process crash recovery

If an app process crashes, Tako detects it through health checks (within a few seconds) and handles it based on the app's desired instance count and scaling configuration. On-demand apps with desired instances `0` will cold-start when the next request arrives.

## Server Installation Issues

### Unsupported libc

**Symptom:** `install-server` exits with `unsupported libc`.

**Fix:** Run on a Linux host with `glibc` or `musl`. For custom base images, set `TAKO_SERVER_URL` to a matching artifact URL.

### Missing service manager

**Symptom:** `install-server` exits saying a supported service manager is required.

**Fix:** Run on a host with active systemd or OpenRC. For build/image workflows where init is not active, rerun with `TAKO_RESTART_SERVICE=0` to refresh the binary/users without starting the service.

### Failed to install proto

**Symptom:** `install-server` exits after reporting a proto install failure.

**Fix:** Install `proto` manually on the host ([proto install docs](https://moonrepo.dev/docs/proto/install)), make sure it is on `PATH`, then rerun the installer. Alternatively, set `TAKO_INSTALL_PROTO=0` to skip installer-managed proto setup.

### SSH host key verification failed

**Symptom:** Server commands fail with a host key verification error.

**Fix:** Verify the host fingerprint out-of-band, then add or update the entry in `~/.ssh/known_hosts` for that host and port.

## Routing Issues

### Route mismatch or wrong app

Verify your environment route config in `tako.toml`. Each non-development environment must define `route` or `routes`. Make sure the hostnames and paths match what you expect. Requests are matched to the most specific route (exact beats wildcard, longer path beats shorter).

### Path-route static asset 404

For path-prefixed routes (like `example.com/app/*`), make sure your asset files exist under the deployed app's `public/` directory. Tako also tries a prefix-stripped static lookup, so `/app/assets/main.js` will look for `/assets/main.js` in the public directory.

### Unexpected edge response cache behavior

**Symptom:** Stale app responses appear on repeated `GET`/`HEAD` requests.

**Expected behavior:** The edge proxy caches only when the response `Cache-Control` or `Expires` headers explicitly allow it. There are no implicit TTL defaults.

**Fix:** For dynamic or user-specific responses, send `Cache-Control: no-store` (or stricter private/no-cache directives) from your app.

## TLS and Certificate Issues

### HTTPS 502 or TLS handshake failure

If no certificate matches an SNI hostname yet, Tako serves a fallback self-signed default certificate. Unmatched hosts or routes can still return `404`.

Check cert files on the host under `/opt/tako/certs/default/fullchain.pem` and `privkey.pem`. If route certs are missing for deployed private or local hosts, re-run deploy to regenerate self-signed route certs under `/opt/tako/certs/<route-host>/`.

### Wildcard certificates

Routing supports wildcard hosts (like `*.example.com`), but automated ACME issuance currently uses HTTP-01, which does not support wildcard certificates. Wildcard certs must be provisioned manually and placed in `/opt/tako/certs/{domain}/`.

## Secrets Issues

### Missing encryption key

Secrets are encrypted per-environment using keys stored at `keys/{env}`. If a key file is missing when you try to read or set a secret, Tako creates it automatically for `tako secrets set`. To share keys between team members, use `tako secrets key export` and `tako secrets key import`.

### Deleted secrets file

If `.tako/secrets.json` is deleted, Tako shows a warning and prompts you to restore secrets before deploying. Secrets are the source of truth locally; they are synced to servers during deploy or via `tako secrets sync`.

### Secrets sync

`tako secrets sync` pushes local secrets to all servers in the target environment. If `--env` is not specified, it syncs all environments. Secrets are sent to `tako-server`, which writes them to a per-app `secrets.json` file (with 0600 permissions) and triggers a rolling restart of running instances.

Environments with no mapped servers are skipped with a warning.

### Deploy pre-validation for secrets

Deploy fails if the target environment is missing secret keys that are used by other environments. Deploy warns (but does not fail) if the target environment has extra secret keys not present in other environments.

## Prometheus Metrics

### Metrics endpoint not responding

**Symptom:** Scraping `http://127.0.0.1:9898/` returns connection refused or no data.

**Fix:**

- Confirm `tako-server` is running.
- Check if `--metrics-port 0` was set (this disables metrics entirely).
- Make sure you are scraping from localhost; the endpoint is not publicly accessible.
- The default port is 9898.

## Config and State Recovery

Tako is designed to recover gracefully from deleted files:

| What was deleted        | What happens                                                |
| ----------------------- | ----------------------------------------------------------- |
| Config/data directory   | Auto-recreated on next command                              |
| `.tako/` directory      | Auto-recreated on next deploy                               |
| `tako.toml`             | Commands that need it fail with guidance to run `tako init` |
| `.tako/secrets.json`    | Warning shown; restore secrets before deploying             |
| `config.toml` corrupted | Parse error with line number context                        |

## Key Paths to Inspect

When debugging, these files are often helpful:

**Local:**

- `{TAKO_HOME}/dev-server.sock` -- dev daemon socket
- `{TAKO_HOME}/ca/ca.crt` -- local CA certificate
- `{TAKO_HOME}/dev-server.log` -- dev daemon log (check this if daemon startup fails)
- `{TAKO_HOME}/certs/fullchain.pem` -- daemon TLS cert
- `{TAKO_HOME}/certs/privkey.pem` -- daemon TLS key

**Remote:**

- `/var/run/tako/tako.sock` -- management socket (symlink to active server socket)
- `/opt/tako/apps/{app}/{env}/current` -- symlink to current release
- `/opt/tako/apps/{app}/{env}/releases/{version}/` -- release files
- `/opt/tako/apps/{app}/{env}/.deploy_lock` -- deploy lock directory
- `/opt/tako/certs/{domain}/` -- TLS certificates

## Gathering an Escalation Bundle

If the issue remains unresolved, capture the following and share it when seeking help:

1. `tako servers status` output
2. `tako logs --env <environment>` output
3. Whether the failure affects one host, some hosts, or all hosts
4. Your route/env/server mapping from `tako.toml`
5. Output of the failing command with `--verbose`
