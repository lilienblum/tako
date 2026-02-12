# Tako Specification

This is the finalized specification for Tako. It describes the system as designed and implemented. Keep this in sync with code changes - when you modify code, update the corresponding sections here.

## Project Overview

Tako is a deployment and development platform consisting of:

- **`tako` CLI** - Local tool for development, deployment, server/secret management
- **`tako-server`** - Remote server binary that manages app processes, routing, and rolling updates
- **`tako.sh` SDK** - Current SDK implementation for JavaScript/TypeScript apps

Built in Rust (2024 edition). The current SDK implementation is `tako.sh` for JavaScript/TypeScript. Uses Pingora (Cloudflare's proxy) for production-grade performance.

## Design Goals

**Performance:** On par with Nginx, faster than Caddy. Built in Rust leveraging Pingora.

**Simplicity:** Opinionated defaults, minimal configuration, convention over configuration.

**Reliability:** Strong test coverage, graceful edge case handling, users can delete files/folders safely with recovery paths.

**Extensibility:** Support multiple runtimes (Bun first, then Node/Deno/Python/Go). Runtime-agnostic architecture.

## Configuration

### App Name Requirements

App names must be URL-friendly (DNS hostname compatible):

- **Allowed:** lowercase letters (a-z), numbers (0-9), hyphens (-)
- **Must start with:** lowercase letter
- **Examples:** `my-app`, `api-server`, `web-frontend`

This ensures names work in DNS (`{app-name}.tako.local` by default), URLs, and environment variables.

### tako.toml (Project Root - Required)

Application configuration for build, variables, routes, and deployment.

```toml
[tako]
name = "my-app"           # Optional - auto-detected from runtime/directory
build = "bun build"       # Optional - uses runtime default if omitted

[vars]
LOG_LEVEL = "info"        # Base variables (all environments)

[vars.production]
API_URL = "https://api.example.com"

[vars.staging]
API_URL = "https://staging-api.example.com"

[servers]
instances = 0             # Default: on-demand/serverless
port = 80                 # Default: 80
idle_timeout = 300        # Default: 5 minutes

[envs.production]
route = "api.example.com"  # Single route, or use 'routes' for multiple

[envs.staging]
routes = [
  "staging.example.com",
  "www.staging.example.com",
  "example.com/api/*"
]

[servers.la]
env = "production"
instances = 3             # Override default

[servers.nyc]
env = "production"
```

**Variable merging order (later overrides earlier):**

1. `[vars]` - base
2. `[vars.{environment}]` - environment-specific
3. Auto-set by Tako: `ENV={environment}`, `TAKO_BUILD={version}`

**Instance behavior:**

- `instances = 0`: On-demand. First request triggers a cold start; proxy returns `503 App is starting` until an instance becomes healthy. Instances are stopped after idle timeout.
- `instances = N` (N > 0): Always-on. Minimum N instances maintained, scales up on load, scales down after idle timeout.
- `idle_timeout`: Applies per-instance (default 300s / 5 minutes)
- Instances are not stopped while serving in-flight requests.

### ~/.tako/config.toml (Global User Config)

Global user-level settings and server inventory (user's home directory, NOT in project).

```toml
[[servers]]
name = "la"
host = "1.2.3.4"
port = 22                 # Optional, defaults to 22

[[servers]]
name = "nyc"
host = "5.6.7.8"
```

`[[servers]]` entries are managed by `tako servers add/rm/ls`. All names and hosts must be globally unique.

**SSH authentication:**

- `tako` authenticates using local SSH keys from `~/.ssh` (common filenames like `id_ed25519`, `id_rsa`, etc.).
- If a key file is passphrase-protected, `tako` will prompt for the passphrase when running interactively (or you can provide `TAKO_SSH_KEY_PASSPHRASE`).
- If no suitable key files are found or usable, `tako` falls back to `ssh-agent` via `SSH_AUTH_SOCK` (when available).

- `tako dev` uses a fixed local HTTPS listen port (`47831`).
- On macOS, `tako dev` uses automatic local forwarding so public URLs stay on default ports (`:443` for HTTPS, `:80` for HTTP redirect).

CLI prompt history is stored separately at `~/.tako/history.toml` (not in `config.toml`).

### .tako/secrets (Project - Encrypted)

Per-environment encrypted secrets (JSON format, AES-256-GCM encryption):

```json
{
  "production": {
    "DATABASE_URL": "encrypted_value",
    "API_KEY": "encrypted_value"
  },
  "staging": {
    "DATABASE_URL": "encrypted_value_different"
  }
}
```

Secret names are plaintext; values encrypted.

Encryption keys are file-based:

- Environment-specific keys: `~/.tako/keys/{env}`

`tako` can import/export these keys via `tako secrets key import` and `tako secrets key export`.

## Tako CLI Commands

### Installation and upgrades

Install the CLI on your local machine:

```bash
curl -fsSL https://tako.sh/install | sh
```

Install from crates.io:

```bash
cargo install tako
```

`cargo install tako` installs both `tako` and `tako-dev-server` binaries from the same package/version.

Upgrade the local CLI to the latest version:

```bash
tako upgrade
```

`tako upgrade` uses the hosted installer script at `https://tako.sh/install`.

### Global options

- `--version`: Print version and exit.
- `-v, --verbose`: Show verbose output (enables detailed command output and info logs).

Directory selection is command-scoped:

- `tako init [DIR]`
- `tako status [DIR]`
- `tako dev [DIR]`
- `tako deploy [DIR]`

### tako init [--force] [DIR]

Create `tako.toml` template with helpful comments.

```bash
tako init
```

Template behavior:

- Leaves only minimal starter options uncommented:
  - `[envs.production].route`
- Includes commented examples/explanations for all supported `tako.toml` options:
  - `[tako].name` and `[tako].build`
  - `[vars]`
  - `[vars.<env>]`
  - `[envs.<env>].routes` and inline env vars
  - `[servers]`
  - `[servers.<name>]` overrides
- Includes a docs link to `https://tako.sh/docs/tako-toml`.

If `tako.toml` already exists:

- Interactive terminal: `tako init` asks for overwrite confirmation.
- Non-interactive terminal: it fails unless `--force` is provided.

### tako help

Show all commands with brief descriptions.

### tako upgrade

Upgrade the local `tako` CLI binary to the latest available release.

- Downloads and runs the same hosted installer script used by `curl -fsSL https://tako.sh/install | sh`
- Requires either `curl` or `wget` on the local machine

### tako dev [--tui|--no-tui] [DIR]

Start (or attach to) a local development session for the current app, backed by a persistent dev daemon.

- `tako dev` is a **client**: it ensures `tako-dev-server` is running, then registers the current app directory with the daemon.
  - When running from a source checkout, `tako dev` prefers the repo-local `target/debug|release/tako-dev-server` binary.
  - If no local daemon binary exists, `tako dev` falls back to `tako-dev-server` on `PATH` and reports an explicit build hint (`cargo build -p tako --bin tako-dev-server`) when missing.
  - If daemon startup fails, `tako dev` reports the last lines from `{TAKO_HOME}/dev-server.log`.
  - `tako dev` waits up to ~15 seconds for the daemon socket after spawn before reporting startup failure.
  - The daemon performs an upfront bind-availability check for its HTTPS listen address and exits immediately with an explicit error when that address is unavailable.
- `tako dev` registers a **lease** with the daemon (TTL + heartbeat).
- The app starts immediately when `tako dev` starts (1 local instance) and is stopped after 30 minutes of no requests.
  - After an idle stop, the next request starts the app again.
  - Idle shutdown is suppressed while there are in-flight requests.
  - When the owning session exits normally while the app is running (for example, terminal disconnect or `Ctrl+c`), Tako keeps the app process and routes alive for a disconnect grace period, then stops that process and removes the lease/routes.
    - Release builds use a 10-minute grace period.
    - Debug builds use a 10-second grace period for faster local iteration.
  - If the app is already stopped (idle) when the owning session exits, Tako removes the lease/routes immediately.
  - `tako dev` uses a per-project lock file under `{TAKO_HOME}/dev/locks/` to keep a single owning session per app/directory.
  - Running `tako dev` again from the same directory attaches as an additional client instead of starting a second local app process.
  - Dev logs are written to a shared per-app/per-project stream at `{TAKO_HOME}/dev/logs/{app}-{hash}.jsonl`.
  - Each persisted log record stores a single `timestamp` token (`hh:mm:ss`) instead of split hour/minute/second fields; attached sessions continue to accept legacy `h`/`m`/`s` records from older streams.
  - When a new owning session starts, Tako truncates that shared stream before writing fresh logs for the new session.
  - Attached clients replay the existing file contents, then follow new lines from the same stream.
  - App lifecycle state (`starting`, `running`, `stopped`, app PID, and startup errors) is persisted to the same shared stream, so attached sessions reconstruct the same status/CPU/RAM view as the owning session.
- The daemon supports **multiple concurrent apps** and maintains hostname-based routing for `*.tako.local`.
- Utility flags:
  - `tako doctor`: print a diagnostic report and exit.
    - Reports dev daemon listen info, local 80/443 forwarding status, and local DNS status.
    - On macOS, includes a preflight section with clear checks for:
      - pf redirect rule for `{loopback-address}:443 -> 127.0.0.1:<dev-port>`
      - pf redirect rule for `{loopback-address}:80 -> 127.0.0.1:<http-redirect-port>`
      - TCP reachability on `{loopback-address}:443` and `{loopback-address}:80`
    - If the local dev daemon is not running (missing/stale socket), doctor reports `status: not running` with a hint to start `tako dev`, and exits successfully.
- The TUI dashboard is enabled by default when running in an interactive terminal.
  - Use `--no-tui` to disable it.
  - Default color theme uses Tako's brand palette (primary `#E88783`, secondary `#9BC4B6`), with slate-muted adapter text and compact `CPU`/`RAM`/`Sessions` labels.
  - Log levels are fixed to `DEBUG`, `INFO`, `WARN`, `ERROR`, and `FATAL`; only the level token is colorized in the logs panel using pastel colors (electric blue, green, yellow, red, and purple respectively).
  - The timestamp token (`hh:mm:ss`) is rendered in the muted text color.
  - The top header is split into two separate panels: a flexible left panel (app + status, adapter, URLs) and a compact fixed right panel for `CPU`/`RAM` plus control-client count.
    - `CPU` is shown as a percentage value.
    - `RAM` is shown in `MB` below 1 GB, and in `GB` with one decimal at or above 1 GB (for example, `1.2 GB`).
    - `Sessions` shows the number of currently connected dev control clients (for example, owning/attached `tako dev` sessions), not browser/app request clients.
    - Metric values in the right panel are right-aligned for easier scanning.
  - The top-left panel uses two columns: a compact 3-row solid block-glyph `TAKO` logo on the left and three info lines on the right: app name with lowercase status value (no `Status` caption), adapter as muted `{adapter} application`, and primary public URL.
    - On narrow terminals, Tako hides the logo so the info column can use the full left-panel width (prioritizing URL readability).
  - The logs panel has a simple top-left `Logs` caption row.
  - While the logs panel is empty during initial log replay, it shows a `Loading logs...` spinner line directly below the caption row. After replay completes and no lines are available yet, the line changes to `Waiting for logs...`; either hint disappears once logs are rendered.
  - Quitting the TUI uses the same disconnect grace as non-TUI exit: if the app is running, Tako keeps app/routes alive for the build-profile grace period before final cleanup (10 minutes in release, 10 seconds in debug).
  - The header panels show a single primary public URL (e.g. `https://my-app.tako.local`) and the local app URL (e.g. `http://localhost:12345`) without a trailing slash.
    - If multiple public routes exist, the primary URL is shown with a muted `(and N more)` suffix.
    - The primary URL prefers `{app}.tako.local` when present; otherwise it uses the first configured URL.
    - Public URLs are rendered with the secondary accent color.
  - Clicking the displayed primary public URL in the TUI copies it to the clipboard and shows feedback in the footer.
  - Keyboard shortcuts:
    - `q` quit
    - `Enter` copy only the focused log message text (without timestamp/level/scope) and show `message copied`
    - `Ctrl+c` quits the TUI session on all platforms
    - `r` restart (only when the app currently has a running instance)
    - `t` terminate (press once to arm confirmation, then press `t` or `y` within 3 seconds to confirm; `n`/`Esc` cancels)
      - Confirmation stops the owning `tako dev` session immediately, without disconnect grace.
    - `e` or `End` jump to the latest log line and re-enable follow-end mode
    - `c` clear logs across all attached sessions for the same app/project
    - If the app has 0 instances (not started or idle), pressing `r` is a no-op and the TUI shows an info message.
  - Log lines are prefixed as `hh:mm:ss LEVEL [scope] message`.
    - Common scopes: `tako` (local dev daemon) and `app` (the app process).
    - For app-process output, Tako infers the level from leading tokens like `DEBUG`, `INFO`, `WARN`/`WARNING`, `ERROR`, and `FATAL` (including bracketed forms such as `[DEBUG]`), and maps `TRACE` to `DEBUG`.
    - Consecutive duplicate log records (same level/scope/message) are collapsed into one full line plus a muted follow-up line such as `also N more times`.
  - `tako dev` always watches `tako.toml` and:
  - restarts the app when effective dev environment variables change
  - updates dev routing when `[envs.development].route(s)` changes
- Source hot-reload is runtime-driven (e.g. Bun watch/dev scripts); Tako does not watch source files for auto-restart.
- HTTPS is terminated by the local dev daemon using certificates issued by the local CA (SNI-based cert selection).
- `tako dev` ensures daemon TLS files exist at `{TAKO_HOME}/certs/fullchain.pem` and `{TAKO_HOME}/certs/privkey.pem` before spawning the daemon.
  - The daemon reuses existing TLS files when present.
- `tako dev` listens on `127.0.0.1:47831` in HTTPS mode.
- By default, Tako registers `https://{app}.tako.local:47831/` on non-macOS and `https://{app}.tako.local/` on macOS.
  - On macOS, Tako configures split DNS for `tako.local` by writing `/etc/resolver/tako.local` (one-time sudo), pointing to a local DNS listener on `127.0.0.1:53535`.
  - The dev daemon answers `A` queries for active `*.tako.local` hosts.
    - On macOS, it maps to a dedicated loopback address (`127.77.0.1`) used by pf forwarding.
    - On non-macOS, it maps to `127.0.0.1`.
  - On macOS, `tako dev` automatically tries to enable scoped local forwarding when missing (one-time sudo prompt):
    - `127.77.0.1:443 -> 127.0.0.1:47831`
    - `127.77.0.1:80 -> 127.0.0.1:47830` (HTTP redirect to HTTPS)
  - On macOS, Tako always requires this forwarding and always advertises `https://{app}.tako.local/` (no explicit port).
  - After applying or repairing local forwarding, Tako retries loopback 80/443 reachability and fails startup if those endpoints remain unreachable.
  - On macOS, Tako probes HTTPS for the app host via loopback and fails startup if that probe does not succeed.
  - If the daemon is reachable on `127.0.0.1:47831` but `https://{app}.tako.local/` still fails, Tako reports a targeted hint that local `:443` traffic is being intercepted/bypassed before reaching Tako.
  - `tako dev` uses routes from `[envs.development]` when configured; otherwise it defaults to `{app}.tako.local`.
    - Dev routes must be `{app}.tako.local` or a subdomain of it.
    - Dev routing matches exact hostnames only; wildcard host entries are ignored.
    - If configured dev routes contain no exact hostnames, `tako dev` fails with an invalid route error.
  - The HTTPS daemon listen port for `tako dev` is fixed at `47831`.

**Local CA architecture:**

- Root CA generated once on first run, private key stored in system keychain
- Keychain storage for the CA private key is scoped per `{TAKO_HOME}` to avoid cross-home key/cert mismatches.
- Leaf certificates generated on-the-fly for each app domain
- Public CA cert available at `{TAKO_HOME}/ca/ca.crt` (for `NODE_EXTRA_CA_CERTS`)
- On first run (or whenever not yet trusted), `tako dev` installs the root CA into the system trust store (may prompt for your password)
- Before the sudo prompt, `tako dev` explains why elevated access is needed and what will change.
- No browser security warnings once the CA is trusted

**Environment variables:**

- Loads from `[vars]` + `[vars.development]` in tako.toml
- `ENV=development`, `BUN_ENV=development`, `NODE_ENV=development`

### tako status [DIR]

Show global deployment status from configured servers, with one server block per configured host and app lines nested under each server:

```
Servers
✓ la (v0.1.0) up
  │ dashboard (production) running
  │ instances: 2/2
  │ build: abc1234
  │ deployed: 2026-02-08 11:48:19
────────────────────────────────────────
! nyc (v0.1.0) up
  │ worker (unknown) unknown
  │ instances: -/-
  │ build: -
  │ deployed: -
```

Shows: a `Servers` section, server connectivity/service lines, and per-app lines in `app-name (environment) state` form.
App heading/detail rows are shown with a left `│` guide border for readability.
Environment is inferred from deployed release metadata when available; otherwise app status uses `unknown`.
App state text is color-coded (`running` success, `idle` muted, `deploying`/`stopped` warning, `error` error).
Each app line includes instance summary (`healthy/total`), build, and deployed timestamp (formatted in the user's current locale and local time, without timezone suffix).
In interactive terminals, each server check runs with a spinner before rendering final status lines.

Status flow helpers:

- `tako status` does not require `tako.toml` and can run from any directory.
- Uses global server inventory from `~/.tako/config.toml`.
- If no servers are configured and the terminal is interactive, status offers to run the add-server wizard.
- If no deployed apps are found, status reports that explicitly.

### tako logs [--env {environment}]

Stream logs from all servers in an environment.

- Environment must exist in `tako.toml`.
- Streams from all mapped servers in parallel.
- Prefixes each line with `[server-name]` so multi-server output is readable.
- Runs until interrupted (`Ctrl+c`).

Logs flow helpers:

- For `production`, if no `[servers.*]` env mapping exists but exactly one global server exists, logs offers to use that server.
- For `production`, if no servers are configured and the terminal is interactive, logs offers to run the add-server wizard.

### tako servers add [host] [--name {name}] [--description {text}] [--port {port}]

Add server to global `~/.tako/config.toml` (`[[servers]]`).

- With `host`: adds directly from CLI args.
- With `host`: `--name` is required (no implicit default to hostname).
- Without `host` (interactive terminal): launches a guided wizard (host, required server name, optional description, SSH port) with a final `Looks good?` confirmation. Choosing `No` restarts the wizard.
- The add-server wizard supports `Tab` autocomplete suggestions for host/name/port from existing servers and persisted CLI history.
  - For name/port prompts, suggestions related to the selected host (and selected name for ports) are prioritized first, then global suggestions are shown.
- Successful adds record host/name/port history in `~/.tako/history.toml` for future autocomplete.
- `--description` stores optional human-readable metadata in `~/.tako/config.toml` (shown in `tako servers ls`).
- Re-running with the same name/host/port is idempotent (reports already configured and succeeds).

Tests SSH connection before adding. Connects as the `tako` user.

If `tako-server` is not installed on the target, `tako` warns and expects the user to install it manually.

### tako servers rm [name]

Remove server from `~/.tako/config.toml` (`[[servers]]`).

When `name` is omitted in an interactive terminal, `tako` opens a server selector.
In non-interactive mode, `name` is required.

Confirms before removal. Warns that projects referencing this server will fail.

Aliases: `tako servers remove [name]`, `tako servers delete [name]`.

### tako servers ls

List all configured servers from global config (`~/.tako/config.toml`) as a table:

- Name
- Host
- Port
- Optional description

Alias: `tako servers list`.

If no servers are configured, `tako servers ls` shows a hint to run `tako servers add`.

### tako servers restart {server-name}

Restart `tako-server` process entirely (causes brief downtime for all apps).

Use for: binary updates, major configuration changes, system recovery.

### tako servers reload {server-name}

Reload configuration (secrets) for all apps without restarting.

Apps must implement `onConfigReload` handler in SDK to handle this.

### tako servers status {server-name}

Show remote server health details:

- whether `tako-server` is installed (and version when available)
- service status (`active` / `inactive` / `failed` / unknown)
- deployed app summary returned by the runtime control socket

### tako secrets set [--env {environment}] {name}

Set/update secret for environment (defaults to production).

When running in an interactive terminal, prompts for value with masked input. In non-interactive mode, reads a single line from stdin. Stores encrypted value locally in `.tako/secrets`.

Uses the environment key at `~/.tako/keys/{env}` (creates it if missing).

### tako secrets rm [--env {environment}] {name}

Remove secret from environment.

Removes from local `.tako/secrets`. Omitting `--env` removes the secret from all environments.

Aliases: `tako secrets remove ...`, `tako secrets delete ...`.

### tako secrets ls

List all secrets with presence table across environments.

Shows which secrets exist in which environments. Warns about missing secrets. Never displays values.

Alias: `tako secrets list`.

### tako secrets sync

Sync all local secrets to all environments.

Source of truth: local `.tako/secrets`.

For each environment, sync decrypts with `~/.tako/keys/{env}`.

Sync flow helpers:

- For `production`, if no `[servers.*]` env mapping exists but exactly one global server exists, sync offers to target that server.
- If no servers are configured and the terminal is interactive, sync offers to run the add-server wizard.

### tako secrets key import [--env {environment}]

Import a base64 key from masked terminal input.

Writes `~/.tako/keys/{env}`.

When `--env` is omitted, `production` is used.

### tako secrets key export [--env {environment}]

Export a key to clipboard.

Reads `~/.tako/keys/{env}`.

When `--env` is omitted, `production` is used.

### tako deploy [--env {environment}] [--yes|-y] [DIR]

Build and deploy application to environment's servers.

When `--env` is omitted, deploy targets `production`.

Deploy target environment must be declared in `tako.toml` (`[envs.<name>]`) and must define `route` or `routes`.

`development` is reserved for `tako dev` and cannot be used with `tako deploy`.

In interactive terminals, deploying to `production` requires an explicit confirmation unless `--yes` (or `-y`) is provided.

Deploy flow helpers:

- If no servers are configured and the terminal is interactive, deploy offers to run the add-server wizard before continuing.
- For `production`, if no `[servers.*]` env mapping exists:
  - with one global server: deploy shows a `tako.toml` mapping example, explains that selecting "Yes" will write it, and if declined offers the normal add-server wizard to add a different production server
  - with multiple global servers (interactive terminal): deploy asks you to pick one, then writes `[servers.<name>] env = "production"` to `tako.toml`
- Interactive deploy progress:
  - single-server deploys show spinner loaders for long per-server steps (connect, lock, upload, extract, etc.) while pending
  - multi-server deploys keep line-based progress output to avoid overlapping spinners

**Steps:**

1. Pre-deployment validation (secrets present, runtime detected)
2. Build locally
3. Create archive (`.tako/build/{version}.tar.gz`) (previous builds are cleared before each deploy)
4. Deploy to all servers in parallel:
   - Require `tako-server` to be pre-installed and running on each server
   - Acquire deploy lock (prevents concurrent deploys)
   - Upload and extract archive
   - Write `.env` with `TAKO_BUILD={version}` and secrets
   - Perform rolling update
   - Release lock and clean up old releases (>30 days)

**Version naming:**

- Clean git tree: `{commit_hash}` (e.g., `abc1234`)
- Dirty working tree: `{commit_hash}_{content_hash}` (first 8 chars each)
- No git commit/repo: `nogit_{timestamp}` (or `nogit_{content_hash}` when provided)

**Deploy lock (server-side):**

- CLI acquires lock on each server by creating `/opt/tako/apps/{app}/.deploy_lock` (atomic `mkdir` over SSH)
- Prevents concurrent deploys of the same app on the same server
- Lock is released at end of deploy (best-effort). If a deploy crashes mid-flight, the lock must be removed manually.

**Rolling update (per server):**

1. Start new instance
2. Wait for health check pass (30s timeout)
3. Add to load balancer
4. Gracefully stop old instance (drain connections, 30s timeout)
5. Repeat until all instances replaced
6. Update `current` symlink to the new release directory
7. Clean up releases older than 30 days

**On failure:** Automatic rollback - kill new instances, keep old ones running, return error to CLI.

**App start command (current):**

- tako-server currently only supports Bun apps.
- If `package.json` contains `scripts.dev`, tako-server starts the app with `bun run dev`.
- Otherwise it starts with `bun run src/index.ts` (fallback: `index.ts`).

**Partial failure:** If some servers fail while others succeed, deployment continues. Failures are reported at the end.

**Disk space preflight:** Before uploading artifacts, `tako deploy` checks free space under `/opt/tako` on each target server.

- Required free space is based on archive size plus unpack headroom.
- If free space is insufficient, deploy fails early with required vs available sizes.

**Failed deploy cleanup:** If a deploy fails after creating a new release directory, `tako deploy` automatically removes that newly-created partial release directory before returning an error.

**Deployment target:**

- If `[servers.*]` sections in tako.toml → deploy to environment's servers
- If deploying to `production` with no `[servers.*]` mapping:
  - exactly one server in `~/.tako/config.toml` `[[servers]]` → prompt to use it, show a mapping example, and persist `[servers.<name>] env = "production"` in `tako.toml` on confirmation
    - if declined, offer the add-server wizard to create/select a different server for production, then persist that mapping
  - multiple servers in `~/.tako/config.toml` `[[servers]]` (interactive terminal) → prompt to select one and persist `[servers.<name>] env = "production"` in `tako.toml`
- If no servers exist in `~/.tako/config.toml` `[[servers]]` → fail with hint to run `tako servers add <host>`
- Otherwise, require explicit `[servers.*]` mapping in tako.toml

## Routing and Multi-App Support

### Route Configuration

Apps specify routes at environment level (not per-server). Routes support:

- Exact hostname: `api.example.com`
- Wildcard subdomain: `*.api.example.com`
- Hostname + path: `api.example.com/api/*`
- Wildcard + path: `*.example.com/admin/*`

**Validation rules:**

- Routes must include hostname (path-only routes invalid: `"/api/*"` ❌)
- Each `[envs.{env}]` can have either `route` or `routes`, not both
- Each non-development environment must define `route` or `routes`
- Empty route lists are invalid for non-development environments
- Development routes must be `{app-name}.tako.local` or a subdomain of it

### Multi-App Scenarios

**Apps with routes:**

- Each app specifies its routes
- Requests matched to most specific route (exact > wildcard, longer path > shorter)
- Conflict detection during deploy prevents overlapping routes
- Requests without a matching route return `404`

**Wildcard subdomains:**

- `*.example.com` routes to app, app handles tenant logic based on subdomain

### Routing Logic (tako-server)

1. Parse incoming request (Host header, path)
2. Match against deployed apps' routes
3. Select most specific match
4. Route to app's load balancer
5. Return 404 if no match

## Tako Server

### Installation

Manual for v1. Users run a server setup script (or equivalent manual steps) to:

1. Create a dedicated `tako` OS user for SSH and running `tako-server`
2. Install `tako-server` to `/usr/local/bin/tako-server`
3. Install and enable a system service (systemd when available)
4. Create and permissions required directories:
   - Data dir: `/opt/tako`
   - Socket dir: `/var/run/tako`

Recommended: run the hosted installer script on the server (as root):

```bash
curl -fsSL https://tako.sh/install-server | sh
```

Installer SSH key behavior:

- If `TAKO_SSH_PUBKEY` is set, installer uses it and skips prompting.
- If unset and running interactively, installer prompts for a public key to authorize for user `tako`.
- If unset and non-interactive, installer continues without key setup and prints a warning.
- Installer ensures `nc` (netcat) is available so CLI management commands can talk to `/var/run/tako/tako.sock`.
- Installer attempts to grant `CAP_NET_BIND_SERVICE` to `/usr/local/bin/tako-server` via `setcap` so non-systemd/manual runs can still bind `:80/:443` as a non-root user (warns when `setcap` is unavailable or fails).
- When systemd is available, installer verifies `tako-server` is active after `enable --now`; if startup fails, installer exits non-zero and prints recent service logs.

Reference script in this repo: `scripts/install-tako-server.sh` (source for `/install-server`, alias `/server-install`).

**Default behavior (no configuration file needed):**

- HTTP: port 80
- HTTPS: port 443
- Data: `/opt/tako`
- Socket: `/var/run/tako/tako.sock`
- ACME: Production Let's Encrypt
- Renewal: Every 12 hours
- HTTP requests redirect to HTTPS (`301`) by default.
- Exceptions: `/.well-known/acme-challenge/*` and `/_tako/status` stay on HTTP.

**Optional `/opt/tako/server-config.toml`:**

```toml
[server]
http_port = 80
https_port = 443

[acme]
staging = false         # true for development/testing
email = "admin@example.com"
```

### Zero-Downtime Operation

- HTTP listener with `SO_REUSEPORT` allows gradual traffic shifting during upgrades
- Graceful shutdown drains connections before exit
- New tako-server version starts alongside old, both accept traffic
- CLI communicates via versioned unix socket symlink
- Old version exits after in-flight requests complete

### Directory Structure

```
/opt/tako/
├── server-config.toml
├── acme/
│   └── credentials.json
├── certs/
│   ├── {domain}/
│   │   ├── fullchain.pem
│   │   └── privkey.pem
└── apps/
    └── {app-name}/
        ├── current -> releases/{version}
        ├── .deploy_lock/
        ├── releases/{version}/
        │   ├── build files...
        │   ├── .env (merged vars/secrets + TAKO_BUILD)
        │   └── logs -> /opt/tako/apps/{app-name}/shared/logs
        └── shared/
            └── logs/
```

## Communication Protocol

### Unix Sockets

**tako-server socket:**

- Path: `/var/run/tako/tako.sock` (symlink to versioned socket)
- Used by: CLI for deploy/reload commands, apps for status/heartbeat

**App instance sockets:**

- Path: `/var/run/tako-app-{app-name}-{pid}.sock`
- Created by app on startup
- Used by: tako-server to proxy HTTP requests

### Environment Variables for Apps

| Name              | Used by         | Meaning                                                                    | Typical source                                                                                |
| ----------------- | --------------- | -------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------- |
| `PORT`            | app             | Listen port for HTTP server                                                | Set by `tako-server` when running app instances; set by `tako dev` for the local app process. |
| `ENV`             | app             | Environment name                                                           | Usually `development` in `tako dev`; `production` under `tako-server`.                        |
| `NODE_ENV`        | app             | Node.js convention env                                                     | Set by runtime adapter / server (`development` or `production`).                              |
| `BUN_ENV`         | app             | Bun convention env                                                         | Set by runtime adapter (`development` or `production`).                                       |
| `TAKO_BUILD`      | app             | Deployed build/version identifier                                          | Written into the release `.env` file during `tako deploy`.                                    |
| `TAKO_APP_SOCKET` | app / `tako.sh` | Unix socket path the app should listen on (if using socket-based proxying) | path string or unset                                                                          |
| `TAKO_SOCKET`     | app / `tako.sh` | Unix socket path for connecting to `tako-server`                           | default `/var/run/tako/tako.sock`                                                             |
| `TAKO_VERSION`    | app / `tako.sh` | App version string (if you choose to set one)                              | string                                                                                        |
| `TAKO_INSTANCE`   | app / `tako.sh` | Instance identifier                                                        | integer string                                                                                |
| _user-defined_    | app             | User config vars/secrets                                                   | From `[vars]` and `[envs.*].vars` plus secrets in release `.env`.                             |

### Messages (JSON over Unix Socket)

**CLI → tako-server (management commands):**

- `hello` (capabilities / protocol negotiation; CLI sends this before other commands):

```json
{ "command": "hello", "protocol_version": 2 }
```

Response:

```json
{
  "status": "ok",
  "data": {
    "protocol_version": 2,
    "server_version": "0.1.0",
    "capabilities": ["deploy_instances_idle_timeout", "on_demand_cold_start", "idle_scale_to_zero"]
  }
}
```

- `deploy` (includes route patterns for routing + TLS/ACME):

```json
{
  "command": "deploy",
  "app": "my-app",
  "version": "1.0.0",
  "path": "/opt/tako/apps/my-app/releases/1.0.0",
  "routes": ["api.example.com", "*.example.com/admin/*"],
  "instances": 0,
  "idle_timeout": 300
}
```

- `routes` (returns app → routes mapping used for conflict detection/debugging):

```json
{ "command": "routes" }
```

**App → tako-server:**

```json
// Ready signal (on startup)
{"type": "ready", "app": "dashboard", "version": "abc1234", "instance_id": 1, "pid": 12345, "socket_path": "/var/run/tako-app-dashboard-12345.sock", "timestamp": "..."}

// Heartbeat (every 1 second)
{"type": "heartbeat", "app": "dashboard", "instance_id": 1, "pid": 12345, "timestamp": "..."}

// Shutdown acknowledgment
{"type": "shutdown_ack", "app": "dashboard", "instance_id": 1, "pid": 12345, "drained": true, "timestamp": "..."}
```

**tako-server → App:**

```json
// Graceful shutdown
{"type": "shutdown", "reason": "deploy", "drain_timeout_seconds": 30}

// Config reload (secrets changed)
{"type": "reload_config", "secrets": {"DATABASE_URL": "...", "API_KEY": "..."}}
```

### Health Checks

Active HTTP probing is the source of truth for instance health:

- **Probe interval**: 1 second by default (configurable)
- **Probe endpoint**: App's configured health check path (default: `/_tako/status`)
- **Unhealthy threshold**: 2 consecutive failures → mark unhealthy, remove from load balancer
- **Dead threshold**: 5 consecutive failures → mark stopped, kill process
- **Recovery**: Single successful probe resets failure count and restores to healthy

#### `/_tako/status` Endpoint

Tako-server exposes a status endpoint for external health monitoring:

```
GET /_tako/status
```

Response (200 if healthy, 503 if unhealthy):

```json
{
  "healthy": true,
  "apps": [
    {
      "name": "my-app",
      "version": "v1.2.3",
      "state": "running",
      "last_error": null,
      "instances": [
        {
          "id": 1,
          "state": "healthy",
          "port": 3000,
          "pid": 12345,
          "uptime_secs": 3600,
          "requests_total": 50000
        }
      ]
    }
  ]
}
```

This endpoint is accessible on both HTTP and HTTPS, bypasses routing, and is not redirected.

## TLS/SSL Certificates

### SNI-Based Certificate Selection

Tako-server uses SNI (Server Name Indication) to select the appropriate certificate during TLS handshake:

1. Client connects and sends SNI hostname
2. Server looks up certificate for that hostname in CertManager
3. If exact match found, use that certificate
4. If no exact match, try wildcard fallback (e.g., `api.example.com` → `*.example.com`)
5. If still no match, TLS handshake fails (prevents serving wrong certificate)

This requires OpenSSL (not rustls) for callback support.

### Automatic Management

- ACME protocol (Let's Encrypt)
- Automatic issuance for domains in app routes
- Automatic renewal 30 days before expiry
- HTTP-01 challenge (port 80)
- Zero-downtime renewal
- DNS-01 is not implemented (automatic wildcard certificate issuance is not available yet)

### Wildcard Certificate Handling

Routing supports wildcard hosts (e.g. `*.example.com`). For TLS:

- wildcard certificates are used when present in cert storage
- automated ACME issuance currently uses HTTP-01, so wildcard certs must be provisioned manually

### Certificate Storage

```
/opt/tako/certs/{domain}/
├── fullchain.pem      # Certificate + intermediates
└── privkey.pem        # Private key (0600 permissions)
```

### Development

Set `staging = true` in `/opt/tako/server-config.toml` to use Let's Encrypt staging:

- No rate limits
- Unlimited certificate issuance
- Certificates not trusted by browsers
- Perfect for development/testing

## tako.sh SDK

### Installation

```bash
npm install tako.sh
```

### Interface

Apps export a Web Standard fetch handler:

```typescript
export default {
  fetch(request: Request): Response | Promise<Response> {
    return new Response("Hello!");
  },
};
```

### Runtime Adapters

```typescript
import { Tako } from "tako.sh/bun"; // Bun
import { Tako } from "tako.sh/node"; // Node.js
import { Tako } from "tako.sh/deno"; // Deno
import { Tako } from "tako.sh"; // Auto-detect
```

### Feature Overview

- Unix socket creation and management
- Ready signal on startup
- Heartbeat every 1 second
- `/_tako/status` endpoint
- Graceful shutdown handling

### Optional Features

```typescript
Tako.onConfigReload((newSecrets) => {
  // Handle config changes (e.g., reconnect to database)
  database.reconnect(newSecrets.DATABASE_URL);
});
```

### Built-in Endpoints

**`GET /_tako/status`**

```json
{
  "status": "healthy",
  "app": "dashboard",
  "version": "abc1234",
  "instance_id": 1,
  "pid": 12345,
  "uptime_seconds": 3600
}
```

Used for health checks during rolling updates and monitoring.

## Edge Cases & Error Handling

| Scenario                           | Behavior                                                                   |
| ---------------------------------- | -------------------------------------------------------------------------- |
| `~/.tako/` deleted                 | Auto-recreate on next command                                              |
| `~/.tako/config.toml` corrupted    | Show parse error with line number, offer to recreate                       |
| `tako.toml` deleted                | Commands that require project config fail with guidance to run `tako init` |
| `.tako/` deleted                   | Auto-recreate on next deploy                                               |
| `.tako/secrets` deleted            | Warn user, prompt to restore secrets                                       |
| Low free space under `/opt/tako`   | Deploy fails before upload with required vs available disk sizes           |
| Deploy lock left behind            | Deploy fails until `/opt/tako/apps/{app}/.deploy_lock` is removed          |
| Deploy fails mid-transfer/setup    | Auto-clean newly-created partial release directory                         |
| Health check fails                 | Automatic rollback to previous version                                     |
| Network interruption during deploy | Partial failure handling, can retry                                        |
| Process crash                      | Auto-restart, health checks detect and handle                              |

## Testing Requirements

- Unit tests for all business logic (config parsing, validation, routing)
- Integration tests for critical paths (deploy, rolling updates, health checks)
- Edge case tests (deleted files, network failures, process crashes)
- Critical-path coverage target: >=80% line coverage across core modules (config parsing, runtime detection, routing, static file resolution, cold-start orchestration)
- TDD mandatory: write tests first, implement after tests pass

## Performance Targets

- Proxy throughput: Faster than Caddy, on par with Nginx
- Cold start: ~100-500ms for on-demand instances
- Health detection: <3s for failed instance detection
- Deploy time: <1 minute for rolling update of 3 instances
- Memory: Minimal footprint with on-demand scaling
