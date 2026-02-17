---
layout: ../../layouts/DocsLayout.astro
title: Tako Docs - How Tako Works
heading: How Tako Works
current: how-tako-works
---

# How Tako Works

Tako has two main paths:

- Management path: commands like `tako dev`, `tako deploy`, `tako logs`, and `tako servers ...`.
- Traffic path: real HTTP/HTTPS requests flowing to your app instances.

This split keeps day-to-day operations predictable: commands change state, routing serves traffic.

## Components and Roles

| Component          | Runs where      | Main role                                                                        |
| ------------------ | --------------- | -------------------------------------------------------------------------------- |
| `tako` CLI         | Your machine    | Project setup, dev client, build/deploy orchestration, server/secrets management |
| `tako-dev-server`  | Your machine    | Local HTTPS ingress for `*.tako.local`, local app lifecycle                      |
| `tako-server`      | Deployment host | Remote app lifecycle, routing, health checks, load balancing, TLS                |
| Your app instances | Local or remote | Serve your app logic                                                             |

## Configuration Sources

Tako reads from three main places:

- Project config: `tako.toml`
- Project secrets: `.tako/secrets` (encrypted)
- Global server inventory: `~/.tako/config.toml` (`[[servers]]` entries)

Key config rules:

- Non-development environments must define `route` or `routes`.
- Each environment can use `route` or `routes`, not both.
- Route values must include a hostname.
- Development routes must be `{app}.tako.local` or a subdomain of it.

See full config details in [`tako.toml` Reference](/docs/tako-toml).

## Local Development (`tako dev`)

When you run `tako dev`, the CLI behaves like a client for a persistent local daemon.

- It ensures `tako-dev-server` is running.
- It registers the current app directory with the daemon.
- It starts one local instance immediately.
- It exposes HTTPS routes on `*.tako.local` with a fixed daemon listen port (`127.0.0.1:47831`).

Default route behavior:

- If `[envs.development]` routes are configured, those are used.
- Otherwise, Tako uses `{app}.tako.local`.

macOS local networking behavior:

- Tako sets up local forwarding so public URLs stay on default ports (`:443` and `:80` redirect behavior).
- On first setup/trust flow, elevated access may be required to install/trust the local CA and configure forwarding.

Session and idle behavior:

- App stays running while active.
- Idle instances stop after timeout.
- Re-running `tako dev` from the same directory attaches instead of starting a separate owner.

## First Deployment (`tako deploy`)

High-level deploy flow:

1. Validate config/runtime/secrets/server target metadata.
2. Resolve source bundle root and app subdirectory.
3. Create a source archive (`.tako/artifacts/{version}-source.tar.gz`) from filtered source files.
   - Version format: clean git tree => `{commit}`; dirty git tree => `{commit}_{source_hash8}`; no git commit => `nogit_{source_hash8}`.
4. Resolve build preset (`[build].preset`, default `bun`) and lock it to a commit in `.tako/build.lock.json`.
5. Build target-specific artifacts locally in Docker (one artifact per required server target), with deterministic local artifact cache reuse when inputs are unchanged.
6. Deploy to target servers in parallel over SSH.
7. On each server: lock, upload/extract target artifact, finalize `app.json`, send deploy command with merged env/secrets payload, run runtime prep (Bun dependency install), rolling update, unlock.

Important deployment behavior:

- `production` is the default environment when `--env` is omitted.
- `development` is reserved for `tako dev` and cannot be deployed.
- Source bundle filtering uses `.gitignore`.
- Deploy always excludes `.git/`, `.tako/`, `.env*`, `node_modules/`, and `target/`.
- Deploy builds artifacts locally in Docker containers (servers do not run app build steps during deploy).
- Deploy reuses per-target Docker dependency cache volumes (keyed by target label + builder image) while still creating fresh build containers each deploy.
- Artifact filters use project `[build].include` (optional), plus preset `exclude` and project `[build].exclude`.
- Bun deploys exclude `node_modules` by default and install release dependencies on server before startup (`bun install --production`).
- If app `package.json` uses `workspace:` dependencies, deploy vendors those packages into the artifact (`tako_vendor/`) and rewrites them to local `file:` specs.
- Target artifacts are cached in `.tako/artifacts/` and reused across deploys when source/preset/target/build inputs are unchanged.
- Cached artifacts are checksum-verified; invalid cached entries are rebuilt automatically.
- On every deploy, Tako prunes local `.tako/artifacts/` cache (best-effort): keeps 30 newest source archives, keeps 90 newest target artifacts, and removes orphan target metadata files.
- Deploy runtime `main` is resolved from `tako.toml main`, then `package.json main`.
- Server install resolves host target (`arch` + `libc`) and downloads matching `tako-server-linux-<arch>-<libc>` artifact.
- For production without explicit server mapping:
  - With one global server, Tako can guide/persist mapping.
  - With multiple global servers (interactive), Tako prompts for selection.
- Deploy lock path: `/opt/tako/apps/{app}/.deploy_lock`.
- Rolling updates are health-gated and rollback old traffic on failure.

## Runtime Traffic Routing

Remote request path:

1. Request lands on `tako-server` (`:80`/`:443` by default).
2. Router matches host/path against deployed app routes.
3. Most specific match wins (exact host/path before broader wildcard patterns).
4. Request is proxied to a healthy instance.
5. If nothing matches, request returns `404`.

Wildcard and path routes are supported, for example:

- `api.example.com`
- `*.example.com`
- `example.com/api/*`
- `*.example.com/admin/*`

## Health and Instance Lifecycle

Tako uses active HTTP probing as the source of truth for instance health.

- Probe interval: 1s (default)
- Probe target: `GET /status` with `Host: tako.internal`
- Failure handling:
  - consecutive failures mark instances unhealthy and remove them from balancing
  - deeper failure threshold marks instances stopped/killed
- Recovery: successful probe restores healthy status

Instance mode by `instances`:

- `instances = 0`: on-demand mode (scale-to-zero when idle). Deploy keeps one warm instance running, then idle timeout can scale it to zero. Once at zero, the next request waits for cold start readiness up to startup timeout (30s default); if still not ready, it returns `504 App startup timed out`. If startup fails early, it returns `502 App failed to start`.
- `instances > 0`: always-on baseline, with health-based rotation during deploy

For on-demand deploys (`instances = 0`), deploy starts one warm instance; if warm startup fails, deploy fails.

## TLS and Certificates

Remote TLS behavior:

- HTTPS is default for remote app routes.
- HTTP requests redirect to HTTPS by default (307 with `Cache-Control: no-store`).
- `/.well-known/acme-challenge/*` remains on HTTP for ACME.
- Internal `Host: tako.internal` + `/status` stays on HTTP.
- Non-internal-host requests are routed to apps normally (no reserved `/_tako/*` edge namespace).

Certificate behavior:

- Certs are selected by SNI.
- ACME (Let's Encrypt) is used for issuance/renewal.
- Private/local route hostnames (`localhost`, `*.localhost`, single-label hosts, and reserved suffixes like `*.local`) use deploy-time self-signed certs instead of ACME.
- Renewal is automatic.
- Wildcard routing is supported, but automated wildcard cert issuance via DNS-01 is not the default path.

## Filesystem Layout (Remote)

Typical remote layout:

- `/opt/tako/apps/{app}/current` -> active release symlink
- `/opt/tako/apps/{app}/releases/{version}/` -> release content
- `/opt/tako/apps/{app}/shared/` -> shared app data (for example logs)
- `/var/run/tako/tako.sock` -> management socket

## Operational Commands in Context

- `tako servers status`: snapshot of server/app state across configured servers.
- `tako logs --env <env>`: live logs across mapped servers for an environment.
- `tako secrets ...`: encrypted secret management and sync to runtime.
- `tako servers restart|reload|upgrade`: runtime lifecycle operations for remote `tako-server`.

Use this page as the mental model, then use [CLI Reference](/docs/cli) for command details.
