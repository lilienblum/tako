# Development (Local)

This guide covers local development with Tako: trusted HTTPS, `.tako.local` URLs, and what `tako dev` is doing behind the scenes.

## Related Docs

- [Quickstart](/docs/quickstart): first-run setup for local + remote environments.
- [Operations](/docs/operations): day-2 runbook for diagnostics and incident response.
- [Deployment](/docs/deployment): remote deployment workflow.
- [Architecture](/docs/architecture): system component/data-flow overview.
- [tako.toml Reference](/docs/tako-toml): full config reference for env/routes/servers.

## Overview

- `tako dev` is a **client** that talks to a background daemon: `tako-dev-server`.
- When running from source, the daemon binary is built from the `tako` package (`cargo build -p tako --bin tako-dev-server`).
- `tako dev` owns your app process lifecycle and spawns the app locally on an ephemeral port.
- `tako-dev-server` terminates HTTPS and routes by `Host` to that app port.
- The client registers a **lease** with the daemon (TTL + heartbeat). If heartbeats stop, the route expires.
- `tako dev` watches [`tako.toml`](/docs/tako-toml) for changes. If dev env vars change, it restarts the app. If `[envs.development]` routes change, it re-registers routes with the daemon.

## Files Created

Paths are under `~/.tako/` in normal installs.
When running from a source checkout in debug builds, Tako prefers `{repo}/debug/.tako/` instead.

Created/used by `tako dev` / `tako doctor`:

- `{TAKO_HOME}/ca/ca.crt`: local dev root CA certificate (public). The private key is stored in the OS keychain.

Created/used by `tako-dev-server`:

- `{TAKO_HOME}/dev-server.sock`: Unix socket for the control protocol.

## Running locally

From an app directory:

```bash
tako dev
```

If `[envs.development]` omits routes, `tako dev` defaults to `{app}.tako.local`.

Useful commands:

- `tako doctor`
- `tako dev` (starts the interactive dashboard by default)
- `tako dev --no-tui` (disable the dashboard)
- `tako dev <DIR>` (run as if invoked from another project directory)

Default URL:

- macOS (with local forwarding): `https://{app}.tako.local/`
- Other platforms: `https://{app}.tako.local:47831/`

## Local Workflow Checklist

Fastest happy-path loop:

1. Run `tako doctor` to confirm local prerequisites and DNS/forwarding state.
2. Run `tako dev` from your app directory.
3. Open the development URL and verify app responses.
4. Make code/config edits while `tako dev` stays running.
5. Use `tako dev --no-tui` when you want logs in a non-interactive terminal.

## Trusted HTTPS (Local CA)

On first run, Tako creates (or reuses) a local root CA and installs it into the system trust store. On macOS, this may ask for your password.

If your browser still complains about certs:

- Quit and restart the browser.
- Verify the CA is installed in Keychain Access and marked trusted.

## Ports and Privileges

- `tako-dev-server` listens on fixed local HTTPS port `47831`.
- On macOS, Tako uses scoped local forwarding so public dev URLs can use `:443` (no explicit port in the URL).
- Binding `:443` requires elevated privileges, so Tako requests one-time setup when needed.

## Running Slow E2E Tests

Some tests use Docker/SSH and are opt-in:

- `TAKO_E2E=1 cargo test -p tako --test deploy_e2e -- --nocapture`

Name resolution for `.tako.local` is done via local split DNS:

- `tako dev` installs `/etc/resolver/tako.local` (one-time sudo) pointing to `127.0.0.1:53535`.
- `tako-dev-server` answers `*.tako.local` queries for active lease hosts and maps them to loopback.

## Environment Variables

These are the environment variables Tako components read and/or set.

| Name              | Used by         | Meaning                                          | Values / default                  | Notes                                                  |
| ----------------- | --------------- | ------------------------------------------------ | --------------------------------- | ------------------------------------------------------ |
| `ENV`             | app             | Environment name                                 | `development` / `production`      | Set by Tako runtime adapter.                           |
| `NODE_ENV`        | app             | Node convention env                              | `development` / `production`      | Set by runtime adapter.                                |
| `BUN_ENV`         | app             | Bun convention env                               | `development` / `production`      | Set by runtime adapter.                                |
| `TAKO_BUILD`      | app             | Deployed build id                                | string                            | Written by `tako deploy` into release `.env`.          |
| `TAKO_SOCKET`     | app / `takokit` | Unix socket path for connecting to `tako-server` | default `/var/run/tako/tako.sock` | Used when `TAKO_APP_SOCKET` is set.                    |
| `TAKO_APP_SOCKET` | app / `takokit` | Unix socket path the app should listen on        | path string                       | Set by `tako-server` when using socket-based proxying. |
| `TAKO_VERSION`    | app / `takokit` | App version string (if you choose to set one)    | string                            | Optional; separate from `TAKO_BUILD`.                  |
| `TAKO_INSTANCE`   | app / `takokit` | Instance ordinal                                 | integer string                    | Allocated by `tako-server`.                            |

## macOS DNS resolver

`tako dev` configures this automatically when missing:

```text
/etc/resolver/tako.local
  nameserver 127.0.0.1
  port 53535
```

## DNS troubleshooting

To check whether a name resolves:

```bash
tako doctor
```

If resolution fails:

- Verify `/etc/resolver/tako.local` exists and points to `127.0.0.1:53535`.
- Ensure `tako dev` is running and your app is listed in `tako doctor`.
- Confirm no local process is conflicting on UDP `127.0.0.1:53535`.
