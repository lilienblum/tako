---
layout: ../../layouts/DocsLayout.astro
title: "Troubleshooting deploy failures, TLS issues, and runtime errors - Tako Docs"
heading: Troubleshooting
current: troubleshooting
description: "Troubleshoot common Tako problems including deploy failures, TLS issues, runtime errors, server status problems, and verbose diagnostics."
---

# Troubleshooting

This page is a problem-solution reference. Skim the section that matches your situation and follow the fix steps. Each issue is tagged with a symptom and a concrete recovery path.

## Quick Triage

When something breaks, work through these four steps before diving deeper. They almost always narrow the problem down to local, one server, or systemic:

1. `tako doctor` — local diagnostic report (dev daemon, DNS, dev proxy, loopback alias, port reachability).
2. `tako servers status` — snapshot of every configured server and the apps deployed on them.
3. `tako logs --env <env>` — recent log output across all servers in that environment.
4. Re-run the failing command with `--verbose` — append-only transcript with timestamps and DEBUG-level detail.

## Debugging Flags

- `--verbose` (`-v`) — switches any command to a timestamped append-only transcript with DEBUG lines. Best single tool for understanding what Tako is doing.
- `--ci` — deterministic, no colors, no spinners, no prompts. Fails fast with actionable errors when a prompt value is missing. Combines with `--verbose`.
- `tako doctor` — local health report. Safe to run from any directory, exits 0 even when the daemon is not running.

Full flag reference lives on the [CLI page](/docs/cli).

## Local Development

### Baseline check

1. Run `tako doctor` and fix anything it flags.
2. Run `tako dev` from the app directory.
3. Open the app:
   - macOS / Linux: `https://{app}.test/`
   - Other platforms: `https://{app}.test:47831/`

### Dev daemon won't start

**Symptom:** `tako dev` reports that the daemon did not come up, or the socket never appeared.

**Fix:**

- **Socket timeout (~15s):** `tako dev` waits up to 15 seconds for the daemon socket after spawn. If that window elapses, read the last lines of `{TAKO_HOME}/dev-server.log` — the daemon prints startup errors there.
- **Port 47831 already bound:** the daemon runs an upfront bind check on its fixed HTTPS port and exits immediately if it's taken. Stop whatever else is listening on 47831 and retry.
- **Missing `tako-dev-server` binary:**
  - Source checkout: `cargo build -p tako --bin tako-dev-server`.
  - Installed CLI: `curl -fsSL https://tako.sh/install.sh | sh`.

### `https://{app}.test/` doesn't load

**Symptom:** Browser can't reach the dev URL, or gets a TLS / connection error.

**Fix:**

1. Confirm `tako dev` is still running in the foreground.
2. Run `tako doctor` and address any preflight failures.
3. On macOS, verify resolver files:
   - `/etc/resolver/test`
   - `/etc/resolver/tako.test`
   - Both must contain `nameserver 127.0.0.1` and `port 53535`.
4. On macOS, if `127.0.0.1:47831` is reachable but `https://{app}.test/` still fails, the launchd dev proxy is not forwarding — see below.

### Local CA / HTTPS

Tako generates a root CA on first run and stores the private key in the system keychain (scoped per `{TAKO_HOME}`). Leaf certs are minted on demand per app domain.

- Public CA cert: `{TAKO_HOME}/ca/ca.crt` — set this as `NODE_EXTRA_CA_CERTS` for Node-based tools.
- First run installs the CA into your system trust store and prompts for a password. Tako explains why before prompting.

**Symptom:** browser shows a cert warning after reinstalling Tako or changing `TAKO_HOME`.

**Fix:** the keychain key no longer matches the on-disk cert. Run `tako dev` again — it re-establishes trust.

### iOS still says "Not Private"

**Symptom:** iOS shows a "Not Private" warning even after installing the Tako CA profile.

**Fix:** installing the profile isn't enough on iOS. Go to **Settings → General → About → Certificate Trust Settings** and enable full trust for `Tako Development CA`. That screen only appears after a CA profile is installed.

### Wildcard subdomain route doesn't reach my phone in LAN mode

**Symptom:** concrete LAN hosts like `app.local` work from a phone, but `*.app.local` does not.

**Fix:** mDNS only advertises concrete records. Wildcard dev routes cannot be discovered from phones over mDNS. Add the specific subdomain you need as an explicit route in `[envs.development]`:

```toml
[envs.development]
routes = ["*.app.test", "api.app.test"]
```

The wildcard keeps matching for clients that resolve through their own DNS; the explicit host is what phones can discover.

### macOS dev proxy inactive

**Symptom:** `tako doctor` reports the dev proxy is inactive, or the daemon is reachable on `127.0.0.1:47831` but `https://{app}.test/` doesn't load.

**Fix:** the dev proxy is a launchd-managed helper that listens on `127.77.0.1:80` and `:443` and forwards to the daemon on `127.0.0.1:47830/47831`. It's socket-activated and may idle out; launchd reactivates it on the next request. If it's truly broken, re-run `tako dev` — Tako will reinstall or repair the launchd helper (one-time sudo) and retry port 80 / 443 reachability. Startup fails if those checks don't pass.

### Linux port redirect missing

**Symptom:** `tako doctor` shows the Linux port redirect as inactive after a reboot or rule flush.

**Fix:**

- Loopback alias: `ip addr show dev lo` should include `127.77.0.1`.
- iptables rules: `sudo iptables -t nat -L OUTPUT -n` should list DNAT entries for `127.77.0.1` on ports 443, 80, and 53.
- Persistence service: `systemctl is-enabled tako-dev-redirect.service`.

Re-running `tako dev` re-applies the rules via the systemd oneshot service.

### NixOS

**Symptom:** port redirect works after first setup, then stops working after `nixos-rebuild switch`.

**Fix:** NixOS wipes imperative network changes on rebuild. Tako detects NixOS and prints a `configuration.nix` snippet instead of running setup commands. Paste that snippet into your config, run `nixos-rebuild switch`, then restart `tako dev`.

### DNS doesn't resolve for `*.test` or `*.tako.test`

**Symptom:** `dig myapp.test` returns nothing or the wrong IP.

**Fix:**

- macOS: check `/etc/resolver/test` and `/etc/resolver/tako.test`. Each file should contain `nameserver 127.0.0.1` and `port 53535`. If an OS update removed them, re-running `tako dev` recreates them (may prompt for sudo). If `/etc/resolver/test` already existed and wasn't written by Tako, Tako leaves it alone and warns — `.tako.test` still works as a fallback.
- Linux: check `systemctl status systemd-resolved` and confirm the forward zones are in place (`resolvectl status` should show `Domains=~tako.test ~test`). Use `resolvectl query myapp.test` to test resolution directly.
- The DNS listener runs inside the dev daemon on `127.0.0.1:53535`. If `tako dev` isn't running, resolution will fail.

### Dev route config errors

**Symptom:** `tako dev` exits with an invalid route error.

**Fix:** development routes must sit under `.test` or `.tako.test` — either `{app}.test` / `{app}.tako.test` or a subdomain of one of those. Dev routing only matches exact hostnames; wildcard hosts are ignored in dev. If your `[envs.development]` block has no exact hostnames, startup fails.

### App keeps crashing or restarting in dev

**Symptom:** the app process keeps dying and the dev daemon keeps restarting it on request.

**Fix:** the daemon polls `try_wait()` every 500ms. When the process exits, the route goes idle and the next HTTP request triggers a restart (wake-on-request). Idle shutdown is suppressed while in-flight requests exist, so mid-request exits are always an app-level issue. Check the `tako dev` log stream for runtime errors, missing modules, or unhandled exceptions.

### HTTPS port 47831 conflict

**Symptom:** `tako dev` exits immediately saying the HTTPS listen address is unavailable.

**Fix:** the dev daemon's HTTPS port is fixed at `47831`. Find and stop the other process listening there (`lsof -i :47831` on macOS/Linux), then retry.

## Deploy Issues

### Build failures

**Symptom:** deploy aborts during the local build before upload.

**Fix — check each of these:**

- **Preset value:** must be a runtime-local alias (`tanstack-start`, `nextjs`, …). Namespaced aliases like `js/tanstack-start` are rejected. `github:` refs are not supported.
- **Build commands mutual exclusion:** `[build].run` and `[[build_stages]]` cannot coexist. `[build].include` / `[build].exclude` cannot be combined with `[[build_stages]]` — use per-stage `exclude` instead.
- **`cwd` escape:** any `cwd` value in `[build]` or `[[build_stages]]` must be a relative path that stays inside the project root.
- **Preset fetch failure:** unpinned official aliases are refreshed from `master` on each deploy. If GitHub is unreachable, Tako falls back to the locally cached manifest (and for runtime base aliases like `bun`/`node`/`deno`/`go`, to embedded defaults). Clear issues with the preset manifest may need to wait for connectivity.

### Deploy entrypoint missing after build

**Symptom:** deploy fails during artifact packaging with a missing-entrypoint error.

**Fix:** make sure the build output actually produces the file referenced by `main`. Resolution order is `main` in `tako.toml` → manifest `main` (e.g. `package.json`) → preset `main`. For JS runtimes where preset `main` points at `index.<ext>` or `src/index.<ext>` (`ts`, `tsx`, `js`, `jsx`), Tako checks `index.<ext>` first, then `src/index.<ext>`. If nothing resolves, deploy fails with guidance.

For Next.js, make sure the build uses `withTako(...)` from `tako.sh/nextjs` so `.next/tako-entry.mjs` is written. If `.next/standalone/server.js` is missing, Tako falls back to `next start`, which still requires the `next` package and a built `.next/` directory.

### Next.js / Turbo cache confusion

**Symptom:** you spot `.next/cache` or `.turbo` inside `.tako/build/` and worry they'll end up on servers.

**Fix:** this is expected. Tako restores those local caches into `.tako/build` to speed up repeated local builds, but strips them back out of the final deploy artifact. They do not ship.

### Concurrent server-side deploy

**Symptom:** deploy fails immediately with `Deploy already in progress for app '{app}'. Please wait and try again.`

**Fix:** `tako-server` serializes deploys per `{app}/{env}` with an in-memory lock. Wait for the other deploy to finish, check `tako servers status`, and retry. No disk state to clean — a `tako-server` restart clears the lock automatically, and the interrupted deploy just fails cleanly.

### Concurrent local deploy

**Symptom:** the CLI exits immediately with `Another deploy is already running for this project (PID ...)`.

**Fix:** another non-dry-run `tako deploy` holds the advisory flock at `.tako/deploy.lock`. Wait for that PID to finish or inspect and stop it, then retry. The lock is advisory `flock` state, so crashed processes release it automatically — no manual deletion required.

### Low disk space

**Symptom:** deploy fails before upload with required vs available disk sizes for `/opt/tako`.

**Fix:** free space on the target server under `/opt/tako`, then redeploy. Tako sizes the preflight check from archive size plus unpack headroom, so the reported numbers tell you exactly how much room you need.

### Failed deploy cleanup

**Symptom:** a deploy failed partway through on the server.

**Fix:** nothing manual. When a deploy fails after creating a new release directory, `tako deploy` removes that partial release directory before returning the error. You do not need to clean up `releases/` by hand.

### Missing server target metadata

**Symptom:** deploy fails early saying the server is missing `arch` or `libc`.

**Fix:** the server was likely added with `--no-test`, which skips SSH checks and target detection. Remove and re-add it without `--no-test`:

```bash
tako servers rm {name}
tako servers add {host} --name {name}
```

The re-add captures `arch` and `libc` via SSH.

### SSH host key verification failed

**Symptom:** server commands fail with a host-key-verification error.

**Fix:** verify the fingerprint out-of-band, then update the corresponding entry in `~/.ssh/known_hosts` for that host and port.

### Network interruption mid-deploy

**Symptom:** some servers report success, others fail with a network error.

**Fix:** deploy runs across all servers in parallel and reports partial failures at the end. Retry is safe: successful servers are idempotent, and the server-side per-app lock rejects any duplicate concurrent deploy for the same `{app}/{env}`.

### Dependency install failed on server

**Symptom:** server responds with `Invalid app release: ... dependency install failed ...`.

**Fix:** the runtime plugin's production install command ran against the deployed release and exited non-zero (for example `bun install --production --frozen-lockfile`, `npm ci --omit=dev`). Check:

- Your lockfile matches the dependency specs in the release.
- Dependencies resolve without dev tools or private registries that aren't reachable from the server.
- Any postinstall scripts work in a headless environment.

### Artifact cache issues

**Symptom:** repeated deploys rebuild unexpectedly, or you see a cache-invalidated warning.

**Fix:** this is usually working as intended. Tako verifies cache entries by checksum and size before reuse and automatically rebuilds anything invalid. Each deploy also best-effort prunes `.tako/artifacts/` to the 90 newest target artifacts and removes orphan metadata files. If behavior still looks wrong, delete `.tako/artifacts/` and redeploy.

### Secrets pre-validation

**Symptom:** deploy fails saying the target environment is missing secret keys used by other environments, or you see a warning about extra keys.

**Fix:**

- **Missing keys → failure:** add the missing secrets to the target env with `tako secrets set --env {env} <NAME>`, then redeploy.
- **Extra keys → warning only:** deploy proceeds. Either remove the extras or add them to the other environments to clear the warning.

## Health Checks and Rolling Updates

### How probes work

- `GET /status` over the instance's private TCP endpoint, `Host: tako.internal`, with `X-Tako-Internal-Token: <per-instance>`.
- Interval: 1 second.
- Fast path: `try_wait()` runs before each probe. Exited processes are marked dead immediately without waiting for a probe timeout.
- Failure threshold: 1 failure marks an instance dead after it has already passed at least once.
- Recovery: a single successful probe resets the failure count.

The SDK wrappers implement and echo this contract automatically.

### Rolling update failures

**Symptom:** a rolling update fails and the CLI reports an error.

**Fix:** Tako waits up to 30 seconds for the new instance to become healthy. If it doesn't, Tako automatically rolls back — it kills the new instance, keeps the old ones running, and returns the error to you. No manual rollback needed. When desired instances is `0` (scale-to-zero), deploy still starts one warm instance for the new build; if that warm start fails, the deploy fails.

### Scale-to-zero cold-start errors

- **`504 App startup timed out`** — cold start didn't become healthy within the 30-second startup timeout. Check startup logs and your `/status` readiness.
- **`502 App failed to start`** — cold start setup failed outright. Check the runtime command, startup errors, and dependencies.
- **`503 App startup queue is full`** — a cold start is already in progress and concurrent waiters exceeded the default cap of 1000 per app. The response includes `Retry-After: 1`. For bursty traffic, keep at least one warm instance with `tako scale 1`.

### Process crash recovery

When the app process exits, Tako sees it via `try_wait()` before the next probe and marks the instance dead. Replacement depends on desired instances: on-demand (`0`) apps cold-start on the next request; `N > 0` apps spin up a replacement immediately.

## Config Issues

### `tako.toml` parse errors

**Symptom:** commands that need project config fail with a TOML parse or validation error.

**Fix — common mistakes:**

- Both `[build].run` and `[[build_stages]]` set (mutually exclusive).
- `[build].include` / `[build].exclude` combined with `[[build_stages]]`.
- Both `route` and `routes` in the same `[envs.*]` block.
- Env vars placed directly under `[envs.*]` instead of `[vars]` / `[vars.<env>]`.
- Namespaced preset aliases like `js/tanstack-start` (use the runtime-local form `tanstack-start` with top-level `runtime` instead).
- Empty route lists for non-development environments.

### Corrupted `config.toml`

**Symptom:** commands fail with a parse error pointing at a line in the global `config.toml` (inside the platform config dir).

**Fix:** open the file and repair the broken TOML, or delete it and re-add your servers with `tako servers add`. The inventory is the only state in that file that matters across sessions.

### `.tako/` directory deleted

**Symptom:** the `.tako/` folder disappeared.

**Fix:** nothing. The next deploy recreates it.

### `.tako/secrets.json` deleted

**Symptom:** commands warn that the secrets file is missing.

**Fix:** the local secrets file is the source of truth. Restore it from source control or teammate export before deploying. Recreate keys with `tako secrets key import` if needed.

### Route validation

- Hostname is required — path-only routes like `"/api/*"` are rejected.
- Exact path routes normalize trailing slashes (`example.com/api` and `example.com/api/` are equivalent).
- Dev routes must use `.test` or `.tako.test`.
- Wildcard host entries are ignored in dev routing.
- Each non-development environment must declare `route` or `routes`.

## Server Installation

### Unsupported libc

**Symptom:** `install-server` exits with `unsupported libc`.

**Fix:** only `glibc` and `musl` (on `x86_64` / `aarch64`) are supported. For a custom base image, set `TAKO_SERVER_URL` to a matching artifact URL.

### Missing service manager

**Symptom:** `install-server` exits saying a supported service manager is required.

**Fix:** Tako supports systemd and OpenRC. For build/image workflows where init isn't active at install time, rerun with `TAKO_RESTART_SERVICE=0` — refresh mode installs the binary and users without registering or starting the service.

## Server Runtime Issues

### Server stuck in upgrading mode

**Symptom:** management commands (`deploy`, `stop`, `delete`, `update-secrets`) are rejected because the server is in `upgrading` mode.

**Fix:** `tako-server` holds a durable single-owner upgrade lock in SQLite during `tako servers upgrade`. Most failure paths clean this up automatically:

- If upgrade fails before the reload signal, the CLI exits upgrade mode as best-effort cleanup.
- If the reload was sent but the primary socket didn't become ready within the timeout, the CLI warns that upgrade mode may remain enabled.

Check the service status (`systemctl status tako-server` or `rc-service tako-server status`). If the primary process is healthy, running `tako servers upgrade` again, or restarting the service, will reconcile the lock. If not, investigate the service failure first — the lock clears once the socket becomes responsive.

## TLS and Certificates

### HTTPS 502 / handshake failure

**Symptom:** HTTPS requests return 502 or the TLS handshake fails.

**Fix:**

- If no cert matches the SNI hostname yet, Tako serves a fallback self-signed default cert so the handshake still completes — unmatched hosts or routes then return normal HTTP status codes (e.g. `404`).
- Private/local hostnames (`localhost`, `*.localhost`, single-label hosts, reserved suffixes like `*.local`, `*.test`, `*.invalid`, `*.example`, `*.home.arpa`) skip ACME and use self-signed certs generated during deploy. A fresh deploy regenerates missing route certs.
- Inspect on-disk certs under `/opt/tako/certs/{domain}/fullchain.pem` and `privkey.pem`.

### Wildcard certificate issuance

**Symptom:** deploy fails telling you to run `tako servers setup-wildcard`.

**Fix:** wildcard certs use DNS-01 challenges and need a DNS provider configured. Run:

```bash
tako servers setup-wildcard
```

The wizard stores credentials on the server at `/opt/tako/dns-credentials.env` (mode 0600) and persists the provider name in `/opt/tako/config.json`. `tako-server` downloads `lego` on demand to drive the DNS-01 challenge.

## Secrets

### Missing encryption key

**Symptom:** a command complains that the per-env key file is missing.

**Fix:** keys live at `keys/{env}` inside `{TAKO_HOME}`. `tako secrets set` creates the key automatically on first use. To share with teammates, use:

- `tako secrets key derive` — derive a key from a passphrase.
- `tako secrets key export` — copy the key to the clipboard for secure transfer.

### Deleted `secrets.json`

**Symptom:** Tako warns that `.tako/secrets.json` is missing.

**Fix:** restore it before deploying. Local secrets are the source of truth; Tako syncs them to servers during deploy or via `tako secrets sync`.

### Sync behavior

`tako secrets sync` decrypts with `keys/{env}` locally, pushes plaintext over the management socket to `tako-server`, which re-encrypts at rest in SQLite using a per-device key, refreshes workflow workers, and rolling-restarts HTTP instances so fresh processes receive the new values via fd 3. Environments with no mapped servers are skipped with a warning.

## Routing

### Request hits the wrong app

**Symptom:** requests to a hostname are reaching the wrong app or returning 404.

**Fix:** requests are matched to the most specific route — exact beats wildcard, longer path beats shorter. Confirm every non-development environment declares `route` or `routes`, and that hostnames and paths line up with what you expect. `tako servers status` shows the deployed state on each server.

### Path-route static asset 404

**Symptom:** `example.com/app/assets/main.js` returns 404 even though the file exists.

**Fix:** static asset resolution on path-prefixed routes strips the prefix before looking up in the app's `public/` directory. So `/app/assets/main.js` looks for `public/assets/main.js`. Make sure the file exists at the prefix-stripped location inside your build output.

### Unexpected edge cache behavior

**Symptom:** stale responses on repeated `GET` / `HEAD` requests.

**Fix:** the edge proxy caches only when the origin response sets `Cache-Control` or `Expires` in a way that admits caching. There are no implicit TTL defaults. For dynamic or user-specific responses, send `Cache-Control: no-store` (or stricter private/no-cache directives) from your app. Cache storage is in-memory LRU (256 MiB total, 8 MiB per body).

## Prometheus Metrics

**Symptom:** scraping `http://127.0.0.1:9898/` returns connection refused or no data.

**Fix:**

- Confirm `tako-server` is running.
- Check that `--metrics-port 0` wasn't set — that disables metrics entirely.
- Scrape from localhost; the endpoint is not publicly accessible.
- Default port is `9898`.

**Symptom:** scrape returns `200 OK` with an empty body.

**Fix:** the Prometheus exporter only emits metric series after at least one observation. On a freshly started server with no apps deployed and no traffic, the body is legitimately empty. Drive a request through the proxy (or deploy an app — `tako_instances_running` populates on first health check) and re-scrape.

## Recovery Paths

Common recovery commands in the order you'll usually reach for them:

- **`tako doctor`** — local diagnostic. Always safe; exits 0 even if the daemon isn't running.
- **`tako releases rollback {release-id} --env production`** — switch back to a previously deployed release. Reuses current routes, env vars, secrets, and scaling; runs the standard rolling update. Partial failures are reported per server.
- **`tako implode`** — remove the local CLI and all local data (config dir, data dir, `tako`/`tako-dev-server`/`tako-dev-proxy` binaries, launchd or systemd helpers, CA cert, loopback alias, resolver files, iptables rules). Confirms before proceeding.
- **`tako servers implode {name}`** — uninstall `tako-server` from a remote server (stops services, removes binaries, `/opt/tako/`, sockets, and drops the server from local `config.toml`). Requires confirmation unless `-y` is passed.
- **Re-add a server** to refresh missing target metadata after an `--no-test` add: `tako servers rm {name} && tako servers add {host} --name {name}`.

## Config and State Recovery

| What was deleted        | What happens next                                                 |
| ----------------------- | ----------------------------------------------------------------- |
| Config/data directory   | Auto-recreated on next command                                    |
| `.tako/` directory      | Auto-recreated on next deploy                                     |
| `tako.toml`             | Commands that need it fail with guidance to run `tako init`       |
| `.tako/secrets.json`    | Warning shown; restore before deploying                           |
| `config.toml` corrupted | Parse error with line number context; fix or delete and re-add    |
| Partial release on host | Auto-removed when the failing deploy returns                      |
| Server-side deploy lock | In-memory only; cleared automatically when `tako-server` restarts |
| Local `deploy.lock`     | Advisory flock; released automatically when the holder exits      |

## Key Paths to Inspect

**Local (inside `{TAKO_HOME}`):**

- `dev-server.sock` — dev daemon socket.
- `dev-server.log` — dev daemon log; first place to look when `tako dev` fails to start.
- `ca/ca.crt` — local CA certificate.
- `certs/fullchain.pem` / `certs/privkey.pem` — daemon TLS material.
- `dev/logs/{app}-{hash}.jsonl` — shared per-app dev log stream.

**Remote (`/opt/tako` and `/var/run/tako`):**

- `/var/run/tako/tako.sock` — management socket (symlink to the active server).
- `/opt/tako/apps/{app}/{env}/current` — symlink to the live release.
- `/opt/tako/apps/{app}/{env}/releases/{version}/` — deployed release files.
- `/opt/tako/certs/{domain}/fullchain.pem` / `privkey.pem` — per-domain TLS material.
- `/opt/tako/tako.db` — server state (app registration, secrets, upgrade lock).
- `/opt/tako/config.json` — server-level config (name, DNS provider).
- `/opt/tako/dns-credentials.env` — DNS provider credentials for wildcard ACME.

## Escalation Bundle

When asking for help, gather the following so the issue can be triaged without round trips:

1. Output of `tako --version`.
2. Output of the failing command with `--verbose`.
3. `tako servers status` output.
4. `tako logs --env <env>` output for the affected environment.
5. Whether the failure is one host, some hosts, or all hosts.
6. Relevant sections of `tako.toml` (routes, env mapping, server mapping) with secrets redacted.
7. For dev issues: `tako doctor` output and the last lines of `{TAKO_HOME}/dev-server.log`.
