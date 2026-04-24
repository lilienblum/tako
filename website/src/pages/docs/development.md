---
layout: ../../layouts/DocsLayout.astro
title: "Local development with Tako dev: HTTPS, domains, and hot reload - Tako Docs"
heading: Development
current: development
description: "Learn how tako dev provides trusted HTTPS, custom .test domains, hot reload, variants, and a persistent local background daemon."
---

# Local Development with Tako

`tako dev` gives you a production-like local environment: trusted HTTPS, custom `.test` domains, hot reload, and a persistent background daemon that keeps your apps running even after the CLI exits.

## How it works

`tako dev` is a **client** that talks to a persistent background process called `tako-dev-server`. When you run `tako dev`, here is what happens:

1. The CLI ensures `tako-dev-server` is running (starts it if needed).
2. Your app is **registered** with the daemon, using the selected config file as a unique key.
3. The daemon spawns your app process on an ephemeral port and routes HTTPS traffic to it.
4. Logs stream to your terminal in real time.

```bash
# Start dev from your project directory
tako dev

# Or point at another config file
tako -c path/to/project/preview dev
```

The app name is resolved from the top-level `name` in the selected config file, or from the sanitized parent directory name if `name` is not set. This name determines your local URL: `https://{app}.test/`.

### DNS variants

Use `--variant` (alias `--var`) to run a DNS variant of the app. This gives you a separate hostname without changing the app name:

```bash
tako dev --variant foo
# → https://myapp-foo.test/
```

This is useful for running multiple branches or configurations of the same app side by side.

## The dev daemon

`tako-dev-server` runs as a background process and handles multiple apps simultaneously. It persists app registrations in a SQLite database at `{TAKO_HOME}/dev-server.db`, so your routing state survives restarts.

The daemon:

- Terminates HTTPS using certificates issued by the local CA (SNI-based cert selection).
- Routes requests by `Host` header to the correct app port.
- Answers DNS queries for `*.test` and `*.tako.test` hostnames.
- Manages app processes (spawn, stop, wake-on-request).
- When LAN mode is enabled, exposes the same routes via `.local` aliases on the local network.

Installed CLI distributions include `tako` and `tako-dev-server` (on macOS, also `tako-dev-proxy`). When running from a source checkout, `tako dev` prefers repo-local debug/release builds of these helpers.

If the daemon binary is missing, `tako dev` reports a build hint (source checkout) or reinstall hint (installed CLI).

If daemon startup fails, `tako dev` reports the last lines from `{TAKO_HOME}/dev-server.log`. The CLI waits up to ~15 seconds for the daemon socket after spawn before reporting startup failure.

The daemon performs an upfront bind-availability check for its HTTPS listen address and exits immediately with an explicit error when that address is unavailable.

## App lifecycle

Each registered app has one of three statuses:

| Status    | Meaning                                                     |
| --------- | ----------------------------------------------------------- |
| `running` | Process is active, routes are live, traffic is being served |
| `idle`    | Process stopped, routes retained for wake-on-request        |
| `stopped` | Unregistered, routes removed, process killed                |

### Starting and attaching

When you run `tako dev`, the app starts immediately with one local instance. If the app is already running or idle from a previous session for the same selected config file, `tako dev` attaches to the existing session instead of starting a new one.

### Going idle

After 30 minutes with no attached CLI clients, a running app transitions to `idle`. The daemon keeps its routes registered but stops the process. Idle shutdown is suppressed while there are in-flight requests.

### Wake-on-request

When an HTTP request arrives for an idle app, the daemon spawns the process and routes the request once the app is healthy. This means your app is always reachable at its `.test` URL, even after going idle. If LAN mode is enabled, the equivalent `.local` alias wakes the app too.

### Stopping

Press `Ctrl+c` to fully stop the app. This unregisters it from the daemon, removes its routes, and kills the process.

### Backgrounding

Press `b` to hand the running process off to the daemon and exit the CLI. The daemon keeps the process alive and routes active. Run `tako dev` again with the same selected config file to reconnect.

## Dev subcommands

### tako dev stop

Stop a running or idle dev app without needing to connect first.

```bash
# Stop the app for the selected config file
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

### Trusting the CA on iOS devices

Scanning the LAN mode QR code installs the Tako CA as a **configuration profile**. That is only step one — iOS does not trust newly installed root CAs by default. Open **Settings → General → About → Certificate Trust Settings** and enable full trust for `Tako Development CA`. Without that toggle, Safari on your phone will still show "This Connection Is Not Private" because the certificate chain ends at an untrusted root.

## LAN mode

Toggling LAN mode (`l` in the interactive UI) exposes your registered dev routes on the local Wi-Fi network under `.local` aliases, advertised via mDNS (Bonjour on macOS, Avahi on Linux) so phones and tablets can resolve them by name. The CLI prints a QR code that opens an HTTP endpoint on the LAN IP serving the Tako root CA so devices can install it in one step.

### Wildcard routes and mDNS

Wildcard routes like `*.app.test` cannot be advertised to devices via mDNS — the protocol only supports concrete records, and each subdomain would need its own. Tako warns about this below the LAN route list and lists the affected wildcard routes. If you need to hit a specific subdomain from your phone (for example `api.app.test`), add it as an explicit route in `[envs.development]` alongside the wildcard:

```toml
[envs.development]
routes = [
  "app.test",
  "*.app.test",       # still works on your laptop
  "api.app.test",     # advertised via mDNS so your phone can reach it
]
```

Concrete routes win over wildcards at the proxy, so request matching is unchanged.

## Local DNS

Tako uses split DNS so `*.test` and `*.tako.test` hostnames resolve locally without touching your `/etc/hosts` file.

### macOS resolver

`tako dev` writes one-time resolver files (requires sudo):

```
/etc/resolver/test
  nameserver 127.0.0.1
  port 53535

/etc/resolver/tako.test
  nameserver 127.0.0.1
  port 53535
```

### Linux resolver

On Linux, `tako dev` configures `systemd-resolved` to forward both `test` and `tako.test` queries (`Domains=~tako.test ~test`) to the local DNS listener. This is part of the one-time setup described in the [Linux port redirect](#linux-port-redirect) section below.

The dev daemon runs a DNS listener on `127.0.0.1:53535` and answers `A` queries for active `*.test` and `*.tako.test` hosts:

- On macOS, app hosts resolve to `127.77.0.1` (the dedicated loopback address used by the dev proxy).
- On Linux, app hosts resolve to `127.77.0.1` (the dedicated loopback address used by iptables redirect).
- On other platforms, app hosts resolve to `127.0.0.1`.

## macOS dev proxy

On macOS, Tako installs a `launchd`-managed dev proxy so your dev URLs use standard ports (no `:47831` in the URL). This is a one-time setup that requires sudo.

The proxy listens only on `127.77.0.1` and forwards:

- `127.77.0.1:443` to `127.0.0.1:47831` (HTTPS)
- `127.77.0.1:80` to `127.0.0.1:47830` (HTTP redirect)

Tako also installs a boot-time `launchd` helper that ensures the `127.77.0.1` loopback alias exists before the proxy is registered. The entire `127.0.0.0/8` block routes to `lo0` on macOS, so no additional network interface configuration is needed.

The dev proxy is socket-activated: it may exit after a long idle window, and `launchd` reactivates it on the next incoming request.

If the dev proxy later appears inactive, `tako dev` explains that it is reloading or reinstalling the launchd helper before prompting for sudo.

After applying or repairing the dev proxy, Tako retries loopback 80/443 reachability and fails startup if those endpoints remain unreachable.

After setup, your dev URLs look like:

```
https://my-app.test/
```

## Linux port redirect

On Linux, Tako uses kernel-level iptables redirect rules instead of a dev proxy. No extra binary is needed -- the dev server binds its unprivileged ports and the kernel transparently redirects traffic from standard ports on `127.77.0.1`.

On first run, `tako dev` performs a one-time setup (requires sudo) that configures:

- A loopback alias (`127.77.0.1` on `lo`)
- iptables DNAT rules: `127.77.0.1:443` to port `47831`, `127.77.0.1:80` to port `47830`, and `127.77.0.1:53` to port `53535`
- A `systemd-resolved` drop-in to forward `test` and `tako.test` DNS queries to the local listener
- A systemd oneshot service (`tako-dev-redirect.service`) so the alias and iptables rules persist across reboots

### NixOS

On NixOS, imperative network changes are wiped by `nixos-rebuild`. Instead of running the setup itself, Tako prints a `configuration.nix` snippet that you can add to your system configuration. After adding the snippet, run `nixos-rebuild switch` and restart `tako dev`.

On platforms without the dev proxy or port redirect, the URL includes the port:

```
https://my-app.test:47831/
```

## Dev routes

By default, `tako dev` registers `{app}.test` as the route for your app.

You can configure custom dev routes in `tako.toml`:

```toml
[envs.development]
route = "my-app.test"
# or multiple routes:
# routes = ["my-app.test", "api.my-app.test"]
```

Dev routes have a few constraints:

- Routes must use `.test` or `.tako.test` -- for example `{app}.test` or a subdomain of `{app}.test` (or the equivalent `.tako.test` forms).
- Wildcard host entries are ignored in dev routing (exact hostnames only).
- If configured routes contain no exact hostnames, `tako dev` fails with an error.

## App startup command

`tako dev` resolves the dev command with this priority:

1. **`dev` in `tako.toml`** -- user override (e.g. `dev = ["custom", "cmd"]`)
2. **Preset dev command** -- framework presets can replace the runtime default, for example `tanstack-start` and `vite` use `vite dev`, while `nextjs` uses `next dev`
3. **Runtime default** -- JS runtimes run your app through the Tako SDK entrypoint, same as production. Go uses `go run .`.

For JS apps, this means your `export default function fetch()` or `export default { fetch }` is automatically wrapped into an HTTP server by the SDK -- no `dev` script in `package.json` needed.

### Process monitoring

`tako dev` monitors the app process by polling `try_wait()` every 500ms to detect exits:

- If the app exits unexpectedly, the status changes to **exited** and the exit code is logged.
- The route goes idle (proxy stops forwarding).
- On the next HTTP request, the app is automatically restarted.
- Before marking the app as "running", `tako dev` waits for the app to accept TCP connections on its port -- preventing 502 errors during startup.

## Development environment variables

`tako dev` sets several environment variables for the app process and workflow worker process.

### Automatically set

| Variable        | Value            | Purpose                                |
| --------------- | ---------------- | -------------------------------------- |
| `ENV`           | `development`    | Generic development environment marker |
| `PORT`          | _(ephemeral)_    | The port your app should listen on     |
| `TAKO_DATA_DIR` | `.tako/data/app` | Persistent app-owned local data dir    |
| `NODE_ENV`      | `development`    | Node.js convention (all JS runtimes)   |
| `BUN_ENV`       | `development`    | Bun convention (Bun runtime only)      |
| `DENO_ENV`      | `development`    | Deno convention (Deno runtime only)    |

### From tako.toml

Variables from `[vars]` (base) and `[vars.development]` (environment-specific) are merged and injected into the app process and workflow worker process. Later values override earlier ones.

`ENV` is reserved. If you set `ENV` in `[vars]` or `[vars.development]`, Tako ignores it and prints a warning.

`tako dev` always uses loopback TCP via `PORT`.

### App log level

Tako does not set `LOG_LEVEL` or any other logging env var. Set one yourself in `[vars.development]` if your logger reads it — most do (pino, winston, tracing-subscriber, zap):

```toml
[vars.development]
LOG_LEVEL = "debug"
```

This is independent of `--verbose`, which controls only Tako CLI and dev-server verbosity.

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

- Adds `.test` and `.tako.test` to Vite's `server.allowedHosts` so local Tako hosts are accepted.
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

| Key      | Action                                                |
| -------- | ----------------------------------------------------- |
| `l`      | Toggle LAN mode (`.local` aliases for current routes) |
| `r`      | Restart the app process                               |
| `b`      | Background the app (hand off to daemon, CLI exits)    |
| `Ctrl+c` | Stop the app and quit                                 |

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

## Diagnostics with tako doctor

`tako doctor` prints a local diagnostic report and exits. It checks everything you need for `tako dev` to work:

```bash
tako doctor
```

The report covers:

- Dev daemon status (listen info, socket connectivity).
- Local DNS status (resolver file, name resolution).
- On macOS:
  - Dev proxy install status
  - Boot-helper load status
  - Dedicated loopback alias (`127.77.0.1`) status
  - `launchd` load status
  - TCP reachability on `127.77.0.1:443` and `127.77.0.1:80`
- On Linux:
  - Port redirect status (loopback alias and iptables rules)
  - TCP reachability on `127.77.0.1:443` and `127.77.0.1:80`

If the daemon is not running, doctor reports `status: not running` with a hint to start `tako dev`, and exits successfully.

### DNS troubleshooting

If name resolution fails:

- On macOS, verify `/etc/resolver/test` and `/etc/resolver/tako.test` exist and point to `127.0.0.1:53535`.
- On Linux, verify `systemd-resolved` is running and the `test` and `tako.test` DNS forward zones are configured.
- Ensure `tako dev` is running and your app is listed in `tako dev ls`.
- On macOS, verify `tako doctor` shows the dev proxy helper loaded, the `127.77.0.1` alias present, and TCP `127.77.0.1:443` reachable.
- On Linux, verify `tako doctor` shows the port redirect rules active and TCP `127.77.0.1:443` reachable.
- Confirm no other process is using UDP `127.0.0.1:53535`.

## Files created by tako dev

Paths follow platform conventions (`~/Library/Application Support/tako/` on macOS, `~/.local/share/tako/` and `~/.config/tako/` on Linux). Source-checkout debug builds use `{repo}/local-dev/.tako/` instead.

| File                                            | Created by        | Purpose                                       |
| ----------------------------------------------- | ----------------- | --------------------------------------------- |
| `{TAKO_HOME}/ca/ca.crt`                         | `tako dev`        | Local dev root CA certificate (public)        |
| `{TAKO_HOME}/dev-server.sock`                   | `tako-dev-server` | Unix socket for the control protocol          |
| `{TAKO_HOME}/dev-server.db`                     | `tako-dev-server` | SQLite database for app registrations         |
| `{TAKO_HOME}/dev/logs/{app}-{hash}.jsonl`       | `tako-dev-server` | Shared per-app log stream                     |
| `{TAKO_HOME}/certs/fullchain.pem`               | `tako dev`        | Dev daemon TLS certificate                    |
| `{TAKO_HOME}/certs/privkey.pem`                 | `tako dev`        | Dev daemon TLS private key                    |
| `/etc/resolver/test`                            | `tako dev`        | macOS DNS resolver config (primary)           |
| `/etc/resolver/tako.test`                       | `tako dev`        | macOS DNS resolver config (fallback)          |
| `/etc/systemd/system/tako-dev-redirect.service` | `tako dev`        | Linux loopback alias and iptables persistence |

Log records use a single `timestamp` field (`hh:mm:ss`). When a new owning session starts, the shared log stream is truncated. Attached clients replay existing contents and then follow new lines.

## Quick start checklist

1. Run `tako doctor` to confirm local prerequisites (DNS, dev proxy, CA).
2. Run `tako dev` from your project directory, or use `tako -c <FILE> dev` for a non-default config file.
3. Open `https://{app}.test/` in your browser.
4. Edit code while `tako dev` stays running -- your runtime handles reload.
5. Press `b` to background the app, or `Ctrl+c` to stop it.
