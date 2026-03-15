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
sudo sh -c "$(curl -fsSL https://tako.sh/install-server)"

# Canary channel (latest from master)
sudo sh -c "$(curl -fsSL https://tako.sh/install-server-canary)"
```

The installer handles everything:

- Creates dedicated OS users (`tako` for SSH access and `tako-app` for process separation)
- Detects host architecture and libc (`x86_64`/`aarch64`, `glibc`/`musl`) and downloads the matching `tako-server` binary
- Installs `tako-server` to `/usr/local/bin/tako-server`
- Sets up a service definition (systemd unit or OpenRC init script)
- Creates required directories (`/opt/tako` for data, `/var/run/tako` for sockets)
- Configures privileged port binding (`:80` and `:443`) via service capabilities
- Installs `mise` for runtime version management
- Installs restricted maintenance helpers and sudoers policy for non-interactive upgrades
- Ensures `nc` (netcat), `tar`, `base64`, and standard shell tools are available
- Verifies `tako-server` starts successfully after installation

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

This targets the `production` environment by default. Use `--env` for other environments:

```bash
tako deploy --env staging
```

In interactive terminals, deploying to `production` requires confirmation unless you pass `--yes` or `-y`.

### What happens during deploy

1. **Pre-validation** -- Checks that secrets are present, server target metadata exists for all selected servers, and routes are valid.
2. **Source bundling** -- Packages source files into a versioned archive under `.tako/artifacts/`. The bundle root is the git root when available, otherwise the app directory. Filtering uses `.gitignore`, and `.git/`, `.tako/`, `.env*`, `node_modules/`, `target/` are always excluded.
3. **Entrypoint resolution** -- Resolves the deploy `main` file from `tako.toml`, then preset defaults, with JS-specific fallback order (`index.<ext>`, then `src/index.<ext>`).
4. **Preset resolution** -- Resolves the build preset from `tako.toml` `preset` or the adapter base preset. Unpinned official presets are fetched from `master` on each deploy.
5. **Artifact build** -- Builds one target artifact per unique server architecture. Uses local cache when build inputs are unchanged. Runs preset build commands first, then app `[[build.stages]]`.
6. **Parallel deploy** -- Deploys to all target servers simultaneously. Each server is handled independently, so partial success is possible.

### Per-server deploy steps

For each server, the CLI:

1. Connects via SSH
2. Acquires a deploy lock
3. Runs a disk-space preflight check
4. Validates `tako-server` is active
5. Checks for route conflicts
6. Creates release and shared directories
7. Uploads and extracts the target artifact into `/opt/tako/apps/<app>/<env>/releases/<version>/`
8. Links shared directories (e.g., `logs`)
9. Syncs secrets if needed (compares hashes; only sends when changed)
10. Runs runtime prep (e.g., `bun install --production` for Bun apps)
11. Performs a rolling update
12. Updates the `current` symlink and cleans up old releases (older than 30 days)

### CLI output modes

- **Default:** Concise output with spinners for long-running steps
- **`--verbose`:** Append-only transcript with timestamps and log levels
- **`--ci`:** No colors, no spinners, no prompts -- deterministic for pipelines
- **`--ci --verbose`:** Detailed transcript without formatting

## Version naming

Deploy versions are derived from your git state:

| Git state | Version format | Example |
|---|---|---|
| Clean tree | `{commit_hash}` | `abc1234` |
| Dirty tree | `{commit}_{content_hash}` | `abc1234_9f8e7d6c` |
| No git repo | `nogit_{content_hash}` | `nogit_9f8e7d6c` |

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

Rollback uses the same rolling-update mechanism, so it is also zero-downtime.

## Scaling

### Changing instance counts

Use `tako scale` to set the number of instances per server:

```bash
tako scale 3                             # Scale production on all mapped servers
tako scale 3 --env staging               # Scale a specific environment
tako scale 3 --server la                 # Scale on one server only
tako scale 0                             # Switch to on-demand mode
```

Outside a project directory, use `--app`:

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

## Secrets management

Secrets are encrypted locally in `.tako/secrets.json` and synced to servers during deploy.

### How secrets flow during deploy

During deploy, the CLI compares a hash of local secrets against the server's current secrets. Secrets are only transmitted when they differ (or when the app is new). On the server, secrets are stored in a per-app `secrets.json` file with `0600` permissions.

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
tako secrets key export --env production   # Copy key to clipboard
tako secrets key import --env production   # Import from masked terminal input
```

## Deploy lock

Each deploy acquires a per-app, per-environment lock on each server by creating `/opt/tako/apps/<app>/<env>/.deploy_lock/` (atomic `mkdir` over SSH). This prevents concurrent deploys of the same app environment on the same server.

The lock is released when the deploy finishes. If a deploy crashes mid-flight, the lock directory must be removed manually before the next deploy can proceed.

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

For single-server deploys, the CLI shows spinner progress. For multi-server deploys, line-based progress is used to avoid overlapping output.

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
  .deploy_lock/
  releases/
    <version>/
      ...app files...
      app.json
      logs -> /opt/tako/apps/<app>/<env>/shared/logs
  shared/
    logs/
```

The `current` symlink always points to the active release. The `app.json` file in each release is the canonical runtime manifest used by `tako-server`.

## Server upgrade

Upgrade `tako-server` on a running host without downtime:

```bash
tako servers upgrade la               # Upgrade using default channel
tako servers upgrade la --canary      # Use canary prerelease
tako servers upgrade la --stable      # Use stable release
```

The upgrade process:

1. Verifies `tako-server` is active on the host
2. Downloads and installs the new binary
3. Acquires an upgrade lock (temporarily rejects mutating commands like deploy)
4. Signals the service manager to reload (`systemctl reload` or `rc-service reload`)
5. Waits for the management socket to report ready
6. Releases the upgrade lock

A supported service manager (systemd or OpenRC) is required. The reload uses `SIGHUP` for graceful in-place restart.

If the reload was sent but the socket does not become ready in time, the CLI warns that upgrade mode may remain active until the server recovers.

## TLS/SSL certificates

### Automatic certificates (ACME)

For public hostnames in your routes, Tako automatically issues and renews TLS certificates using Let's Encrypt:

- Certificates are issued during deploy for domains in app routes
- HTTP-01 challenge is used (requires port 80)
- Automatic renewal runs 30 days before expiry with zero downtime
- Certificates are stored at `/opt/tako/certs/{domain}/` (`fullchain.pem` and `privkey.pem`)

### Self-signed certificates for local domains

For private/local route hostnames (`localhost`, `*.localhost`, single-label hosts, and reserved suffixes like `*.local`, `*.test`), Tako skips ACME and generates a self-signed certificate during deploy.

If no certificate exists yet for an incoming SNI hostname, Tako serves a fallback self-signed default certificate so TLS handshakes complete and unmatched routes return `404`.

### Wildcard certificates

Routing supports wildcard hosts (e.g., `*.example.com`). For TLS:

- Wildcard certificates are used when present in cert storage
- Automated ACME issuance uses HTTP-01, so wildcard certs must currently be provisioned manually
- DNS-01 challenge support for automatic wildcard cert issuance is not yet available

### SNI-based selection

Tako uses Server Name Indication to pick the right certificate during TLS handshake:

1. Look up exact match for the SNI hostname
2. Try wildcard fallback (e.g., `api.example.com` matches `*.example.com`)
3. Serve the fallback default certificate if nothing matches

### HTTPS behavior

- HTTP requests are redirected to HTTPS by default (`307` with `Cache-Control: no-store`)
- Exception: `/.well-known/acme-challenge/*` stays on HTTP for ACME validation
- Forwarded requests for private/local hostnames are treated as already HTTPS when proxy protocol metadata is missing, preventing redirect loops behind local proxies

## Monitoring and metrics

### Prometheus endpoint

`tako-server` exposes Prometheus-compatible metrics at `http://127.0.0.1:9898/` (localhost only, not publicly accessible).

Configure with `--metrics-port` (set to `0` to disable).

### Available metrics

| Metric | Type | Description |
|---|---|---|
| `tako_http_requests_total` | Counter | Total proxied requests by status class |
| `tako_http_request_duration_seconds` | Histogram | Request latency distribution |
| `tako_http_active_connections` | Gauge | Currently active connections |
| `tako_cold_starts_total` | Counter | Cold starts triggered |
| `tako_cold_start_duration_seconds` | Histogram | Cold start duration distribution |
| `tako_instance_health` | Gauge | Instance health (1=healthy, 0=unhealthy) |
| `tako_instances_running` | Gauge | Running instance count |

All metrics carry `server` and `app` labels. `tako_instance_health` also includes an `instance` label. Only proxied requests are measured -- ACME challenges, static asset responses, and unmatched-host 404s are excluded.

### Connecting to monitoring platforms

- **Self-hosted Prometheus/Grafana:** Add `127.0.0.1:9898` as a scrape target
- **Hosted platforms (Grafana Cloud, Datadog, etc.):** Install the platform agent on your server and configure it to scrape `http://127.0.0.1:9898/metrics`
- **Private network access:** Expose port 9898 over Tailscale or WireGuard for remote scraping

## Post-deploy verification

After a deploy completes:

1. Run `tako servers status` to confirm routes and instances are healthy
2. Open your public routes and check response headers/body
3. Stream logs with `tako logs --env production` to watch for startup errors
4. If some servers failed, fix the issue and re-run deploy

## Edge cases and error handling

| Scenario | Behavior |
|---|---|
| Low disk space on server | Deploy fails before upload with required vs. available sizes |
| Stale deploy lock | Deploy fails until `.deploy_lock` is manually removed |
| Deploy fails mid-transfer | Partial release directory is auto-cleaned |
| Health check fails during rolling update | Automatic rollback to previous version |
| Network interruption during deploy | Partial failure reported, safe to retry |
| Process crash after deploy | Auto-restart with health check detection |
| Missing server target metadata | Deploy fails early with guidance to re-add the server |
