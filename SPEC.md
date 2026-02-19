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
`name` is optional in `tako.toml`. If omitted, Tako resolves app name from the project directory name.
Using top-level `name` is recommended for stability: it must be unique per server. Renaming it later creates a new app identity/path; delete the old deployment manually.

### tako.toml (Project Root - Required)

Application configuration for build, variables, routes, and deployment.

```toml
name = "my-app"           # Optional but recommended stable identity used by deploy/dev
main = "server/index.mjs" # Optional override; required only when preset does not define top-level `main`
runtime = "bun"           # Optional override; defaults to detected adapter
# preset = "tanstack-start" # Optional runtime-local preset; omit for adapter base preset

[build]
# include = ["dist/**", ".output/**"]
# exclude = ["**/*.map"]
# assets = ["dist/client", "assets/shared"]
# [[build.stages]]
# name = "frontend-assets"
# working_dir = "frontend"
# install = "bun install"
# run = "bun run build"

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
3. Auto-set by Tako during deploy: `TAKO_ENV={environment}`, `TAKO_BUILD={version}`, plus runtime env vars (for Bun: `NODE_ENV`, `BUN_ENV`)

**Build/deploy behavior:**

- `name` in `tako.toml` is optional.
- App name resolution order for deploy/dev/logs/secrets/delete:
  1. top-level `name` (when set)
  2. sanitized project directory name fallback
- For predictable deploy identity, set `name` explicitly and keep it unique per server.
- Renaming app identity (`name` or directory fallback) is treated as a different app; remove the previous deployment manually if needed.
- `main` in `tako.toml` is an optional runtime entrypoint override written to deployed `app.json`.
- If `main` is omitted in `tako.toml`, deploy/dev use preset top-level `main` when present.
- If neither `tako.toml main` nor preset `main` is set, deploy/dev fail with guidance.
- Legacy top-level `dist` and `assets` keys are not supported.
- Top-level `runtime` is optional; when set to `bun`, `node`, or `deno`, it overrides adapter detection for default preset selection in `tako deploy`/`tako dev`.
- Top-level `preset` is optional; when omitted, `tako deploy`/`tako dev` use adapter base preset from top-level `runtime` when set, otherwise detected adapter (`unknown` falls back to `bun`).
- `preset` supports:
  - runtime-local aliases: `tanstack-start` (resolved under selected runtime, e.g. `runtime = "bun"`)
  - pinned runtime-local aliases: `tanstack-start@<commit-hash>`
- namespaced preset aliases in `tako.toml` (for example `js/tanstack-start`) are rejected; choose runtime via top-level `runtime` and keep `preset` runtime-local.
- `github:` preset references are not supported in `tako.toml`.
- Adapter base presets (`bun`, `node`, `deno`) are built into the CLI (not loaded from workspace preset files).
- Runtime family preset definitions live in `presets/<family>.toml` (for example `presets/js.toml`), where each preset is a section (`[tanstack-start]`, etc.).
- Bun `tanstack-start` defaults `main = "dist/server/tako-entry.mjs"` and adds `[build].assets = ["dist/client"]`.
- Runtime base presets (`bun`, `node`, `deno`) define lifecycle defaults (`dev`, `install`, `start`, `[build].install`, `[build].build`).
- Runtime base presets also provide default build filters/targets (`[build].exclude`, `[build].targets`, `[build].container`) and default `assets`.
- Preset `[build].exclude` entries are appended to runtime-base excludes (base-first, deduplicated).
- Preset `[build].targets` and `[build].container` override runtime defaults when set (including explicit empty arrays or explicit `container` values).
- Preset `[build].assets` override runtime-base `assets` when set.
- Build preset TOML supports optional top-level `name` (fallback: preset section name), top-level `main` (default app entrypoint), and `[build]` (`assets`, `exclude`, optional `targets = ["linux-<arch>-<libc>", ...]`, optional `container = true|false`). Presets can still override runtime lifecycle fields (`dev`, `install`, `start`, `[build].install`, `[build].build`) when needed. Legacy preset top-level `assets`, `[dev]`, `[deploy]`, preset `include`, `[artifact]`, top-level `dev_cmd`, and `[build].docker` are not supported.
- Deploy resolves the preset source and writes `.tako/build.lock.json` (`preset_ref`, `repo`, `path`, `commit`) for reproducible preset fetches on later deploys.
- During `tako deploy`, source files are bundled from source root (`git` root when available, otherwise app directory).
- Source bundle filtering uses `.gitignore`.
- Deploy always excludes `.git/`, `.tako/`, `.env*`, `node_modules/`, and `target/`.
- Deploy sends merged app vars + runtime vars + decrypted secrets to `tako-server` in the `deploy` command payload; `tako-server` injects them directly into app process environment on spawn.
- Deploy build mode is controlled by preset `[build].container`:
  - `true`: build each target artifact in Docker.
  - `false`: run build commands on the local host workspace for each target.
  - when unset, default is `true` if `[build].targets` is non-empty, otherwise `false`.
- App-level custom build stages can be declared in `tako.toml` under `[[build.stages]]`:
  - `name` (optional display label)
  - `working_dir` (optional, relative to app root; absolute paths and `..` are rejected)
  - `install` (optional command run before `run`)
  - `run` (required command)
- Build presets do not support `[[build.stages]]`; custom stages are configured only in app `tako.toml`.
- Per-target build execution order is fixed:
  - stage 1: preset `[build].install` then preset `[build].build` (when present)
  - stage 2+: app `[[build.stages]]` in declaration order (`install` then `run` per stage)
- Docker build containers are ephemeral; dependency caches are persisted with target-scoped Docker volumes keyed by cache kind + target label + builder image (mise cache: `/var/cache/tako/mise`, Bun cache: `/var/cache/tako/bun/install/cache`).
- Runtime version resolution is mise-aware:
  - local builds try `mise exec -- <tool> --version` from app workspace when `mise` is installed.
  - if local mise probing is unavailable/fails, deploy falls back to reading `mise.toml` (`[tools]` in app then workspace), then `latest`.
  - Docker target builds bootstrap `mise` in-container and probe with `mise exec -- <tool> --version`.
  - deploy writes the resolved runtime tool version into release `mise.toml` before packaging.
- Built target artifacts are cached locally under `.tako/artifacts/` using a deterministic cache key that includes source hash, target label, resolved preset source/commit, target build commands/image, app custom build stages, include/exclude patterns, asset roots, and app subdirectory.
- Cached artifacts are checksum/size verified before reuse; invalid cache entries are automatically discarded and rebuilt.
- After each target build (and asset merge), deploy verifies the resolved runtime `main` file exists in the build workspace before artifact packaging; missing files fail deploy with an explicit error.
- On every deploy, local artifact cache is pruned automatically (best-effort): keep 30 most recent source archives (`*-source.tar.gz`), keep 90 most recent target artifacts (`artifact-cache-*.tar.gz`), and remove orphan target metadata files.
- Artifact include patterns are resolved in this order:
  - `build.include` (if set)
  - fallback `**/*`
- Artifact exclude patterns are preset `[build].exclude` plus app `build.exclude`.
- For Bun deploys, default preset excludes `node_modules`; `tako-server` installs dependencies on server (`bun install --production`, plus `--frozen-lockfile` when Bun lockfile is present).
- Asset roots are preset `[build].assets` plus app `build.assets` (deduplicated), then merged into app `public/` after container build in listed order (later entries overwrite earlier ones).

**Instance behavior:**

- `instances = 0`: On-demand with scale-to-zero. Deploy keeps one warm instance running so the app is immediately reachable after deploy. Instances are stopped after idle timeout.
  - Once scaled to zero, the next request triggers a cold start and waits for readiness up to startup timeout (default 30 seconds). If no healthy instance is ready before timeout, proxy returns `504 App startup timed out`.
  - If cold start setup fails before readiness, proxy returns `502 App failed to start`.
  - If warm-instance startup fails during deploy, deploy fails.
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

[server_targets.la]
arch = "x86_64"
libc = "glibc"

[server_targets.nyc]
arch = "aarch64"
libc = "musl"
```

`[[servers]]` entries are managed by `tako servers add/rm/ls`. All names and hosts must be globally unique.
Detected server build target metadata is stored under `[server_targets.<name>]` (`arch`, `libc`).

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
- `tako dev [DIR]`
- `tako deploy [DIR]`
- `tako delete [DIR]`

### tako init [--force] [--runtime <bun|node|deno>] [DIR]

Create `tako.toml` template with helpful comments.

```bash
tako init
```

Template behavior:

- Leaves only minimal starter options uncommented:
  - `name`
  - `[envs.production].route`
  - top-level `runtime`
  - top-level `preset` only when a non-base preset is selected (for base adapter presets and custom mode, it remains commented/unset)
- Includes commented examples/explanations for all supported `tako.toml` options:
  - `name`, `main`, top-level `runtime`/`preset`, and `[build]` (`include`, `exclude`, `assets`, `[[build.stages]]`)
  - `[vars]`
  - `[vars.<env>]`
  - `[envs.<env>]` route declarations (`route`/`routes`)
  - `[servers]`
  - `[servers.<name>]` overrides
- Includes a docs link to `https://tako.sh/docs/tako-toml`.
- Prompts for required app `name` (default from directory-derived app name).
- Prompts for required production route (`[envs.production].route`) with default `{name}.example.com`.
- Detects adapter (`bun`, `node`, `deno`, fallback `unknown`) and prompts for runtime selection unless `--runtime` is provided.
- In interactive mode, init fetches runtime-family preset names from official family manifest files (`presets/<family>.toml`) and shows `Fetching presets...` while loading.
- For built-in base adapters, init defaults to:
  - Bun: `bun`
  - Node: `node`
  - Deno: `deno`
- If no family presets are available after fetch, init skips preset selection and uses the runtime base preset.
- When "custom preset reference" is selected, init leaves top-level `preset` unset (commented) but still writes top-level `runtime`.
- For `main`, init behavior is:
  - if adapter inference finds an entrypoint and it differs from preset default `main`, write it as top-level `main`;
  - if inferred `main` matches preset default (or preset default exists and no inference is available), omit top-level `main`;
  - prompt only when neither adapter inference nor preset default `main` is available.

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
  - If forwarding later appears inactive, `tako dev` explains why it is re-requesting sudo before repair (missing pf rules, runtime forwarding reset after reboot/pf reset, or conflicting local listeners on `127.0.0.1:80/443`).
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

### tako servers status

Show global deployment status from configured servers, with one server block per configured host and one app block per running build nested under each server:

```
✓ la (v0.1.0) up
  ┌ dashboard (production) running
  │ instances: 2/2
  │ build: abc1234
  └ deployed: 2026-02-08 11:48:19
────────────────────────────────────────
! nyc (v0.1.0) up
  ┌ worker (unknown) running
  │ instances: 1/1
  │ build: old5678
  └ deployed: 2026-02-08 11:40:10
  ┌ worker (unknown) deploying
  │ instances: -/-
  │ build: new9012
  └ deployed: -
```

Shows server connectivity/service lines and per-build app blocks with heading lines in `app-name (environment) state` form.
Each app block uses a tree connector (`┌` heading, `│` detail continuation, `└` final deployed line).
Environment is inferred from deployed release metadata when available; otherwise app status uses `unknown`.
App state text is color-coded (`running` success, `idle` muted, `deploying`/`stopped` warning, `error` error).
Each app block includes instance summary (`healthy/total`), build, and deployed timestamp (formatted in the user's current locale and local time, without timezone suffix).
`tako servers status` prints a single snapshot and exits.

Status flow helpers:

- `tako servers status` does not require `tako.toml` and can run from any directory.
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

During SSH checks, `tako servers add` also detects and stores target metadata (`arch`, `libc`) in `~/.tako/config.toml` under `[server_targets.<name>]`.

If `--no-test` is used, SSH checks and target detection are skipped; deploy later fails for that server until target metadata is captured by re-adding the server with SSH checks enabled.

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

Systemd-managed servers use `KillMode=control-group` and a 30-minute stop timeout for restart/stop operations, allowing all app processes in the service cgroup time to handle graceful shutdown before systemd force-kills remaining processes.

`tako-server` persists app runtime registration (app config, routes, and server-side env/secrets map) in SQLite under the data directory and restores it on startup so app routing/config survives process restarts and crashes.

During single-host upgrade orchestration, `tako-server` may enter an internal `upgrading` server mode that temporarily rejects mutating management commands (`deploy`, `stop`, `delete`, `reload`, `update-secrets`) until the upgrade window ends.
Upgrade mode transitions are guarded by a durable single-owner upgrade lock in SQLite so only one upgrade controller can hold the upgrade window at a time.

### tako servers upgrade {server-name}

Single-host upgrade handoff with a temporary candidate process:

1. CLI acquires the durable upgrade lock (`enter_upgrading`) and sets server mode to `upgrading`.
2. CLI reads active runtime settings (`server_info`) from the management socket.
3. CLI starts a temporary candidate `tako-server` process on a temporary socket path (for example `/var/run/tako/tako-upgrade-<owner>.sock`) with the same HTTP/HTTPS listener ports.
4. Candidate startup uses an instance port offset (`--instance-port-offset 10000`) so candidate-managed app instances avoid local port collisions with the active server during overlap.
5. CLI waits until the candidate answers protocol `hello` on the temporary socket.
6. CLI restarts the primary systemd service (inherits graceful stop behavior: `KillMode=control-group`, `TimeoutStopSec=30min`).
7. CLI waits for the primary management socket to become healthy again.
8. CLI stops the temporary candidate process and releases upgrade mode (`exit_upgrading`).

On failure, CLI performs best-effort cleanup: stop candidate (if started) and release upgrade mode once primary is reachable.

### tako servers reload {server-name}

Reload configuration (secrets) for all apps without restarting.

Apps must implement `onConfigReload` handler in SDK to handle this.

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

### tako secrets sync [--env {environment}]

Sync local secrets to servers.

Source of truth: local `.tako/secrets`.

By default, sync processes all environments declared in `tako.toml`.
When `--env` is provided, sync processes only that environment.

For each target environment, sync decrypts with `~/.tako/keys/{env}`.

Sync flow helpers:

- For `production`, if no `[servers.*]` env mapping exists but exactly one global server exists, sync offers to target that server.
- If no servers are configured and the terminal is interactive, sync offers to run the add-server wizard.
- Sync sends `update_secrets` (and best-effort `reload`) to `tako-server`; it does not write remote `.env` files.

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

1. Pre-deployment validation (secrets present, server target metadata present/valid for all selected servers)
2. Resolve source bundle root (git root when available; otherwise app directory)
3. Resolve app subdirectory relative to source bundle root
4. Resolve deploy runtime `main` (`main` from `tako.toml`, otherwise preset top-level `main`)
5. Create source archive (`.tako/artifacts/{version}-source.tar.gz`) and write `app.json` at app path inside archive
   - Version format: clean git tree => `{commit}`; dirty git tree => `{commit}_{source_hash8}`; no git commit => `nogit_{source_hash8}`
   - Best-effort local artifact cache prune runs before target builds (retention: 30 source archives, 90 target artifacts; orphan target metadata is removed).
6. Resolve build preset (top-level `preset` override or adapter base preset from top-level `runtime`/detection) and persist lock metadata in `.tako/build.lock.json`
7. Build target artifacts locally (one artifact per unique server target label):
   - Resolve deterministic cache key per target.
   - On cache hit, reuse existing verified target artifact.
   - On cache miss (or invalid cache entry), extract source archive into a temporary workspace.
   - Build in Docker when preset `[build].container = true`.
   - Build on local host workspace when `[build].container = false`.
   - When `container` is unset, default to Docker only when `[build].targets` is non-empty.
   - Run build commands in fixed order: preset stage first (`[build].install`, then `[build].build`), then app `[[build.stages]]` in declaration order.
   - Merge configured assets into app `public/`.
   - Verify resolved runtime `main` exists in the built app directory.
   - Materialize release `mise.toml` in app dir with resolved runtime tool version for server runtime parity.
   - Package filtered artifact tarball for that target using include/exclude rules and store it in local cache.
   - Per-target cache writes are serialized with a local lock to avoid duplicate concurrent builds.
8. Deploy to all servers in parallel:
   - Require `tako-server` to be pre-installed and running on each server
   - Acquire deploy lock (prevents concurrent deploys)
   - Upload and extract target-specific artifact
   - Write final `app.json` in app directory using resolved runtime `main`
   - Send deploy command with merged environment payload (`TAKO_BUILD`, `TAKO_ENV`, runtime vars, user vars, decrypted secrets)
   - Runtime prep runs on server before rolling update (Bun: dependency install in release directory)
   - Perform rolling update
   - Release lock and clean up old releases (>30 days)

**Version naming:**

- Clean git tree: `{commit_hash}` (e.g., `abc1234`)
- Dirty working tree: `{commit_hash}_{content_hash}` (first 8 chars each)
- No git commit/repo: `nogit_{content_hash}` (first 8 chars)

**Source deploy contract:**

- Deploy archive source is the app's source bundle root (git root when available; otherwise app directory).
- Deploy target app path is `DIR` from CLI (`tako deploy [DIR]`) relative to the source bundle root.
- Source filtering uses `.gitignore`.
- These paths are always excluded from archive payload: `.git/`, `.tako/`, `.env*`, `node_modules/`, `target/`.
- Deploy always builds target-specific artifacts locally (Docker when preset build mode resolves to container, local host otherwise); servers receive prebuilt artifacts and do not run app build steps during deploy.
- For Bun runtime, `tako-server` runs dependency install for the release before starting/rolling instances.
- Build logic runs in fixed order per target: preset `[build].install`/`[build].build` stage first, then app `[[build.stages]]` from `tako.toml`.
- Runtime prep/start on server comes from preset top-level `install` and `start`.
- During container builds, deploy reuses target-scoped dependency cache volumes (mise and runtime-specific cache mounts such as Bun), keyed by cache kind, target label, and builder image.
- During local builds, deploy resolves runtime version by asking `mise` directly (`mise exec -- <tool> --version`) when available, then falls back to `mise.toml` and finally `latest`.
- Artifact include precedence: `build.include` -> `**/*`.
- Artifact exclude list: preset `[build].exclude` plus `build.exclude`.
- Asset roots are preset `[build].assets` plus app `build.assets` (deduplicated), merged into app `public/` after container build with ordered overwrite.
- Target artifacts are cached locally by deterministic key and reused across deploys when build inputs are unchanged.
- Cached artifacts are validated by checksum/size before reuse; invalid cache entries are rebuilt automatically.
- Artifact cache keys include runtime tool + resolved runtime version + Docker/local mode to avoid cross-target and cross-runtime cache contamination.
- Final `app.json` is written in the deployed app directory and contains runtime `main` used by `tako-server`.
- Deploy does not write a release `.env` file; runtime environment is provided through the `deploy` command payload and applied by `tako-server` when spawning instances.
- Deploy requires valid `[server_targets.<name>]` metadata for each selected server (`arch` and `libc`).
- Deploy does not probe server targets during deploy; missing/invalid target metadata fails deploy early with guidance to remove/re-add affected servers.
- Deploy pre-validation still fails when target environment is missing secret keys used by other environments.
- Deploy pre-validation warns (but does not fail) when target environment has extra secret keys not present in other secret environments.

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

Rolling update target counts use the configured `instances` value for the incoming build itself (not old+new combined counts).
When deploying with `instances = 0`, rolling deploy starts one warm instance for the new build so traffic is immediately served after deploy.

**On failure:** Automatic rollback - kill new instances, keep old ones running, return error to CLI.

**App start command (current):**

- tako-server currently only supports Bun apps.
- Release `app.json` is required for app startup.
- For `runtime = "bun"`, tako-server resolves `node_modules/tako.sh/src/wrapper.ts` by searching the app directory and its parent directories, then starts the app as `bun run <resolved-wrapper-path> <app.json.main>`.
  - If no wrapper file is found, warm-instance startup fails with an explicit missing-wrapper error.

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

### tako delete [--env {environment}] [--yes|-y] [DIR]

Delete a deployed app from environment servers.

Environment selection behavior:

- If `--env` is provided, that environment is used.
- If `--env` is omitted in an interactive terminal:
  - when running in a project (`tako.toml` present), Tako discovers deployed app/env pairs and prompts for an environment selector filtered to the current app.
  - when running outside a project, Tako discovers deployed app/env pairs across configured servers and prompts for a deployed app/environment target selector.
- If `--env` is omitted in non-interactive mode, project mode defaults to `production`; outside project mode it fails (no safe target selector).

Environment validation:

- In project mode, target environment must be declared in `tako.toml` (`[envs.<name>]`).
- Outside project mode, environments are discovered from deployed releases.
- `development` is reserved for `tako dev` and cannot be used with `tako delete`.

Delete confirmation:

- Interactive terminals: requires explicit confirmation unless `--yes` (or `-y`) is provided.
- Non-interactive terminals: requires `--yes`.

**Steps (per server):**

1. Connect over SSH.
2. Send `delete` to `tako-server` to remove runtime registration and routes for the app.
3. Remove `/opt/tako/apps/{app-name}` from disk.

Delete runs across target servers in parallel. If some servers fail while others succeed, all errors are reported and the command exits with failure.

Delete is idempotent for absent app runtime state (safe to re-run for cleanup).

**Delete target:**

- Project mode:
  - If `[servers.*]` sections in tako.toml map the target env → delete on those servers.
  - If deleting from `production` with no `[servers.*]` mapping:
    - exactly one server in `~/.tako/config.toml` `[[servers]]` → delete on that server.
  - If no servers exist in `~/.tako/config.toml` `[[servers]]` → fail with hint to run `tako servers add <host>`
  - Otherwise, require explicit `[servers.*]` mapping in tako.toml.
- Outside project mode:
  - Tako targets the subset of configured servers where the selected app/environment is currently deployed.

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
- `[envs.{env}]` accepts only route keys (`route`/`routes`); env vars belong in `[vars]` / `[vars.{env}]`
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
- Installer detects host target (`arch` + `libc`) and downloads matching artifact name `tako-server-linux-{arch}-{libc}` (supported: `x86_64`/`aarch64` with `glibc`/`musl`).
- Installer ensures `nc` (netcat) is available so CLI management commands can talk to `/var/run/tako/tako.sock`.
- Installer installs `mise` on the server (package-manager first; fallback to upstream installer when distro packages are unavailable).
- Installer attempts to grant `CAP_NET_BIND_SERVICE` to `/usr/local/bin/tako-server` via `setcap` so non-systemd/manual runs can still bind `:80/:443` as a non-root user (warns when `setcap` is unavailable or fails).
- Installer configures systemd with `KillMode=control-group` and `TimeoutStopSec=30min`, so restart/stop waits up to 30 minutes for graceful app shutdown across all service child processes before forced termination.
- When systemd is available, installer verifies `tako-server` is active after `enable --now`; if startup fails, installer exits non-zero and prints recent service logs.

Reference script in this repo: `scripts/install-tako-server.sh` (source for `/install-server`, alias `/server-install`).

**Default behavior (no configuration file needed):**

- HTTP: port 80
- HTTPS: port 443
- Data: `/opt/tako`
- Socket: `/var/run/tako/tako.sock`
- ACME: Production Let's Encrypt
- Renewal: Every 12 hours
- HTTP requests redirect to HTTPS (`307`, non-cacheable) by default.
- Exceptions: `/.well-known/acme-challenge/*` and internal `Host: tako.internal` + `/status` stay on HTTP.
- Forwarded requests for private/local hostnames (`localhost`, `*.localhost`, single-label hosts, and reserved suffixes like `*.local`) are treated as already HTTPS when proxy proto metadata is missing, so local forwarding setups do not enter redirect loops.
- No application path namespace is reserved at the edge proxy. Non-internal-host requests are routed to apps.

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

- HTTP and HTTPS listeners are created with `SO_REUSEPORT` so a temporary candidate process can bind the same external ports during single-host upgrade overlap.
- During `tako servers upgrade`, CLI uses a temporary management socket (`tako-<owner>.sock`) for candidate readiness checks.
- Candidate app instances use an instance port offset (`--instance-port-offset`) to avoid local app-port collisions with active instances.
- Primary service remains systemd-owned; the candidate is temporary and is stopped after primary service promotion.
- Restart/stop still honor graceful shutdown semantics from systemd (`KillMode=control-group`, `TimeoutStopSec=30min`).

### Directory Structure

```
/opt/tako/
├── server-config.toml
├── runtime-state.sqlite3
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
        │   └── logs -> /opt/tako/apps/{app-name}/shared/logs
        └── shared/
            └── logs/
```

## Communication Protocol

### Unix Sockets

**tako-server socket:**

- Primary path: `/var/run/tako/tako.sock`
- Temporary upgrade candidate path pattern: `/var/run/tako/tako-<owner>.sock`
- Used by: CLI for deploy/reload/delete/status/routes commands, apps for status/heartbeat

**App instance sockets:**

- Path: `/var/run/tako-app-{app-name}-{pid}.sock`
- Created by app on startup
- Used by: tako-server to proxy HTTP requests

### Environment Variables for Apps

| Name              | Used by         | Meaning                                                                    | Typical source                                                                                |
| ----------------- | --------------- | -------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------- |
| `PORT`            | app             | Listen port for HTTP server                                                | Set by `tako-server` when running app instances; set by `tako dev` for the local app process. |
| `TAKO_ENV`        | app             | Environment name                                                           | Set during deploy manifest generation (`production`, `staging`, etc.).                        |
| `NODE_ENV`        | app             | Node.js convention env                                                     | Set by runtime adapter / server (`development` or `production`).                              |
| `BUN_ENV`         | app             | Bun convention env                                                         | Set by runtime adapter (`development` or `production`).                                       |
| `TAKO_BUILD`      | app             | Deployed build/version identifier                                          | Included in deploy command payload and injected by `tako-server` at process spawn.            |
| `TAKO_APP_SOCKET` | app / `tako.sh` | Unix socket path the app should listen on (if using socket-based proxying) | path string or unset                                                                          |
| `TAKO_SOCKET`     | app / `tako.sh` | Unix socket path for connecting to `tako-server`                           | default `/var/run/tako/tako.sock`                                                             |
| `TAKO_VERSION`    | app / `tako.sh` | App version string (if you choose to set one)                              | string                                                                                        |
| `TAKO_INSTANCE`   | app / `tako.sh` | Instance identifier                                                        | integer string                                                                                |
| _user-defined_    | app             | User config vars/secrets                                                   | From `[vars]` and `[vars.*]` plus decrypted secrets in deploy command payload.                |

### Messages (JSON over Unix Socket)

**CLI → tako-server (management commands):**

- `hello` (capabilities / protocol negotiation; CLI sends this before other commands):

```json
{ "command": "hello", "protocol_version": 3 }
```

Response:

```json
{
  "status": "ok",
  "data": {
    "protocol_version": 3,
    "server_version": "0.1.0",
    "capabilities": [
      "deploy_instances_idle_timeout",
      "on_demand_cold_start",
      "idle_scale_to_zero",
      "upgrade_mode_control",
      "server_runtime_info"
    ]
  }
}
```

- `server_info` (returns runtime config + upgrade mode):

```json
{ "command": "server_info" }
```

- `enter_upgrading` / `exit_upgrading` (durable single-owner lock transitions):

```json
{ "command": "enter_upgrading", "owner": "upgrade-prod-..." }
```

```json
{ "command": "exit_upgrading", "owner": "upgrade-prod-..." }
```

- `deploy` (includes route patterns and launch env for routing/runtime):

```json
{
  "command": "deploy",
  "app": "my-app",
  "version": "1.0.0",
  "path": "/opt/tako/apps/my-app/releases/1.0.0",
  "routes": ["api.example.com", "*.example.com/admin/*"],
  "env": {
    "TAKO_ENV": "production",
    "TAKO_BUILD": "1.0.0",
    "NODE_ENV": "production"
  },
  "instances": 0,
  "idle_timeout": 300
}
```

- `routes` (returns app → routes mapping used for conflict detection/debugging):

```json
{ "command": "routes" }
```

- `delete` (remove runtime state/routes for an app):

```json
{ "command": "delete", "app": "my-app" }
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
- **Probe endpoint**: App's configured health check path (default: `/status`) with `Host: tako.internal`
- **Unhealthy threshold**: 2 consecutive failures → mark unhealthy, remove from load balancer
- **Dead threshold**: 5 consecutive failures → mark stopped, kill process
- **Recovery**: Single successful probe resets failure count and restores to healthy

#### Internal Status Endpoint

Tako-server exposes a host-gated status endpoint for health monitoring:

```
GET /status
Host: tako.internal
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

Requests that do not use internal host `tako.internal` are routed to apps normally.
The internal status endpoint is accessible on both HTTP and HTTPS, bypasses routing, and is not redirected.

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
- For private/local route hostnames (`localhost`, `*.localhost`, single-label hosts, and reserved suffixes such as `*.local`, `*.test`, `*.invalid`, `*.example`, `*.home.arpa`), Tako skips ACME and generates a self-signed certificate during deploy.
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

### Vite Plugin

```typescript
import { takoVitePlugin } from "tako.sh/vite";
```

- `tako.sh/vite` provides a plugin that prepares a deploy entry wrapper in Vite output.
- It emits `<outDir>/tako-entry.mjs`, which normalizes the compiled server module to a default-exported fetch handler.
- During `vite dev`, it adds `.tako.local` to `server.allowedHosts`.
- During `vite dev`, when `PORT` is set, it binds Vite to `127.0.0.1:$PORT` with `strictPort: true`.
- Deploy does not read Vite metadata files.
- To use the generated wrapper as deploy entry, set `main` in `tako.toml` to the generated file (for example `dist/server/tako-entry.mjs`) or define preset top-level `main`.

### Feature Overview

- Unix socket creation and management
- Ready signal on startup
- Heartbeat every 1 second
- Internal status endpoint (`Host: tako.internal` + `/status`)
- Graceful shutdown handling

### Optional Features

```typescript
Tako.onConfigReload((newSecrets) => {
  // Handle config changes (e.g., reconnect to database)
  database.reconnect(newSecrets.DATABASE_URL);
});
```

### Built-in Endpoints

**`GET /status` with `Host: tako.internal`**

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
