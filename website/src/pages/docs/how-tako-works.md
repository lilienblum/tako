---
layout: ../../layouts/DocsLayout.astro
title: "How Tako works: rolling deploys, TLS, health checks, and scale to zero - Tako Docs"
heading: How Tako Works
current: how-tako-works
description: "Learn how Tako handles local development, rolling deploys, TLS, health checks, request routing, scaling, and runtime management."
---

# How Tako Works

Tako is a deployment and development platform that takes your JavaScript/TypeScript or Go app from local development to production with minimal configuration. It handles building, deploying, routing, TLS, health checks, scaling, and rolling updates -- so you can focus on your app.

This page walks through Tako's architecture, how requests flow through it, and the key concepts that make it tick.

## The Three Components

Tako is made up of three parts that work together:

**`tako` CLI** -- Runs on your machine. Handles project setup (`tako init`), local development (`tako dev`), building and deploying (`tako deploy`), scaling (`tako scale`), and managing servers and secrets.

**`tako-server`** -- Runs on your deployment server(s). Manages app processes, routes incoming traffic, handles TLS certificates, performs health checks, and orchestrates rolling updates. Built on [Pingora](https://github.com/cloudflare/pingora), Cloudflare's production proxy.

**`tako.sh` SDK** -- A package your app uses. For JavaScript/TypeScript, it's an npm package providing runtime adapters for Bun, Node.js, and Deno with a standard fetch handler interface. For Go, install with `go get tako.sh` and use `tako.ListenAndServe(handler)`. Both provide a built-in health check endpoint that Tako uses to verify your app is running.

## Two Paths: Management and Traffic

Everything in Tako falls into one of two paths:

- **Management path** -- CLI commands that change state: `tako deploy`, `tako scale`, `tako secrets set`, `tako servers add`. These talk to `tako-server` over a Unix socket via SSH.
- **Traffic path** -- Real HTTP/HTTPS requests from users flowing through the proxy to your app instances over per-instance private TCP endpoints on loopback. This is pure Pingora performance.

This separation keeps things predictable. Commands change your deployment state. The proxy serves traffic.

## Local Development

When you run `tako dev`, the CLI starts a persistent local daemon (`tako-dev-server`) that manages your app process and provides HTTPS routing on `*.test` domains (with `*.tako.test` always available as a fallback).

```
tako dev
# Your app is now live at https://my-app.test
```

Here is what happens under the hood:

1. The CLI ensures the dev daemon is running (starts it if needed).
2. It registers the selected config file with the daemon.
3. One local instance of your app starts immediately.
4. HTTPS is set up using a local Certificate Authority -- no browser security warnings once the CA is trusted.

The daemon supports multiple apps simultaneously, each on its own `*.test` subdomain (`*.tako.test` also works). If you enable LAN mode from the interactive UI with `l`, those same routes are also reachable through `.local` aliases, preserving subdomains and path prefixes. On macOS, Tako installs a dev proxy so your app is available on standard ports (443/80) without `sudo`. On Linux, Tako uses iptables redirect rules to achieve the same portless URLs.

Your app stays running while you work. If you press `b`, it backgrounds to the daemon and the CLI exits -- your app keeps serving. Run `tako dev` again to reconnect. Press `Ctrl+c` to stop the app entirely.

After 30 minutes with no attached CLI client, the daemon idles the app process. The next HTTP request wakes it back up automatically.

### Variants

You can run DNS variants of your app with `--variant`:

```
tako dev --variant admin
# Live at https://my-app-admin.test
```

This is useful for running multiple configurations of the same app side by side.

### Dev Command Resolution

`tako dev` resolves the command to run your app in this order:

1. `dev` in `tako.toml` (your override, e.g. `dev = ["custom", "cmd"]`)
2. Preset `dev` command (for example `vite`/`tanstack-start` use `vite dev`, `nextjs` uses `next dev`)
3. Runtime default: JS runtimes run through the SDK entrypoint, Go uses `go run .`

JS dev uses the same SDK entrypoint as production -- the SDK wraps your `export default function fetch()` into a proper HTTP server.

### Config Watching

`tako dev` watches your `tako.toml` and automatically:

- Restarts the app when effective dev environment variables change
- Updates dev routing when `[envs.development].route(s)` changes

Source hot-reload is runtime-driven (e.g. Bun's built-in watch mode); Tako does not watch source files for auto-restart.

## Deploying to Production

Deployment is a single command:

```
tako deploy
```

This builds your app locally, uploads the artifact to your server(s), and performs a zero-downtime rolling update. Here is the full flow:

### 1. Validate and Prepare

Tako validates the selected config file, resolves your app name, checks that secrets are in order, and verifies server connectivity. It also confirms that each target server has valid architecture and libc metadata (captured when you first added the server).

### 2. Build Locally

Your app is always built on your machine, never on the server. Tako:

- Copies your project into a clean build dir at `.tako/build` (respecting `.gitignore`), symlinks `node_modules/` from the original tree (JS runtimes only)
- Restores local JS build caches like workspace `.turbo/` and app `.next/cache/` into that build dir when present
- Runs your build commands: `[[build_stages]]` if set, otherwise `[build]`, otherwise the runtime default (`<pm> run --if-present build` for JS runtimes)
- Merges configured asset directories into `public/`
- Verifies the resolved entrypoint file exists in the built workspace
- Packages the result into a deploy artifact (excluding `node_modules/` and local JS build cache directories like `.turbo/` and `.next/cache` -- for JS runtimes, the server installs its own production dependencies; Go deploys a self-contained binary with no production install step)
- Caches artifacts locally so unchanged builds are instant on subsequent deploys

### 3. Upload and Deploy

For each target server (in parallel):

- Checks free disk space under `/opt/tako` before uploading
- Uploads and extracts the target artifact
- Syncs secrets only if they have changed (compares hashes)
- Sends a `prepare_release` command to download the runtime and install production dependencies
- Sends the `deploy` command to `tako-server`
- `tako-server` acquires its per-app in-memory deploy lock
- Performs a first start or rolling update of app instances
- Updates `current` symlink and cleans up releases older than 30 days

If a deploy fails after creating a release directory, Tako automatically cleans up the partial release.

### Version Naming

Deploy versions are derived from your git state:

| Git state  | Version format    | Example            |
| ---------- | ----------------- | ------------------ |
| Clean tree | `{commit}`        | `abc1234`          |
| Dirty tree | `{commit}_{hash}` | `abc1234_def56789` |
| No git     | `nogit_{hash}`    | `nogit_def56789`   |

## Traffic Routing

When a request hits your server, `tako-server` routes it like this:

1. The request arrives on port 80 or 443.
2. HTTP requests are redirected to HTTPS (`307`) except ACME challenges.
3. The router matches the `Host` header and path against deployed app routes.
4. The most specific match wins -- exact hostnames beat wildcards, longer paths beat shorter ones.
5. For paths with a file extension, Tako serves static files directly from the app's `public/` directory when present. For path-prefixed routes (e.g. `example.com/app/*`), the prefix is stripped when looking up the file.
6. Otherwise, the request is proxied to a healthy app instance via round-robin load balancing.
7. If nothing matches, the request gets a `404`.

### Route Patterns

Routes are configured per environment in `tako.toml` and support several patterns:

```toml
[envs.production]
routes = [
  "api.example.com",           # Exact hostname
  "*.example.com",             # Wildcard subdomain
  "example.com/api/*",         # Hostname + path prefix
  "*.example.com/admin/*",     # Wildcard + path prefix
]
```

Multiple apps can run on the same server with different routes. Route conflicts are detected at deploy time.

### Per-IP Rate Limiting

Tako limits each client IP to a maximum of 2048 concurrent connections. Requests beyond this limit receive a `429` response. Maximum request body size is 128 MiB.

## Health Checks and Instance Lifecycle

Tako actively monitors every app instance with HTTP health probes:

- **Probe interval**: every 1 second
- **Probe request**: `GET /status` with `Host: tako.internal` header and `X-Tako-Internal-Token` header
- **Transport**: the instance's private TCP endpoint on loopback
- **Process exit detection**: before each probe, Tako checks if the process has exited and immediately marks it dead if so
- **Failure threshold**: 1 failure marks the instance dead and triggers replacement. Once an instance passes its first health check, any single probe failure means something is genuinely wrong.
- **Recovery**: a single successful probe restores the instance to healthy

The `tako.sh` SDK implements this health endpoint automatically -- you do not need to add it yourself. The SDK also echoes the internal token header for authentication.

## Channels

Channel reads/connects use one route:

- `GET /channels/<name>` with `Accept: text/event-stream` for SSE
- `GET /channels/<name>` with `Upgrade: websocket` for WebSocket

Channel names are hierarchical: everything after `/channels/` (up to the optional `/messages` suffix for publishes) is the channel name. `chat/room-123` is a valid channel; it lives at `/channels/chat/room-123`.

Channels are declared as files under `channels/<name>.ts`, each default-exporting `defineChannel(pattern, config).$messageTypes<M>()`. Patterns are Hono-style paths with `:param` captures and optional trailing `*` wildcards. The presence of `handler` in the config determines transport: with `handler` the channel is WebSocket (client frames route through the handler and its return value fans out), without it the channel is SSE (broadcast-only; client POSTs are rejected with 405).

Channels keep a bounded replay window for reconnects and reloads. SSE resumes from `Last-Event-ID`, and WebSocket resumes from `last_message_id` in the query string. WebSocket frames stay JSON text frames for both replayed messages and client publish payloads.

## Scaling

The desired instance count is per-server runtime state, managed with `tako scale`:

```bash
tako scale 3                    # 3 instances on every production server
tako scale 0                    # Scale to zero (on-demand mode)
tako scale 2 --server la        # 2 instances on the "la" server only
```

The desired count persists across deploys, rollbacks, and server restarts. You can also use `tako scale` outside a project directory with `--app` and `--env` flags.

### Scale-to-Zero (On-Demand Mode)

When desired instances is `0`, Tako enters on-demand mode. This is the default for new deployments:

- After a deploy, one warm instance runs so traffic is served immediately.
- After the idle timeout (default: 5 minutes), the instance shuts down.
- The next request triggers a cold start -- Tako spins up an instance and holds the request until it is healthy (up to 30 seconds). If no healthy instance is ready before timeout, the proxy returns `504 App startup timed out`.
- If cold start setup fails before readiness, the proxy returns `502 App failed to start`.
- While a cold start is in progress, additional requests queue up (up to 1000 by default). If the queue is full, the proxy returns `503 App startup queue is full` with a `Retry-After: 1` header.

Scaling to zero keeps costs low for apps with intermittent traffic.

## Rolling Updates

When you deploy a new version, Tako replaces instances one at a time with zero downtime:

1. Start a new instance with the new version.
2. Wait for it to pass health checks (up to 30 seconds).
3. Add the new instance to the load balancer.
4. Gracefully drain the old instance (finish in-flight requests, up to 30 seconds).
5. Stop the old instance.
6. Repeat until all instances are replaced.
7. Update the `current` symlink to the new release.

If a new instance fails its health check, Tako automatically rolls back: it kills the new instance, keeps the old ones running, and reports the failure.

When the desired instance count is `0`, rolling deploy still starts one warm instance for the new version so traffic is served immediately after deploy.

### Deploy Lock

Each app environment on a server can only have one deploy running at a time. If a second deploy is attempted for the same app while one is already in progress, it fails immediately with a retry message. Restarting `tako-server` clears the lock -- there is no manual cleanup needed.

## TLS and Certificates

In production, Tako handles TLS automatically:

- **ACME (Let's Encrypt)** issues and renews certificates for your app's domains.
- **SNI-based selection** picks the right certificate during the TLS handshake.
- **Automatic renewal** happens 30 days before expiry with zero downtime. Renewal checks run every 12 hours.
- **HTTP-01 challenges** are handled transparently on port 80.
- **DNS-01 challenges** are supported for wildcard certificates via the [`lego`](https://go-acme.github.io/lego/) ACME client, which `tako-server` downloads and installs on-demand. Run `tako servers setup-wildcard` to configure DNS credentials before deploying wildcard routes.
- **Fallback certificate**: if no certificate exists yet for a hostname, Tako serves a self-signed default so HTTPS still completes and routing can return normal HTTP status codes.
- **Private/local hostnames** (like `localhost`, `*.local`, `*.test`): Tako skips ACME and generates a self-signed certificate during deploy.

For local development, the dev daemon uses its own local CA:

- Root CA generated once on first run, private key stored in system keychain
- Leaf certificates generated on-the-fly for each app domain
- On first run, Tako installs the root CA into the system trust store (may prompt for your password)
- No browser security warnings once the CA is trusted
- Public CA cert available at `{TAKO_HOME}/ca/ca.crt` (useful for `NODE_EXTRA_CA_CERTS`)

## Edge Proxy Caching

The proxy includes a built-in response cache for `GET` and `HEAD` requests:

- Cache follows your app's `Cache-Control` and `Expires` headers -- no implicit TTL is added. Responses without explicit cache directives are not stored.
- Cache keys are scoped by host + URI.
- Storage is in-memory LRU with a 256 MiB total limit and 8 MiB per response.
- WebSocket upgrades bypass the cache.

## Communication Protocol

The CLI and `tako-server` communicate over a Unix socket at `/var/run/tako/tako.sock` using JSON messages. The socket uses a symlink-based path: the active server creates a PID-specific socket and atomically updates the `tako.sock` symlink, so clients always connect to the current process.

### Protocol Commands

| Command            | Purpose                                                                 |
| ------------------ | ----------------------------------------------------------------------- |
| `hello`            | Protocol negotiation and capability discovery                           |
| `prepare_release`  | Download runtime and install production dependencies before deploy      |
| `deploy`           | Deploy a new version with routes and optional secrets                   |
| `scale`            | Change desired instance count                                           |
| `delete`           | Remove an app's state and routes                                        |
| `rollback`         | Roll back to a previous release                                         |
| `routes`           | List current route mappings                                             |
| `stop`             | Stop a running app                                                      |
| `status`           | Get status of a specific app                                            |
| `list`             | List all deployed apps with their status                                |
| `update_secrets`   | Update secrets for a deployed app (refreshes workers + rolling restart) |
| `list_releases`    | Return release/build history for an app                                 |
| `get_secrets_hash` | Get the SHA-256 hash of an app's current secrets                        |
| `server_info`      | Return server runtime config and upgrade mode                           |
| `enter_upgrading`  | Acquire the durable upgrade lock (rejects mutating commands)            |
| `exit_upgrading`   | Release the durable upgrade lock                                        |

App instances do not connect to this management socket. Instead, `tako-server` manages their lifecycle directly (spawn, health check, stop) and proxies HTTP traffic to each instance's private TCP endpoint on loopback.

### Instance Transport

Deployed app instances bind to `127.0.0.1` on an OS-assigned port (`PORT=0`, `HOST=127.0.0.1`). The SDK signals readiness to `tako-server` by writing the bound port to fd 4 once listening. The server then routes traffic and health probes to that loopback endpoint.

Secrets are passed to instances via file descriptor 3 at spawn time -- the server writes JSON-serialized secrets to a pipe, and the child process reads fd 3 before any user code runs. Secrets never touch disk as plaintext.

## Server Filesystem Layout

On each deployment server, Tako organizes files under `/opt/tako/`:

```
/opt/tako/
  config.json              # Server-level config (name, DNS provider)
  tako.db                  # Persisted app state (SQLite)
  runtimes/{tool}/{version}/  # Downloaded runtime binaries
  acme/credentials.json    # ACME account credentials
  certs/{domain}/          # TLS certificates (fullchain.pem, privkey.pem)
  apps/{app}/{env}/
    current -> releases/{version}   # Active release symlink
    releases/{version}/             # Release files + app.json
    shared/logs/                    # Persistent log storage
```

Each app + environment combination gets its own directory, so you can run `my-app/production` and `my-app/staging` on the same server without conflicts.

Runtime binaries are downloaded directly from upstream releases by `tako-server` using download specs in runtime plugins (no external version manager needed). Binaries are cached and verified with SHA-256 checksums.

## Monitoring

Tako-server exposes Prometheus metrics at `http://127.0.0.1:9898/` (localhost only, configurable with `--metrics-port`):

| Metric                               | Type      | Description                              |
| ------------------------------------ | --------- | ---------------------------------------- |
| `tako_http_requests_total`           | Counter   | Proxied requests by status class         |
| `tako_http_request_duration_seconds` | Histogram | Request latency distribution             |
| `tako_http_active_connections`       | Gauge     | Currently active connections             |
| `tako_cold_starts_total`             | Counter   | Cold starts triggered                    |
| `tako_cold_start_duration_seconds`   | Histogram | Cold start duration distribution         |
| `tako_instance_health`               | Gauge     | Instance health (1=healthy, 0=unhealthy) |
| `tako_instances_running`             | Gauge     | Number of running instances              |

All metrics carry `server` and `app` labels. Only proxied requests are measured -- ACME challenges, static asset responses, and unmatched 404s are excluded.

Scrape with Prometheus, Grafana Cloud, Datadog, or any compatible platform. For remote scraping, expose port 9898 on a private network interface (e.g. via Tailscale or WireGuard).

## Workflows

Apps can run durable background tasks alongside their HTTP instances. Drop a file in `workflows/` (JS/TS) or register handlers in a separate `cmd/worker/main.go` binary (Go), then enqueue from any request handler:

```ts
import sendEmail from "../workflows/send-email";
await sendEmail.enqueue({ to: "user@example.com" });
```

Each workflow file default-exports a typed handle from `defineWorkflow<P>("name", handler)`. Its `.enqueue(payload, opts?)` is type-checked against the declared `P` — no typegen required for enqueue typing.

Tako runs each app's workers in a **separate process** from HTTP instances (so heavy workflow deps — image libs, ML bindings — don't inflate the HTTP binary). The worker is scale-to-zero by default: it spawns on the first enqueue or cron tick, exits after 5 minutes idle, and respawns on demand.

Features:

- **Retries with exponential backoff** (1s base, capped at 1h, ±20% jitter)
- **Delayed runs** (`runAt: new Date(...)`) and **cron schedules** (`export const schedule = "0 9 * * *"`)
- **Multi-step workflows** with `ctx.run("name", fn)` — step results are checkpointed to SQLite, so a crashed workflow resumes from the last completed step on retry
- **Graceful drain** — `tako stop` and `tako delete` wait for in-flight tasks (up to 120s) before tearing down

Queue state lives in `{tako_data_dir}/apps/<app>/runs.db` (SQLite with WAL). tako-server owns the file; the worker process polls it. Enqueues from the SDK go over a per-app unix socket — no external queue service, no Redis, no Postgres required.

See [`tako.toml` → Workflows](/docs/tako-toml#workflows) for `[servers.X.workflows]` config details.

## What to Read Next

- [CLI Reference](/docs/cli) for command details and flags
- [`tako.toml` Reference](/docs/tako-toml) for configuration options
- [Presets](/docs/presets) for runtime and framework preset details
- [Deployment Guide](/docs/deployment) for production setup walkthrough
- [Development Guide](/docs/development) for local dev workflow
