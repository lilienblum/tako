---
layout: ../../layouts/DocsLayout.astro
title: "tako.toml reference for routes, builds, secrets, and scaling - Tako Docs"
heading: "tako.toml Reference"
current: tako-toml
description: "Complete tako.toml reference covering routes, runtime settings, builds, secrets, scaling, environments, and deployment configuration."
---

# `tako.toml` Reference

`tako.toml` is Tako's project config file. It usually lives in your project root and tells Tako how to build, configure, and deploy your app. Run `tako init` to generate one with helpful comments and sensible defaults.

App-scoped commands use `./tako.toml` by default. If you pass `-c` / `--config <CONFIG>`, Tako uses that file instead and treats its parent directory as the project directory. If the path does not end with `.toml`, Tako appends it automatically, and omitting the suffix is the recommended shorthand. That lets you keep multiple config files in one folder when needed.

## Minimal Config

A working `tako.toml` only needs a route:

```toml
name = "my-app"
runtime = "bun"
runtime_version = "1.2.3"

[envs.production]
route = "my-app.example.com"
```

`tako init` prompts you for the app name, production route, and runtime, then writes a starter file with commented examples for every option. It also pins your locally-installed runtime version as `runtime_version`.

---

## Top-Level Fields

These fields sit at the root of `tako.toml`, outside any section.

### `name`

Optional but recommended. A stable identifier used in deploy paths and local dev hostnames.

```toml
name = "my-app"
```

If omitted, Tako falls back to a sanitized version of the selected config file's parent directory name. The remote server identity for each deployment is `{name}/{env}`, so the same app name can be deployed to multiple environments on one server. Renaming the app later creates a new identity on the server -- remove the old deployment manually if needed.

**Name rules:**

- Lowercase letters (`a-z`), numbers (`0-9`), and hyphens (`-`) only
- Must start with a lowercase letter
- Must be DNS-compatible (it becomes `{name}.test` in local dev)

Valid names: `my-app`, `api-server`, `web-frontend`

### `main`

Optional entrypoint override for your app. Written into the deployed `app.json` and used by both `tako dev` and `tako deploy`. Accepts file paths and module specifiers (e.g. `@scope/pkg`).

```toml
main = "server/index.mjs"
```

If omitted, Tako checks the manifest main field (e.g. `package.json` `main`) first, then falls back to the preset's top-level `main`. For JS runtimes (`bun`, `node`, `deno`), when the preset `main` is `index.<ext>` or `src/index.<ext>` (where `<ext>` is `ts`, `tsx`, `js`, or `jsx`), Tako resolves the entrypoint by checking for an existing `index.<ext>` first, then `src/index.<ext>`, then falls back to the preset value. For Go, the default `main` is `app` (the compiled binary name).

If neither `tako.toml`, manifest main, nor the preset provides a `main`, deploy and dev will fail with guidance.

During deploy, Tako verifies this resolved path exists in the post-build app directory and fails if it is missing.

### `dev`

Optional dev command override for `tako dev`. When set, this takes priority over preset and runtime defaults.

```toml
dev = ["vite", "dev"]
```

If omitted, `tako dev` checks the preset for a `dev` command, then falls back to the runtime default (JS runtimes use the SDK entrypoint with `main`, Go uses `go run .`).

### `runtime`

Optional runtime adapter override. Controls which base preset is used when `preset` is not set.

```toml
runtime = "bun"
```

Accepted values: `bun`, `node`, `deno`, `go`. When omitted, Tako auto-detects the runtime from your project files (e.g. `go.mod` for Go). If detection returns `unknown`, it defaults to `bun`.

### `runtime_version`

Optional pinned runtime version. When set, `tako deploy` uses this version directly instead of running `<runtime> --version` to auto-detect.

```toml
runtime_version = "1.2.3"
```

`tako init` pins the locally-installed version by default. To update, change the value or remove the field to let deploy auto-detect.

### `package_manager`

Optional package manager override. Controls which package manager Tako uses for dependency installation.

```toml
package_manager = "pnpm"
```

Accepted values: `npm`, `pnpm`, `yarn`, `bun`. When omitted, Tako auto-detects from your `package.json` `packageManager` field or lockfiles.

### `preset`

Optional app preset. Provides default `main` entrypoint and `assets` directories for framework-specific apps.

```toml
preset = "nextjs"
```

When omitted, Tako uses the base preset for the selected runtime (from `runtime` or auto-detection).

**Supported formats:**

- Runtime-local alias: `tanstack-start` or `nextjs` (resolved under the runtime set by `runtime`)
- Pinned alias: `tanstack-start@abc1234def` or `nextjs@abc1234def` (pinned to a specific commit hash)

**Not supported:**

- Namespaced aliases like `js/tanstack-start` -- set the runtime with `runtime` and keep `preset` runtime-local
- `github:` references

**How presets work:**

- Presets are metadata-only: they define `name`, `main`, and `assets` defaults. They do not contain build, install, start, or dev commands (though they can define a `dev` command override).
- Preset definitions live in `presets/<language>.toml` (e.g. `presets/javascript.toml`) in the Tako repo. Tako caches them locally; `tako dev` prefers cached or embedded manifests, while deploy refreshes unpinned aliases from GitHub and falls back to cache if refresh fails.
- Example built-in JS presets: `tanstack-start` (`vite dev`, `@tanstack/react-start/server-entry`) and `nextjs` (`next dev`, `.next/tako-entry.mjs`).

See [Presets](/docs/presets) for the full preset schema and available presets.

### `assets`

Optional top-level list of project-relative directories to merge into the deployed app's `public/` directory after build.

```toml
assets = ["dist/client", "assets/shared"]
```

Asset directories are merged in listed order. When files conflict, later entries overwrite earlier ones. These are combined with preset-defined assets (deduplicated).

---

## `[build]`

Deploy artifact build configuration. Defines how your app is built before deployment.

```toml
[build]
run = "bun run build"
install = "bun install"
```

### `run`

The build command to execute during deploy.

```toml
[build]
run = "vinxi build"
```

### `install`

Optional pre-build install command.

```toml
[build]
install = "bun install"
```

### `cwd`

Optional working directory for build commands, relative to the project root. Allows `..` for monorepo traversal (but must not escape the project root). Absolute paths are rejected.

```toml
[build]
cwd = "packages/app"
```

### `include`

Glob patterns controlling which files are included in the deploy artifact.

```toml
[build]
include = ["dist/**", ".output/**"]
```

Defaults to `**/*` when not set. These patterns are applied after the build completes.

Cannot be used alongside `[[build_stages]]`.

### `exclude`

Glob patterns for files to exclude from the deploy artifact.

```toml
[build]
exclude = ["**/*.map", "tests/**"]
```

Some paths are always excluded regardless of config: `.git/`, `.tako/`, `.env*`, and `node_modules/`. Additional exclusions come from `.gitignore`.

Cannot be used alongside `[[build_stages]]`.

---

## `[[build_stages]]`

Custom multi-stage build pipeline. Each stage is a top-level TOML array-of-tables entry. **Mutually exclusive with `[build]` when `[build]` has a `run` field** -- use one or the other. `[build].include` and `[build].exclude` also cannot be used alongside `[[build_stages]]`; use per-stage `exclude` instead.

```toml
[[build_stages]]
name = "frontend-assets"
cwd = "frontend"
install = "bun install"
run = "bun run build"

[[build_stages]]
name = "generate-api-types"
run = "bun run generate:types"
exclude = ["**/*.map"]
```

**Stage fields:**

| Field     | Required | Description                                                                                                       |
| --------- | -------- | ----------------------------------------------------------------------------------------------------------------- |
| `name`    | No       | Display label shown in deploy output                                                                              |
| `cwd`     | No       | Working directory relative to tako.toml location. Allows `..` for monorepo traversal but must not escape the root |
| `install` | No       | Command run before `run`                                                                                          |
| `run`     | Yes      | The build command to execute                                                                                      |
| `exclude` | No       | Array of file globs to exclude from the deploy artifact                                                           |

Stages run in declaration order. Each stage runs its `install` (if set) then `run`.

---

## `[vars]`

Global environment variables applied to every environment.

```toml
[vars]
API_BASE_URL = "https://api.example.com"
LOG_FORMAT = "json"
```

All values must be strings. These variables are included in the deploy payload and injected into your app's process environment.

## `[vars.<environment>]`

Per-environment variable overrides, merged on top of `[vars]`.

```toml
[vars.production]
API_BASE_URL = "https://api.example.com"
LOG_FORMAT = "json"

[vars.staging]
API_BASE_URL = "https://staging-api.example.com"
LOG_FORMAT = "text"

[vars.development]
API_BASE_URL = "http://localhost:3001"
```

Environment-specific values override base `[vars]` values when keys match.

### Variable Merge Order

For any target environment, variables are merged in this order (later overrides earlier):

1. **`[vars]`** -- base variables shared by all environments
2. **`[vars.<environment>]`** -- environment-specific overrides
3. **Auto-set by Tako** -- injected automatically at runtime:
   - `ENV=<environment>` (set in both dev and deploy)
   - `TAKO_BUILD=<version>` (deploy only)
   - `TAKO_DATA_DIR=<app data dir>` (set in both dev and deploy)
   - Runtime convention vars (e.g. `NODE_ENV` for all JS runtimes, `BUN_ENV` for Bun, `DENO_ENV` for Deno)

Since auto-set variables are applied last, they override any manually set values for those keys.

If you set `ENV` in `[vars]` or `[vars.<environment>]`, Tako ignores it and prints a warning. Log-level env vars like `LOG_LEVEL` are owned by you — set them in `[vars]` / `[vars.<environment>]` if you want them per environment.

---

## `[envs.<environment>]`

Environment sections declare routes, server membership, and runtime settings. Each environment you want to deploy to needs its own section.

```toml
[envs.production]
route = "api.example.com"
servers = ["la", "nyc"]
idle_timeout = 300

[envs.staging]
routes = ["staging.example.com", "www.staging.example.com"]
servers = ["staging"]
idle_timeout = 120
```

Environment sections accept only `route`/`routes`, `servers`, and `idle_timeout`. Put env vars — including log-level env vars like `LOG_LEVEL` — in `[vars]` / `[vars.<environment>]`.

### `route` / `routes`

Define which hostnames and paths route traffic to your app.

Use `route` for a single pattern:

```toml
[envs.production]
route = "api.example.com"
```

Use `routes` for multiple patterns:

```toml
[envs.production]
routes = ["api.example.com", "www.api.example.com"]
```

You can use `route` or `routes` in an environment, but not both.

**Route patterns:**

- Hostname only: `example.com`
- Hostname with path: `example.com/api/*`
- `example.com/api` and `example.com/api/` are equivalent (trailing slash is normalized)

**Route rules:**

- Every non-development environment must define `route` or `routes`
- Routes must include a hostname (path-only routes like `"/api/*"` are invalid)
- Empty route lists are rejected for non-development environments
- Development routes must use `.test` or `.tako.test` -- for example `{app}.test` or a subdomain of it
- `[envs.development]` may omit routes entirely, in which case `tako dev` uses the default `{app}.test`. If explicit dev routes are configured, only those routes are registered — the default `{app}.test` host is not added, leaving that slug available for other apps

### `servers`

A list of server names (previously added with `tako servers add`) that this environment deploys to.

```toml
[envs.production]
servers = ["la", "nyc"]
```

The same server name can appear in multiple non-development environments. Each environment maintains its own identity and files on the server under `/opt/tako/apps/{app}/{env}`.

Servers listed under `[envs.development]` are ignored by deploy validation -- development is for `tako dev` only.

### `idle_timeout`

Seconds before an idle instance is stopped. Applies per instance.

```toml
[envs.production]
idle_timeout = 300
```

Defaults to `300` (5 minutes). Instances are never stopped while serving in-flight requests.

### App log level

Tako does not own `LOG_LEVEL` or any other logging env var — set them yourself in `[vars]` / `[vars.<environment>]` if your logger reads them:

```toml
[vars.production]
LOG_LEVEL = "info"

[vars.development]
LOG_LEVEL = "debug"
```

Most logging libraries (pino, winston, tracing-subscriber, zap) read `LOG_LEVEL` directly from env. Tako's own `--verbose` flag controls only CLI and dev-server logs.

---

## App Name Resolution

Deploy, dev, logs, secrets, delete, and other app-scoped commands resolve the app name in this order:

1. Top-level `name` field in the selected config file (when set)
2. Sanitized parent directory name of the selected config file (fallback)

The remote deployment identity on servers is `{app}/{env}`. Setting `name` explicitly keeps the `{app}` segment stable. Changing the app identity (either by editing `name` or moving the config directory) creates a new app on the server -- remove the previous deployment manually if needed.

---

## Config File Selection

App-scoped commands honor `-c` / `--config`:

- **Default:** `./tako.toml`
- **Override:** `-c path/to/config` (recommended shorthand; `.toml` suffix is optional and appended automatically)
- **Project directory:** parent directory of the selected config file

Commands that support `-c/--config`: `init`, `dev`, `logs`, `deploy`, `releases`, `delete`, `secrets`, and `scale` (when using project context).

---

## Build and Deploy Behavior

- Build uses a `build_dir` approach: copies the project from the source root into `.tako/build_dir` (respecting `.gitignore`), symlinks `node_modules/` directories from the original tree, runs build commands, then archives the result without `node_modules/`.
- Source bundle root is the git root when available, otherwise the app directory.
- The app subdirectory is the selected config file's parent directory relative to the source bundle root.
- Deploy always force-excludes `.git/`, `.tako/`, `.env*`, and `node_modules/` from the deploy archive.
- After extracting the deploy artifact, `tako-server` runs the runtime plugin's production install command (e.g. `bun install --production`) before starting instances.
- When `runtime_version` is set, deploy uses it directly. Otherwise, deploy runs `<runtime> --version` to detect the version, falling back to `latest`.
- Built target artifacts are cached locally under `.tako/artifacts/` using a deterministic cache key. Cached artifacts are checksum/size-verified before reuse; invalid entries are rebuilt automatically.
- Deploy verifies the resolved runtime `main` file exists in the build workspace before packaging the artifact.

**Version naming:**

- Clean git tree: `{commit_hash}` (e.g. `abc1234`)
- Dirty working tree: `{commit_hash}_{content_hash}` (first 8 chars each)
- No git commit/repo: `nogit_{content_hash}` (first 8 chars)

---

## Instance Scaling Behavior

Instance counts are not configured in `tako.toml`. They are runtime state stored on each server and managed with `tako scale`.

- New deploys start with desired instances `0` on each server.
- `tako scale` changes the count, and that value persists across deploys, rollbacks, and server restarts.
- Desired instances `0` means scale-to-zero: instances start on demand and stop after `idle_timeout`. Deploy keeps one warm instance running so the app is immediately reachable after deploy.
- Desired instances `N` (N > 0): keep at least `N` instances running on that server.
- Instances are never stopped while serving in-flight requests.
- Explicit scale-down drains in-flight requests before stopping excess instances.

---

## Config Validation Rules

Tako validates your `tako.toml` and reports clear errors when something is wrong:

- **Top-level keys**: Only `name`, `main`, `runtime`, `runtime_version`, `package_manager`, `preset`, `dev`, `assets`, `[build]`, `[[build_stages]]`, `[vars]`, and `[envs]` are allowed at the top level.
- **Mutual exclusion**: `[build]` with a `run` field and `[[build_stages]]` cannot both be present. `[build].include`/`[build].exclude` also cannot be used alongside `[[build_stages]]`.
- **Environment sections**: `[envs.<env>]` accepts only `route`/`routes`, `servers`, and `idle_timeout`. Env vars belong in `[vars]` / `[vars.<env>]`.
- **Route exclusivity**: Each environment can set `route` or `routes`, but not both.
- **Non-development routes required**: Every non-development environment must have `route` or `routes` defined (empty lists are rejected).
- **Development route restrictions**: Must use `.test` or `.tako.test` -- for example `{app}.test` or a subdomain of it.
- **Route hostnames required**: Path-only routes (like `"/api/*"`) are invalid.
- **Build stage paths**: `cwd` allows `..` for monorepo traversal but must not escape the project root. Absolute paths are rejected.
- **Build stage run**: Each `[[build_stages]]` entry must have a `run` field.
- **Preset namespacing**: Namespaced aliases like `js/tanstack-start` in `preset` are rejected. Use `runtime` for the runtime and keep `preset` runtime-local.
- **App name format**: Must be DNS-compatible: lowercase letters, numbers, hyphens, starting with a letter.

---

## Full Annotated Example

```toml
# App identity (optional but recommended for stability)
name = "my-app"

# Entrypoint override (optional; preset provides a default)
main = "server/index.mjs"

# Runtime adapter (optional; auto-detected from project files)
runtime = "bun"

# Pinned runtime version (optional; auto-detected if omitted)
runtime_version = "1.2.3"

# Package manager (optional; auto-detected from package.json or lockfiles)
# package_manager = "pnpm"

# Build preset (optional; omit to use the base runtime preset)
# preset = "tanstack-start"
# preset = "tanstack-start@abc1234def"  # pinned to a commit

# Custom dev command override (optional; preset or runtime default used if omitted)
# dev = ["vite", "dev"]

# Directories merged into deployed public/ after build (optional)
# assets = ["dist/client", "assets/shared"]

# -- Build Configuration -----------------------------------------------
[build]
run = "bun run build"
install = "bun install"
# cwd = "packages/app"  # optional working directory

# Artifact include globs (default: all files)
# include = ["dist/**", ".output/**"]

# Artifact exclude globs
# exclude = ["**/*.map"]

# -- Or use multi-stage builds (mutually exclusive with [build].run) ----
# [[build_stages]]
# name = "frontend-assets"
# cwd = "frontend"
# install = "bun install"
# run = "bun run build"
# exclude = ["**/*.map"]

# [[build_stages]]
# name = "generate-api-types"
# run = "bun run generate:types"

# -- Global Variables ---------------------------------------------------
[vars]
LOG_FORMAT = "json"
API_BASE_URL = "https://api.example.com"

# -- Per-Environment Variables ------------------------------------------
[vars.production]
LOG_FORMAT = "json"

[vars.staging]
API_BASE_URL = "https://staging-api.example.com"
LOG_FORMAT = "text"

# -- Environments -------------------------------------------------------
[envs.production]
routes = ["api.example.com", "www.api.example.com"]
servers = ["primary", "secondary"]
idle_timeout = 300

[envs.staging]
route = "staging.example.com"
servers = ["staging"]
idle_timeout = 120

# Development environment (used by tako dev, not deployable)
[envs.development]
# route defaults to {name}.test
```

---

# Global Config Files

In addition to the per-project `tako.toml`, Tako uses a few global files stored outside your project.

## `config.toml` (Global User Config)

Global user-level settings and server inventory. Stored in the platform config directory (`~/Library/Application Support/tako/` on macOS, `~/.config/tako/` on Linux). This file is not part of your project.

```toml
[[servers]]
name = "la"
host = "1.2.3.4"
port = 22
description = "Primary production server"
arch = "x86_64"
libc = "glibc"

[[servers]]
name = "nyc"
host = "5.6.7.8"
arch = "aarch64"
libc = "musl"
```

### `[[servers]]` entries

Each server is a `[[servers]]` array-of-tables entry managed by `tako servers add`, `tako servers rm`, and `tako servers ls`.

| Field         | Required | Description                                                                  |
| ------------- | -------- | ---------------------------------------------------------------------------- |
| `name`        | Yes      | Unique name used in `[envs.<env>].servers` to target this server             |
| `host`        | Yes      | IP address or hostname                                                       |
| `port`        | No       | SSH port (defaults to `22`)                                                  |
| `description` | No       | Optional human-readable label shown in `tako servers ls`                     |
| `arch`        | Auto     | CPU architecture (`x86_64` or `aarch64`), detected during `tako servers add` |
| `libc`        | Auto     | C library (`glibc` or `musl`), detected during `tako servers add`            |

All names and hosts must be globally unique. The `arch` and `libc` fields are detected automatically via SSH when you add a server. Deploy requires valid target metadata for all selected servers; if metadata is missing or invalid, deploy fails early with guidance to remove and re-add the affected server.

**SSH authentication:**

- Tako authenticates using local SSH keys from `~/.ssh` (common filenames like `id_ed25519`, `id_rsa`, etc.).
- If a key file is passphrase-protected, Tako will prompt for the passphrase when running interactively (or you can provide `TAKO_SSH_KEY_PASSPHRASE`).
- If no suitable key files are found or usable, Tako falls back to `ssh-agent` via `SSH_AUTH_SOCK` (when available).

## `upgrade_channel.toml`

Stored in the platform config directory alongside `config.toml`. Contains the default upgrade channel (`stable` or `canary`).

- Explicit channel flags on `tako upgrade` (e.g. `--canary`, `--stable`) update this file.
- Without a channel flag, `tako upgrade` uses the channel saved here (default: `stable`).
- Upgrade commands print the active channel before execution.

You do not need to edit this file directly -- use `tako upgrade --canary` or `tako upgrade --stable`.

## `.tako/secrets.json` (Project - Encrypted)

Per-environment encrypted secrets stored in your project's `.tako/` directory. Each environment has a `salt` (base64-encoded Argon2id salt for key derivation) and a `secrets` map. Secret names are plaintext; values are encrypted with AES-256-GCM.

```json
{
  "production": {
    "salt": "base64_encoded_argon2id_salt",
    "secrets": {
      "DATABASE_URL": "encrypted_value",
      "API_KEY": "encrypted_value"
    }
  },
  "staging": {
    "salt": "base64_encoded_argon2id_salt",
    "secrets": {
      "DATABASE_URL": "encrypted_value_different"
    }
  }
}
```

Encryption uses environment-specific key files at `keys/{env}`. You can derive and share keys with `tako secrets key derive` and `tako secrets key export`.

`tako init` ensures your app's `.tako/` directory stays ignored while `.tako/secrets.json` remains trackable:

- Inside a git repo, it updates the repo root `.gitignore` with app-relative rules
- Outside a git repo, it creates or updates `.gitignore` in the app directory

Manage secrets with:

- `tako secrets set [--env <env>] <name>` -- set or update a secret
- `tako secrets rm [--env <env>] <name>` -- remove a secret
- `tako secrets ls` -- list all secrets across environments
- `tako secrets sync [--env <env>]` -- push local secrets to servers

## Workflows

Durable task engine config. Controls whether your app runs worker processes alongside HTTP instances.

```toml
[servers.workflows]           # default for every server in the env
workers = 1
concurrency = 10

[servers.lax.workflows]       # per-server override
workers = 2
```

Fields:

- **`workers`** — always-on worker processes per server. `0` = scale-to-zero (tako-server spawns the worker on enqueue/cron tick, worker exits after 5 minutes idle). Default `0`.
- **`concurrency`** — parallel task slots per worker. Default `10`.

Precedence: per-server (`[servers.<name>.workflows]`) > default (`[servers.workflows]`) > zero-config (`workers = 0`, `concurrency = 10`).

If your app has a `workflows/` directory (JS) but no `[servers.*.workflows]` block, you get scale-to-zero on every server automatically — no extra config needed.

The name `workflows` under `[servers]` is reserved and cannot be used as a server name.
