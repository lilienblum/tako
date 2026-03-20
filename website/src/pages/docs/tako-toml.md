---
layout: ../../layouts/DocsLayout.astro
title: "Tako Docs - tako.toml Reference"
heading: "tako.toml Reference"
current: tako-toml
---

# `tako.toml` Reference

`tako.toml` is Tako's default project config file. It usually lives in your project root and tells Tako how to build, configure, and deploy your app. Run `tako init` to generate one with helpful comments and sensible defaults.

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
- Must be DNS-compatible (it becomes `{name}.tako.test` in local dev)

Valid names: `my-app`, `api-server`, `web-frontend`

### `main`

Optional entrypoint override for your app. Written into the deployed `app.json` and used by both `tako dev` and `tako deploy`. Accepts file paths and module specifiers (e.g. `@scope/pkg`).

```toml
main = "server/index.mjs"
```

If omitted, Tako checks the manifest main field (e.g. `package.json` `main`) first, then falls back to the preset's top-level `main`. For JS runtimes (`bun`, `node`, `deno`), when the preset `main` is `index.<ext>` or `src/index.<ext>` (where `<ext>` is `ts`, `tsx`, `js`, or `jsx`), Tako resolves the entrypoint by checking for an existing `index.<ext>` first, then `src/index.<ext>`, then falls back to the preset value.

If neither `tako.toml`, manifest main, nor the preset provides a `main`, deploy and dev will fail with guidance.

During deploy, Tako verifies this resolved path exists in the post-build app directory and fails if it is missing.

### `runtime`

Optional runtime adapter override. Controls which base preset is used when `preset` is not set.

```toml
runtime = "bun"
```

Accepted values: `bun`, `node`, `deno`. When omitted, Tako auto-detects the runtime from your project files. If detection returns `unknown`, it defaults to `bun`.

### `runtime_version`

Optional pinned runtime version. When set, `tako deploy` uses this version directly instead of running `<runtime> --version` to auto-detect.

```toml
runtime_version = "1.2.3"
```

`tako init` pins the locally-installed version by default. To update, change the value or remove the field to let deploy auto-detect.

### `preset`

Optional app preset. Provides default `main` entrypoint and `assets` directories for framework-specific apps.

```toml
preset = "tanstack-start"
```

When omitted, Tako uses the base preset for the selected runtime (from `runtime` or auto-detection).

**Supported formats:**

- Runtime-local alias: `tanstack-start` (resolved under the runtime set by `runtime`)
- Pinned alias: `tanstack-start@abc1234def` (pinned to a specific commit hash)

**Not supported:**

- Namespaced aliases like `js/tanstack-start` -- set the runtime with `runtime` and keep `preset` runtime-local
- `github:` references

**How presets work:**

- Presets are metadata-only: they define `name`, `main`, and `assets` defaults. They do not contain build, install, start, or dev commands.
- Base presets (`bun`, `node`, `deno`) are built into the CLI from embedded runtime definitions.
- Family presets (like `tanstack-start`) live in `presets/<language>/<language>.toml` in the Tako repo and are fetched from `master` on each resolve. Fetch failures fail the resolve.
- Base runtime aliases (`bun`, `node`, `deno`) fall back to embedded defaults when missing from the fetched family manifest.
- Resolved preset metadata is written to `.tako/build.lock.json` for visibility and cache-key inputs.

**Preset effect on `tako dev`:**

- When `preset` is omitted, Tako runs the runtime-default dev script:
  - Bun: `bun run dev`
  - Node: `npm run dev`
  - Deno: `deno task dev`
- When `preset` is explicitly set, the same runtime-default dev scripts are used (presets do not define dev commands).

See [Presets](/docs/presets) for the full preset schema and available presets.

### `assets`

Optional top-level list of project-relative directories to merge into the deployed app's `public/` directory after build.

```toml
assets = ["dist/client", "assets/shared"]
```

Asset directories are merged in listed order. When files conflict, later entries overwrite earlier ones. These are combined with preset-defined assets (deduplicated).

### `package_manager`

Optional package manager override. Controls which package manager Tako uses for dependency installation.

```toml
package_manager = "pnpm"
```

When omitted, Tako auto-detects the package manager from your `package.json` `packageManager` field or lockfiles.

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

Optional working directory for build commands, relative to the project root. Allows `..` for monorepo traversal (but must not escape the project root).

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

### `exclude`

Glob patterns for files to exclude from the deploy artifact.

```toml
[build]
exclude = ["**/*.map", "tests/**"]
```

Some paths are always excluded regardless of config: `.git/`, `.tako/`, and `.env*`. Additional exclusions (like `node_modules/`) come from `.gitignore`.

---

## `[[build_stages]]`

Custom multi-stage build pipeline. Each stage is a top-level TOML array-of-tables entry. **Mutually exclusive with `[build]` when `[build]` has a `run` field** -- use one or the other.

```toml
[[build_stages]]
name = "frontend-assets"
cwd = "frontend"
install = "bun install"
run = "bun run build"

[[build_stages]]
name = "generate-api-types"
run = "bun run generate:types"
```

**Stage fields:**

| Field     | Required | Description                                                                                      |
| --------- | -------- | ------------------------------------------------------------------------------------------------ |
| `name`    | No       | Display label shown in deploy output                                                             |
| `cwd`     | No       | Working directory relative to tako.toml location. Allows `..` for monorepo traversal.            |
| `install` | No       | Command run before `run`                                                                         |
| `run`     | Yes      | The build command to execute                                                                     |

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
3. **Auto-set by Tako** -- injected automatically during deploy:
   - `TAKO_ENV=<environment>`
   - `TAKO_BUILD=<version>`
   - Runtime convention vars (for Bun/Node: `NODE_ENV`, `BUN_ENV`)

Since auto-set variables are applied last, they override any manually set values for those keys.

---

## `[envs.<environment>]`

Environment sections declare routes, server membership, and runtime settings. Each environment you want to deploy to needs its own section.

```toml
[envs.production]
route = "api.example.com"
servers = ["la", "nyc"]
idle_timeout = 300
log_level = "info"

[envs.staging]
routes = ["staging.example.com", "www.staging.example.com"]
servers = ["staging"]
idle_timeout = 120
log_level = "debug"

[envs.development]
log_level = "debug"
```

Environment sections accept only `route`/`routes`, `servers`, `idle_timeout`, and `log_level`. Put env vars in `[vars]` / `[vars.<environment>]`.

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
- `example.com` and `example.com/*` are equivalent (both match all paths)
- `example.com/api` and `example.com/api/` are equivalent (trailing slash is normalized)
- `example.com/api/*` matches `/api`, `/api/`, and `/api/anything/else`

**Route rules:**

- Every non-development environment must define `route` or `routes`
- Routes must include a hostname (path-only routes like `"/api/*"` are invalid)
- Empty route lists are rejected for non-development environments
- Development routes must be `{app}.tako.test` or a subdomain of it
- `[envs.development]` may omit routes entirely and defaults to `{app}.tako.test` for `tako dev`

**TLS behavior:**

- Public route hostnames get automatic ACME (Let's Encrypt) certificates
- Private/local hostnames (`localhost`, `*.localhost`, single-label hosts, reserved suffixes like `*.local`) get self-signed certificates generated during deploy

**Static assets:**

- Requests for paths with file extensions are served from the deployed `public/` directory when present
- For path-prefixed routes (like `example.com/app/*`), static lookup also tries the prefix-stripped path

### `servers`

A list of server names (previously added with `tako servers add`) that this environment deploys to.

```toml
[envs.production]
servers = ["la", "nyc"]
```

The same server name can appear in multiple non-development environments. Each environment maintains its own identity and files on the server under `/opt/tako/apps/{app}/{env}`.

Servers listed under `[envs.development]` are ignored by deploy validation -- development is for `tako dev` only.

Server architecture (`arch`, `libc`) is not configured in `tako.toml`. Tako detects and stores this metadata in each server's entry in the global config (`config.toml`) when you run `tako servers add`.

### `idle_timeout`

Seconds before an idle instance is stopped. Applies per instance.

```toml
[envs.production]
idle_timeout = 300
```

Defaults to `300` (5 minutes). Instances are never stopped while serving in-flight requests.

When desired instances is `0` (scale-to-zero), instances start on demand when a request arrives and stop after this timeout with no traffic.

### `log_level`

App log verbosity for this environment. Passed to your app as the `TAKO_APP_LOG_LEVEL` environment variable.

```toml
[envs.production]
log_level = "info"
```

Accepted values: `debug`, `info`, `warn`, `error`.

Defaults: `debug` for `development`, `info` for all other environments.

This is independent of `--verbose`, which controls only Tako CLI and dev-server logs.

---

## Config Validation Rules

Tako validates your `tako.toml` and reports clear errors when something is wrong:

- **Top-level keys**: Only `name`, `main`, `runtime`, `runtime_version`, `package_manager`, `preset`, `assets`, `[build]`, `[[build_stages]]`, `[vars]`, and `[envs]` are allowed at the top level.
- **Mutual exclusion**: `[build]` with a `run` field and `[[build_stages]]` cannot both be present.
- **Environment sections**: `[envs.<env>]` accepts only `route`/`routes`, `servers`, `idle_timeout`, and `log_level`. Env vars belong in `[vars]` / `[vars.<env>]`.
- **Route exclusivity**: Each environment can set `route` or `routes`, but not both.
- **Non-development routes required**: Every non-development environment must have `route` or `routes` defined (empty lists are rejected).
- **Development route restrictions**: Must be `{app}.tako.test` or a subdomain of it.
- **Route hostnames required**: Path-only routes (like `"/api/*"`) are invalid.
- **Build stage paths**: `cwd` allows `..` for monorepo traversal but must not escape the project root. Absolute paths are rejected.
- **Build stage run**: Each `[[build_stages]]` entry must have a `run` field.
- **Preset namespacing**: Namespaced aliases like `js/tanstack-start` in `preset` are rejected. Use `runtime` for the runtime and keep `preset` runtime-local.
- **Preset references**: `github:` preset references are not supported.
- **App name format**: Must be DNS-compatible: lowercase letters, numbers, hyphens, starting with a letter.

---

## Instance Scaling Behavior

Instance counts are not configured in `tako.toml`. They are runtime state stored on each server and managed with `tako scale`.

- New deploys start with desired instances `0` on each server.
- `tako scale` changes the count, and that value persists across deploys, rollbacks, and server restarts.
- Desired instances `0` means scale-to-zero: instances start on demand and stop after `idle_timeout`.
- Deploy always starts one warm instance so the app is immediately reachable after deploy.

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

# Directories merged into deployed public/ after build (optional)
# assets = ["dist/client", "assets/shared"]

# ── Build Configuration ──────────────────────────────────────
[build]
run = "bun run build"
install = "bun install"
# cwd = "packages/app"  # optional working directory

# Artifact include globs (default: all files)
# include = ["dist/**", ".output/**"]

# Artifact exclude globs
# exclude = ["**/*.map"]

# ── Or use multi-stage builds (mutually exclusive with [build].run) ──
# [[build_stages]]
# name = "frontend-assets"
# cwd = "frontend"
# install = "bun install"
# run = "bun run build"

# ── Global Variables ─────────────────────────────────────────
[vars]
LOG_FORMAT = "json"
API_BASE_URL = "https://api.example.com"

# ── Per-Environment Variables ────────────────────────────────
[vars.production]
LOG_FORMAT = "json"

[vars.staging]
API_BASE_URL = "https://staging-api.example.com"
LOG_FORMAT = "text"

# ── Environments ─────────────────────────────────────────────
[envs.production]
routes = ["api.example.com", "www.api.example.com"]
servers = ["primary", "secondary"]
idle_timeout = 300
log_level = "info"

[envs.staging]
route = "staging.example.com"
servers = ["staging"]
idle_timeout = 120
log_level = "debug"

# Development environment (used by tako dev, not deployable)
[envs.development]
log_level = "debug"
# route defaults to {name}.tako.test
```
