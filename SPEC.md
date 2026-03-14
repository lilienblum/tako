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

This ensures names work in DNS (`{app-name}.tako.test` by default), URLs, and environment variables.
`name` is optional in `tako.toml`. If omitted, Tako resolves app name from the project directory name.
Using top-level `name` is recommended for stability. Remote server identity is `{name}/{env}`, so the same app name can be deployed to multiple environments on one server. Renaming `name` later creates a new app identity/path; delete the old deployment manually.

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
TAKO_APP_LOG_LEVEL = "info"        # Base variables (all environments)

[vars.production]
API_URL = "https://api.example.com"

[vars.staging]
API_URL = "https://staging-api.example.com"

[envs.production]
route = "api.example.com"  # Single route, or use 'routes' for multiple
servers = ["la", "nyc"]
idle_timeout = 300         # Optional, default: 5 minutes

[envs.staging]
routes = [
  "staging.example.com",
  "www.staging.example.com",
  "example.com/api/*"
]
servers = ["staging"]
idle_timeout = 120

[envs.development]
log_level = "debug"        # App log level (default: "debug" for development, "info" for others)
```

**Environment `log_level`:**

Each `[envs.*]` block can set `log_level` to control the application's log verbosity: `debug`, `info`, `warn`, or `error`. Defaults: `debug` for `development`, `info` for all other environments. This is independent of `--verbose`, which controls only Tako CLI and dev-server logs. The resolved level is passed to the app as the `TAKO_APP_LOG_LEVEL` environment variable.

**Variable merging order (later overrides earlier):**

1. `[vars]` - base
2. `[vars.{environment}]` - environment-specific
3. Auto-set by Tako during deploy: `TAKO_ENV={environment}`, `TAKO_BUILD={version}`, plus runtime env vars (for Bun: `NODE_ENV`, `BUN_ENV`)

**Build/deploy behavior:**

- `name` in `tako.toml` is optional.
- App name resolution order for deploy/dev/logs/secrets/delete:
  1. top-level `name` (when set)
  2. sanitized project directory name fallback
- Remote deployment identity on servers is `{app}/{env}`. Set `name` explicitly to keep the `{app}` segment stable across deploys.
- Renaming app identity (`name` or directory fallback) is treated as a different app; remove the previous deployment manually if needed.
- `main` in `tako.toml` is an optional runtime entrypoint override written to deployed `app.json`.
- If `main` is omitted in `tako.toml`, deploy/dev use preset top-level `main` when present.
- For JS adapters (`bun`, `node`, `deno`), when preset `main` is `index.<ext>` or `src/index.<ext>` (`ext`: `ts`, `tsx`, `js`, `jsx`), deploy/dev resolve in this order: existing `index.<ext>`, then existing `src/index.<ext>`, then preset `main`.
- If neither `tako.toml main` nor preset `main` is set, deploy/dev fail with guidance.
- Top-level deploy/build keys in `tako.toml` are `main`, `runtime`, `preset`, and `[build]`; standalone top-level `dist` and `assets` keys are rejected.
- Top-level `runtime` is optional; when set to `bun`, `node`, or `deno`, it overrides adapter detection for default preset selection in `tako deploy`/`tako dev`.
- Server membership is declared per environment with `[envs.<name>].servers`.
- The same server name may be assigned to multiple non-development environments in one project. Each environment deploys to its own server-side app identity and filesystem path under `/opt/tako/apps/{app}/{env}`.
- `development` is for `tako dev`; `servers` declared there are ignored by deploy validation.
- Top-level `preset` is optional; when omitted, `tako deploy`/`tako dev` use adapter base preset from top-level `runtime` when set, otherwise detected adapter (`unknown` falls back to `bun`).
  - For `tako dev`, when top-level `preset` is omitted, Tako ignores preset top-level `dev` and runs a runtime-default command using resolved `main`:
    - Bun: `bun run node_modules/tako.sh/src/entrypoints/bun.ts {main}`
    - Node: `node --experimental-strip-types node_modules/tako.sh/src/entrypoints/node.ts {main}`
    - Deno: `deno run --allow-net --allow-env --allow-read node_modules/tako.sh/src/entrypoints/deno.ts {main}`
  - For `tako dev`, when top-level `preset` is explicitly set, Tako uses preset top-level `dev`.
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
- JS runtime base presets (`bun`, `node`, `deno`) set `[build].container = false`, so JS builds run locally by default unless a preset explicitly sets `container = true`.
- Preset `[build].exclude` entries are appended to runtime-base excludes (base-first, deduplicated).
- Preset `[build].targets` and `[build].container` override runtime defaults when set (including explicit empty arrays or explicit `container` values).
- Preset `[build].assets` override runtime-base `assets` when set.
- Build preset TOML supports optional top-level `name` (fallback: preset section name), top-level `main` (default app entrypoint), top-level lifecycle overrides (`dev`, `install`, `start`), and `[build]` (`assets`, `exclude`, optional `targets = ["linux-<arch>-<libc>", ...]`, optional `container = true|false`, optional `[build].install`, optional `[build].build`).
- Deploy resolves the preset source and writes `.tako/build.lock.json` (`preset_ref`, `repo`, `path`, `commit`) for visibility and cache-key inputs.
- Unpinned official preset aliases are fetched from the `master` branch on each resolve; if fetch fails, preset resolution fails.
- Runtime base aliases (`bun`, `node`, `deno`) fall back to embedded runtime defaults when their section is missing from a fetched family manifest.
- During `tako deploy`, source files are bundled from source root (`git` root when available, otherwise app directory).
- Source bundle filtering uses `.gitignore`.
- Deploy always excludes `.git/`, `.tako/`, `.env*`, `node_modules/`, and `target/`.
- Deploy sends merged app vars + runtime vars + decrypted secrets to `tako-server` in the `deploy` command payload; `tako-server` injects them directly into app process environment on spawn.
- Deploy build mode is controlled by preset `[build].container`:
  - `true`: build each target artifact in Docker.
  - `false`: run build commands on the local host workspace for each target.
  - when unset, default is `true` if `[build].targets` is non-empty, otherwise `false`.
  - built-in JS base presets set `container = false` explicitly.
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
- Default Docker builder images are target-libc specific: `ghcr.io/lilienblum/tako-builder-musl:v1` for `*-musl` targets and `ghcr.io/lilienblum/tako-builder-glibc:v1` for `*-glibc` targets.
- Runtime version resolution is mise-aware:
  - local builds try `mise exec -- <tool> --version` from app workspace when `mise` is installed.
  - local build stage commands run through `mise exec -- sh -lc ...` when `mise` is installed.
  - if local mise probing is unavailable/fails, deploy falls back to reading `mise.toml` (`[tools]` in app then workspace), then `latest`.
  - Docker target builds bootstrap `mise` in-container and probe with `mise exec -- <tool> --version`.
  - deploy writes the resolved runtime tool version into release `mise.toml` before packaging.
- Built target artifacts are cached locally under `.tako/artifacts/` using a deterministic cache key that includes source hash, target label, resolved preset source/commit, target build commands/image, app custom build stages, include/exclude patterns, asset roots, and app subdirectory.
- Cached artifacts are checksum/size verified before reuse; invalid cache entries are automatically discarded and rebuilt.
- After each target build (and asset merge), deploy verifies the resolved runtime `main` file exists in the build workspace before artifact packaging; missing files fail deploy with an explicit error.
- On every deploy, local artifact cache is pruned automatically (best-effort): keep 30 most recent source archives (`*-source.tar.zst`), keep 90 most recent target artifacts (`artifact-cache-*.tar.zst`), and remove orphan target metadata files.
- Artifact include patterns are resolved in this order:
  - `build.include` (if set)
  - fallback `**/*`
- Artifact exclude patterns are preset `[build].exclude` plus app `build.exclude`.
- For Bun deploys, default preset excludes `node_modules`; `tako-server` installs dependencies on server (`bun install --production`, plus `--frozen-lockfile` when Bun lockfile is present).
- Asset roots are preset `[build].assets` plus app `build.assets` (deduplicated), then merged into app `public/` after container build in listed order (later entries overwrite earlier ones).

**Instance behavior:**

- Desired instances are runtime app state stored on each server, not `tako.toml` config.
- New app deploys start with desired instances `0` on each server.
- `tako scale` changes the desired instance count per targeted server, and that value persists across server restarts, deploys, and rollbacks.
- Desired instances `0`: On-demand with scale-to-zero. Deploy keeps one warm instance running so the app is immediately reachable after deploy. Instances are stopped after idle timeout.
  - Once scaled to zero, the next request triggers a cold start and waits for readiness up to startup timeout (default 30 seconds). If no healthy instance is ready before timeout, proxy returns `504 App startup timed out`.
  - If cold start setup fails before readiness, proxy returns `502 App failed to start`.
  - While a cold start is already in progress, requests are queued up to 100 waiters per app (default). If the queue is full, proxy returns `503 App startup queue is full` with `Retry-After: 1`.
  - If warm-instance startup fails during deploy, deploy fails.
- Desired instances `N` (`N > 0`): keep at least `N` instances running on that server.
- `idle_timeout`: Applies per-instance (default 300s / 5 minutes)
- Instances are not stopped while serving in-flight requests.
- Explicit scale-down drains in-flight requests first, then stops excess instances.

### config.toml (Global User Config)

Global user-level settings and server inventory. Stored in the platform config directory (`~/Library/Application Support/tako/` on macOS, `~/.config/tako/` on Linux). NOT in the project.

```toml
[[servers]]
name = "la"
host = "1.2.3.4"
port = 22                 # Optional, defaults to 22
arch = "x86_64"
libc = "glibc"

[[servers]]
name = "nyc"
host = "5.6.7.8"
arch = "aarch64"
libc = "musl"
```

`[[servers]]` entries are managed by `tako servers add/rm/ls`. All names and hosts must be globally unique.
Detected server build target metadata is stored directly in each `[[servers]]` entry (`arch`, `libc`).

**SSH authentication:**

- `tako` authenticates using local SSH keys from `~/.ssh` (common filenames like `id_ed25519`, `id_rsa`, etc.).
- If a key file is passphrase-protected, `tako` will prompt for the passphrase when running interactively (or you can provide `TAKO_SSH_KEY_PASSPHRASE`).
- If no suitable key files are found or usable, `tako` falls back to `ssh-agent` via `SSH_AUTH_SOCK` (when available).

- `tako dev` uses a fixed local HTTPS listen port (`47831`).
- On macOS, `tako dev` uses automatic local forwarding so public URLs stay on default ports (`:443` for HTTPS, `:80` for HTTP redirect).

CLI prompt history is stored separately at `history.toml` (not in `config.toml`).

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

- Environment-specific keys: `keys/{env}`

`tako` can import/export these keys via `tako secrets key import` and `tako secrets key export`.

## Tako CLI Commands

### Installation and upgrades

Install the CLI on your local machine:

```bash
curl -fsSL https://tako.sh/install | sh
```

The hosted installer installs both `tako` and `tako-dev-server` binaries from the same channel/archive.

Install canary CLI artifacts directly:

```bash
curl -fsSL https://tako.sh/install-canary | sh
```

Install from crates.io:

```bash
cargo install tako
```

`cargo install tako` also installs both `tako` and `tako-dev-server` binaries from the same package/version.

Upgrade local CLI:

```bash
tako upgrade
tako upgrade --canary
tako upgrade --stable
```

`tako upgrade` upgrades only the local CLI installation.

Canary distribution model:

- `canary` is a single moving GitHub prerelease tag updated by CI on each `master` push.
- Canary CLI artifacts embed the source commit in `tako --version` output as `<semver>-canary-<sha7>`.
- Stable versioned releases remain maintainer-driven and are published from local release flows.

Upgrade channel state:

- Global config key `upgrade_channel` in `config.toml` stores the default upgrade channel (`stable` or `canary`).
- Explicit channel flags update this key.
- Upgrade commands print the active channel before execution (`You're on {channel} channel`).

### Global options

- `--version`: Print version and exit (`<semver>` on stable builds, `<semver>-canary-<sha7>` on canary builds).
- `-v, --verbose`: Show verbose output as an append-only execution transcript with timestamps and log levels.
- `--ci`: Deterministic non-interactive output (no colors, no spinners, no prompts). Can be combined with `--verbose`.

CLI output modes:

- **Normal mode** (default): Concise interactive UX with spinners, line replacement, and rich prompts.
- **Verbose mode** (`--verbose`): Append-only execution transcript. Each line: `HH:MM:SS LEVEL message`. Spinners degrade to log lines. Prompts render as transcript-style (still interactive). DEBUG-level messages are shown.
- **CI mode** (`--ci`): No ANSI colors, no spinners, no interactive prompts. If a required prompt value is missing, fails with an actionable error message suggesting CLI flags or config.
- **CI + Verbose** (`--ci --verbose`): Detailed append-only transcript with no colors.

All status/progress/log output goes to stderr. Only actual command results (URLs, machine-readable data) go to stdout.

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
  - `[envs.<env>]` route declarations (`route`/`routes`), server membership (`servers`), and idle scaling policy (`idle_timeout`)
- Includes a docs link to `https://tako.sh/docs/tako-toml`.
- Prompts for required app `name` (default from directory-derived app name).
- Prompts for required production route (`[envs.production].route`) with default `{name}.example.com`.
- Detects adapter (`bun`, `node`, `deno`, fallback `unknown`) and prompts for runtime selection unless `--runtime` is provided.
- In interactive mode, init fetches runtime-family preset names from official family manifest files (`presets/<family>.toml`) and shows `Fetching presets...` while loading.
- For built-in base adapters, init defaults to:
  - Bun: `bun`
  - Node: `node`
  - Deno: `deno`
- Init prints the full "Detected" summary block only in verbose mode; default output keeps setup concise and action-oriented.
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

### tako upgrade [--canary|--stable]

Upgrade the local `tako` CLI binary to the latest available release.

CLI upgrade strategy:

- Homebrew install detection: runs `brew upgrade tako`
- Cargo install detection (`~/.cargo/bin/tako`): runs `cargo install tako --locked`
- Default/fallback: downloads and runs hosted installer (`https://tako.sh/install`) via `curl`/`wget`
- `--canary`: always uses hosted installer mode and sets `TAKO_DOWNLOAD_BASE_URL=https://github.com/lilienblum/tako/releases/download/canary`
- `--stable`: forces stable channel and persists it as default
- Without channel flags, `tako upgrade` uses persisted `upgrade_channel` from global config (default: `stable`)

### tako dev [DIR]

Start (or attach to) a local development session for the current app, backed by a persistent dev daemon.

- `tako dev` is a **client**: it ensures `tako-dev-server` is running, then registers the current app directory with the daemon.
  - When running from a source checkout, `tako dev` prefers the repo-local `target/debug|release/tako-dev-server` binary.
  - If no local daemon binary exists, `tako dev` falls back to `tako-dev-server` on `PATH`.
  - If that fallback binary is missing:
    - source checkout flow reports a build hint (`cargo build -p tako --bin tako-dev-server`)
    - installed CLI flow reports a reinstall hint (`curl -fsSL https://tako.sh/install | sh`)
  - If daemon startup fails, `tako dev` reports the last lines from `{TAKO_HOME}/dev-server.log`.
  - `tako dev` waits up to ~15 seconds for the daemon socket after spawn before reporting startup failure.
  - The daemon performs an upfront bind-availability check for its HTTPS listen address and exits immediately with an explicit error when that address is unavailable.
- `tako dev` **registers** the app with the daemon (project directory is the unique key, state is persisted in SQLite at `{TAKO_HOME}/dev-server.db`).
- App statuses: `running` (actively serving), `idle` (process stopped, routes retained for wake-on-request), `stopped` (unregistered, routes removed).
- The app starts immediately when `tako dev` starts (1 local instance) and transitions to idle after 10 minutes of no attached CLI clients.
  - After an idle transition, the next HTTP request triggers wake-on-request: the daemon spawns the app process and routes the request once the app is healthy.
  - Idle shutdown is suppressed while there are in-flight requests.
  - When `Ctrl+c` is pressed, Tako unregisters the app (sets status to stopped, removes routes, kills the process).
  - Pressing `b` (background) hands the running process off to the daemon and exits the CLI. The daemon monitors the process and keeps routes active.
  - Running `tako dev` again from the same directory attaches to the existing session if the app is running or idle.
  - Dev logs are written to a shared per-app/per-project stream at `{TAKO_HOME}/dev/logs/{app}-{hash}.jsonl`.
  - Each persisted log record stores a single `timestamp` token (`hh:mm:ss`) instead of split hour/minute/second fields.
  - When a new owning session starts, Tako truncates that shared stream before writing fresh logs for the new session.
  - Attached clients replay the existing file contents, then follow new lines from the same stream.
  - App lifecycle state (`starting`, `running`, `stopped`, app PID, and startup errors) is persisted to the same shared stream, so attached sessions reconstruct the same status/CPU/RAM view as the owning session.
- The daemon supports **multiple concurrent apps** and maintains hostname-based routing for `*.tako.test`.
- Utility flags:
  - `tako doctor`: print a diagnostic report and exit.
    - Reports dev daemon listen info, local 80/443 forwarding status, and local DNS status.
    - On macOS, includes a preflight section with clear checks for:
      - pf redirect rule for `{loopback-address}:443 -> 127.0.0.1:<dev-port>`
      - pf redirect rule for `{loopback-address}:80 -> 127.0.0.1:<http-redirect-port>`
      - TCP reachability on `{loopback-address}:443` and `{loopback-address}:80`
    - If the local dev daemon is not running (missing/stale socket), doctor reports `status: not running` with a hint to start `tako dev`, and exits successfully.
- When running in an interactive terminal, `tako dev` prints a branded header (logo + version + app info) once at startup, then streams logs and status updates directly to stdout.
  - Native terminal features (scrollback, search, copy/paste, clickable links) are preserved — no alternate screen is used.
  - Log levels are `DEBUG`, `INFO`, `WARN`, `ERROR`, and `FATAL`; the level token is colorized using pastel colors (electric blue, green, yellow, red, and purple respectively).
  - The timestamp token (`hh:mm:ss`) is rendered in a muted color.
  - Log lines are prefixed as `hh:mm:ss LEVEL [scope] message`.
    - Common scopes: `tako` (local dev daemon) and `app` (the app process).
    - For app-process output, Tako infers the level from leading tokens like `DEBUG`, `INFO`, `WARN`/`WARNING`, `ERROR`, and `FATAL` (including bracketed forms such as `[DEBUG]`), and maps `TRACE` to `DEBUG`.
  - App lifecycle state changes (starting, stopped, errors) are printed inline as `── {status} ──` lines in the log stream.
  - Keyboard shortcuts (interactive terminal only):
    - `r` restart the app process
    - `b` background the app (hand off to daemon, CLI exits)
    - `Ctrl+c` stop the app and quit
  - When stdout is not a terminal (piped or redirected), `tako dev` falls back to plain `println`-style output with no color or raw mode.
  - `tako dev` always watches `tako.toml` and:
  - restarts the app when effective dev environment variables change
  - updates dev routing when `[envs.development].route(s)` changes
- Source hot-reload is runtime-driven (e.g. Bun watch/dev scripts); Tako does not watch source files for auto-restart.
- HTTPS is terminated by the local dev daemon using certificates issued by the local CA (SNI-based cert selection).
- `tako dev` ensures daemon TLS files exist at `{TAKO_HOME}/certs/fullchain.pem` and `{TAKO_HOME}/certs/privkey.pem` before spawning the daemon.
  - The daemon reuses existing TLS files when present.
- `tako dev` listens on `127.0.0.1:47831` in HTTPS mode.
- By default, Tako registers `https://{app}.tako.test:47831/` on non-macOS and `https://{app}.tako.test/` on macOS.
  - On macOS, Tako configures split DNS for `tako.test` by writing `/etc/resolver/tako.test` (one-time sudo), pointing to a local DNS listener on `127.0.0.1:53535`.
  - The dev daemon answers `A` queries for active `*.tako.test` hosts.
    - On macOS, it maps to a dedicated loopback address (`127.77.0.1`) used by pf forwarding.
    - On non-macOS, it maps to `127.0.0.1`.
  - On macOS, `tako dev` automatically tries to enable scoped local forwarding when missing (one-time sudo prompt):
    - `127.77.0.1:443 -> 127.0.0.1:47831`
    - `127.77.0.1:80 -> 127.0.0.1:47830` (HTTP redirect to HTTPS)
  - If forwarding later appears inactive, `tako dev` explains why it is re-requesting sudo before repair (missing pf rules, runtime forwarding reset after reboot/pf reset, or conflicting local listeners on `127.0.0.1:80/443`).
  - On macOS, Tako always requires this forwarding and always advertises `https://{app}.tako.test/` (no explicit port).
  - After applying or repairing local forwarding, Tako retries loopback 80/443 reachability and fails startup if those endpoints remain unreachable.
  - On macOS, Tako probes HTTPS for the app host via loopback and fails startup if that probe does not succeed.
  - If the daemon is reachable on `127.0.0.1:47831` but `https://{app}.tako.test/` still fails, Tako reports a targeted hint that local `:443` traffic is being intercepted/bypassed before reaching Tako.
  - `tako dev` uses routes from `[envs.development]` when configured; otherwise it defaults to `{app}.tako.test`.
    - Dev routes must be `{app}.tako.test` or a subdomain of it.
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
- Uses global server inventory from `config.toml`.
- If no servers are configured and the terminal is interactive, status offers to run the add-server wizard.
- If no deployed apps are found, status reports that explicitly.

### tako logs [--env {environment}]

Stream logs from all servers in an environment.

- Environment must exist in `tako.toml`.
- Streams from all mapped servers in parallel.
- Prefixes each line with `[server-name]` so multi-server output is readable.
- Runs until interrupted (`Ctrl+c`).

Logs flow helpers:

- For `production`, if no servers are configured and the terminal is interactive, logs offers to run the add-server wizard.

### tako servers add [host] [--name {name}] [--description {text}] [--port {port}]

Add server to global `config.toml` (`[[servers]]`).

- With `host`: adds directly from CLI args.
- With `host`: `--name` is required (no implicit default to hostname).
- Without `host` (interactive terminal): launches a guided wizard (host, required server name, optional description, SSH port) with a final `Looks good?` confirmation. Choosing `No` restarts the wizard.
- The add-server wizard supports `Tab` autocomplete suggestions for host/name/port from existing servers and persisted CLI history.
  - For name/port prompts, suggestions related to the selected host (and selected name for ports) are prioritized first, then global suggestions are shown.
- Successful adds record host/name/port history in `history.toml` for future autocomplete.
- `--description` stores optional human-readable metadata in `config.toml` (shown in `tako servers ls`).
- Re-running with the same name/host/port is idempotent (reports already configured and succeeds).

Tests SSH connection before adding. Connects as the `tako` user.

During SSH checks, `tako servers add` also detects and stores target metadata (`arch`, `libc`) in the matching `[[servers]]` entry in `config.toml`.

If `--no-test` is used, SSH checks and target detection are skipped; deploy later fails for that server until target metadata is captured by re-adding the server with SSH checks enabled.

If `tako-server` is not installed on the target, `tako` warns and expects the user to install it manually.

### tako servers rm [name]

Remove server from `config.toml` (`[[servers]]`).

When `name` is omitted in an interactive terminal, `tako` opens a server selector.
In non-interactive mode, `name` is required.

Confirms before removal. Warns that projects referencing this server will fail.

Aliases: `tako servers remove [name]`, `tako servers delete [name]`.

### tako servers ls

List all configured servers from global config (`config.toml`) as a table:

- Name
- Host
- Port
- Optional description

Alias: `tako servers list`.

If no servers are configured, `tako servers ls` shows a hint to run `tako servers add`.

### tako servers restart {server-name}

Restart `tako-server` process entirely (causes brief downtime for all apps).

Use for: binary updates, major configuration changes, system recovery.

Service-manager restart/stop behavior:

- On systemd hosts, installer configures `KillMode=control-group` and `TimeoutStopSec=30min`, allowing all app processes in the service cgroup time to handle graceful shutdown before forced termination.
- On OpenRC hosts, installer configures `retry="TERM/1800/KILL/5"` in the init script so restart/stop waits up to 30 minutes before forced termination.

`tako-server` persists app runtime registration (app config and routes) in SQLite under the data directory and restores it on startup so app routing/config survives process restarts and crashes. Env vars are stored in `app.json` in the release directory; secrets are stored in a per-app `secrets.json` file (0600 permissions) rather than in SQLite.

During single-host upgrade orchestration, `tako-server` may enter an internal `upgrading` server mode that temporarily rejects mutating management commands (`deploy`, `stop`, `delete`, `update-secrets`) until the upgrade window ends.
Upgrade mode transitions are guarded by a durable single-owner upgrade lock in SQLite so only one upgrade controller can hold the upgrade window at a time.

### tako servers upgrade {server-name} [--canary|--stable]

Single-host in-place upgrade via service-manager reload:

1. CLI verifies `tako-server` is active on the host.
2. CLI installs the new server binary on the host.
   - default: latest stable installer artifact
   - `--canary`: installer uses canary prerelease assets via `TAKO_DOWNLOAD_BASE_URL=https://github.com/lilienblum/tako/releases/download/canary`
   - `--stable`: installer uses stable artifacts and persists stable as default channel
   - without channel flags: uses persisted `upgrade_channel` from global config (default: `stable`)
3. CLI acquires the durable upgrade lock (`enter_upgrading`) and sets server mode to `upgrading`.
4. CLI signals the primary service with:
   - `systemctl reload tako-server` on systemd hosts, or
   - `rc-service tako-server reload` on OpenRC hosts.
     Both paths send SIGHUP for graceful in-place reload and run with root privileges (root login or sudo-capable user).
5. CLI waits for the primary management socket to report ready.
6. CLI releases upgrade mode (`exit_upgrading`).

`tako servers upgrade` requires a supported service manager on the host (systemd or OpenRC).

Failure behavior:

- If failure happens before the reload signal, CLI performs best-effort cleanup (exits upgrade mode).
- If the reload was sent but the socket did not become ready within the timeout, CLI warns that upgrade mode may remain enabled until the primary recovers.

### tako secrets set [--env {environment}] [--sync] {name}

Set/update secret for environment (defaults to production).

When running in an interactive terminal, prompts for value with masked input. In non-interactive mode, reads a single line from stdin. Stores encrypted value locally in `.tako/secrets`.

Uses the environment key at `keys/{env}` (creates it if missing).

When `--sync` is provided, immediately syncs secrets to all servers in the target environment after the local change, triggering a rolling restart of running instances.

### tako secrets rm [--env {environment}] [--sync] {name}

Remove secret from environment.

Removes from local `.tako/secrets`. Omitting `--env` removes the secret from all environments.

When `--sync` is provided, immediately syncs secrets to servers after the local change. If `--env` is specified, syncs to that environment; otherwise syncs to all environments.

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

For each target environment, sync decrypts with `keys/{env}`.

Shows a spinner with the total number of target servers while syncing, and reports the elapsed time on completion.

Sync flow helpers:

- If no servers are configured and the terminal is interactive, sync offers to run the add-server wizard.
- Environments with no mapped servers are skipped with a warning.
- Sync sends `update_secrets` to `tako-server`; it does not write remote `.env` files. App instances restart automatically when secrets are updated via `UpdateSecrets`.

### tako secrets key import [--env {environment}]

Import a base64 key from masked terminal input.

Writes `keys/{env}`.

When `--env` is omitted, `production` is used.

### tako secrets key export [--env {environment}]

Export a key to clipboard.

Reads `keys/{env}`.

When `--env` is omitted, `production` is used.

### tako deploy [--env {environment}] [--yes|-y] [DIR]

Build and deploy application to environment's servers.

When `--env` is omitted, deploy targets `production`.

Deploy target environment must be declared in `tako.toml` (`[envs.<name>]`) and must define `route` or `routes`.

`development` is reserved for `tako dev` and cannot be used with `tako deploy`.

In interactive terminals, deploying to `production` requires an explicit confirmation unless `--yes` (or `-y`) is provided.

Deploy flow helpers:

- If no servers are configured and the terminal is interactive, deploy offers to run the add-server wizard before continuing.
- For `production`, if `[envs.production].servers` is empty:
  - with one global server: deploy selects it and writes it to `[envs.production].servers` in `tako.toml`
  - with multiple global servers (interactive terminal): deploy asks you to pick one, then writes it to `[envs.production].servers`
- Interactive deploy progress:
  - single-server deploys show spinner loaders for long per-server steps (connect, lock, upload, extract, etc.) while pending
  - multi-server deploys keep line-based progress output to avoid overlapping spinners

**Steps:**

1. Pre-deployment validation (secrets present, server target metadata present/valid for all selected servers)
2. Resolve source bundle root (git root when available; otherwise app directory)
3. Resolve app subdirectory relative to source bundle root
4. Resolve deploy runtime `main` (`main` from `tako.toml`; otherwise preset top-level `main`, with JS index fallback order: `index.<ext>` then `src/index.<ext>` for `ts`/`tsx`/`js`/`jsx` when applicable)
5. Create source archive (`.tako/artifacts/{version}-source.tar.zst`) with canonical deployed `app.json` at app path inside archive
   - Version format: clean git tree => `{commit}`; dirty git tree => `{commit}_{source_hash8}`; no git commit => `nogit_{source_hash8}`
   - Best-effort local artifact cache prune runs before target builds (retention: 30 source archives, 90 target artifacts; orphan target metadata is removed).
6. Resolve build preset (top-level `preset` override or adapter base preset from top-level `runtime`/detection), fetching unpinned official aliases from `master` (fetch failure still fails resolution; runtime base aliases fall back to embedded defaults when missing from fetched family manifests), then persist resolved metadata in `.tako/build.lock.json`
7. Build target artifacts locally (one artifact per unique server target label):
   - Resolve deterministic cache key per target.
   - On cache hit, reuse existing verified target artifact.
   - On cache miss (or invalid cache entry), extract source archive into a temporary workspace.
   - Build in Docker when preset `[build].container = true`.
   - Build on local host workspace when `[build].container = false`.
   - When `container` is unset, default to Docker only when `[build].targets` is non-empty.
   - Run build commands in fixed order: preset stage first (`[build].install`, then `[build].build`), then app `[[build.stages]]` in declaration order.
   - For local builds, when `mise` is available, run stage commands through `mise exec -- sh -lc ...`.
   - Merge configured assets into app `public/`.
   - Verify resolved runtime `main` exists in the built app directory.
   - Materialize release `mise.toml` in app dir with resolved runtime tool version for server runtime parity.
   - Package filtered artifact tarball for that target using include/exclude rules and store it in local cache.
   - Per-target cache writes are serialized with a local lock to avoid duplicate concurrent builds.
8. Deploy to all servers in parallel:
   - Require `tako-server` to be pre-installed and running on each server
   - Acquire deploy lock (prevents concurrent deploys)
   - Upload and extract target-specific artifact
   - Query server for the app's current secrets hash; if it matches the local secrets hash, skip sending secrets (server keeps existing). If hashes differ (or app is new), include decrypted secrets in the deploy command.
   - Send deploy command with routes and optional secrets payload; `tako-server` reads non-secret runtime/app config from release `app.json` and injects runtime vars (`TAKO_BUILD`, `TAKO_ENV`) when spawning instances
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
- During container builds, deploy defaults to `ghcr.io/lilienblum/tako-builder-musl:v1` for `*-musl` targets and `ghcr.io/lilienblum/tako-builder-glibc:v1` for `*-glibc` targets.
- During local builds, deploy resolves runtime version by asking `mise` directly (`mise exec -- <tool> --version`) when available, then falls back to `mise.toml` and finally `latest`.
- During local builds, deploy runs stage commands through `mise exec -- sh -lc ...` when `mise` is available.
- Artifact include precedence: `build.include` -> `**/*`.
- Artifact exclude list: preset `[build].exclude` plus `build.exclude`.
- Asset roots are preset `[build].assets` plus app `build.assets` (deduplicated), merged into app `public/` after container build with ordered overwrite.
- Target artifacts are cached locally by deterministic key and reused across deploys when build inputs are unchanged.
- Cached artifacts are validated by checksum/size before reuse; invalid cache entries are rebuilt automatically.
- Artifact cache keys include runtime tool + resolved runtime version + Docker/local mode to avoid cross-target and cross-runtime cache contamination.
- Deploy artifacts include the canonical `app.json` used by `tako-server` at runtime.
- Release `app.json` contains resolved runtime metadata (`runtime`, `main`, optional `install`/`start`), non-secret env vars, environment idle timeout, and optional release metadata (`commit_message`, `git_dirty`) used by `tako releases ls`.
- Deploy does not write a release `.env` file; non-secret env vars live in release `app.json`, secrets live in per-app `secrets.json`, and `tako-server` injects runtime vars (`TAKO_BUILD`, `TAKO_ENV`) when spawning instances.
- Deploy queries each server's secrets hash before sending the deploy command. If the hash matches the local secrets, secrets are omitted from the payload and the server keeps its existing secrets. This avoids unnecessary secret transmission and ensures new servers or servers with stale secrets are automatically provisioned.
- Deploy requires valid `arch` and `libc` metadata in each selected `[[servers]]` entry.
- Deploy does not probe server targets during deploy; missing/invalid target metadata fails deploy early with guidance to remove/re-add affected servers.
- Deploy pre-validation still fails when target environment is missing secret keys used by other environments.
- Deploy pre-validation warns (but does not fail) when target environment has extra secret keys not present in other secret environments.

**Deploy lock (server-side):**

- CLI acquires lock on each server by creating `/opt/tako/apps/{app}/{env}/.deploy_lock` (atomic `mkdir` over SSH)
- Prevents concurrent deploys of the same app environment on the same server
- Lock is released at end of deploy (best-effort). If a deploy crashes mid-flight, the lock must be removed manually.

**Rolling update (per server):**

1. Start new instance
2. Wait for health check pass (30s timeout)
3. Add to load balancer
4. Gracefully stop old instance (drain connections, 30s timeout)
5. Repeat until all instances replaced
6. Update `current` symlink to the new release directory
7. Clean up releases older than 30 days

Rolling update target counts use the app's current desired instance count stored on that server (not old+new combined counts).
When the stored desired instance count is `0`, rolling deploy still starts one warm instance for the new build so traffic is immediately served after deploy.

**On failure:** Automatic rollback - kill new instances, keep old ones running, return error to CLI.

**App start command (current):**

- Release `app.json` is required for app startup.
- If release `app.json` includes non-empty `start`, tako-server uses that command (expanding `{main}` placeholders).
- If `start` is missing/empty, tako-server falls back by runtime, resolving the SDK entrypoint (`node_modules/tako.sh/src/entrypoints/{runtime}.ts`) from app dir or parent dirs:
  - `bun`: `bun run <resolved-entrypoint> <app.json.main>`
  - `node`: `node --experimental-strip-types <resolved-entrypoint> <app.json.main>`
  - `deno`: `deno run --allow-net --allow-env --allow-read <resolved-entrypoint> <app.json.main>`
  - if the entrypoint is missing, warm-instance startup fails with an explicit error
- Unknown runtime values in `app.json` are rejected with an explicit unsupported-runtime error.

**Partial failure:** If some servers fail while others succeed, deployment continues. Failures are reported at the end.

**Disk space preflight:** Before uploading artifacts, `tako deploy` checks free space under `/opt/tako` on each target server.

- Required free space is based on archive size plus unpack headroom.
- If free space is insufficient, deploy fails early with required vs available sizes.

**Failed deploy cleanup:** If a deploy fails after creating a new release directory, `tako deploy` automatically removes that newly-created partial release directory before returning an error.

**Deployment target:**

- If `[envs.<env>].servers` exists in `tako.toml` → deploy to those servers
- If deploying to `production` with no `[envs.production].servers` mapping:
  - exactly one server in `config.toml` `[[servers]]` → use it and persist it into `[envs.production].servers`
  - multiple servers in `config.toml` `[[servers]]` (interactive terminal) → prompt to select one and persist it into `[envs.production].servers`
- If no servers exist in `config.toml` `[[servers]]` → fail with hint to run `tako servers add <host>`
- Otherwise, require explicit `[envs.<env>].servers` mapping in tako.toml

### tako releases ls [--env {environment}]

List release/build history for the current app across mapped environment servers.

- Environment defaults to `production`.
- Environment must exist in `tako.toml` (`[envs.<name>]`).
- Server targeting follows `[envs.<name>].servers` for the selected environment.
- Output is release-centric and sorted newest-first:
  - line 1: release/build id + deployed timestamp
    - when deployed within 24 hours, append a muted relative hint in braces (for example `{3h ago}`)
  - line 2: commit message + cleanliness marker (`[clean]`, `[dirty]`, or `[unknown]`)
- `[current]` marks the release currently pointed to by server `current` symlink.
- Commit metadata (`commit_message`, `git_dirty`) comes from release `app.json` when available; older releases may show `[unknown]` or `(no commit message)`.

### tako releases rollback {release-id} [--env {environment}] [--yes|-y]

Roll back the current app/environment to a previously deployed release/build id.

- Environment defaults to `production`.
- In interactive terminals, rollback to `production` requires explicit confirmation unless `--yes` (or `-y`) is provided.
- Rollback is executed per mapped server in parallel.
- tako-server performs rollback by reusing current app routes/env/secrets/scaling config and switching runtime path/version to the target release, then running the standard rolling-update flow.
- Partial failures are reported per server; successful servers remain rolled back.

### tako scale {instances} [--env {environment}] [--server {server}] [--app {app}]

Change the desired instance count for a deployed app.

- `instances` is the desired instance count per targeted server.
- In a project directory, Tako resolves the app name from `tako.toml` (or directory fallback when top-level `name` is unset).
- In project context, app-scoped server commands target the remote deployment identity `{app}/{env}`.
- Outside a project directory, `--app` is required. Use `--app <app> --env <env>` or pass the full deployment id as `--app <app>/<env>`.
- When `--server` is omitted, `--env` is required and Tako scales every server listed in `[envs.<env>].servers`.
- When `--server` is provided, Tako scales only that server.
- In a project directory, `tako scale --server <server>` defaults to `production`.
- When both `--env` and `--server` are provided, the server must belong to that environment.
- Scale uses persisted runtime app state on the server, so the desired instance count survives deploys, rollbacks, and server restarts.
- Scaling to `0` drains and stops excess instances after in-flight requests finish (or drain timeout).

### tako delete [--env {environment}] [--server {server}] [--yes|-y] [DIR]

Delete a deployed app from one specific environment/server deployment target.

Target selection behavior:

- `tako delete` removes exactly one deployment target, not every server in an environment.
- In an interactive terminal, when Tako needs more information it first loads deployment state with a `Getting deployment information` spinner.
- In a project (`tako.toml` present), Tako resolves the app name from the project:
  - with neither `--env` nor `--server`, Tako prompts with deployed targets like `production from hkg`
  - with `--env` only, Tako prompts for a matching server
  - with `--server` only, Tako prompts for a matching environment
  - with both `--env` and `--server`, Tako skips discovery and goes straight to confirmation
- Outside a project, Tako discovers deployed targets across configured servers and includes app selection when needed because there is no local app context.
- In non-interactive mode, `--yes`, `--env`, and `--server` are all required. Outside a project, those flags must still identify a single deployed target; otherwise the command fails with guidance to rerun interactively from the app directory.

Validation:

- In project mode, `--env` must be declared in `tako.toml` (`[envs.<name>]`).
- `--server` must name a configured server from `config.toml` `[[servers]]`.
- `development` is reserved for `tako dev` and cannot be used with `tako delete`.

Delete confirmation:

- Interactive terminals require explicit confirmation unless `--yes` (or `-y`) is provided.
- The confirmation prompt always names the app, environment, and server being removed.
- Non-interactive terminals require `--yes`.

**Steps:**

1. Connect over SSH to the selected server.
2. Send `delete` to `tako-server` for the remote deployment id `{app-name}/{env-name}`.
3. Remove `/opt/tako/apps/{app-name}/{env-name}` from disk.

- Interactive single-target deletes show a spinner while the selected server is being cleaned up.
- Delete is idempotent for absent app runtime state (safe to re-run for cleanup).

## Routing and Multi-App Support

### Route Configuration

Apps specify routes at environment level (not per-server). Routes support:

- Exact hostname: `api.example.com`
- Wildcard subdomain: `*.api.example.com`
- Hostname + path: `api.example.com/api/*`
- Wildcard + path: `*.example.com/admin/*`

**Validation rules:**

- Routes must include hostname (path-only routes invalid: `"/api/*"` ❌)
- Exact path routes normalize trailing slash (`example.com/api` and `example.com/api/` are equivalent)
- Each `[envs.{env}]` can have either `route` or `routes`, not both
- `[envs.{env}]` accepts only route keys (`route`/`routes`); env vars belong in `[vars]` / `[vars.{env}]`
- Each non-development environment must define `route` or `routes`
- Empty route lists are invalid for non-development environments
- Development routes must be `{app-name}.tako.test` or a subdomain of it

### Multi-App Scenarios

**Apps with routes:**

- Each app specifies its routes
- Requests matched to most specific route (exact > wildcard, longer path > shorter)
- For static asset requests (paths with a file extension), `tako-server` serves files directly from the deployed app `public/` directory when present.
- For path-prefixed routes (for example `example.com/app/*`), static asset lookup also tries the prefix-stripped path (for example `/app/assets/main.js` -> `/assets/main.js`) so public assets work on subpaths.
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

1. Create dedicated OS users: `tako` for SSH access and running `tako-server` (plus `tako-app` for optional privileged process-separation setups)
2. Install `tako-server` to `/usr/local/bin/tako-server`
3. Install and enable a host service definition for `tako-server`:
   - systemd unit on systemd hosts
   - OpenRC init script on OpenRC hosts
4. Create and permissions required directories:
   - Data dir: `/opt/tako`
   - Socket dir: `/var/run/tako`

Recommended: run the hosted installer script on the server (as root):

```bash
sudo sh -c "$(curl -fsSL https://tako.sh/install-server)"
```

Installer SSH key behavior:

- If `TAKO_SSH_PUBKEY` is set, installer uses it and skips prompting.
- If unset and a terminal is available, installer prompts for a public key to authorize for user `tako` (including `sudo sh -c "$(curl ...)"` and common piped installs such as `curl ... | sudo sh`) and re-prompts on invalid input until a valid SSH public key line is provided.
- If terminal key input cannot be read, installer attempts to reuse the first valid key from the invoking `SUDO_USER` `~/.ssh/authorized_keys`; if unavailable, installer continues without key setup and prints a warning with a `TAKO_SSH_PUBKEY` rerun hint.
- If unset and no terminal is available, installer attempts the same invoking-user key fallback before warning and continuing without key setup.
- CLI SSH connections require host key verification against `~/.ssh/known_hosts` (or configured SSH keys directory); unknown/changed host keys are rejected.
- Installer detects host target (`arch` + `libc`) and downloads matching artifact name `tako-server-linux-{arch}-{libc}` (supported: `x86_64`/`aarch64` with `glibc`/`musl`).
- Installer ensures `nc` (netcat) is available so CLI management commands can talk to `/var/run/tako/tako.sock`.
- Installer installs `mise` on the server (package-manager first; fallback to upstream installer when distro packages are unavailable).
- Installer creates both `tako` and `tako-app` OS users.
- Installer installs restricted maintenance helpers and scoped sudoers policy so the `tako` SSH user can perform non-interactive server upgrade/reload operations.
- Installer supports systemd and OpenRC hosts.
- Installer supports install-refresh mode (`TAKO_RESTART_SERVICE=0`) for build/image workflows without active init; in this mode, it refreshes binary/users and skips service-definition install/start.
- Installer configures service capability support for privileged binds:
  - systemd: `AmbientCapabilities=CAP_NET_BIND_SERVICE`, `CapabilityBoundingSet=CAP_NET_BIND_SERVICE`
  - non-systemd hosts: installer applies `setcap cap_net_bind_service=+ep /usr/local/bin/tako-server` when available
- Installer configures graceful stop semantics:
  - systemd: `KillMode=control-group`, `TimeoutStopSec=30min`
  - OpenRC: `retry="TERM/1800/KILL/5"`
- Installer verifies `tako-server` is active after service start; if startup fails, installer exits non-zero and prints available service diagnostics.

Reference scripts in this repo:

- `scripts/install-tako-server.sh` (source for `/install-server`, alias `/server-install`)
- `scripts/install-tako-server-canary.sh` (source for `/install-server-canary`)
- `scripts/install-tako-cli-canary.sh` (source for `/install-canary`)

**Default behavior (no configuration file needed):**

- HTTP: port 80
- HTTPS: port 443
- Data: `/opt/tako`
- Socket: `/var/run/tako/tako.sock`
- ACME: Production Let's Encrypt
- Renewal: Every 12 hours
- HTTP requests redirect to HTTPS (`307`, non-cacheable) by default.
- Exception: `/.well-known/acme-challenge/*` stays on HTTP.
- Forwarded requests for private/local hostnames (`localhost`, `*.localhost`, single-label hosts, and reserved suffixes like `*.local`) are treated as already HTTPS when proxy proto metadata is missing, so local forwarding setups do not enter redirect loops.
- Upstream response caching is enabled at the edge proxy for `GET`/`HEAD` requests (websocket upgrades are excluded).
- Cache admission follows response headers (`Cache-Control` / `Expires`) with no implicit TTL defaults; responses without explicit cache directives are not stored.
- Cache key includes request host + URI so different route hosts are isolated.
- Proxy cache storage is in-memory with bounded LRU eviction (256 MiB total, 8 MiB per cached response body).
- No application path namespace is reserved at the edge proxy. Requests are routed strictly by configured routes.

**`/opt/tako/config.json`** — server-level configuration:

```json
{
  "server_name": "prod",
  "dns": {
    "provider": "cloudflare"
  }
}
```

- `server_name` — identity label for Prometheus metrics (defaults to hostname if absent).
- `dns.provider` — DNS provider for Let's Encrypt DNS-01 wildcard challenges (set via `tako servers dns`).
- Written by the installer (server name) and CLI (DNS config). Read by `tako-server` at startup.

### Zero-Downtime Operation

- `tako servers upgrade` performs an in-place upgrade via service-manager reload (`systemctl reload tako-server` on systemd, `rc-service tako-server reload` on OpenRC) with root privileges (root login or sudo-capable user), with no temporary candidate process or port overlap required.
- Management socket uses a symlink-based path: the active server creates a PID-specific socket (`tako-{pid}.sock`) and atomically updates the `tako.sock` symlink on ready, so clients always connect to the current process.
- Restart/stop still honor graceful shutdown semantics from the host service manager (systemd or OpenRC as described above).

### Directory Structure

```
/opt/tako/
├── config.json
├── runtime-state.sqlite3
├── acme/
│   └── credentials.json
├── certs/
│   ├── {domain}/
│   │   ├── fullchain.pem
│   │   └── privkey.pem
└── apps/
    └── {app-name}/
        └── {env-name}/
            ├── current -> releases/{version}
            ├── .deploy_lock/
            ├── releases/{version}/
            │   ├── build files...
            │   └── logs -> /opt/tako/apps/{app-name}/{env-name}/shared/logs
            └── shared/
                └── logs/
```

## Communication Protocol

### Unix Sockets

**tako-server socket:**

- Symlink path: `/var/run/tako/tako.sock` (always points to the active server socket)
- PID-specific socket path: `/var/run/tako/tako-{pid}.sock` (created by active server; symlink updated atomically on ready)
- Used by: CLI for deploy/delete/status/routes commands, apps for status/heartbeat

**App instance sockets:**

- Path: `/var/run/tako-app-{app-name}-{pid}.sock`
- Created by app on startup
- Used by: tako-server to proxy HTTP requests
- Required on Unix deploys: instances must expose health/status and request traffic on this socket.

### Environment Variables for Apps

| Name              | Used by         | Meaning                                       | Typical source                                                                            |
| ----------------- | --------------- | --------------------------------------------- | ----------------------------------------------------------------------------------------- |
| `PORT`            | app             | Listen port for HTTP server                   | Set by `tako dev` for the local app process.                                              |
| `TAKO_ENV`        | app             | Environment name                              | Set during deploy manifest generation (`production`, `staging`, etc.).                    |
| `NODE_ENV`        | app             | Node.js convention env                        | Set by runtime adapter / server (`development` or `production`).                          |
| `BUN_ENV`         | app             | Bun convention env                            | Set by runtime adapter (`development` or `production`).                                   |
| `TAKO_BUILD`      | app             | Deployed build/version identifier             | Included in deploy command payload and injected by `tako-server` at process spawn.        |
| `TAKO_APP_SOCKET` | app / `tako.sh` | Unix socket path the app should listen on     | path string on Unix deploys (with `{pid}` token); unset in local dev                      |
| `TAKO_VERSION`    | app / `tako.sh` | App version string (if you choose to set one) | string                                                                                    |
| `TAKO_INSTANCE`   | app / `tako.sh` | Instance identifier                           | integer string                                                                            |
| _user-defined_    | app             | User config vars/secrets                      | From `app.json` in the release dir (env vars) and per-app `secrets.json` (0600, secrets). |

### Messages (JSON over Unix Socket)

**CLI → tako-server (management commands):**

- `hello` (capabilities / protocol negotiation; CLI sends this before other commands):

```json
{ "command": "hello", "protocol_version": 0 }
```

Response:

```json
{
  "status": "ok",
  "data": {
    "protocol_version": 0,
    "server_version": "0.1.0",
    "capabilities": [
      "on_demand_cold_start",
      "idle_scale_to_zero",
      "scale",
      "upgrade_mode_control",
      "server_runtime_info",
      "release_history",
      "rollback"
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

- `deploy` (includes route patterns and optional secrets payload; env vars are read from `app.json` in the release dir). When `secrets` is omitted or `null`, the server keeps existing secrets for the app:

```json
{
  "command": "deploy",
  "app": "my-app/production",
  "version": "1.0.0",
  "path": "/opt/tako/apps/my-app/production/releases/1.0.0",
  "routes": ["api.example.com", "*.example.com/admin/*"],
  "secrets": {
    "DATABASE_URL": "...",
    "API_KEY": "..."
  }
}
```

- `scale` (updates the desired instance count for an app on one server):

```json
{ "command": "scale", "app": "my-app/production", "instances": 3 }
```

- `get_secrets_hash` (returns the SHA-256 hash of an app's current secrets; used by deploy to skip sending secrets when unchanged):

```json
{ "command": "get_secrets_hash", "app": "my-app/production" }
```

Server-side validation on `deploy` and app-scoped commands:

- `app` is the deployment id used on the server. CLI app-scoped commands send `{app}/{env}`. Each segment must be normalized (`[a-z][a-z0-9-]{0,62}` with no trailing `-`).
- `version` must be a simple release id (letters/digits/`.-_`, no path separators).
- `path` must resolve under `<data-dir>/apps/<app>/releases/`.

- `routes` (returns app → routes mapping used for conflict detection/debugging):

```json
{ "command": "routes" }
```

- `list_releases` (returns release/build history for an app):

```json
{ "command": "list_releases", "app": "my-app" }
```

- `rollback` (roll back an app to a previous release/build id):

```json
{ "command": "rollback", "app": "my-app", "version": "abc1234" }
```

- `delete` (remove runtime state/routes for an app):

```json
{ "command": "delete", "app": "my-app" }
```

**Instance communication model:**

- App processes do not connect to the management socket.
- `tako-server` controls lifecycle directly (spawn/stop/rolling update) and determines readiness/health via active HTTP probing only.
- On Unix deploys, app processes receive `TAKO_APP_SOCKET` and must bind that per-instance Unix socket for request traffic.
- Secret updates are applied by writing per-app `secrets.json` and rolling restart; there is no app-facing `reload_config` protocol message.

### Health Checks

Active HTTP probing is the source of truth for instance health:

- **Probe interval**: 1 second by default (configurable)
- **Probe endpoint**: App's configured health check path (default: `/status`) with `Host: tako`
- **Transport**: On Unix deploys, probes use the instance Unix socket path from `TAKO_APP_SOCKET` (no TCP fallback).
- **Unhealthy threshold**: 2 consecutive failures → mark unhealthy, remove from load balancer
- **Dead threshold**: 5 consecutive failures → mark stopped, kill process
- **Recovery**: Single successful probe resets failure count and restores to healthy

#### Internal Probe Contract

Tako-server performs health checks against the deployed app process:

```
GET /status
Host: tako
```

Expected response:

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

The SDK wrappers implement this endpoint automatically. The edge proxy does not reserve or bypass `Host: tako` routes.

### Prometheus Metrics

Tako-server exposes a Prometheus-compatible metrics endpoint for observability.

**Endpoint:** `http://127.0.0.1:9898/` (localhost only, not publicly accessible)

**CLI flag:** `--metrics-port <port>` (default: 9898, set to 0 to disable)

**Exposed metrics:**

| Metric | Type | Labels | Description |
|---|---|---|---|
| `tako_http_requests_total` | Counter | `server`, `app`, `status` | Total proxied requests, grouped by status class (2xx/3xx/4xx/5xx) |
| `tako_http_request_duration_seconds` | Histogram | `server`, `app` | Request latency distribution |
| `tako_http_active_connections` | Gauge | `server`, `app` | Currently active connections |
| `tako_cold_starts_total` | Counter | `server`, `app` | Total cold starts triggered (scale-to-zero apps) |
| `tako_cold_start_duration_seconds` | Histogram | `server`, `app` | Cold start duration distribution |
| `tako_instance_health` | Gauge | `server`, `app`, `instance` | Instance health status (1=healthy, 0=unhealthy) |
| `tako_instances_running` | Gauge | `server`, `app` | Number of running instances |

All metrics carry a `server` label (machine hostname) so multi-server deployments are distinguishable without scraper-side relabeling. A single scrape returns data for all deployed apps on that server.

Only proxied requests (routed to a backend) are measured. ACME challenges, direct static asset responses, and unmatched-host 404s are excluded.

**Usage with monitoring platforms:**

- **Self-hosted Prometheus/Grafana**: Add `127.0.0.1:9898` as a scrape target.
- **Hosted platforms (Grafana Cloud, Datadog, etc.)**: Install the platform's agent on the server, configure it to scrape `http://127.0.0.1:9898/metrics`.
- **Tailscale/WireGuard**: Expose port 9898 on the private network interface for remote scraping.

The endpoint uses Pingora's built-in Prometheus server with gzip compression.

## TLS/SSL Certificates

### SNI-Based Certificate Selection

Tako-server uses SNI (Server Name Indication) to select the appropriate certificate during TLS handshake:

1. Client connects and sends SNI hostname
2. Server looks up certificate for that hostname in CertManager
3. If exact match found, use that certificate
4. If no exact match, try wildcard fallback (e.g., `api.example.com` → `*.example.com`)
5. If still no match, serve fallback default certificate so HTTPS can complete and routing can return normal HTTP status codes (for example `404` for unknown routes/hosts)

This requires OpenSSL (not rustls) for callback support.

### Automatic Management

- ACME protocol (Let's Encrypt)
- Automatic issuance for domains in app routes
- For private/local route hostnames (`localhost`, `*.localhost`, single-label hosts, and reserved suffixes such as `*.local`, `*.test`, `*.invalid`, `*.example`, `*.home.arpa`), Tako skips ACME and generates a self-signed certificate during deploy.
- If no certificate exists yet for an SNI hostname, Tako serves a fallback self-signed default certificate so TLS handshakes still complete.
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

Pass `--acme-staging` to `tako-server` to use Let's Encrypt staging:

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
export default function fetch(request: Request): Response | Promise<Response> {
  return new Response("Hello!");
}
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
import { tako } from "tako.sh/vite";
```

- `tako.sh/vite` provides a plugin that prepares a deploy entry wrapper in Vite output.
- It emits `<outDir>/tako-entry.mjs`, which normalizes the compiled server module to a default-exported fetch handler.
- During `vite dev`, it adds `.tako.test` to `server.allowedHosts`.
- During `vite dev`, when `PORT` is set, it binds Vite to `127.0.0.1:$PORT` with `strictPort: true`.
- Deploy does not read Vite metadata files.
- To use the generated wrapper as deploy entry, set `main` in `tako.toml` to the generated file (for example `dist/server/tako-entry.mjs`) or define preset top-level `main`.

### Feature Overview

- Fetch handler adapters for Bun/Node/Deno runtimes
- Unix socket serving for deployed Unix apps via `TAKO_APP_SOCKET`; `tako dev` remains TCP (`PORT`)
- Internal status endpoint (`Host: tako` + `/status`)
- Graceful shutdown handling

### Built-in Endpoints

**`GET /status` with `Host: tako`**

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
| Config/data directory deleted       | Auto-recreate on next command                                              |
| `config.toml` corrupted    | Show parse error with line number, offer to recreate                       |
| `tako.toml` deleted                | Commands that require project config fail with guidance to run `tako init` |
| `.tako/` deleted                   | Auto-recreate on next deploy                                               |
| `.tako/secrets` deleted            | Warn user, prompt to restore secrets                                       |
| Low free space under `/opt/tako`   | Deploy fails before upload with required vs available disk sizes           |
| Deploy lock left behind            | Deploy fails until `/opt/tako/apps/{app}/{env}/.deploy_lock` is removed    |
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
