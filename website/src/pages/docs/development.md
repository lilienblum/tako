---
layout: ../../layouts/DocsLayout.astro
title: "Tako Docs - Development"
heading: Development
current: development
---

# Local Development with Tako

`tako dev` gives you a production-like local environment: trusted HTTPS, custom `.tako.test` domains, hot reload, and a persistent background daemon that keeps your apps running even after the CLI exits.

## How it works

`tako dev` is a **client** that talks to a persistent background process called `tako-dev-server`. When you run `tako dev` from your project directory, here is what happens:

1. The CLI ensures `tako-dev-server` is running (starts it if needed).
2. Your app is **registered** with the daemon, using the project directory as a unique key.
3. The daemon spawns your app process on an ephemeral port and routes HTTPS traffic to it.
4. Logs stream to your terminal in real time.

```bash
# Start dev from your project directory
tako dev

# Or point at another directory
tako dev path/to/project
```

The app name is resolved from the top-level `name` in `tako.toml`, or from the sanitized project directory name if `name` is not set. This name determines your local URL: `https://{app}.tako.test/`.

## The dev daemon

`tako-dev-server` runs as a background process and handles multiple apps simultaneously. It persists app registrations in a SQLite database at `{TAKO_HOME}/dev-server.db`, so your routing state survives restarts.

The daemon:

- Terminates HTTPS using certificates issued by the local CA (SNI-based cert selection).
- Routes requests by `Host` header to the correct app port.
- Answers DNS queries for `*.tako.test` hostnames.
- Manages app processes (spawn, stop, wake-on-request).

Installed CLI distributions include three binaries: `tako`, `tako-dev-server`, and `tako-loopback-proxy`. When running from a source checkout, `tako dev` prefers repo-local debug/release builds of these helpers.

If the daemon binary is missing, `tako dev` reports a build hint (source checkout) or reinstall hint (installed CLI).

## App lifecycle

Each registered app has one of three statuses:

| Status    | Meaning                                                     |
| --------- | ----------------------------------------------------------- |
| `running` | Process is active, routes are live, traffic is being served |
| `idle`    | Process stopped, routes retained for wake-on-request        |
| `stopped` | Unregistered, routes removed, process killed                |

### Starting and attaching

When you run `tako dev`, the app starts immediately with one local instance. If the app is already running or idle from a previous session, `tako dev` attaches to the existing session instead of starting a new one.

### Going idle

After 10 minutes with no attached CLI clients, a running app transitions to `idle`. The daemon keeps its routes registered but stops the process. Idle shutdown is suppressed while there are in-flight requests.

### Wake-on-request

When an HTTP request arrives for an idle app, the daemon spawns the process and routes the request once the app is healthy. This means your app is always reachable at its `.tako.test` URL, even after going idle.

### Stopping

Press `Ctrl+c` to fully stop the app. This unregisters it from the daemon, removes its routes, and kills the process.

### Backgrounding

Press `b` to hand the running process off to the daemon and exit the CLI. The daemon keeps the process alive and routes active. Run `tako dev` again from the same directory to re-attach.

## Dev subcommands

### tako dev stop

Stop a running or idle dev app without needing to attach first.

```bash
# Stop the app for the current directory
tako dev stop

# Stop a specific app by name
tako dev stop my-app

# Stop all registered dev apps
tako dev stop --all
```

### tako dev ls

List all registered dev apps and their statuses.

```bash
tako dev ls
# Alias: tako dev list
```

## Trusted HTTPS and the local CA

On first run, Tako creates a local root Certificate Authority and installs it into your system trust store. On macOS, this may prompt for your password -- Tako explains what it needs before asking.

The architecture:

- The root CA is generated once. Its private key is stored in the system keychain (scoped per `{TAKO_HOME}` to avoid cross-home mismatches).
- The public CA certificate is available at `{TAKO_HOME}/ca/ca.crt`.
- Leaf certificates are generated on-the-fly for each app domain.
- Once the CA is trusted, there are no browser security warnings.

The dev daemon listens on `127.0.0.1:47831` in HTTPS mode. TLS certificate and key files are stored at `{TAKO_HOME}/certs/fullchain.pem` and `{TAKO_HOME}/certs/privkey.pem`.

If your browser still shows certificate warnings after setup, try quitting and restarting it, then verify the CA is installed in Keychain Access and marked as trusted.

## Local DNS

Tako uses split DNS so `*.tako.test` hostnames resolve locally without touching your `/etc/hosts` file.

### macOS resolver

`tako dev` writes a one-time resolver file (requires sudo):

```
/etc/resolver/tako.test
  nameserver 127.0.0.1
  port 53535
```

The dev daemon runs a DNS listener on `127.0.0.1:53535` and answers `A` queries for active `*.tako.test` hosts:

- On macOS, app hosts resolve to `127.77.0.1` (the dedicated loopback address used by the loopback proxy).
- On other platforms, app hosts resolve to `127.0.0.1`.

## macOS loopback proxy

On macOS, Tako installs a `launchd`-managed loopback proxy so your dev URLs use standard ports (no `:47831` in the URL). This is a one-time setup that requires sudo.

The proxy listens only on `127.77.0.1` and forwards:

- `127.77.0.1:443` to `127.0.0.1:47831` (HTTPS)
- `127.77.0.1:80` to `127.0.0.1:47830` (HTTP redirect)

Tako also installs a boot-time `launchd` helper that ensures the `127.77.0.1` loopback alias exists before the proxy is registered. The entire `127.0.0.0/8` block routes to `lo0` on macOS, so no additional network interface configuration is needed.

The loopback proxy is socket-activated: it may exit after a long idle window, and `launchd` reactivates it on the next incoming request.

After setup, your dev URLs look like:

```
https://my-app.tako.test/
```

On platforms without the loopback proxy, the URL includes the port:

```
https://my-app.tako.test:47831/
```

## Development environment variables

`tako dev` sets several environment variables for the app process.

### Automatically set

| Variable   | Value         | Purpose                            |
| ---------- | ------------- | ---------------------------------- |
| `PORT`     | _(ephemeral)_ | The port your app should listen on |
| `ENV`      | `development` | General environment hint           |
| `NODE_ENV` | `development` | Node.js convention                 |
| `BUN_ENV`  | `development` | Bun convention                     |

### From tako.toml

Variables from `[vars]` (base) and `[vars.development]` (environment-specific) are merged and injected into the app process. Later values override earlier ones.

### App log level

Each `[envs.*]` block can set `log_level` to control the app's log verbosity: `debug`, `info`, `warn`, or `error`. Development defaults to `debug`. The resolved level is passed to your app as `TAKO_APP_LOG_LEVEL`.

This is independent of `--verbose`, which controls only Tako CLI and dev-server verbosity.

```toml
[envs.development]
log_level = "debug"  # default for development
```

## Hot reload

Source-level hot reload is **runtime-driven**. Tako does not watch your source files for changes. Instead, your runtime handles it:

- Bun's built-in watch mode
- Vite's HMR
- Any framework dev server with its own file watching

Tako's role is to keep the HTTPS proxy and DNS routing stable while the runtime handles reloading.

### Vite apps

If your app uses Vite with `tako.sh/vite`:

```typescript
import { tako } from "tako.sh/vite";

export default defineConfig({
  plugins: [tako()],
});
```

The plugin:

- Adds `.tako.test` to Vite's `server.allowedHosts` so local Tako hosts are accepted.
- When `PORT` is set by `tako dev`, binds Vite to `127.0.0.1:$PORT` with `strictPort: true`.

## Interactive terminal UI

When running in an interactive terminal, `tako dev` provides a branded experience.

### Startup header

A branded header with the Tako logo, version, and app info is printed once at startup.

### Log format

Logs are formatted as:

```
hh:mm:ss LEVEL [scope] message
```

- **Timestamp** (`hh:mm:ss`) is rendered in a muted color.
- **Level** is colorized with pastel colors: `DEBUG` (electric blue), `INFO` (green), `WARN` (yellow), `ERROR` (red), `FATAL` (purple).
- **Scope** identifies the source: `tako` (the dev daemon) or `app` (your app process).

For app output, Tako infers the log level from leading tokens like `DEBUG`, `INFO`, `WARN`, `WARNING`, `ERROR`, `FATAL` (including bracketed forms like `[DEBUG]`). `TRACE` is mapped to `DEBUG`.

App lifecycle changes (starting, stopped, errors) appear inline as `-- {status} --` separator lines.

### Keyboard shortcuts

| Key      | Action                                             |
| -------- | -------------------------------------------------- |
| `r`      | Restart the app process                            |
| `b`      | Background the app (hand off to daemon, CLI exits) |
| `Ctrl+c` | Stop the app and quit                              |

### Scrollback and search

Tako does not use an alternate terminal screen. Your native terminal scrollback, search, copy/paste, and clickable links all work as expected.

## Non-terminal output

When stdout is piped or redirected (not a terminal), `tako dev` falls back to plain text output with no color or raw mode. This makes it easy to pipe logs through other tools:

```bash
tako dev | grep error
```

## tako.toml watching and auto-restart

`tako dev` watches your `tako.toml` file for changes while running:

- If dev environment variables change (from `[vars]`, `[vars.development]`, or `[envs.development]`), the app process is restarted automatically.
- If `[envs.development]` routes change, Tako re-registers routes with the daemon without restarting the app.

## Dev routes

By default, `tako dev` registers `{app}.tako.test` as the route for your app.

You can configure custom dev routes in `tako.toml`:

```toml
[envs.development]
route = "my-app.tako.test"
# or multiple routes:
# routes = ["my-app.tako.test", "api.my-app.tako.test"]
```

Dev routes have a few constraints:

- Routes must be `{app}.tako.test` or a subdomain of it.
- Wildcard host entries are ignored in dev routing (exact hostnames only).
- If configured routes contain no exact hostnames, `tako dev` fails with an error.

## App startup command

How Tako decides what command to run for your app:

- When top-level `preset` is **omitted** in `tako.toml`, Tako ignores the preset's `dev` command and runs a runtime-default command with resolved `main`:
  - Bun: `bun run node_modules/tako.sh/src/entrypoints/bun.ts {main}`
  - Node: `node --experimental-strip-types node_modules/tako.sh/src/entrypoints/node.ts {main}`
  - Deno: `deno run --allow-net --allow-env --allow-read node_modules/tako.sh/src/entrypoints/deno.ts {main}`

- When top-level `preset` is **explicitly set**, Tako uses the preset's top-level `dev` command.

## Diagnostics with tako doctor

`tako doctor` prints a local diagnostic report and exits. It checks everything you need for `tako dev` to work:

```bash
tako doctor
```

The report covers:

- Dev daemon status (listen info, socket connectivity).
- Local DNS status (resolver file, name resolution).
- On macOS:
  - Loopback proxy install status
  - Boot-helper load status
  - Dedicated loopback alias (`127.77.0.1`) status
  - `launchd` load status
  - TCP reachability on `127.77.0.1:443` and `127.77.0.1:80`

If the daemon is not running, doctor reports `status: not running` with a hint to start `tako dev`, and exits successfully.

### DNS troubleshooting

If name resolution fails:

- Verify `/etc/resolver/tako.test` exists and points to `127.0.0.1:53535`.
- Ensure `tako dev` is running and your app is listed in `tako dev ls`.
- On macOS, verify `tako doctor` shows the loopback proxy helper loaded, the `127.77.0.1` alias present, and TCP `127.77.0.1:443` reachable.
- Confirm no other process is using UDP `127.0.0.1:53535`.

## Files created by tako dev

Paths follow platform conventions (`~/Library/Application Support/tako/` on macOS, `~/.local/share/tako/` and `~/.config/tako/` on Linux). Source-checkout debug builds use `{repo}/local-dev/.tako/` instead.

| File                                      | Created by        | Purpose                                |
| ----------------------------------------- | ----------------- | -------------------------------------- |
| `{TAKO_HOME}/ca/ca.crt`                   | `tako dev`        | Local dev root CA certificate (public) |
| `{TAKO_HOME}/dev-server.sock`             | `tako-dev-server` | Unix socket for the control protocol   |
| `{TAKO_HOME}/dev-server.db`               | `tako-dev-server` | SQLite database for app registrations  |
| `{TAKO_HOME}/dev/logs/{app}-{hash}.jsonl` | `tako-dev-server` | Shared per-app log stream              |
| `{TAKO_HOME}/certs/fullchain.pem`         | `tako dev`        | Dev daemon TLS certificate             |
| `{TAKO_HOME}/certs/privkey.pem`           | `tako dev`        | Dev daemon TLS private key             |
| `/etc/resolver/tako.test`                 | `tako dev`        | macOS DNS resolver config              |

Log records use a single `timestamp` field (`hh:mm:ss`). When a new owning session starts, the shared log stream is truncated. Attached clients replay existing contents and then follow new lines.

## Quick start checklist

1. Run `tako doctor` to confirm local prerequisites (DNS, loopback proxy, CA).
2. Run `tako dev` from your project directory.
3. Open `https://{app}.tako.test/` in your browser.
4. Edit code while `tako dev` stays running -- your runtime handles reload.
5. Press `b` to background the app, or `Ctrl+c` to stop it.
