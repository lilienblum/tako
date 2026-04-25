---
layout: ../../layouts/DocsLayout.astro
title: "Self-hosted app deployment: server setup, rolling deploys, and scaling - Tako Docs"
heading: Deployment
current: deployment
description: "Guide to deploying apps with Tako on your own servers, including server setup, rolling deploys, scaling, secrets, and production operations."
---

# Deployment

This guide walks through everything between "I have an app" and "it's serving traffic on my servers": installing `tako-server`, registering machines, running deploys, scaling, rotating secrets, TLS, and day-two operations.

## Server setup

Each target server runs a single long-lived process, `tako-server`, that terminates TLS, routes requests, spawns app instances, and manages certificates.

### Installing tako-server

Run the hosted installer as root on the server:

```bash
sudo sh -c "$(curl -fsSL https://tako.sh/install-server.sh)"
```

The installer:

- Creates dedicated OS users (`tako` for SSH and running the control plane, `tako-app` for privileged process-separation setups)
- Detects the host target (`arch` + `libc`) and downloads the matching `tako-server-linux-{arch}-{libc}` binary (`x86_64`/`aarch64` on `glibc`/`musl`)
- Installs the binary at `/usr/local/bin/tako-server`
- Installs a systemd unit or OpenRC init script, starts the service, and verifies it is active
- Creates `/opt/tako` (data) and `/var/run/tako` (management socket) with correct permissions
- Configures privileged bind for ports 80/443 (`AmbientCapabilities=CAP_NET_BIND_SERVICE` on systemd; `setcap cap_net_bind_service=+ep` elsewhere)
- Installs scoped sudoers policy + restricted helpers so the `tako` SSH user can perform upgrades/reloads non-interactively
- Ensures `nc` (netcat) is present for management-socket access
- Configures graceful shutdown (`KillMode=control-group`, `TimeoutStopSec=30min` on systemd; `retry="TERM/1800/KILL/5"` on OpenRC)

For image builders and base-image workflows where `init` is not active, set `TAKO_RESTART_SERVICE=0` — the installer refreshes binaries and users but skips service install/start.

### SSH key setup

The installer needs an SSH public key for the `tako` user so the CLI can connect later. It resolves one in this order:

1. `TAKO_SSH_PUBKEY` env var (skips all prompts)
2. Terminal prompt for a public key (including under `sudo sh -c "$(curl ...)"` and piped installs). Invalid input re-prompts.
3. Fallback: first valid key in the invoking `SUDO_USER`'s `~/.ssh/authorized_keys`
4. No terminal, no fallback: installer warns and skips, printing a `TAKO_SSH_PUBKEY` rerun hint

Client-side, CLI SSH connections verify host keys against `~/.ssh/known_hosts`. Unknown or changed host keys are rejected — the first real deploy is usually what adds the entry.

### Default configuration

`tako-server` runs with sensible defaults and no config file:

| Setting           | Default                                 |
| ----------------- | --------------------------------------- |
| HTTP port         | `80`                                    |
| HTTPS port        | `443`                                   |
| Data directory    | `/opt/tako`                             |
| Management socket | `/var/run/tako/tako.sock` (symlink)     |
| ACME              | Production Let's Encrypt                |
| Renewal check     | Every 12h, renews 30 days before expiry |
| Metrics           | `http://127.0.0.1:9898/` (Prometheus)   |

Traffic on port 80 gets a `307` redirect to HTTPS (non-cacheable), except for `/.well-known/acme-challenge/*` and requests targeting private/local hostnames.

### Optional server config

Create `/opt/tako/config.json` to customize server-level identity and DNS:

```json
{
  "server_name": "prod",
  "dns": {
    "provider": "cloudflare"
  }
}
```

- `server_name` — identity label for Prometheus metrics. Defaults to hostname.
- `dns.provider` — DNS provider for Let's Encrypt DNS-01 wildcard challenges. Managed by `tako servers setup-wildcard`; you rarely edit this file by hand.

## Adding servers to the inventory

The local CLI keeps its own server inventory in `config.toml`. Adding a server here makes it available to deploys, scales, and status commands.

```bash
# Interactive wizard
tako servers add

# Direct, non-interactive
tako servers add 1.2.3.4 --name la --description "LA Metro" --port 22
```

The wizard supports `Tab` autocomplete for host, name, and port based on existing servers and prompt history. It runs a final `Looks good?` confirmation; `No` restarts the wizard.

Behind the scenes, `tako servers add`:

- Tests SSH as the `tako` user over the configured port
- Detects and stores target metadata (`arch`, `libc`) in the `[[servers]]` entry — deploy needs this to pick the right artifact
- Records host/name/port in `history.toml` for future autocomplete
- Is idempotent — re-running with the same values succeeds

If you pass `--no-test`, SSH and target detection are skipped. Deploys to that server will fail until you re-add it with SSH checks enabled.

Other inventory commands:

```bash
tako servers ls                       # list configured servers
tako servers rm [name]                # remove (interactive picker when name omitted)
tako servers status                   # global status across all servers
```

`tako servers status` works from any directory and does not require `tako.toml`; if the inventory is empty in an interactive shell, it offers to run the add-server wizard.

## Running a deploy

With at least one server registered and `tako.toml` present, deploy the current app:

```bash
tako deploy                  # defaults to production
tako deploy --env staging
tako deploy -c tako.prod.toml
tako deploy --dry-run        # no SSH, uploads, or remote mutations
tako deploy --yes            # skip production confirmation
```

Rules and defaults:

- `--env` defaults to `production`.
- The target environment must exist in `tako.toml` as `[envs.<env>]` and must declare `route` or `routes`.
- `development` is reserved for `tako dev` and cannot be used with `tako deploy`.
- Production deploys require an interactive confirmation unless you pass `--yes`.
- `-c` selects an alternate config file. Its parent directory is treated as the app directory, so multiple apps can live side-by-side.
- Non-dry-run deploys take a project-local `.tako/deploy.lock` (advisory `flock`). A second deploy in the same project exits immediately with the owning PID.

## What happens during a deploy

A deploy is roughly: validate locally → build once → upload the same artifact to every target server in parallel → rolling update on each server.

1. **Validate.** Check that every selected server has `arch`/`libc` metadata and that the environment's secrets are complete. Missing keys fail deploy; extra keys only warn.
2. **Resolve sources.** The source bundle root is the git root when available, otherwise the selected config's parent directory. The app subdirectory is the config-parent path relative to that root.
3. **Resolve runtime + preset.** `main` comes from `tako.toml`, then manifest `main`, then preset `main` (with JS index fallback: `index.<ext>` → `src/index.<ext>`). Unpinned preset aliases refresh from the `master` branch and fall back to cached content on fetch failure.
4. **Prepare the build dir.** Copy from source root into `.tako/build`, respecting `.gitignore`. `node_modules/` directories are **symlinked** from the original tree (build tools read but don't write). Local JS caches (workspace-root `.turbo/`, app-root `.next/cache/`) are restored into the workspace and stripped from the final archive.
5. **Build.** Resolve stages by precedence: `[[build_stages]]` → `[build]` (single-stage form) → runtime default → no-op. Each stage runs `install` then `run` in declaration order.
6. **Merge assets.** Preset `assets` + top-level `assets` merge into the app's `public/` (deduped, later wins).
7. **Verify + archive.** Confirm the resolved `main` exists inside the built app directory, write resolved runtime metadata into `app.json`, and archive the build dir (excluding `node_modules/`). For Go apps, deploy auto-injects `GOOS=linux` + `GOARCH` and runs `CGO_ENABLED=0 go build -o app .` to produce a static binary.
8. **Deploy to servers.** In parallel, one connection per target server.

These paths are always force-excluded from the deploy archive: `.git/`, `.tako/`, `.env*`, `node_modules/`. Additional excludes come from `[build].exclude` and `.gitignore`.

### Per-server steps

For each target server:

1. **Connect.** SSH as the `tako` user with the user's local keys.
2. **Disk preflight.** Ask the server for free space under `/opt/tako`. Deploy fails early with required-vs-available sizes if there's not enough room.
3. **Route conflict check.** `tako-server` rejects the deploy if its routes overlap with another app's.
4. **Lay down the release.** Create `/opt/tako/apps/{app}/{env}/releases/{version}/` and the shared `data/app` + `data/tako` directories.
5. **Upload + extract.** Stream the artifact, extract into the new release directory.
6. **Sync secrets if needed.** CLI asks the server for its current secrets hash for this app. If it matches local, the deploy payload omits secrets (server keeps existing). If it differs, or the app is new, decrypted secrets ride along with the deploy command.
7. **`prepare_release`.** Server downloads the pinned runtime binary (bun/node/deno) if needed and runs the production install (e.g. `bun install --production`).
8. **`deploy`.** Server acquires a per-app, in-memory deploy lock, registers routes, and hands off to the rolling-update path. A second deploy for the same `{app}/{env}` on the same server fails immediately with `Deploy already in progress for app '{app}'. Please wait and try again.` The lock is in-memory only — restarting `tako-server` releases it and the interrupted deploy can simply be retried.
9. **Rolling update.** Start new instance → wait for health pass (30s timeout) → add to load balancer → drain + stop old (30s timeout). Repeat until every instance is on the new build. See "Rolling updates" below.
10. **Finalize.** Update `current -> releases/{version}` and clean up releases older than 30 days.

If a deploy fails after creating the new release directory, `tako deploy` removes that partial release before returning the error — so a failed run doesn't leave half-unpacked directories around.

### CLI output modes

- **Normal:** interactive task tree — waiting rows use `○` with `...` labels, running rows spin, completed rows stay visible. Deploy renders sections in this order: `Connecting` (with one sub-task per server if there are multiple), `Building`, then one `Deploying to <server>` block per target with `Uploading`, `Preparing`, `Starting` sub-tasks.
- **Verbose (`--verbose`):** append-only transcript. Each line is `HH:MM:SS LEVEL message`. Only work that has started or finished prints — nothing is pre-rendered.
- **CI (`--ci`):** transcript-style, no ANSI, no spinners, no prompts. Missing required prompt values fail with an actionable error.
- **CI + verbose:** detailed transcript without colors or timestamps.
- `Ctrl-C` clears the active prompt/spinner, prints `Operation cancelled`, and exits 130.

## Build configuration

### Default (no config)

If neither `[build]` nor `[[build_stages]]` is set, Tako uses the runtime's default build command:

| Runtime | Default build                                                |
| ------- | ------------------------------------------------------------ |
| Bun     | `bun run --if-present build`                                 |
| Node    | `<pm> run --if-present build` (npm/pnpm/yarn, auto-detected) |
| Deno    | `deno task build 2>/dev/null \|\| true`                      |
| Go      | None (deploy handles `go build`)                             |

### Simple form: `[build]`

```toml
[build]
install = "bun install"
run = "vinxi build"
cwd = "packages/web"     # optional, relative to project root
include = ["dist/**"]    # optional artifact includes
exclude = ["**/*.map"]   # optional artifact excludes
```

Anything missing falls back to the defaults above.

### Multi-stage: `[[build_stages]]`

For monorepos or multi-package builds:

```toml
[[build_stages]]
name = "shared-ui"
cwd = "packages/ui"
install = "bun install"
run = "bun run build"

[[build_stages]]
name = "web"
cwd = "packages/web"
run = "vinxi build"
exclude = ["**/*.map"]
```

`[build]` and `[[build_stages]]` are mutually exclusive. `[build].include`/`[build].exclude` cannot be combined with `[[build_stages]]` — use per-stage `exclude` instead. `cwd` accepts `..` for monorepo traversal but is guarded against escaping the workspace root.

### Assets

Top-level `assets` plus preset `assets` are merged into the deployed app's `public/` directory after build, deduped and with later entries overwriting earlier ones:

```toml
assets = ["dist/client"]
```

## Artifact caching

Built artifacts are cached locally in `.tako/artifacts/` under a deterministic key that includes:

- Source hash
- Target label (`arch`/`libc`)
- Resolved preset source + commit
- Build commands
- Include/exclude patterns
- Asset roots
- App subdirectory

Before reuse, entries are verified by checksum and size; invalid entries are discarded and rebuilt automatically. On every deploy, Tako best-effort prunes the cache to the 90 most recent target artifacts and removes orphan target metadata files.

## Version naming

The deploy artifact version is derived from git state:

| Git state  | Version format                  | Example            |
| ---------- | ------------------------------- | ------------------ |
| Clean tree | `{commit_hash}` (7+ chars)      | `abc1234`          |
| Dirty tree | `{commit_hash}_{content_hash8}` | `abc1234_9d8e7f6a` |
| No git     | `nogit_{content_hash8}`         | `nogit_9d8e7f6a`   |

## Rolling updates

Per server, per app:

1. Start a new instance.
2. Wait up to 30s for a healthy probe.
3. Add to the load balancer.
4. Gracefully drain + stop an old instance (30s timeout).
5. Repeat until all instances are on the new build.
6. Update `current -> releases/{version}`.
7. Clean up releases older than 30 days.

Target counts use the app's **current desired instance count** on that server — not old+new combined. When the stored desired count is `0` (scale-to-zero), rolling deploy still starts **one warm instance** so traffic is served immediately after the deploy completes.

On failure, `tako-server` performs an automatic rollback: kill the new instance, keep the old ones running, return the error to the CLI.

## Scaling

Desired instance count is runtime state stored on each server, not `tako.toml` config. New app deploys start at `1` on every server (one hot instance, no cold start on the first request). Opt into scale-to-zero with `tako scale 0`.

```bash
# project context
tako scale 2 --env production                # scale every server in env
tako scale 3 --env production --server la    # scale a single server
tako scale 0                                 # scale to zero (on-demand)

# outside project context
tako scale 2 --app dashboard/production
```

Rules:

- `instances` is the desired count **per targeted server**.
- With no `--server`, `--env` is required and every server in `[envs.<env>].servers` is scaled.
- With `--server` and no `--env`, Tako defaults to `production` when in project context.
- When both are provided, the server must belong to that environment.
- The desired count persists across deploys, rollbacks, and server restarts.
- Scaling down drains in-flight requests before stopping excess instances.

### On-demand (scale = 0)

Scale-to-zero apps stop after the environment's `idle_timeout` (default `300s`). The next request drives a cold start. While the cold start is in flight:

| Scenario                                               | Response                                          |
| ------------------------------------------------------ | ------------------------------------------------- |
| Instance ready within 30s startup timeout              | Normal `2xx/3xx/4xx/5xx` from the app             |
| Startup exceeds 30s                                    | `504 App startup timed out`                       |
| Startup fails before readiness                         | `502 App failed to start`                         |
| Cold-start queue full (>1000 waiters per app, default) | `503 App startup queue is full`, `Retry-After: 1` |

Even after a deploy, one warm instance is kept alive so traffic is immediately served.

### Always-on (scale ≥ 1)

Tako maintains at least `N` healthy instances per server. Instances are not stopped while serving in-flight requests. Explicit scale-down drains first, then stops excess.

### Upstream transport

`tako-server` spawns each instance with:

- `PORT=0`, `HOST=127.0.0.1` — the SDK binds to an OS-assigned loopback port.
- The SDK writes the actual port to **fd 4**, signaling readiness. Server then routes traffic and health probes to that endpoint.
- `TAKO_APP_NAME` + `TAKO_INTERNAL_SOCKET` — identify the app to the SDK for internal RPC (workflows, channels).
- `TAKO_BUILD` — deployed build identifier (version).
- `TAKO_DATA_DIR` — persistent per-app data directory.

Per-instance identity (`--instance <8-char-nanoid>`) is passed as a CLI argument and parsed by the SDK at startup. Secrets and the per-instance internal auth token ride on a pipe on **fd 3** — never as env vars, so they don't inherit into subprocesses the app spawns.

## Secrets management

Secrets live locally as encrypted JSON in `.tako/secrets.json` — AES-256-GCM with Argon2id key derivation, one salt per environment. Values are encrypted; names are plaintext. `.tako/secrets.json` is safe to commit; encryption keys are file-based under `keys/{env}` in your Tako data directory and are **not** committed.

### Commands

```bash
tako secrets set DATABASE_URL                       # prompts for value (masked)
tako secrets set DATABASE_URL --sync                # also push to all prod servers
tako secrets set DATABASE_URL --env staging

tako secrets rm DATABASE_URL                        # remove from every env
tako secrets rm DATABASE_URL --env staging --sync   # remove + push

tako secrets ls                                     # presence table across envs
tako secrets sync                                   # push local → servers (all envs)
tako secrets sync --env production                  # push local → servers (one env)
```

### Key sharing

```bash
tako secrets key derive --env production   # derive from passphrase
tako secrets key export --env production   # copy to clipboard
```

Teammates derive the same key from the same passphrase (or paste an exported key) to decrypt the shared `.tako/secrets.json`.

### On the server

Secrets are stored in SQLite inside `/opt/tako`, encrypted per-device. They're pushed to instances via fd 3 at spawn time — never written to disk as plaintext. An `update_secrets` command refreshes storage, drains/restarts workflow workers, and triggers a rolling restart of HTTP instances so fresh processes pick up new values.

### During deploy

Before sending the deploy command, the CLI asks each server for its current secrets hash. If it matches local, secrets are **omitted** from the payload and the server keeps what it has. If it differs, or the app is new, decrypted secrets ride along automatically — so new servers are always provisioned.

Pre-validation:

- **Missing keys fail.** If the target environment is missing secret keys that appear in other envs, deploy fails.
- **Extra keys warn.** Extra keys in the target env (not in others) only warn.

## Multi-server and multi-environment

Map servers to environments in `tako.toml`:

```toml
[envs.production]
route = "api.example.com"
servers = ["la", "nyc"]
idle_timeout = 300

[envs.staging]
route = "staging.example.com"
servers = ["staging"]
```

A single server may appear in multiple non-development environments of the same project. Each environment deploys to its own remote app identity, so `production` and `staging` live side-by-side under `/opt/tako/apps/{app}/production/` and `/opt/tako/apps/{app}/staging/`.

Deploys run in parallel across an environment's servers. If some servers fail while others succeed, the deploy continues and reports failures at the end — partial rollouts are a normal state.

For production specifically: if `[envs.production].servers` is empty, Tako picks from the global inventory. With one server, it selects it automatically and writes it back into `tako.toml`. With multiple servers in an interactive terminal, it prompts and persists the choice.

## Remote directory layout

Everything a server knows about deployed apps lives under `/opt/tako/apps/{app}/{env}/`:

```
/opt/tako/apps/dashboard/production/
├── current -> releases/abc1234          # atomic pointer
├── releases/
│   ├── abc1234/
│   │   ├── app.json                     # resolved runtime + env + metadata
│   │   └── <built app files>
│   └── def5678/
├── data/
│   ├── app/                             # TAKO_DATA_DIR, app-owned
│   └── tako/                            # Tako-owned per-app internal state
└── logs/
    └── current.log
```

`app.json` holds resolved `runtime`, `main`, `package_manager`, non-secret env vars, env idle timeout, plus release metadata (`commit_message`, `git_dirty`) used by `tako releases ls`. Deploy does **not** write a `.env` file; secrets live in the server's SQLite and ride fd 3 at spawn.

## TLS and HTTPS

### Automatic ACME

For publicly reachable hostnames, `tako-server` obtains and renews certificates via Let's Encrypt automatically:

- HTTP-01 challenge on port 80 (the `/.well-known/acme-challenge/*` exception bypasses the HTTPS redirect)
- Renewal 30 days before expiry, zero-downtime — renewals happen in-process, reloading certs in place
- Certificates stored at `/opt/tako/certs/{domain}/fullchain.pem` and `/opt/tako/certs/{domain}/privkey.pem` (key is `0600`)
- Renewal check loop runs every 12 hours

### Private and fallback certs

For private/local hostnames — `localhost`, `*.localhost`, single-label hosts, and reserved suffixes (`*.local`, `*.test`, `*.invalid`, `*.example`, `*.home.arpa`) — Tako skips ACME and generates a self-signed certificate at deploy time. These hosts are also exempted from the HTTP→HTTPS redirect.

If no cert exists yet for a given SNI hostname, `tako-server` serves a **fallback self-signed default certificate** so the TLS handshake completes and the proxy can return a normal HTTP status (e.g. `404` for an unknown host).

### Wildcards

Wildcard routes (`*.example.com`) require DNS-01 via the `lego` ACME client, which `tako-server` downloads on demand. Configure DNS provider credentials once:

```bash
tako servers setup-wildcard
```

This wizard prompts for provider + credentials, verifies them locally, then applies to every server in parallel: writes `/opt/tako/dns-credentials.env` (mode `0600`), merges `dns.provider` into `/opt/tako/config.json`, drops in a systemd override to inject the env file, restarts `tako-server`, and polls for provider activation. Deploys that carry wildcard routes without a provider configured fail with a pointer back to `setup-wildcard`.

### SNI-based selection

Per-request cert selection flow:

1. Client sends SNI hostname.
2. Exact match in CertManager wins.
3. Wildcard fallback (`api.example.com` → `*.example.com`).
4. Fallback self-signed default cert so TLS completes and HTTP status codes can be returned normally.

## Releases and rollback

List deploy history:

```bash
tako releases ls                     # production
tako releases ls --env staging
```

Output is release-centric, newest-first: a deployed timestamp (with a relative hint inside 24h) and the commit message + cleanliness marker (`[clean]`, `[dirty]`, or `[unknown]`). The current release gets a `[current]` marker.

Roll back:

```bash
tako releases rollback abc1234                  # production
tako releases rollback abc1234 --env staging
tako releases rollback abc1234 --yes            # skip prod confirm
```

Rollback reuses the app's current routes, env vars, secrets, and desired instance count, switches the runtime path/version to the target release, and runs the standard rolling update. It executes per mapped server in parallel; partial failures are reported per-server and successful servers remain rolled back.

## Deleting deployments

`tako delete` removes a **single** deployment target — one app in one env on one server:

```bash
tako delete                                  # interactive target selection
tako delete --env production --server la --yes
```

Inside a project:

- No flags → prompt with deployed targets like `production from la`
- `--env` only → pick a matching server
- `--server` only → pick a matching environment
- Both → skip discovery, go to confirmation

Outside a project, you must fully specify the target with `--env`, `--server`, and (often) `--app`. The command is idempotent — rerunning after removal cleans up gracefully. Aliases: `rm`, `remove`, `undeploy`, `destroy`.

## Server operations

### Upgrading tako-server

```bash
tako servers upgrade           # all configured servers
tako servers upgrade la        # one server
```

Upgrade flow:

1. Verify `tako-server` is active on the host.
2. Install the new binary. Tako verifies a signed `tako-server-sha256s.txt` release manifest with an embedded public key, selects the expected SHA-256 for the target archive, and the remote host verifies that SHA-256 before extracting into `/usr/local/bin/tako-server`.
3. Acquire the durable upgrade lock (`enter_upgrading`) — mutating commands (`deploy`, `stop`, `delete`, `update-secrets`) are rejected until the window ends.
4. Send `SIGHUP` via `systemctl reload tako-server` or `rc-service tako-server reload`. A replacement process starts, takes over the management socket and listener ports, then the old process drains and exits.
5. Wait for the primary management socket to report ready.
6. Release upgrade mode.

The previous on-disk binary is kept until the replacement reports ready; if readiness never arrives, the previous binary is restored. `tako servers upgrade` requires systemd or OpenRC. For local testing, `TAKO_DOWNLOAD_BASE_URL` can override the download host — it must use HTTPS unless you explicitly set `TAKO_ALLOW_INSECURE_DOWNLOAD_BASE=1`.

### Restarting tako-server

```bash
tako servers restart la          # graceful reload (default)
tako servers restart la --force  # full service restart
```

Default reload sends `SIGHUP` and uses the same process-handover handshake as upgrade — zero downtime. `--force` calls `systemctl restart`/`rc-service restart` and may briefly interrupt traffic for all apps on that host. Both paths honor graceful-shutdown semantics (`KillMode=control-group`, `TimeoutStopSec=30min` on systemd; `retry="TERM/1800/KILL/5"` on OpenRC).

App runtime state (config, routes) is persisted in SQLite and restored on startup, so reloads, restarts, and crashes all preserve routing.

### Imploding a server

```bash
tako servers implode la          # prompts for confirmation
tako servers implode la --yes
```

This removes `tako-server` and all data from the host: stops and disables the service, removes systemd/OpenRC service files and binaries, deletes `/opt/tako/` and `/var/run/tako/`, and drops the server from the local `config.toml`. Alias: `tako servers uninstall`.

### Local CLI removal

```bash
tako implode          # prompts
tako implode --yes
```

This removes local Tako state: user-level config/data/binaries plus system-level dev items installed by `tako dev` (dev proxy, `/etc/resolver/test`, CA certs, loopback alias, iptables rules). Best-effort; partial removal is reported. Alias: `tako uninstall`.

## Monitoring and metrics

`tako-server` exposes Prometheus metrics on `http://127.0.0.1:9898/` — localhost only, not publicly accessible. Override with `--metrics-port <port>` (`0` disables).

| Metric                                   | Type      | Labels                      | Description                                                                      |
| ---------------------------------------- | --------- | --------------------------- | -------------------------------------------------------------------------------- |
| `tako_http_requests_total`               | Counter   | `server`, `app`, `status`   | Proxied requests, grouped by status class                                        |
| `tako_http_request_duration_seconds`     | Histogram | `server`, `app`             | End-to-end proxy request latency                                                 |
| `tako_upstream_request_duration_seconds` | Histogram | `server`, `app`             | Upstream-only latency (origin time); subtract from end-to-end for proxy overhead |
| `tako_http_active_connections`           | Gauge     | `server`, `app`             | Currently active connections                                                     |
| `tako_cold_starts_total`                 | Counter   | `server`, `app`             | Cold starts triggered (scale-to-zero)                                            |
| `tako_cold_start_duration_seconds`       | Histogram | `server`, `app`             | Cold start duration (success and failure)                                        |
| `tako_cold_start_failures_total`         | Counter   | `server`, `app`, `reason`   | Cold start failures by reason (`spawn_failed`, `instance_dead`)                  |
| `tako_tls_handshake_failures_total`      | Counter   | `server`, `reason`          | TLS handshake failures by reason (`no_sni`, `cert_missing`)                      |
| `tako_instance_health`                   | Gauge     | `server`, `app`, `instance` | Instance health (1=healthy, 0=unhealthy)                                         |
| `tako_instances_running`                 | Gauge     | `server`, `app`             | Running instance count                                                           |

Every metric carries a `server` label (the configured `server_name`, defaulting to hostname), so multi-server setups are distinguishable without scraper-side relabeling. One scrape returns data for every app on that server. Only proxied requests are counted for the request/upstream histograms — ACME challenges, static asset responses, and 404s for unmatched hosts are excluded. `tako_tls_handshake_failures_total` only tracks Tako-visible reasons; raw TLS protocol failures inside Pingora's listener are not counted.

Suggested scrape setups:

- **Self-hosted Prometheus/Grafana:** add `127.0.0.1:9898` as a target locally on the box.
- **Hosted (Grafana Cloud, Datadog, etc.):** install the platform agent on the server, point it at `http://127.0.0.1:9898/metrics`.
- **Tailscale/WireGuard:** expose port `9898` on the private interface for remote scraping.

## Edge proxy features

Beyond routing, the proxy applies a few default policies:

- **Response cache.** GET/HEAD only (websocket upgrades excluded). Admission follows response headers (`Cache-Control` / `Expires`) — no implicit TTL. Cache key includes host + URI so different hosts are isolated. Storage is in-memory LRU: 256 MiB total, 8 MiB per body.
- **Per-IP rate limit.** Max 2048 concurrent connections per client IP; excess requests get `429`.
- **Max request body.** 128 MiB; larger requests get `413`.
- **No reserved paths.** The edge proxy reserves no application path namespace. Requests are routed strictly by the routes you configure.

## Post-deploy verification

```bash
tako servers status      # snapshot: every server + every running build
curl -I https://api.example.com/
tako logs                # last 3 days of logs, paged
tako logs --tail         # live stream until Ctrl+C
tako logs --env staging --days 7
```

`tako servers status` renders one block per server and one nested block per running build: state (`running`, `idle`, `deploying`, `stopped`, `error`), instances (healthy/total), build id, and deployed timestamp in your local timezone.

## Edge cases

| Scenario                             | Behavior                                                                |
| ------------------------------------ | ----------------------------------------------------------------------- |
| Config/data directory deleted        | Auto-recreate on next command                                           |
| `config.toml` corrupted              | Show parse error with line number, offer to recreate                    |
| `tako.toml` deleted                  | Commands requiring project config fail with guidance to run `tako init` |
| `.tako/` deleted                     | Auto-recreate on next deploy                                            |
| `.tako/secrets.json` deleted         | Warn, prompt to restore secrets                                         |
| Low free space under `/opt/tako`     | Deploy fails before upload with required vs available sizes             |
| Concurrent deploy already running    | Later deploy fails immediately with a retry message                     |
| `tako-server` restarts during deploy | In-flight deploy fails; retry does not require lock cleanup             |
| Deploy fails mid-transfer/setup      | Auto-clean newly-created partial release directory                      |
| Health check fails                   | Automatic rollback to previous version                                  |
| Network interruption during deploy   | Partial failure handling, can retry                                     |
| Process crash                        | Auto-restart, health checks detect and handle                           |
