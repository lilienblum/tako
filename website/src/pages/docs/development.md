---
layout: ../../layouts/DocsLayout.astro
title: "Local development with Tako dev: HTTPS, domains, and hot reload - Tako Docs"
heading: Development
current: development
description: "Learn how tako dev provides trusted HTTPS, custom .test domains, hot reload, variants, and a persistent local background daemon."
---

# Local Development with Tako

`tako dev` is the local development loop: one command starts your app on a trusted HTTPS URL like `https://myapp.test/`, reloads on source changes, and keeps running in the background even after you close the terminal.

You get:

- Trusted HTTPS out of the box (no browser warnings).
- Friendly hostnames on the `.test` top-level domain, with no `/etc/hosts` editing.
- Hot reload driven by your framework (Vite, Bun watch, Next, etc.).
- A persistent background daemon so apps survive CLI restarts and come back on demand.
- Multiple apps side by side, each on its own `.test` hostname.

## How it works

`tako dev` is a **client**. The actual server lives in a separate long-running process called `tako-dev-server`. When you run `tako dev`, the CLI:

1. Ensures `tako-dev-server` is running (spawning it if needed).
2. Registers your app with the daemon. The selected config file path is the unique key.
3. Asks the daemon to spawn your app process on an ephemeral loopback port.
4. Streams logs and lifecycle events back to your terminal.

The resulting URL is `https://{app}.test/`. The app name comes from the top-level `name` in your `tako.toml`, or from the sanitized parent directory name if `name` is not set.

```bash
# Start dev from your project directory
tako dev

# Or point at another config file
tako -c path/to/project/preview.toml dev
```

## DNS variants

Use `--variant` (alias `--var`) to run the same app under a different hostname. The variant is appended to the app slug, so nothing else about the app changes:

```bash
tako dev --variant foo
# → https://myapp-foo.test/
```

Variants are useful when you want two copies of the same project running at once, for example a main branch and a feature branch, without renaming either one.

## The dev daemon (`tako-dev-server`)

`tako-dev-server` is a background process that handles every Tako dev app on your machine. It persists registrations in SQLite at `{TAKO_HOME}/dev-server.db` so routing state survives restarts.

The daemon is responsible for:

- Terminating HTTPS with certificates from your local CA (SNI-based cert selection).
- Routing requests by `Host` header to each app's loopback port.
- Answering DNS queries for active `*.test` and `*.tako.test` hostnames.
- Starting, stopping, and waking app processes.
- When LAN mode is on, exposing the same routes via `.local` aliases via mDNS.

### Which binary runs

- **Installed CLI:** the daemon binary shipped in the same archive as `tako` (on macOS that archive also includes `tako-dev-proxy`).
- **Source checkout:** `tako dev` prefers the repo-local `target/debug/tako-dev-server` or `target/release/tako-dev-server` if present, and falls back to `tako-dev-server` on `PATH`.

If the daemon binary is missing entirely, `tako dev` prints a hint that matches your install:

- From a source checkout: `cargo build -p tako --bin tako-dev-server`
- From an installed CLI: `curl -fsSL https://tako.sh/install.sh | sh`

### Startup failures

If the daemon does not come up, `tako dev` shows you the last lines of `{TAKO_HOME}/dev-server.log` so you can see what crashed. The CLI waits about 15 seconds for the daemon socket before giving up. The daemon itself does an upfront bind-availability check for its HTTPS listen address and exits immediately with an explicit error if the port is already taken.

## App lifecycle

Each registered app has one of three statuses:

| Status    | Meaning                                              |
| --------- | ---------------------------------------------------- |
| `running` | Process is active, routes are live                   |
| `idle`    | Process stopped, routes retained for wake-on-request |
| `stopped` | Unregistered, routes removed, process killed         |

### Starting and attaching

When you run `tako dev`, the app starts immediately with one local instance. If the app is already `running` or `idle` from a previous session for the same config file, `tako dev` attaches to that session instead of starting a new one. You will see the existing logs replay and then continue following.

### Going idle

After 30 minutes with no attached CLI clients, a running app transitions to `idle`. Routes stay registered but the process is stopped. Idle shutdown is suppressed while there are in-flight requests.

### Wake-on-request

Idle apps wake automatically. The next HTTP request triggers a spawn, and the daemon routes that request once the app is healthy. Your `.test` URL keeps working even if you have not had the CLI open for hours. In LAN mode, a hit on the `.local` alias wakes the app the same way.

### Stopping vs backgrounding

- `Ctrl+c` — stop the app. The daemon unregisters it, removes routes, and kills the process.
- `b` — background. Hand the running process off to the daemon and exit the CLI. Routes stay live. Run `tako dev` again in the same directory to reattach.

## Dev subcommands

### tako dev stop

Stop a registered dev app without attaching first.

```bash
# Stop the app for the selected config file
tako dev stop

# Stop a specific app by name
tako dev stop my-app

# Stop everything
tako dev stop --all
```

### tako dev ls

List every app registered with the daemon, with its status and routes. `tako dev list` is an alias.

## Trusted HTTPS and the local CA

Tako runs a private certificate authority inside your `{TAKO_HOME}` directory. On first run (or any time the CA is not yet trusted), `tako dev` installs the root CA into your system trust store. On macOS this triggers a password prompt; Tako explains what it is about to do before invoking `sudo`.

Leaf certificates are generated on the fly for each app hostname, so the CA is issued once and then new apps inherit trust automatically. The CA private key lives in your system keychain, scoped per `{TAKO_HOME}` so two home directories never end up with mismatched keys and certs.

Useful paths:

- `{TAKO_HOME}/ca/ca.crt` — the public CA certificate. Point Node/Bun at it with `NODE_EXTRA_CA_CERTS=$TAKO_HOME/ca/ca.crt` to silence TLS errors from internal HTTP clients.
- `{TAKO_HOME}/certs/fullchain.pem` and `{TAKO_HOME}/certs/privkey.pem` — the daemon's TLS files. `tako dev` ensures these exist before starting the daemon; existing files are reused.

The daemon listens on `127.0.0.1:47831` for HTTPS.

### Trusting the CA on iOS

LAN mode can help you reach your dev app from a phone, but iOS needs two separate steps to trust the CA. Installing the CA profile is step one:

1. Open the CA install URL on the phone (LAN mode shows a QR code for it).
2. Install the configuration profile from Settings.
3. Enable full trust: **Settings → General → About → Certificate Trust Settings**, then toggle the Tako root on.

Without the second step iOS will still reject the cert.

## LAN mode

Press `l` in the interactive UI to toggle LAN mode. Tako keeps your existing dev routes and additionally advertises the concrete hostnames via mDNS (Bonjour on macOS, Avahi on Linux). `myapp.test` becomes reachable as `myapp.local` from other devices on the same network. The interactive UI renders a QR code linking to the CA install URL so you can trust the cert from a phone.

Hostnames are rewritten only at the suffix, so subdomains and path-prefixed routes keep their shape: `app.test/api/*` answers on `app.local/api/*`.

### Wildcard caveat

mDNS can only advertise concrete hostnames. Wildcard routes like `*.app.test` are not broadcast, so plain mDNS clients (phones, tablets) cannot reach them. They still match at the proxy, so machines with their own DNS forwarder can use them. If you need a phone to reach a specific tenant, add an explicit subdomain route (`api.app.test`, `acme.app.test`, etc.) — Tako will flag this with a warning under the LAN route list.

## Local DNS

Tako does **not** modify `/etc/hosts`. Instead, it configures split DNS so that `.test` and `.tako.test` route to a local DNS listener, and everything else goes through your normal resolver.

- **macOS:** Tako writes resolver files at `/etc/resolver/test` and `/etc/resolver/tako.test` pointing at `127.0.0.1:53535`. If `/etc/resolver/test` already exists and was not created by Tako, it skips the file and warns about the conflict; `.tako.test` still works in that case.
- **Linux:** systemd-resolved is configured with `Domains=~tako.test ~test` via a drop-in file, pointing at the same local DNS listener.

The daemon runs a DNS listener on `127.0.0.1:53535` and answers `A` queries only for hosts that are actually registered. Inactive names return nothing, so stale routes never mask real DNS.

The loopback target varies by platform:

- **macOS:** `127.77.0.1` — a dedicated loopback alias owned by the dev proxy.
- **Linux:** `127.77.0.1` — the same alias, set up alongside iptables redirects.
- **Other platforms:** `127.0.0.1`.

## macOS dev proxy

To serve `:443` and `:80` without running the daemon as root every time, Tako installs a small launchd-managed proxy called `tako-dev-proxy`. It is socket-activated, so launchd owns the listening sockets and revives the proxy on demand after an idle window.

The proxy forwards:

- `127.77.0.1:443 → 127.0.0.1:47831` (HTTPS)
- `127.77.0.1:80 → 127.0.0.1:47830` (HTTP redirect to HTTPS)

A boot-time launchd helper ensures the `127.77.0.1` loopback alias exists before the proxy re-registers, so the setup survives reboots. Install and repair are automatic — `tako dev` prompts for `sudo` once, explains what it is about to do first, and then retries reachability on `127.77.0.1:443` and `:80`. If those probes fail, startup fails with a pointed hint that the proxy is not forwarding correctly.

## Linux port redirect

Linux takes a lighter-weight route than macOS: no proxy binary, just iptables DNAT rules on `127.77.0.1`:

- `443 → 47831`
- `80 → 47830`
- `53 → 53535`

`tako dev` asks for `sudo` once to install the rules and a systemd oneshot service that reapplies them on boot. On **NixOS**, Tako does not run imperative commands; instead it prints a `configuration.nix` snippet you can add to your config.

On platforms where neither the proxy nor iptables redirect applies, the URL simply includes the daemon port — `https://{app}.test:47831/`.

## Dev routes

By default, `tako dev` serves your app at `{app}.test`. You can override this with an environment block:

```toml
[envs.development]
routes = [
  "dashboard.test",
  "api.dashboard.test",
  "dashboard.test/api/*",
]
```

A few rules to know:

- When explicit `routes` are set, they **replace** the default entirely — `{app}.test` is not added, leaving that slug free for other apps.
- Dev routes must be on `.test` or `.tako.test` (or subdomains of either).
- Dev routing matches exact hostnames only. Wildcard entries (`*.app.test`) are ignored in dev.
- If your configured routes contain no exact hostnames, `tako dev` fails with an invalid route error.

Both `.test` and `.tako.test` resolve simultaneously. The proxy only routes hosts that are actually registered, so `.tako.test` is a safe fallback if something on your system owns `.test`.

## App startup command priority

Tako picks the dev command in this order:

1. Top-level `dev` in `tako.toml` (e.g. `dev = ["vite", "dev"]`).
2. The preset's `dev` command (e.g. `["vite", "dev"]` for `tanstack-start`).
3. The runtime default — JavaScript apps go through the SDK's dev entrypoint (`bun-dev.mjs`, `node-dev.mjs`, `deno-dev.mjs`); Go uses `go run .`.

Runtime-specific overrides in presets (for example a different command under Bun) are applied when they apply.

## Process monitoring

`tako dev` polls `try_wait()` every 500ms to notice when the app process exits. On exit, the route goes idle and the daemon waits for the next request to restart the app. Before marking the app `running` on startup, Tako waits for TCP readiness so the first request does not race the bind.

## Development environment variables

Tako sets these automatically in dev:

| Variable               | Value                                                |
| ---------------------- | ---------------------------------------------------- |
| `ENV`                  | `development`                                        |
| `PORT`                 | `0` (SDK binds to an OS-assigned port)               |
| `HOST`                 | `127.0.0.1`                                          |
| `TAKO_APP_NAME`        | Resolved app name                                    |
| `TAKO_INTERNAL_SOCKET` | Path to the SDK's internal socket                    |
| `TAKO_DATA_DIR`        | `.tako/data/app` (per-app persistent data directory) |
| `NODE_ENV`             | `development` (for JS runtimes)                      |
| `BUN_ENV`              | `development` (under Bun)                            |
| `DENO_ENV`             | `development` (under Deno)                           |

Your own vars come from `[vars]` and `[vars.development]` in `tako.toml` and merge on top of the auto-set table.

`ENV` is reserved — if you set it in `[vars]` or `[vars.development]`, Tako ignores it and prints a warning. Your app's log level (`LOG_LEVEL` or whatever your framework reads) is **not** controlled by Tako; set it yourself in `[vars.development]`. The CLI's `--verbose` flag controls Tako CLI and dev-server verbosity only, never your app.

## Hot reload

Source hot reload is driven by the runtime, not by Tako. Vite, Bun's watch mode, and framework dev servers watch files themselves and restart or HMR on change. `tako dev` does not watch your source tree — it only monitors the app process itself, your `tako.toml`, and the daemon's routing table.

## Vite apps

Projects using Vite should add the `tako.sh/vite` plugin:

```typescript
import { defineConfig } from "vite";
import { tako } from "tako.sh/vite";

export default defineConfig({
  plugins: [tako()],
});
```

During `vite dev` the plugin:

- Adds `.test` and `.tako.test` to `server.allowedHosts` so Vite accepts requests from Tako's hostnames.
- When `PORT` is set (always true under Tako), binds Vite to `127.0.0.1:$PORT` with `strictPort: true` so the SDK and Vite agree on the port.

During `vite build` the plugin emits `dist/server/tako-entry.mjs`, the wrapper used as the deploy entrypoint.

## The interactive terminal UI

When stdout is a real TTY, `tako dev` prints a branded header at startup (logo, version, app info) and then streams logs and status to stdout without entering an alternate screen. Native scrollback, search, copy/paste, and clickable links all keep working.

Log lines are formatted `hh:mm:ss LEVEL [scope] message`:

- Timestamps are muted.
- Levels (`DEBUG`, `INFO`, `WARN`, `ERROR`, `FATAL`) are colored pastel blue / green / yellow / red / purple.
- The `[scope]` is usually `tako` (daemon) or `app` (your process). Tako infers the level from leading tokens in app output so `INFO something` from your code shows up as INFO-level.
- Lifecycle changes print as separator lines: `── starting ──`, `── stopped ──`, etc.

Keyboard shortcuts (interactive mode only):

- `r` — restart the app process
- `l` — toggle LAN mode
- `b` — background and exit the CLI
- `Ctrl+c` — stop the app and quit

## Non-terminal output

When stdout is piped or redirected, the interactive UI is skipped. Output becomes plain `println` style — no colors, no raw mode, no header — so it plays well with grep, tee, and file redirection.

## `tako.toml` watching and auto-restart

`tako dev` always watches your `tako.toml`:

- If effective dev environment variables change, the app restarts.
- If `[envs.development].route(s)` changes, routes are re-registered live. No restart needed.

This means tweaking a route does not blow away the app's in-memory state.

## Diagnostics with `tako doctor`

When something looks wrong, run `tako doctor`:

- Dev daemon listen info.
- On macOS, a preflight section covering the dev proxy install status, the boot-helper load status, the `127.77.0.1` loopback alias, launchd load status, and TCP reachability on `:443` and `:80`.
- Local DNS status.
- If the daemon is not running, `tako doctor` reports `status: not running`, hints to start `tako dev`, and exits successfully.

## Files created by `tako dev`

| Path                                            | Purpose                                                      |
| ----------------------------------------------- | ------------------------------------------------------------ |
| `{TAKO_HOME}/ca/ca.crt`                         | Public CA certificate (use with `NODE_EXTRA_CA_CERTS`)       |
| `{TAKO_HOME}/certs/fullchain.pem`               | Daemon TLS chain                                             |
| `{TAKO_HOME}/certs/privkey.pem`                 | Daemon TLS private key                                       |
| `{TAKO_HOME}/dev-server.sock`                   | Daemon control socket                                        |
| `{TAKO_HOME}/dev-server.db`                     | SQLite store of registrations, routes, lifecycle state       |
| `{TAKO_HOME}/dev-server.log`                    | Daemon log (surfaced on startup failures)                    |
| `{TAKO_HOME}/dev/logs/{app}-{hash}.jsonl`       | Per-app/per-config log stream (shared with attached clients) |
| `/etc/resolver/test`, `/etc/resolver/tako.test` | macOS split-DNS resolver files                               |
| `tako-dev-redirect.service` (systemd)           | Linux unit that reapplies iptables redirect rules at boot    |

When running a debug build from a source checkout, all of the above live under `{repo}/local-dev/.tako/` instead of your global `{TAKO_HOME}`, so nothing from development pollutes an installed setup.

## Quick-start checklist

1. Install Tako: `curl -fsSL https://tako.sh/install.sh | sh`.
2. In your project directory, run `tako init` and accept the defaults.
3. Start dev: `tako dev`.
4. Approve the one-time `sudo` prompts — CA trust on first run, plus the macOS dev proxy or Linux iptables redirect depending on your platform.
5. Open `https://{app}.test/`.
6. Press `l` to toggle LAN mode if you want to reach the app from another device; scan the QR code from your phone to install the CA.
7. Press `b` to background the app, or `Ctrl+c` to stop it. Run `tako dev ls` any time to see what is registered.
