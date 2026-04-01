---
layout: ../../layouts/DocsLayout.astro
title: "Tako Docs - Deployment"
heading: Deployment
current: deployment
---

# Deployment

This guide covers everything involved in deploying apps with Tako: setting up servers, running deploys, managing scaling, handling secrets, and keeping things running smoothly in production.

## Server setup

Before you can deploy, each target server needs `tako-server` installed and running.

### Installing tako-server

Run the hosted installer as root on your server:

```bash
# Stable channel
sudo sh -c "$(curl -fsSL https://tako.sh/install-server.sh)"

# Canary channel (latest from master)
sudo sh -c "$(curl -fsSL https://tako.sh/install-server-canary.sh)"
```

The installer handles everything:

- Creates dedicated OS users (`tako` for SSH access and `tako-app` for process separation)
- Detects host architecture and libc (`x86_64`/`aarch64`, `glibc`/`musl`) and downloads the matching `tako-server` binary
- Installs `tako-server` to `/usr/local/bin/tako-server`
- Sets up a service definition (systemd unit or OpenRC init script)
- Creates required directories (`/opt/tako` for data, `/var/run/tako` for sockets)
- Configures privileged port binding (`:80` and `:443`) via service capabilities
- Installs restricted maintenance helpers and sudoers policy for non-interactive upgrades
- Ensures `nc` (netcat), `tar`, `base64`, `which`, and standard shell tools are available
- Verifies `tako-server` starts successfully after installation

The installer also supports install-refresh mode (`TAKO_RESTART_SERVICE=0`) for build/image workflows without an active init system -- it refreshes the binary and users but skips service-definition install and start.

**SSH key setup:** The installer needs an SSH public key for the `tako` user so the CLI can connect later.

- Set `TAKO_SSH_PUBKEY` before running the installer, or
- The installer prompts for a key when a terminal is available (including piped installs like `curl ... | sudo sh`)
- If no key input is available, the installer tries to reuse a key from the invoking sudo user's `~/.ssh/authorized_keys`

### Server prerequisites

Each server needs:

- SSH access as the `tako` user
- Local `~/.ssh/known_hosts` entry for each host (unknown/changed host keys are rejected)
- `tako-server` installed and running
- A supported service manager: systemd or OpenRC

### Default server configuration

Out of the box, `tako-server` runs with sensible defaults and no configuration file is needed:

- HTTP on port 80, HTTPS on port 443
- Data directory at `/opt/tako`
- Management socket at `/var/run/tako/tako.sock`
- Let's Encrypt production ACME
- Certificate renewal every 12 hours

### Server configuration file

For optional customization, `tako-server` reads `/opt/tako/config.json`:

```json
{
  "server_name": "prod",
  "dns": {
    "provider": "cloudflare"
  }
}
```

- `server_name` -- identity label for Prometheus metrics (defaults to hostname if absent).
- `dns.provider` -- DNS provider for Let's Encrypt DNS-01 wildcard challenges (configured interactively during deploy when wildcard routes are detected).

This file is written by the installer (server name) and CLI (DNS config), and read by `tako-server` at startup.

## Adding servers to your inventory

Once a server is set up, register it locally so Tako knows where to deploy.

### `tako servers add`

Add a server interactively with a guided wizard:

```bash
tako servers add
```

Or directly from CLI arguments:

```bash
tako servers add 1.2.3.4 --name la --port 22
```

When adding, Tako tests the SSH connection (as the `tako` user) and detects the server's architecture and libc. This target metadata is stored in your global `config.toml` and is required for deploy to build the correct artifact.

If `tako-server` is not found on the host, Tako warns you to install it manually.

Use `--no-test` to skip SSH checks, but deploy will fail for that server until you re-add it with checks enabled.

The wizard supports `Tab` autocomplete suggestions for host, name, and port from existing servers and persisted CLI history.

Other server management commands:

```bash
tako servers ls          # List all configured servers
tako servers rm la       # Remove a server
tako servers status      # See deployed apps and health across all servers
```

## Deploy workflow

From your app directory, run:

```bash
tako deploy
```

If you keep multiple config files in one folder, point deploy at the exact file you want:

```bash
tako -c configs/staging deploy
```

This targets the `production` environment by default. Use `--env` for other environments:

```bash
tako deploy --env staging
```

In interactive terminals, deploying to `production` requires confirmation unless you pass `--yes` or `-y`.

Use `--dry-run` to preview the deploy without performing any side effects -- validation runs normally, but SSH connections, builds, and uploads are skipped:

```bash
tako deploy --dry-run
```

### What happens during deploy

1. **Pre-validation** -- Checks that secrets are present, server target metadata exists for all selected servers, and routes are valid.
2. **Workdir setup** -- Copies project files into a clean workdir (respecting `.gitignore`), symlinks `node_modules/` from the original tree for JS projects so build tools can resolve dependencies without a full install. `.git/`, `.tako/`, and `.env*` are always excluded.
3. **Entrypoint resolution** -- Resolves the deploy `main` file from `tako.toml`, then preset defaults, with JS-specific fallback order (`index.<ext>`, then `src/index.<ext>`). For Go, the default main is `app` (the compiled binary name).
4. **Preset resolution** -- Resolves the app preset from `tako.toml` `preset` or the adapter base preset. Unpinned official presets are fetched from `master` on each deploy.
5. **Artifact build** -- Runs your build commands (`[build]` or `[[build_stages]]`) in the workdir. Uses local cache when build inputs are unchanged. For JS projects, the resulting artifact excludes `node_modules/` -- the server installs its own production dependencies after extracting the artifact. For Go, the build produces a self-contained binary (default: `CGO_ENABLED=0 go build -o app .`) and Tako auto-injects `GOOS=linux` and the target `GOARCH` for cross-compilation. No production install step is needed.
6. **Parallel deploy** -- Deploys to all target servers simultaneously. Each server is handled independently, so partial success is possible.

### Per-server deploy steps

For each server, the CLI:

1. Connects via SSH
2. Runs a disk-space preflight check
3. Validates `tako-server` is active
4. Checks for route conflicts
5. Creates release and shared directories
6. Uploads and extracts the target artifact into `/opt/tako/apps/<app>/<env>/releases/<version>/`
7. Links shared directories (e.g., `logs`)
8. Syncs secrets if needed (compares hashes; only sends when changed)
9. Sends the deploy command to `tako-server`
10. `tako-server` acquires its per-app in-memory deploy lock, runs runtime prep (production dependency install via the runtime's package manager for JS; skipped for Go since the binary is self-contained), and performs a first start or rolling update over per-instance private TCP upstreams
11. Updates the `current` symlink and cleans up old releases (older than 30 days)

Server-side runtime prep uses the runtime's package manager to install production dependencies from the deployed manifest. For Go apps, this step is skipped -- the deployed binary runs directly with no runtime download or dependency install.

### CLI output modes

- **Default:** Concise interactive output. Once deploy planning is known, pretty mode may render task groups and task reporters with waiting rows shown up front.
- **`--verbose`:** Append-only transcript with timestamps and log levels. Only current work is printed.
- **`--ci`:** No colors, no prompts, transcript-style only -- deterministic for pipelines
- **`--ci --verbose`:** Detailed transcript without colors or timestamps

## Build options

### Simple build

Use `[build]` in `tako.toml` for straightforward build setups:

```toml
[build]
install = "bun install"       # Optional pre-build install command
run = "bun run build"         # Build command
cwd = "packages/web"          # Optional working directory relative to project root
include = ["dist/**"]         # Optional artifact include globs
exclude = ["**/*.map"]        # Optional artifact exclude globs
```

When `[build]` is used, Tako runs `install` first (if set), then `run`.

### Multi-stage builds

For monorepos or complex projects, use `[[build_stages]]` instead (mutually exclusive with `[build]`):

```toml
[[build_stages]]
name = "shared-ui"
cwd = "packages/ui"
install = "bun install"
run = "bun run build"
include = ["dist/**"]

[[build_stages]]
name = "app"
cwd = "packages/app"
install = "bun install"
run = "bun run build"
include = ["dist/**", ".output/**"]
```

Stages run in declaration order. Each stage has:

- `name` (optional display label)
- `cwd` (optional, relative to app root; `..` is allowed for monorepo traversal but guarded against escaping the workspace root)
- `install` (optional command run before `run`)
- `run` (required command)
- `include` (optional array of file globs relative to the stage's `cwd`; stages without `include` are intermediate and contribute nothing to the artifact)

Having both `[build].run` and `[[build_stages]]` is an error. `[build].include`/`[build].exclude` cannot be used alongside `[[build_stages]]`.

### Asset handling

Asset directories are declared with the top-level `assets` field and/or the preset's `assets`. Both sources are deduplicated and merged into the app's `public/` directory after build, with later entries overwriting earlier ones:

```toml
assets = ["dist/client"]
```

## Deploy artifact caching

Built target artifacts are cached locally under `.tako/artifacts/` using a deterministic cache key that includes source hash, target label, resolved preset source/commit, build commands, include/exclude patterns, asset roots, and app subdirectory.

Cached artifacts are checksum/size verified before reuse; invalid cache entries are automatically discarded and rebuilt.

On every deploy, local artifact cache is pruned automatically (best-effort): keep 30 most recent source archives, keep 90 most recent target artifacts, and remove orphan target metadata files.

## Version naming

Deploy versions are derived from your git state:

| Git state   | Version format            | Example            |
| ----------- | ------------------------- | ------------------ |
| Clean tree  | `{commit_hash}`           | `abc1234`          |
| Dirty tree  | `{commit}_{content_hash}` | `abc1234_9f8e7d6c` |
| No git repo | `nogit_{content_hash}`    | `nogit_9f8e7d6c`   |

Each hash uses the first 8 characters.

## Rolling updates

When a deploy reaches a server, Tako performs a rolling update to replace instances with zero downtime.

### How rolling updates work

1. Start a new instance with the new release
2. Wait for the health check to pass (30-second timeout)
3. Add the new instance to the load balancer
4. Gracefully stop the old instance (drain connections, 30-second timeout)
5. Repeat until all instances are replaced
6. Update the `current` symlink to the new release
7. Clean up releases older than 30 days

The rolling update targets the app's current desired instance count on that server. Even when desired instances is `0` (on-demand mode), deploy starts one warm instance so the app is immediately reachable afterward. If that warm instance fails to start, the deploy fails.

### Failure and rollback

If a health check fails during rolling update, Tako automatically rolls back: new instances are killed and old ones keep running. The error is reported back to the CLI.

You can also manually roll back to any previous release:

```bash
tako releases ls --env production         # See release history
tako releases rollback abc1234 --env production  # Roll back to a specific release
```

Rollback uses the same rolling-update mechanism, so it is also zero-downtime. In interactive terminals, rollback to `production` requires explicit confirmation unless `--yes` (or `-y`) is provided.

## Scaling

### Changing instance counts

Use `tako scale` to set the number of instances per server:

```bash
tako scale 3                             # Scale production on all mapped servers
tako scale 3 --env staging               # Scale a specific environment
tako scale 3 --server la                 # Scale on one server only
tako scale 0                             # Switch to on-demand mode
```

Outside the selected config context, use `--app`:

```bash
tako scale 2 --app my-app --env production --server la
```

The desired instance count is persisted on the server and survives deploys, rollbacks, and server restarts. Deploy does not set instance counts -- new apps start at `0` and you change the count with `tako scale`.

### On-demand vs always-on

**On-demand (desired instances = 0):**

- Instances are started when a request arrives (cold start)
- After deploy, one warm instance is kept running so the app is reachable immediately
- Idle instances are stopped after the configured timeout (default: 5 minutes)
- Cold start waits up to 30 seconds for readiness; timeout returns `504`
- If cold start setup fails, proxy returns `502`
- During a cold start, up to 100 requests queue; overflow returns `503` with `Retry-After: 1`

**Always-on (desired instances > 0):**

- At least N instances stay running on that server at all times
- Scaling down drains in-flight requests before stopping excess instances

### Idle timeout

Configure per-environment in `tako.toml`:

```toml
[envs.production]
route = "api.example.com"
servers = ["la", "nyc"]
idle_timeout = 300  # 5 minutes (default)

[envs.staging]
route = "staging.example.com"
servers = ["staging"]
idle_timeout = 120  # 2 minutes
```

Instances are never stopped while serving in-flight requests.

### Upstream transport

Production instances bind to `127.0.0.1` on an OS-assigned port. The SDK signals readiness to `tako-server` via a `TAKO:READY:<port>` line on stdout once listening, and the server routes traffic to that loopback endpoint.

## Secrets management

Secrets are encrypted locally in `.tako/secrets.json` and synced to servers during deploy.

### How secrets flow during deploy

During deploy, the CLI compares a hash of local secrets against the server's current secrets. Secrets are only transmitted when they differ (or when the app is new). On the server, secrets are stored encrypted in Tako's SQLite state database and passed to fresh instances via fd 3 at spawn time, before any user code runs.

### Managing secrets

```bash
tako secrets set DATABASE_URL                      # Set for production (default)
tako secrets set API_KEY --env staging              # Set for a specific environment
tako secrets rm OLD_KEY                             # Remove from all environments
tako secrets rm OLD_KEY --env staging               # Remove from one environment
tako secrets ls                                    # See which secrets exist per environment
tako secrets sync                                  # Push local secrets to all servers
tako secrets sync --env production                 # Push to one environment
```

Use `--sync` with `set` or `rm` to immediately push changes to servers (triggers a rolling restart of running instances):

```bash
tako secrets set DATABASE_URL --sync
```

### Secret validation

- Deploy fails if the target environment is missing secret keys that other environments have
- Deploy warns (but proceeds) if the target environment has extra keys not present elsewhere

### Encryption keys

Each environment has its own encryption key stored at `keys/{env}`. Keys are created automatically when you first set a secret for an environment. Share keys with teammates using:

```bash
tako secrets key derive --env production   # Derive key from a passphrase
tako secrets key export --env production   # Copy key to clipboard
tako secrets key import --env production   # Import from masked terminal input
```

## Deploy lock

`tako-server` serializes deploys per app/environment using an in-memory lock. This prevents concurrent deploys of the same app environment on the same server.

If a second deploy command arrives while one is already running for that app/environment, the server rejects it immediately with a retry message. No `.deploy_lock` directory is written to disk, and a server restart clears the lock automatically.

## Multi-server and multi-environment deployments

### Assigning servers to environments

Declare which servers handle which environments in `tako.toml`:

```toml
[envs.production]
route = "api.example.com"
servers = ["la", "nyc"]

[envs.staging]
route = "staging.example.com"
servers = ["staging"]
```

The same server can appear in multiple environments. Each environment deploys to its own path under `/opt/tako/apps/{app}/{env}/`.

### How multi-server deploy works

All target servers for an environment are deployed to in parallel. Each server is independent, so:

- Some servers may succeed while others fail
- Failures are reported at the end with per-server detail
- Re-run deploy after fixing failed hosts

### Server auto-selection for production

If `[envs.production].servers` is empty when you deploy:

- With one global server: it is selected automatically and written to `tako.toml`
- With multiple servers in an interactive terminal: you are prompted to pick one

## Disk space preflight

Before uploading artifacts, deploy checks free space under `/opt/tako` on each server. The check accounts for archive size plus unpack headroom. If space is insufficient, deploy fails early and reports the required vs. available sizes.

## Failed deploy cleanup

If a deploy fails after creating a release directory on a server, Tako automatically removes that partial release directory before returning the error. This keeps the server clean and avoids orphaned release artifacts.

## Remote directory layout

```
/opt/tako/apps/<app>/<env>/
  current -> releases/<version>
  releases/
    <version>/
      ...app files...
      app.json
  shared/
    logs/
```

The `current` symlink always points to the active release. The `app.json` file in each release is the canonical runtime manifest used by `tako-server`.

## TLS/SSL certificates

### Automatic certificates (ACME)

For public hostnames in your routes, Tako automatically issues and renews TLS certificates using Let's Encrypt:

- Certificates are issued during deploy for domains in app routes
- HTTP-01 challenge is used by default (requires port 80)
- Automatic renewal runs 30 days before expiry with zero downtime
- Certificates are stored at `/opt/tako/certs/{domain}/` (`fullchain.pem` and `privkey.pem`)

### Self-signed certificates for local domains

For private/local route hostnames (`localhost`, `*.localhost`, single-label hosts, and reserved suffixes like `*.local`, `*.test`), Tako skips ACME and generates a self-signed certificate during deploy.

If no certificate exists yet for an incoming SNI hostname, Tako serves a fallback self-signed default certificate so TLS handshakes complete and unmatched routes return `404`.

### Wildcard certificates

Routing supports wildcard hosts (e.g., `*.example.com`). For TLS:

- Wildcard certificates are issued automatically via DNS-01 challenges when a DNS provider is configured
- When wildcard routes are deployed and no DNS provider is configured, deploy prompts interactively for provider credentials
- Credentials are stored on the server at `/opt/tako/dns-credentials.env` and the provider name is persisted in `/opt/tako/config.json`
- DNS-01 challenges are handled via the `lego` ACME client

### SNI-based selection

Tako uses Server Name Indication to pick the right certificate during TLS handshake:

1. Look up exact match for the SNI hostname
2. Try wildcard fallback (e.g., `api.example.com` matches `*.example.com`)
3. Serve the fallback default certificate if nothing matches

### HTTPS behavior

- HTTP requests are redirected to HTTPS by default (`307` with `Cache-Control: no-store`)
- Exception: `/.well-known/acme-challenge/*` stays on HTTP for ACME validation
- Forwarded requests for private/local hostnames are treated as already HTTPS when proxy protocol metadata is missing, preventing redirect loops behind local proxies

## Releases

### Listing releases

View release history for the current app:

```bash
tako releases ls                         # Production (default)
tako releases ls --env staging           # Specific environment
```

Output is release-centric and sorted newest-first. Each entry shows the release/build id, deployed timestamp (with a relative hint like `{3h ago}` for recent deploys), commit message, and cleanliness marker (`[clean]`, `[dirty]`, or `[unknown]`). The current active release is marked with `[current]`.

### Rolling back

Roll back to a previously deployed release:

```bash
tako releases rollback abc1234 --env production
```

Rollback reuses the current app's routes, env vars, secrets, and scaling config, switching only the runtime path and version to the target release. It uses the same rolling-update flow, so it is zero-downtime. Partial failures are reported per server; successful servers remain rolled back.

## Deleting deployments

Remove a deployed app from a specific environment/server:

```bash
tako delete                                       # Interactive target selection
tako delete --env production --server la           # Direct target
tako delete --env staging --server staging --yes   # Skip confirmation
```

`tako delete` removes exactly one deployment target, not every server in an environment. In interactive mode, Tako discovers deployed targets and prompts you to pick one. In non-interactive mode, `--yes`, `--env`, and `--server` are all required.

Delete is idempotent for absent app state -- safe to re-run for cleanup.

Aliases: `tako rm`, `tako remove`, `tako undeploy`, `tako destroy`.

## Server upgrade

Upgrade `tako-server` on a running host without downtime:

```bash
tako servers upgrade la               # Upgrade using default channel
tako servers upgrade la --canary      # Use canary prerelease
tako servers upgrade la --stable      # Use stable release
```

When `server-name` is omitted, all configured servers are upgraded.

The upgrade process:

1. Verifies `tako-server` is active on the host
2. Verifies the signed server checksum manifest, then downloads and installs the new binary only if the archive SHA-256 matches the signed entry for that target
3. Acquires an upgrade lock (temporarily rejects mutating commands like deploy)
4. Signals the service manager to reload (`systemctl reload` or `rc-service reload`)
5. Waits for the management socket to report ready
6. Releases the upgrade lock

A supported service manager (systemd or OpenRC) is required. The reload uses `SIGHUP` for graceful in-place restart.

If you override `TAKO_DOWNLOAD_BASE_URL`, it must use `https://` unless you also set `TAKO_ALLOW_INSECURE_DOWNLOAD_BASE=1` for a local test environment.

If the reload was sent but the socket does not become ready in time, the CLI warns that upgrade mode may remain active until the server recovers.

## Server restart

Restart `tako-server` entirely (causes brief downtime for all apps on the server):

```bash
tako servers restart la
```

Use for: binary updates outside the normal upgrade flow, major configuration changes, system recovery.

`tako-server` persists app runtime registration (app config and routes) in SQLite and restores it on startup, so app routing and config survive restarts and crashes. Secrets remain encrypted in the same SQLite database.

Graceful shutdown semantics:

- On systemd: `KillMode=control-group` and `TimeoutStopSec=30min`, allowing all app processes time to handle shutdown before forced termination
- On OpenRC: `retry="TERM/1800/KILL/5"` waits up to 30 minutes before forced termination

## Server implode

Remove tako-server and all data from a remote server:

```bash
tako servers implode la
tako servers implode la --yes   # Skip confirmation
```

When the server name is omitted in an interactive terminal, Tako prompts you to select from configured servers.

What it does:

1. Displays what will be removed and asks for confirmation
2. SSHes into the server and stops/disables services, removes service files, binaries, data directory (`/opt/tako/`), and socket directory (`/var/run/tako/`)
3. Removes the server from your local `config.toml`

Alias: `tako servers uninstall`.

## Local CLI removal

Remove the local Tako CLI and all local data:

```bash
tako implode
tako implode --yes   # Skip confirmation
```

This removes config directories, data directories, CLI binaries, and platform-specific services installed by `tako dev` (launchd helpers on macOS, systemd services on Linux, CA certificates, loopback aliases).

Alias: `tako uninstall`.

## Monitoring and metrics

### Prometheus endpoint

`tako-server` exposes Prometheus-compatible metrics at `http://127.0.0.1:9898/` (localhost only, not publicly accessible).

Configure with `--metrics-port` (set to `0` to disable).

### Available metrics

| Metric                               | Type      | Description                              |
| ------------------------------------ | --------- | ---------------------------------------- |
| `tako_http_requests_total`           | Counter   | Total proxied requests by status class   |
| `tako_http_request_duration_seconds` | Histogram | Request latency distribution             |
| `tako_http_active_connections`       | Gauge     | Currently active connections             |
| `tako_cold_starts_total`             | Counter   | Cold starts triggered                    |
| `tako_cold_start_duration_seconds`   | Histogram | Cold start duration distribution         |
| `tako_instance_health`               | Gauge     | Instance health (1=healthy, 0=unhealthy) |
| `tako_instances_running`             | Gauge     | Running instance count                   |

All metrics carry `server` and `app` labels. `tako_instance_health` also includes an `instance` label. Only proxied requests are measured -- ACME challenges, static asset responses, and unmatched-host 404s are excluded.

### Connecting to monitoring platforms

- **Self-hosted Prometheus/Grafana:** Add `127.0.0.1:9898` as a scrape target
- **Hosted platforms (Grafana Cloud, Datadog, etc.):** Install the platform agent on your server and configure it to scrape `http://127.0.0.1:9898/metrics`
- **Private network access:** Expose port 9898 over Tailscale or WireGuard for remote scraping

## Edge proxy features

The built-in edge proxy provides several features out of the box:

- **Response caching:** Upstream response caching is enabled for `GET`/`HEAD` requests (websocket upgrades excluded). Cache admission follows response headers (`Cache-Control`/`Expires`) with no implicit TTL defaults -- responses without explicit cache directives are not stored. Cache storage is in-memory with bounded LRU eviction (256 MiB total, 8 MiB per cached response body).
- **Rate limiting:** Per-IP rate limiting caps at 2048 concurrent connections per client IP. Excess requests receive `429`.
- **Max body size:** Maximum request body size is 128 MiB. Larger requests receive `413`.

## Post-deploy verification

After a deploy completes:

1. Run `tako servers status` to confirm routes and instances are healthy
2. Open your public routes and check response headers/body
3. Stream logs with `tako logs --env production` to watch for startup errors
4. If some servers failed, fix the issue and re-run deploy

## Edge cases and error handling

| Scenario                                 | Behavior                                                     |
| ---------------------------------------- | ------------------------------------------------------------ |
| Low disk space on server                 | Deploy fails before upload with required vs. available sizes |
| Concurrent deploy already running        | Later deploy fails immediately with a retry message          |
| `tako-server` restarts during deploy     | In-flight deploy fails; retry does not require lock cleanup  |
| Deploy fails mid-transfer                | Partial release directory is auto-cleaned                    |
| Health check fails during rolling update | Automatic rollback to previous version                       |
| Network interruption during deploy       | Partial failure reported, safe to retry                      |
| Process crash after deploy               | Auto-restart with health check detection                     |
| Missing server target metadata           | Deploy fails early with guidance to re-add the server        |
