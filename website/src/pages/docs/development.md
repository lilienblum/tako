---
layout: ../../layouts/DocsLayout.astro
title: Tako Docs - Development
heading: Development
current: development
---

# Development (Local)

This guide covers local development with Tako: trusted HTTPS, `.tako.test` URLs, and what `tako dev` is doing behind the scenes.

CLI output follows shared conventions: concise by default, append-only execution transcript with `--verbose`, and spinner progress for long interactive steps. Note: `--verbose` controls Tako CLI and dev-server verbosity; the app's own log level is controlled separately by `log_level` in `[envs.development]` (defaults to `debug`).

## Overview

- `tako dev` is a **client** that talks to a background daemon: `tako-dev-server`.
- Installed CLI distributions include `tako`, `tako-dev-server`, and `tako-loopback-proxy`.
- When running from source, the local helper binaries are built from the `tako` package (`cargo build -p tako --bin tako-dev-server --bin tako-loopback-proxy`).
- `tako dev` owns your app process lifecycle and spawns the app locally on an ephemeral port.
- `tako-dev-server` terminates HTTPS and routes by `Host` to that app port. App registrations are persisted in SQLite (in the platform data directory).
- On macOS, `tako dev` also ensures a dedicated `127.77.0.1` loopback alias plus a socket-activated `launchd` helper (`tako-loopback-proxy`) for loopback-only `:80/:443` ingress.
- The client **registers** the app with the daemon (project directory is the unique key). App statuses: `running`, `idle`, `stopped`.
- Press `b` to background the app — the CLI exits but the daemon keeps the process alive and routes active. Press `Ctrl+c` to stop the app.
- `tako dev` watches [`tako.toml`](/docs/tako-toml) for changes. If dev env vars change, it restarts the app. If `[envs.development]` routes change, it re-registers routes with the daemon.

## Files Created

Paths follow platform conventions (`~/Library/Application Support/tako/` on macOS, `~/.local/share/tako/` and `~/.config/tako/` on Linux).
When running from a source checkout in debug builds, Tako prefers `{repo}/local-dev/.tako/` instead.

Created/used by `tako dev` / `tako doctor`:

- `{TAKO_HOME}/ca/ca.crt`: local dev root CA certificate (public). The private key is stored in the OS keychain.

Created/used by `tako-dev-server`:

- `{TAKO_HOME}/dev-server.sock`: Unix socket for the control protocol.
- `{TAKO_HOME}/dev/logs/{app}-{hash}.jsonl`: shared app log stream; persisted records use a single `timestamp` field (`hh:mm:ss`).

## Running locally

From an app directory:

```bash
tako dev
```

`tako dev` resolves app name from top-level `name` when set, otherwise from sanitized project directory name. This name is used for `{app}.tako.test`.

If `[envs.development]` omits routes, `tako dev` defaults to `{app}.tako.test`.

Useful commands:

- `tako doctor`
- `tako dev` (streams logs directly to stdout in interactive terminals)
- `tako dev <DIR>` (run as if invoked from another project directory)
- `tako releases ls --env <environment>` (inspect remote release history after deploys)

Default URL:

- macOS (with loopback proxy): `https://{app}.tako.test/`
- Other platforms: `https://{app}.tako.test:47831/`

## Vite Apps

If your app runs `vite dev` under `tako dev` and uses `tako.sh/vite`:

- configure Vite with `import { tako } from "tako.sh/vite"` and `plugins: [tako()]`
- the plugin adds `.tako.test` to Vite `server.allowedHosts` so local Tako hosts are accepted
- when `PORT` is set by `tako dev`, Vite binds to `127.0.0.1:$PORT` with `strictPort: true`

## Local Workflow Checklist

Fastest happy-path loop:

1. Run `tako doctor` to confirm local prerequisites and DNS/loopback-proxy state.
2. Run `tako dev` from your app directory.
3. Open the development URL and verify app responses.
4. Make code/config edits while `tako dev` stays running.
5. Pipe or redirect stdout for non-interactive use (e.g. `tako dev | grep error`).

## Trusted HTTPS (Local CA)

On first run, Tako creates (or reuses) a local root CA and installs it into the system trust store. On macOS, this may ask for your password.

If your browser still complains about certs:

- Quit and restart the browser.
- Verify the CA is installed in Keychain Access and marked trusted.

## Ports and Privileges

- `tako-dev-server` listens on fixed local HTTPS port `47831`.
- On macOS, Tako installs a root `launchd` setup helper that restores the `127.77.0.1` loopback alias at boot, then re-registers the loopback proxy.
- The proxy itself listens only on `127.77.0.1:443` and `127.77.0.1:80`.
- That proxy forwards raw TCP to `127.0.0.1:47831` and `127.0.0.1:47830`, so public dev URLs can use `:443` with no explicit port.
- The helper is socket-activated and may exit after a long idle window; launchd starts it again on the next request.
- Installing or repairing the helper requires elevated privileges, so Tako requests one-time setup when needed.

## Running Slow E2E Tests

Deploy e2e uses Docker/SSH and is opt-in:

- `just e2e e2e/fixtures/js/bun`
- `just e2e e2e/fixtures/js/tanstack-start`

Deploy e2e exercises artifact-cache behavior too: first deploy builds target artifacts, then unchanged redeploy reuses verified cached artifacts from `.tako/artifacts/`.
When top-level `preset` is omitted, dev/deploy choose adapter base preset from top-level `runtime` when set, otherwise adapter detection (`unknown` falls back to `bun`). In `tako dev`, omitted top-level `preset` ignores preset top-level `dev` and runs runtime-default command with resolved `main` (`bun run node_modules/tako.sh/src/entrypoints/bun.ts {main}`, `node --experimental-strip-types node_modules/tako.sh/src/entrypoints/node.ts {main}`, or `deno run --allow-net --allow-env --allow-read node_modules/tako.sh/src/entrypoints/deno.ts {main}`). When top-level `preset` is explicitly set, `tako dev` uses preset top-level `dev`. Namespaced aliases like `js/tanstack-start` are rejected, and `github:` refs are not supported.
For unpinned official aliases, preset resolution fetches from `master`; fetch failures fail resolution, and runtime base aliases (`bun`, `node`, `deno`) fall back to embedded defaults when missing from fetched family manifests.
When preset build mode resolves to container (`[build].container`), containerized builds reuse per-target Docker dependency cache volumes (prefix `tako-build-cache-`) across deploy runs.
Containerized deploy builds default to `ghcr.io/lilienblum/tako-builder-musl:v1` for `*-musl` targets and `ghcr.io/lilienblum/tako-builder-glibc:v1` for `*-glibc` targets.
Preset artifact filters use preset `[build].exclude`; runtime base presets provide defaults for `install`/`start`, `[build].install`/`[build].build`, and `[build].exclude`/`[build].targets`/`[build].container`, while explicit top-level `preset` uses preset top-level `dev`. JS runtime base presets (`bun`, `node`, `deno`) set `[build].container = false`, so JS builds default to local host mode unless preset `container = true` is set. Preset `[build].exclude` appends to runtime-base excludes (base-first, deduplicated), while `[build].targets` and `[build].container` override when set.
During local builds, when `mise` is available, stage commands run through `mise exec -- sh -lc ...`.
Preset `[[build.stages]]` is not supported; app-level custom stages are configured in `tako.toml` under `[[build.stages]]`.
Per target build order is fixed: preset `[build].install`/`[build].build` first, then app `[[build.stages]]` in declaration order.
Deploy validates that the resolved runtime `main` exists after build and before artifact packaging.
Bun release dependencies are installed on server before rollout (`bun install --production`).
Hosted server install resolves Linux host `arch` + `libc` and downloads matching `tako-server-linux-<arch>-<libc>` artifact.
Hosted server install also installs `mise` (package-manager first, with upstream installer fallback when distro packages are unavailable).
Each deploy also prunes local `.tako/artifacts/` cache (best-effort), keeping 30 newest source archives (`*-source.tar.zst`) and 90 newest target artifacts (`artifact-cache-*.tar.zst`), and removing orphan target metadata files.
When deploy targets private/local route hostnames (for example `*.local`), `tako-server` generates self-signed certs for those routes during deploy instead of ACME issuance.
If no cert matches an SNI hostname yet, `tako-server` serves a fallback self-signed default cert so HTTPS still completes and unmatched hosts/routes return `404`.
Remote edge proxy response caching stores proxied `GET`/`HEAD` responses only when response `Cache-Control` / `Expires` headers explicitly allow caching.

Name resolution for `.tako.test` is done via local split DNS:

- `tako dev` installs `/etc/resolver/tako.test` (one-time sudo) pointing to `127.0.0.1:53535`.
- `tako-dev-server` answers `*.tako.test` queries for registered app hosts and maps them to loopback.
  - On macOS, app hosts resolve to `127.77.0.1`.
  - On other platforms, app hosts resolve to `127.0.0.1`.

## Environment Variables

These are the environment variables Tako components read and/or set.

| Name              | Used by         | Meaning                                       | Values / default             | Notes                                                               |
| ----------------- | --------------- | --------------------------------------------- | ---------------------------- | ------------------------------------------------------------------- |
| `PORT`            | app             | Listen port for HTTP server                   | number                       | Set by `tako dev` for local runs.                                   |
| `ENV`             | app             | Local development environment hint            | `development`                | Set by `tako dev` for local app process conventions.                |
| `TAKO_ENV`        | app             | Deployed environment name                     | `production`, `staging`, ... | Set during deploy manifest generation for remote runtime.           |
| `NODE_ENV`        | app             | Node convention env                           | `development` / `production` | Set by runtime adapter.                                             |
| `BUN_ENV`         | app             | Bun convention env                            | `development` / `production` | Set by runtime adapter.                                             |
| `TAKO_BUILD`      | app             | Deployed build id                             | string                       | Sent by `tako deploy` in deploy payload; injected by `tako-server`. |
| `TAKO_APP_SOCKET` | app / `tako.sh` | Unix socket path the app should listen on     | path string                  | Set by `tako-server` on Unix deploys (includes `{pid}` token).      |
| `TAKO_VERSION`    | app / `tako.sh` | App version string (if you choose to set one) | string                       | Optional; separate from `TAKO_BUILD`.                               |
| `TAKO_INSTANCE`   | app / `tako.sh` | Instance ordinal                              | integer string               | Allocated by `tako-server`.                                         |

## macOS DNS resolver

`tako dev` configures this automatically when missing:

```text
/etc/resolver/tako.test
  nameserver 127.0.0.1
  port 53535
```

## DNS troubleshooting

To check whether a name resolves:

```bash
tako doctor
```

If resolution fails:

- Verify `/etc/resolver/tako.test` exists and points to `127.0.0.1:53535`.
- Ensure `tako dev` is running and your app is listed in `tako doctor`.
- On macOS, verify `tako doctor` shows the loopback proxy boot helper loaded, the `127.77.0.1` alias present, and `tcp 127.77.0.1:443` reachable.
- Confirm no local process is conflicting on UDP `127.0.0.1:53535`.
