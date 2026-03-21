---
layout: ../../layouts/DocsLayout.astro
title: "Tako Docs - How Tako Works"
heading: How Tako Works
current: how-tako-works
---

# How Tako Works

Tako is a deployment and development platform that takes your JavaScript/TypeScript app from local development to production with minimal configuration. It handles building, deploying, routing, TLS, health checks, scaling, and rolling updates -- so you can focus on your app.

This page walks through Tako's architecture, how requests flow through it, and the key concepts that make it tick.

## The Three Components

Tako is made up of three parts that work together:

**`tako` CLI** -- Runs on your machine. Handles project setup (`tako init`), local development (`tako dev`), building and deploying (`tako deploy`), scaling (`tako scale`), and managing servers and secrets.

**`tako-server`** -- Runs on your deployment server(s). Manages app processes, routes incoming traffic, handles TLS certificates, performs health checks, and orchestrates rolling updates. Built on [Pingora](https://github.com/cloudflare/pingora), Cloudflare's production proxy.

**`tako.sh` SDK** -- An npm package your app uses. Provides runtime adapters for Bun, Node.js, and Deno, a standard fetch handler interface, and a built-in health check endpoint that Tako uses to verify your app is running.

## Two Paths: Management and Traffic

Everything in Tako falls into one of two paths:

- **Management path** -- CLI commands that change state: `tako deploy`, `tako scale`, `tako secrets set`, `tako servers add`. These talk to `tako-server` over a Unix socket.
- **Traffic path** -- Real HTTP/HTTPS requests from users flowing through the proxy to your app instances. This is pure Pingora performance.

This separation keeps things predictable. Commands change your deployment state. The proxy serves traffic.

## Local Development

When you run `tako dev`, the CLI starts a persistent local daemon (`tako-dev-server`) that manages your app process and provides HTTPS routing on `*.tako.test` domains.

```
tako dev
# Your app is now live at https://my-app.tako.test
```

Here is what happens under the hood:

1. The CLI ensures the dev daemon is running (starts it if needed).
2. It registers the selected config file with the daemon.
3. One local instance of your app starts immediately.
4. HTTPS is set up using a local Certificate Authority -- no browser security warnings once the CA is trusted.

The daemon supports multiple apps simultaneously, each on its own `*.tako.test` subdomain. On macOS, Tako installs a loopback proxy so your app is available on standard ports (443/80) without `sudo`.

Your app stays running while you work. If you press `b`, it backgrounds to the daemon and the CLI exits -- your app keeps serving. Run `tako dev` again to reattach. Press `Ctrl+c` to stop the app entirely.

After 10 minutes with no attached CLI client, the daemon idles the app process. The next HTTP request wakes it back up automatically.

## Deploying to Production

Deployment is a single command:

```
tako deploy
```

This builds your app locally, uploads the artifact to your server(s), and performs a zero-downtime rolling update. Here is the full flow:

### 1. Validate and Prepare

Tako validates the selected config file, resolves your app name, checks that secrets are in order, and verifies server connectivity.

### 2. Build Locally

Your app is always built on your machine, never on the server. Tako:

- Copies your project into a clean workdir (respecting `.gitignore`), symlinks `node_modules/` from the original tree
- Runs your build commands (`[build]` or `[[build_stages]]`)
- Packages the result into a deploy artifact (excluding `node_modules/` -- the server installs its own production dependencies)
- Caches artifacts locally so unchanged builds are instant on subsequent deploys

### 3. Upload and Deploy

For each target server (in parallel):

- Acquires a deploy lock to prevent concurrent deploys
- Uploads and extracts the target artifact
- Syncs secrets only if they have changed (compares hashes)
- Sends the deploy command to `tako-server`
- Runs runtime prep (production dependency install via the runtime's package manager)
- Performs a rolling update of app instances
- Releases the lock and cleans up old releases

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
2. HTTP requests are redirected to HTTPS (except ACME challenges).
3. The router matches the `Host` header and path against deployed app routes.
4. The most specific match wins -- exact hostnames beat wildcards, longer paths beat shorter ones.
5. For paths with a file extension, Tako serves static files directly from the app's `public/` directory when present.
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

## Health Checks and Instance Lifecycle

Tako actively monitors every app instance with HTTP health probes:

- **Probe interval**: every 1 second
- **Probe request**: `GET /status` with `Host: tako` header
- **Transport**: Unix socket (in production deployments)
- **Unhealthy**: 2 consecutive failures removes the instance from load balancing
- **Dead**: 5 consecutive failures kills the instance process
- **Recovery**: a single successful probe restores the instance to healthy

The `tako.sh` SDK implements this health endpoint automatically -- you do not need to add it yourself.

## Scaling

The desired instance count is per-server runtime state, managed with `tako scale`:

```bash
tako scale 3                    # 3 instances on every production server
tako scale 0                    # Scale to zero (on-demand mode)
tako scale 2 --server la        # 2 instances on the "la" server only
```

The desired count persists across deploys, rollbacks, and server restarts.

### Scale-to-Zero (On-Demand Mode)

When desired instances is `0`, Tako enters on-demand mode:

- After a deploy, one warm instance runs so traffic is served immediately.
- After the idle timeout (default: 5 minutes), the instance shuts down.
- The next request triggers a cold start -- Tako spins up an instance and holds the request until it is healthy (up to 30 seconds).
- While a cold start is in progress, additional requests queue up (up to 100 by default).

This is the default for new deployments, keeping costs low for apps with intermittent traffic.

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

## TLS and Certificates

In production, Tako handles TLS automatically:

- **ACME (Let's Encrypt)** issues and renews certificates for your app's domains.
- **SNI-based selection** picks the right certificate during the TLS handshake.
- **Automatic renewal** happens 30 days before expiry with zero downtime.
- **HTTP-01 challenges** are handled transparently on port 80.
- **Fallback certificate**: if no certificate exists yet for a hostname, Tako serves a self-signed default so HTTPS still completes.

For local development, the dev daemon uses its own local CA with certificates generated on-the-fly for each `*.tako.test` domain.

### Edge Proxy Caching

The proxy includes a built-in response cache for `GET` and `HEAD` requests:

- Cache follows your app's `Cache-Control` and `Expires` headers -- no implicit TTL is added.
- Cache keys are scoped by host + URI.
- Storage is in-memory LRU with a 256 MiB total limit and 8 MiB per response.
- WebSocket upgrades bypass the cache.

## Communication Protocol

The CLI and `tako-server` communicate over a Unix socket at `/var/run/tako/tako.sock` using JSON messages. Key commands include:

| Command    | Purpose                                               |
| ---------- | ----------------------------------------------------- |
| `hello`    | Protocol negotiation and capability discovery         |
| `deploy`   | Deploy a new version with routes and optional secrets |
| `scale`    | Change desired instance count                         |
| `delete`   | Remove an app's state and routes                      |
| `rollback` | Roll back to a previous release                       |
| `routes`   | List current route mappings                           |

App instances do not connect to this management socket. Instead, `tako-server` manages their lifecycle directly (spawn, health check, stop) and proxies HTTP traffic to per-instance Unix sockets.

## Server Filesystem Layout

On each deployment server, Tako organizes files under `/opt/tako/`:

```
/opt/tako/
  config.json              # Server-level config
  tako.db                  # Persisted app state
  runtimes/{tool}/{version}/  # Downloaded runtime binaries
  certs/{domain}/          # TLS certificates
  apps/{app}/{env}/
    current -> releases/{version}   # Active release symlink
    releases/{version}/             # Release files + app.json
    shared/logs/                    # Persistent log storage
```

Each app + environment combination gets its own directory, so you can run `my-app/production` and `my-app/staging` on the same server without conflicts.

## Monitoring

Tako-server exposes Prometheus metrics at `http://127.0.0.1:9898/` (localhost only):

- Request counts by status class (2xx/3xx/4xx/5xx)
- Request latency distribution
- Active connections
- Cold start count and duration
- Instance health and running instance count

All metrics carry `server` and `app` labels. Scrape with Prometheus, Grafana Cloud, Datadog, or any compatible platform.

## What to Read Next

- [CLI Reference](/docs/cli) for command details and flags
- [`tako.toml` Reference](/docs/tako-toml) for configuration options
- [Presets](/docs/presets) for runtime and framework preset details
- [Deployment Guide](/docs/deployment) for production setup walkthrough
- [Development Guide](/docs/development) for local dev workflow
