---
layout: ../../layouts/DocsLayout.astro
title: "Troubleshooting deploy failures, TLS issues, and runtime errors - Tako Docs"
heading: Troubleshooting
current: troubleshooting
description: "Troubleshoot common Tako problems including deploy failures, TLS issues, runtime errors, server status problems, and verbose diagnostics."
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

Adding `-v` or `--verbose` to any Tako command switches output to an append-only execution transcript with timestamps and log levels. Each line is formatted as `HH:MM:SS LEVEL message`. It only prints work as it starts or finishes, and DEBUG-level messages are included. This is the single best tool for understanding what Tako is doing under the hood.

```bash
tako deploy --verbose
tako dev --verbose
```

### `--ci`

The `--ci` flag produces deterministic, non-interactive output with no colors or prompts. It stays transcript-style and only emits current work plus final results. If a required prompt value is missing, the command fails with an actionable error message suggesting CLI flags or config to set. Combined with `--verbose`, it stays append-only but still omits colors and timestamps:

```bash
tako deploy --ci --verbose
```

### `tako doctor`

`tako doctor` prints a local diagnostic report covering:

- Dev daemon listen info and status
- Local DNS status
- On macOS: loopback proxy install and load status, boot-helper status, dedicated loopback alias, launchd load status, TCP reachability on `127.77.0.1:443` and `:80`
- On Linux: port redirect status (loopback alias and iptables rules), TCP reachability on `127.77.0.1:443` and `:80`

If the dev daemon is not running, doctor reports `status: not running` with a hint to start `tako dev`. It exits successfully either way, so you can always run it safely.

## Local Development Issues

### Basic health check

1. Run `tako doctor` and fix any issues it reports.
2. Run `tako dev`.
3. Open your app:
   - macOS / Linux: `https://{app}.tako.test/`
   - Other platforms: `https://{app}.tako.test:47831/`

### Dev daemon not starting

If `tako dev` fails to start the daemon:

- **Socket timeout:** `tako dev` waits up to ~15 seconds for the daemon socket after spawn. If this times out, check `{TAKO_HOME}/dev-server.log` for startup errors.
- **Port conflict:** The daemon checks that port 47831 is available before starting. If the port is in use, the daemon exits immediately with an explicit error. Stop whatever is using that port and retry.
- **Missing binary:** If no `tako-dev-server` binary is found:
  - Source checkout: build it with `cargo build -p tako --bin tako-dev-server`
  - Installed CLI: reinstall with `curl -fsSL https://tako.sh/install.sh | sh`
- **Startup failure details:** When daemon startup fails, `tako dev` reports the last lines from `{TAKO_HOME}/dev-server.log`.

### Local URL does not load

Make sure `tako dev` is currently running. If it is, run `tako doctor` and check for preflight failures.

On macOS, verify that `/etc/resolver/tako.test` exists and points to `127.0.0.1:53535`. This file is created automatically during first setup, but macOS updates can sometimes remove it.

### Local CA and HTTPS

Tako generates a root CA on first run and stores the private key in the system keychain (scoped per `{TAKO_HOME}`). Leaf certificates are generated on-the-fly for each app domain.

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

### Linux port redirect

On Linux, Tako uses iptables redirect rules on a loopback alias (`127.77.0.1`) instead of a loopback proxy binary. If `tako doctor` shows the port redirect as inactive:

- Verify the loopback alias exists: `ip addr show dev lo` should include `127.77.0.1`.
- Verify iptables rules are in place: `sudo iptables -t nat -L OUTPUT -n` should show DNAT rules for `127.77.0.1` on ports 443, 80, and 53.
- If rules are missing (e.g. after a reboot without the systemd service), run `tako dev` again to re-apply them.
- Check that `tako-dev-redirect.service` is enabled: `systemctl is-enabled tako-dev-redirect.service`.

### NixOS

On NixOS, `nixos-rebuild` wipes imperative network changes. Tako detects NixOS and prints a `configuration.nix` snippet instead of running setup commands. If port redirect stops working after a rebuild:

- Verify the Tako networking snippet is in your `configuration.nix`.
- Run `nixos-rebuild switch` to re-apply it.
- Restart `tako dev`.

### DNS issues

Tako uses split DNS so `*.tako.test` hostnames resolve locally. The dev daemon answers `A` queries for active `*.tako.test` hosts on `127.0.0.1:53535`.

If DNS resolution fails for `*.tako.test`:

- On macOS, check that `/etc/resolver/tako.test` exists and contains `nameserver 127.0.0.1` and `port 53535`. If the file was removed by an OS update, running `tako dev` again recreates it (may prompt for sudo).
- On Linux, check that `systemd-resolved` is running (`systemctl status systemd-resolved`) and the `tako.test` forward zone is configured. Run `resolvectl query test.tako.test` to test resolution directly.
- Make sure `tako dev` is running (the DNS listener runs inside the dev daemon).

### Dev route configuration

Routes for `[envs.development]` must be `{app}.tako.test` or a subdomain of it. Dev routing matches exact hostnames only; wildcard host entries are ignored. If your configured dev routes contain no exact hostnames, `tako dev` fails with an invalid route error.

### App crashing or restarting in dev

`tako dev` polls `try_wait()` every 500ms to detect when the app process exits. On exit, the route goes idle (proxy stops forwarding) and the next HTTP request triggers a restart (wake-on-request). If your app is crashing repeatedly, check the dev log output for runtime errors or missing dependencies.

Idle shutdown is suppressed while there are in-flight requests, so crashes during active traffic indicate an app-level issue rather than a Tako timeout.

### HTTPS listen port conflict

The dev daemon performs an upfront bind-availability check for port 47831 and exits immediately with an explicit error if the port is already in use. Check for other processes using that port and stop them before running `tako dev`.

## Deploy Issues

### Build failures

**Symptom:** Deploy fails during artifact build before upload.

Things to check:

- **Preset resolution:** Make sure your `preset` value is a runtime-local alias (like `tanstack-start` or `nextjs`), not a namespaced alias (like `js/tanstack-start`, which is rejected). `github:` refs are also not supported.
- **Build commands:** Check your `[build].run` or `[[build_stages]]` entries. These two are mutually exclusive -- you cannot have both. Combining `[build].include`/`[build].exclude` with `[[build_stages]]` is also an error.
- **Working directory:** If using `cwd` in `[build]` or `[[build_stages]]`, make sure the path is relative and does not escape the project root.
- **Preset fetch:** Unpinned official aliases are fetched from `master` on each resolve. If fetch fails, resolution fails. Runtime base aliases (`bun`, `node`, `deno`, `go`) fall back to embedded defaults when missing from fetched family manifests.

### Deploy entrypoint missing after build

**Symptom:** Deploy fails during artifact preparation saying the deploy entrypoint (`main`) was not found after build.

**Fix:** Make sure your build output creates the file specified by `main` (from `tako.toml` or the preset). For JS runtimes with preset `main` set to `index.<ext>` or `src/index.<ext>` (where ext is `ts`, `tsx`, `js`, or `jsx`), Tako checks `index.<ext>` first, then `src/index.<ext>`.

If neither `tako.toml main`, manifest main (e.g. `package.json` `main`), nor preset `main` is set, deploy fails with guidance.

For Next.js deploys, make sure your build is using `withTako(...)` from `tako.sh/nextjs` or otherwise writing `.next/tako-entry.mjs`. If `.next/standalone/server.js` is missing, Tako falls back to `next start`, so the installed `next` package and built `.next/` directory still need to be present.

### Next.js or Turbo cache confusion

**Symptom:** You see `.next/cache` or `.turbo` inside the build workdir locally and are unsure whether they deploy.

**Expected behavior:** Tako restores those local caches into the temporary build workdir when present, but strips them back out of the final deploy artifact. They speed up repeated local builds and are not shipped to servers.

### Concurrent deploy already in progress

**Symptom:** Deploy fails immediately with `Deploy already in progress for app ... Please wait and try again.`

**Fix:** Another deploy for the same app/environment is still running on that server. Wait for it to finish, or check current state with `tako servers status` and retry.

`tako-server` uses an in-memory per-app lock for deploys. No `.deploy_lock` directory is written to disk. If `tako-server` restarted mid-deploy, the in-flight deploy fails and a retry does not require manual lock cleanup.

### Low disk space

**Symptom:** Deploy fails before upload with a message showing required vs. available disk sizes.

**Fix:** Free space under `/opt/tako` on the target server and redeploy. Tako checks disk space before uploading artifacts so you get an early, clear failure.

### Failed deploy cleanup

If a deploy fails after creating a new release directory on the server, Tako automatically removes that partial release directory before returning the error. You should not need to clean up partial releases manually.

### Missing server target metadata

Deploy requires valid `arch` and `libc` metadata for each server in `config.toml`. If a server was added with `--no-test`, this metadata may be missing.

**Fix:** Remove the server with `tako servers rm` and re-add it with `tako servers add` (without `--no-test`) to capture target metadata via SSH.

### SSH host key verification failed

**Symptom:** Server commands fail with a host key verification error.

**Fix:** Verify the host fingerprint out-of-band, then add or update the entry in `~/.ssh/known_hosts` for that host and port.

### Network interruption during deploy

Deploy runs against all servers in parallel. If some servers succeed while others fail due to network issues, Tako reports the partial failure at the end. You can safely retry the deploy, as it is idempotent for servers that already succeeded and the server rejects a second concurrent deploy command for the same app/environment.

### Dependency install failed on server

**Symptom:** Server responds with `Invalid app release: ... dependency install failed ...`.

**Fix:** Make sure your release dependencies are resolvable in production and that your lockfile (if present) matches the packaged dependency specs. The server runs the runtime's package manager production install command on the deployed release (e.g. `bun install --production --frozen-lockfile` for Bun, `npm ci --omit=dev` for npm).

### Artifact cache issues

**Symptom:** Repeated deploys unexpectedly rebuild, or a cache warning appears before rebuild.

**Expected behavior:** Tako verifies cached artifact checksum/size and automatically rebuilds if the cache is invalid. Each deploy also prunes local `.tako/artifacts/` cache (best-effort), keeping 90 newest target artifacts and removing orphan target metadata files.

**Fix:** If cache behavior is unexpected, remove the local cache directory `.tako/artifacts/` and redeploy.

### Deploy pre-validation for secrets

Deploy fails if the target environment is missing secret keys that are used by other environments. Deploy warns (but does not fail) if the target environment has extra secret keys not present in other environments.

## Health Checks and Rolling Updates

### How health checks work

Tako-server probes each instance by sending `GET /status` with `Host: tako` over the instance's private TCP endpoint. The request includes the per-instance internal token header (`X-Tako-Internal-Token`), and the SDK implements and echoes that contract automatically.

- **Probe interval:** 1 second
- **Process exit fast path:** Before each probe, `try_wait()` checks if the process has exited. If so, the instance is immediately marked dead without waiting for the probe timeout.
- **Failure threshold:** 1 failure marks the instance dead and triggers replacement. After the first successful probe confirms the app is healthy, any single probe failure means something is genuinely wrong.
- **Recovery:** A single successful probe resets the failure count and restores the instance to healthy.

### Rolling update failures

During a rolling update, Tako starts a new instance and waits up to 30 seconds for it to pass health checks. If the new instance does not become healthy in time, Tako automatically rolls back: it kills the new instance, keeps the old ones running, and returns an error to the CLI.

When the desired instance count is `0` (scale-to-zero mode), a deploy still starts one warm instance so the app is immediately reachable. If that warm-instance startup fails, the deploy fails.

### Scale-to-zero cold start errors

- **504 App startup timed out:** The app scaled to zero and cold start did not become healthy within the startup timeout (30 seconds by default). Check startup logs and health probe readiness.
- **502 App failed to start:** Cold start failed before the app reached a healthy state. Check the runtime command, startup errors, and app dependencies.
- **503 App startup queue is full:** A cold start is already in progress and concurrent startup waiters exceeded the queue limit (1000 per app by default). Retry shortly (the response includes `Retry-After: 1`), or keep at least one warm instance with `tako scale 1` for bursty traffic.

### Process crash recovery

If an app process crashes, Tako detects it through `try_wait()` before the next health probe and immediately marks the instance dead. Replacement behavior depends on the app's desired instance count and scaling configuration. On-demand apps with desired instances `0` will cold-start when the next request arrives.

## Config Issues

### tako.toml parse errors

If `tako.toml` has syntax errors, commands that require it fail with guidance. Common issues:

- Using both `[build].run` and `[[build_stages]]` (mutually exclusive).
- Using `[build].include`/`[build].exclude` alongside `[[build_stages]]`.
- Using both `route` and `routes` in the same `[envs.*]` block.
- Putting env vars directly inside `[envs.*]` (they belong in `[vars]` / `[vars.*]`).
- Namespaced preset aliases like `js/tanstack-start` (use runtime-local `tanstack-start` with top-level `runtime` instead).
- Empty route lists for non-development environments.

### config.toml corrupted

If your global `config.toml` (stored in the platform config directory) is corrupted, Tako shows a parse error with line number context. You can fix the file manually or delete it and re-add your servers with `tako servers add`.

### .tako/ directory deleted

The `.tako/` directory is auto-recreated on next deploy. No manual action needed.

### secrets.json deleted

If `.tako/secrets.json` is deleted, Tako shows a warning and prompts you to restore secrets before deploying. Secrets are the source of truth locally; they are synced to servers during deploy or via `tako secrets sync`.

### Route validation errors

- Routes must include a hostname (path-only routes like `"/api/*"` are invalid).
- Exact path routes normalize trailing slashes (`example.com/api` and `example.com/api/` are equivalent).
- Development routes must be `{app}.tako.test` or a subdomain of it.
- Wildcard host entries are ignored in dev routing (exact hostnames only).
- Each non-development environment must define `route` or `routes`.

## Server Installation Issues

### Unsupported libc

**Symptom:** `install-server` exits with `unsupported libc`.

**Fix:** Run on a Linux host with `glibc` or `musl`. For custom base images, set `TAKO_SERVER_URL` to a matching artifact URL.

### Missing service manager

**Symptom:** `install-server` exits saying a supported service manager is required.

**Fix:** Run on a host with active systemd or OpenRC. For build/image workflows where init is not active, rerun with `TAKO_RESTART_SERVICE=0` to refresh the binary/users without starting the service.

## Server Runtime Issues

### Server upgrade stuck in upgrading mode

During `tako servers upgrade`, the server enters an internal `upgrading` mode that temporarily rejects mutating management commands (`deploy`, `stop`, `delete`, `update-secrets`). Upgrade mode uses a durable single-owner lock in SQLite.

- If failure happens before the reload signal, the CLI performs best-effort cleanup and exits upgrade mode.
- If the reload was sent but the socket did not become ready within the timeout, the CLI warns that upgrade mode may remain enabled until the primary recovers.
- If the server is stuck in upgrading mode, check that the `tako-server` process is running and healthy. A process restart clears transient state, and the durable upgrade lock can be released once the primary socket becomes responsive.

## TLS and Certificate Issues

### HTTPS 502 or TLS handshake failure

If no certificate matches an SNI hostname yet, Tako serves a fallback self-signed default certificate so the TLS handshake can still complete and routing can return normal HTTP status codes. Unmatched hosts or routes return `404`.

Check cert files on the host under `/opt/tako/certs/default/fullchain.pem` and `privkey.pem`. For private or local hostnames (like `localhost`, `*.localhost`, single-label hosts, or reserved suffixes like `*.local`, `*.test`), Tako skips ACME and generates a self-signed certificate during deploy. If route certs are missing, re-run deploy to regenerate them.

### Wildcard certificates

Routing supports wildcard hosts (like `*.example.com`). Wildcard certificates are issued automatically via DNS-01 challenges when a DNS provider is configured. When wildcard routes are deployed and no DNS provider is configured, deploy prompts interactively for provider credentials. Credentials are stored on the server at `/opt/tako/dns-credentials.env` and the provider name is persisted in `/opt/tako/config.json`.

## Secrets Issues

### Missing encryption key

Secrets are encrypted per-environment using keys stored at `keys/{env}`. If a key file is missing when you try to set a secret, Tako creates it automatically for `tako secrets set`. To share keys between team members, use `tako secrets key export` and `tako secrets key import`.

### Deleted secrets file

If `.tako/secrets.json` is deleted, Tako shows a warning and prompts you to restore secrets before deploying. Secrets are the source of truth locally; they are synced to servers during deploy or via `tako secrets sync`.

### Secrets sync

`tako secrets sync` pushes local secrets to all servers in the target environment. If `--env` is not specified, it syncs all environments. Secrets are sent to `tako-server`, which stores them encrypted in its SQLite state database and triggers a rolling restart so fresh instances receive updated secrets via fd 3.

Environments with no mapped servers are skipped with a warning.

## Routing Issues

### Route mismatch or wrong app

Verify your environment route config in `tako.toml`. Each non-development environment must define `route` or `routes`. Make sure the hostnames and paths match what you expect. Requests are matched to the most specific route (exact beats wildcard, longer path beats shorter).

### Path-route static asset 404

For path-prefixed routes (like `example.com/app/*`), make sure your asset files exist under the deployed app's `public/` directory. Tako also tries a prefix-stripped static lookup, so `/app/assets/main.js` will look for `/assets/main.js` in the public directory.

### Unexpected edge response cache behavior

**Symptom:** Stale app responses appear on repeated `GET`/`HEAD` requests.

**Expected behavior:** The edge proxy caches only when the response `Cache-Control` or `Expires` headers explicitly allow it. There are no implicit TTL defaults.

**Fix:** For dynamic or user-specific responses, send `Cache-Control: no-store` (or stricter private/no-cache directives) from your app.

## Prometheus Metrics

### Metrics endpoint not responding

**Symptom:** Scraping `http://127.0.0.1:9898/` returns connection refused or no data.

**Fix:**

- Confirm `tako-server` is running.
- Check if `--metrics-port 0` was set (this disables metrics entirely).
- Make sure you are scraping from localhost; the endpoint is not publicly accessible.
- The default port is 9898.

## Recovery Paths

### tako doctor

Run `tako doctor` from any directory for a local diagnostic report. It checks dev daemon status, DNS resolution, and platform-specific networking (loopback proxy on macOS, iptables on Linux). Doctor exits successfully even when the daemon is not running, so it is always safe to run.

### tako releases rollback

Roll back to a previously deployed release:

```bash
tako releases rollback {release-id} --env production
```

Rollback reuses the current app routes, env vars, secrets, and scaling config, then switches the runtime path and version to the target release and runs the standard rolling-update flow. Partial failures are reported per server; successful servers remain rolled back.

### tako implode

Remove the local Tako CLI and all local data:

```bash
tako implode
```

This removes config directories, data directories, CLI binaries (`tako`, `tako-dev-server`, `tako-loopback-proxy`), and platform-specific system-level items (launchd services on macOS, systemd services on Linux, CA certificates, loopback aliases). It asks for confirmation before proceeding.

### tako servers implode

Remove tako-server and all data from a remote server:

```bash
tako servers implode {server-name}
```

This stops and disables services, removes binaries, data (`/opt/tako/`), and sockets (`/var/run/tako/`), then removes the server from your local `config.toml`. Requires confirmation unless `-y` is passed.

### Re-adding servers for target metadata

If deploy fails because a server is missing `arch`/`libc` metadata (common when the server was added with `--no-test`):

```bash
tako servers rm {name}
tako servers add {host} --name {name}
```

The re-add with SSH checks captures the target metadata automatically.

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
- `/opt/tako/certs/{domain}/` -- TLS certificates

## Gathering an Escalation Bundle

If the issue remains unresolved, capture the following and share it when seeking help:

1. `tako servers status` output
2. `tako logs --env <environment>` output
3. Whether the failure affects one host, some hosts, or all hosts
4. Your route/env/server mapping from `tako.toml`
5. Output of the failing command with `--verbose`
