---
layout: ../../layouts/DocsLayout.astro
title: Tako Docs - How Tako Works
heading: How Tako Works
current: architecture
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

See full config details in [`tako.toml` Reference](/docs/tako-toml-reference).

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

1. Validate config/runtime/secrets.
2. Build locally.
3. Create a release archive (`.tako/build/{version}.tar.gz`).
4. Deploy to target servers in parallel over SSH.
5. On each server: lock, upload/extract, apply env/secrets, rolling update, unlock.

Important deployment behavior:

- `production` is the default environment when `--env` is omitted.
- `development` is reserved for `tako dev` and cannot be deployed.
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
- Failure handling:
  - consecutive failures mark instances unhealthy and remove them from balancing
  - deeper failure threshold marks instances stopped/killed
- Recovery: successful probe restores healthy status

Instance mode by `instances`:

- `instances = 0`: on-demand mode (scale-to-zero when idle)
- `instances > 0`: always-on baseline, with health-based rotation during deploy

For on-demand deploys (`instances = 0`), deploy still does a startup validation instance before returning to idle-on-demand mode.

## TLS and Certificates

Remote TLS behavior:

- HTTPS is default for remote app routes.
- HTTP requests redirect to HTTPS by default.
- Exceptions like ACME challenge paths remain on HTTP where needed.

Certificate behavior:

- Certs are selected by SNI.
- ACME (Let's Encrypt) is used for issuance/renewal.
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

Use this page as the mental model, then use [CLI Reference](/docs/cli-reference) for command details.
