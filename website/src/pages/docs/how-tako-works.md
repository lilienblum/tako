---
layout: ../../layouts/DocsLayout.astro
title: Tako Docs - How Tako Works
heading: How Tako Works
current: how-tako-works
---

# How Tako Works

Tako has two main paths:

- Management path: commands like `tako dev`, `tako deploy`, `tako releases ...`, `tako logs`, and `tako servers ...`.
- Traffic path: real HTTP/HTTPS requests flowing to your app instances.

This split keeps day-to-day operations predictable: commands change state, routing serves traffic.

CLI output conventions across commands:

- default output is concise for humans
- `--verbose` enables detailed technical progress
- interactive long-running steps use spinner progress indicators

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
- App identity comes from top-level `name` when set, otherwise from sanitized project directory name.

macOS local networking behavior:

- Tako sets up local forwarding so public URLs stay on default ports (`:443` and `:80` redirect behavior).
- On first setup/trust flow, elevated access may be required to install/trust the local CA and configure forwarding.
- If forwarding later appears inactive, `tako dev` prints the detected reason before prompting again (missing pf rules, runtime reset after reboot/pf reset, or local listeners on `127.0.0.1:80/443`).

Session and idle behavior:

- App stays running while active.
- Idle instances stop after timeout.
- Re-running `tako dev` from the same directory attaches instead of starting a separate owner.

## First Deployment (`tako deploy`)

High-level deploy flow:

1. Validate config/runtime/secrets/server target metadata.
2. Resolve source bundle root and app subdirectory (git root when available; otherwise app directory).
3. Create a source archive (`.tako/artifacts/{version}-source.tar.gz`) from filtered source files.
   - Version format: clean git tree => `{commit}`; dirty git tree => `{commit}_{source_hash8}`; no git commit => `nogit_{source_hash8}`.
4. Resolve build preset (top-level runtime-local `preset` override or adapter base default from top-level `runtime`/detection), fetching unpinned official aliases from `master`, then write resolved metadata to `.tako/build.lock.json`.
5. Build target-specific artifacts locally (Docker or local host based on preset `[build].container`, with defaults derived from `[build].targets`), running preset stage first then app `[[build.stages]]`, with deterministic local artifact cache reuse when inputs are unchanged.
6. Deploy to target servers in parallel over SSH.
7. On each server: lock, upload/extract target artifact, finalize `app.json`, send deploy command with merged env/secrets payload, run runtime prep (Bun dependency install), rolling update, unlock.

Important deployment behavior:

- `production` is the default environment when `--env` is omitted.
- `development` is reserved for `tako dev` and cannot be deployed.
- Source bundle filtering uses `.gitignore`.
- Deploy always excludes `.git/`, `.tako/`, `.env*`, `node_modules/`, and `target/`.
- Deploy always builds artifacts locally (Docker or local host based on preset build mode); servers do not run app build steps during deploy.
- Docker builds reuse per-target dependency cache volumes (mise + runtime cache mounts) keyed by cache kind + target label + builder image while still creating fresh build containers each deploy.
- Default Docker builder images are target-libc specific: `ghcr.io/lilienblum/tako-builder-musl:v1` for `*-musl` targets and `ghcr.io/lilienblum/tako-builder-glibc:v1` for `*-glibc` targets.
- Runtime version resolution is mise-aware: Tako tries `mise exec -- <tool> --version` when local `mise` is available (and in Docker build contexts), then falls back to `mise.toml`, then `latest`; deploy writes release `mise.toml` so server runtime matches build runtime.
- During local builds, when `mise` is available, stage commands run through `mise exec -- sh -lc ...`.
- Preset runtime fields use top-level `main`/`install`/`start` keys (legacy preset `[deploy]` is not supported).
- Top-level `preset` in `tako.toml` must be runtime-local (for example `tanstack-start` with `runtime = "bun"`); namespaced aliases like `js/tanstack-start` are rejected and `github:` refs are not supported.
- Runtime base presets provide defaults for `dev`/`install`/`start`, `[build].install`/`[build].build`, and `[build].exclude`/`[build].targets`/`[build].container`.
- JS runtime base presets (`bun`, `node`, `deno`) set `[build].container = false`, so JS builds default to local host mode unless preset `container = true` is set.
- Preset `[build].exclude` appends to runtime-base excludes (base-first, deduplicated), while preset `[build].targets` and `[build].container` override when set.
- Preset `[[build.stages]]` is not supported; app-level custom stages are configured in `tako.toml` under `[[build.stages]]`.
- Per target build order is fixed: preset `[build].install`/`[build].build` first, then app `[[build.stages]]` in declaration order.
- Artifact filters use project `[build].include` (optional), plus effective preset `[build].exclude` and project `[build].exclude`.
- Bun deploys exclude `node_modules` by default and install release dependencies on server before startup (`bun install --production`).
- Final runtime `app.json` written on server includes optional release metadata (`commit_message`, `git_dirty`) used by `tako releases ls`.
- Target artifacts are cached in `.tako/artifacts/` and reused across deploys when source/preset/target/build inputs are unchanged.
- Cached artifacts are checksum-verified; invalid cached entries are rebuilt automatically.
- Before packaging each target artifact, deploy verifies the resolved `main` exists in the post-build app directory.
- On every deploy, Tako prunes local `.tako/artifacts/` cache (best-effort): keeps 30 newest source archives, keeps 90 newest target artifacts, and removes orphan target metadata files.
- Deploy runtime `main` is resolved from `tako.toml main`, then preset top-level `main`; for JS runtimes (`bun`, `node`, `deno`) when preset `main` is `index.<ext>` or `src/index.<ext>` (`ts`/`tsx`/`js`/`jsx`), Tako tries `index.<ext>` first, then `src/index.<ext>`.
- Deploy app identity is resolved from top-level `name` when set, otherwise sanitized project directory name.
- Server install resolves host target (`arch` + `libc`) and downloads matching `tako-server-linux-<arch>-<libc>` artifact.
- Server install also installs `mise` (package-manager first, then upstream installer fallback when unavailable).
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
4. For static asset requests (paths with a file extension), Tako serves matching files directly from the deployed app `public/` directory when present. For path-prefixed routes, it also tries a prefix-stripped static lookup.
5. Otherwise request is proxied to a healthy instance.
6. If nothing matches, request returns `404`.

Wildcard and path routes are supported, for example:

- `api.example.com`
- `*.example.com`
- `example.com/api/*`
- `*.example.com/admin/*`

Exact path routes normalize trailing slash, so `example.com/api` and `example.com/api/` are equivalent.

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
- Forwarded private/local hosts (`localhost`, `*.localhost`, single-label hosts, and reserved suffixes like `*.local`) are treated as already HTTPS when proxy proto metadata is missing to avoid local redirect loops.
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
- `tako releases ls` / `tako releases rollback`: inspect release history and roll back to a previous release id.
- `tako secrets ...`: encrypted secret management and sync to runtime.
- `tako servers restart|reload|upgrade`: runtime lifecycle operations for remote `tako-server`.

Use this page as the mental model, then use [CLI Reference](/docs/cli) for command details.
