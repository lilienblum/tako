---
layout: ../../layouts/DocsLayout.astro
title: "tako.toml reference for routes, builds, secrets, and scaling - Tako Docs"
heading: "tako.toml Reference"
current: tako-toml
description: "Complete tako.toml reference covering routes, runtime settings, builds, secrets, scaling, environments, and deployment configuration."
---

# `tako.toml` Reference

`tako.toml` is the per-project configuration file that tells Tako how to build, run, and deploy your app. It normally lives at the project root next to your `package.json` or `go.mod`. Running `tako init` generates one with commented examples and sensible defaults.

App-scoped commands (`dev`, `deploy`, `secrets`, `logs`, `scale`, `delete`, `releases`) read `./tako.toml` by default. Pass `-c` / `--config <name>` to point at a different file:

- The path's parent directory becomes the project directory for that invocation. There is no separate `app_dir` field.
- If the path does not already end with `.toml`, Tako appends the suffix for you. `tako deploy -c staging` and `tako deploy -c staging.toml` are equivalent, and the suffix-less form is the recommended shorthand.
- This makes it easy to keep `tako.toml`, `tako.staging.toml`, and `tako.preview.toml` side by side in one folder.

## Minimal Config

The smallest useful `tako.toml` just needs a name and one environment with a route:

```toml
name = "my-app"
runtime = "bun"
runtime_version = "1.2.3"

[envs.production]
route = "my-app.example.com"
servers = ["la"]
```

`tako init` writes a richer version with every top-level field present and commented so you can uncomment what you need.

---

## Top-Level Fields

These fields sit at the root of `tako.toml`, outside any section.

### `name`

Optional but recommended. A stable identifier used in deploy paths, remote server identity, and local dev hostnames.

```toml
name = "my-app"
```

If omitted, Tako falls back to a sanitized version of the selected config file's parent directory name. Remote server identity is `{name}/{env}`, so the same app name can be deployed to multiple environments on one server. Renaming `name` later (or moving the config to a new directory when there is no top-level `name`) creates a new identity on the server — remove the old deployment manually if you no longer need it.

**Name rules:**

| Rule               | Pattern                                |
| ------------------ | -------------------------------------- |
| Regex              | `[a-z][a-z0-9-]{0,62}`                 |
| Starts with        | lowercase letter                       |
| Allowed characters | lowercase `a-z`, digits `0-9`, hyphens |
| Trailing hyphen    | not allowed                            |
| Max length         | 63 characters                          |

Valid: `my-app`, `api-server`, `web-frontend`. Invalid: `MyApp`, `2fast`, `api-`.

### `main`

Optional entrypoint override. Written into the deployed `app.json` and used by both `tako dev` and `tako deploy`. Accepts file paths and module specifiers such as `@scope/pkg`.

```toml
main = "server/index.mjs"
```

Resolution order:

1. `main` in `tako.toml`
2. Manifest main field (`package.json#main` for JS runtimes)
3. Preset `main`

For JS adapters (`bun`, `node`, `deno`), when the preset `main` is `index.<ext>` or `src/index.<ext>` (with `ext` of `ts`, `tsx`, `js`, or `jsx`), Tako picks the first of `index.<ext>`, `src/index.<ext>`, or preset `main` that actually exists. For Go, the default entrypoint is `app`.

After build, deploy verifies the resolved `main` file exists in the build workspace and fails with a clear error if it doesn't.

### `dev`

Optional custom dev command. When set, it overrides both the preset's dev command and the runtime default used by `tako dev`.

```toml
dev = ["vite", "dev"]
```

### `runtime`

Optional runtime selector. Supported values:

- `bun`
- `node`
- `deno`
- `go`

```toml
runtime = "bun"
```

When omitted, Tako auto-detects the runtime from your project files. Setting this explicitly also controls which preset family is consulted when you set `preset`.

### `runtime_version`

Optional pinned runtime version. When set, `tako deploy` sends this value to the server verbatim instead of shelling out to `<runtime> --version`. `tako init` pins the currently-installed version for you.

```toml
runtime_version = "1.2.3"
```

The resolved version is saved into the deployed `app.json`.

### `package_manager`

Optional package manager override. Supported values:

- `npm`
- `pnpm`
- `yarn`
- `bun`

```toml
package_manager = "pnpm"
```

When omitted, Tako detects the package manager from `package.json#packageManager` or the lockfile in your project.

### `preset`

Optional app preset that supplies defaults for `main`, `assets`, and `dev`. Presets are resolved under the selected `runtime` — they are runtime-local aliases.

```toml
preset = "tanstack-start"
# or pinned to a specific preset commit:
# preset = "tanstack-start@a1b2c3d"
```

**Accepted forms:**

- Runtime-local alias: `tanstack-start`, `nextjs`, `vite`
- Pinned runtime-local alias: `tanstack-start@<commit-hash>`

**Rejected forms:**

- Namespaced aliases like `js/tanstack-start` (choose the runtime family via top-level `runtime` instead)
- `github:` references

Preset definitions live in `presets/<language>.toml` and supply only metadata (`name`, `main`, `assets`, `dev`). They never contain install, build, or start commands — runtime behavior lives in runtime plugins.

### `assets`

Optional array of directories merged into the deployed app's `public/` directory after build. Entries are appended to the preset's `assets` list (deduplicated) and merged in listed order, with later entries overwriting earlier ones.

```toml
assets = ["dist/client", "static"]
```

---

## `[build]` Section

The `[build]` table configures a single-stage build.

```toml
[build]
run = "vinxi build"
install = "bun install"
cwd = "packages/web"
include = ["dist/**/*", "public/**/*"]
exclude = ["**/*.map", "**/*.test.*"]
```

| Field     | Required               | Description                                                               |
| --------- | ---------------------- | ------------------------------------------------------------------------- |
| `run`     | yes (for this section) | The build command.                                                        |
| `install` | no                     | Pre-build install command, runs before `run`.                             |
| `cwd`     | no                     | Working directory relative to the project root. `..` is not allowed here. |
| `include` | no                     | Globs to include in the deploy artifact. Defaults to `**/*`.              |
| `exclude` | no                     | Globs to exclude from the deploy artifact.                                |

`[build]` is shorthand for a single-element `[[build_stages]]` list.

**Mutual exclusion with `[[build_stages]]`:**

- Setting both `build.run` and `[[build_stages]]` is an error.
- `build.include` / `build.exclude` cannot be combined with `[[build_stages]]` — use per-stage `exclude` instead.

**Build stage resolution precedence** (first non-empty wins):

1. `[[build_stages]]`
2. `[build]` (normalized to a single stage)
3. Runtime default (e.g. `bun run --if-present build`, `deno task build 2>/dev/null || true`, no default for Go)
4. No-op

---

## `[[build_stages]]` Section

Use `[[build_stages]]` when a single command isn't enough — for example, to build shared packages in a monorepo before the app.

```toml
[[build_stages]]
name = "shared-ui"
cwd = "packages/ui"
install = "bun install"
run = "bun run build"
exclude = ["**/*.map"]

[[build_stages]]
name = "web"
cwd = "packages/web"
run = "bun run build"
```

| Field     | Required | Description                                                                                                                         |
| --------- | -------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| `name`    | no       | Display label in CLI output.                                                                                                        |
| `cwd`     | no       | Working directory relative to the app root. `..` is allowed for monorepo traversal but guarded against escaping the workspace root. |
| `install` | no       | Command run before `run` inside the stage's `cwd`.                                                                                  |
| `run`     | yes      | Stage build command.                                                                                                                |
| `exclude` | no       | Per-stage globs excluded from the deploy artifact.                                                                                  |

Stages run in declaration order.

---

## `[vars]` and `[vars.<env>]`

Non-secret environment variables passed into your app at runtime.

```toml
[vars]
API_URL = "https://api.example.com"
LOG_LEVEL = "info"

[vars.production]
API_URL = "https://api.example.com"

[vars.staging]
API_URL = "https://staging-api.example.com"
LOG_LEVEL = "debug"
```

**Merge order (later overrides earlier):**

1. `[vars]` — base values for every environment
2. `[vars.<env>]` — environment-specific overrides
3. Auto-injected by Tako:
   - `ENV=<environment>` — set in both dev and deploy
   - `TAKO_BUILD=<version>` — set on deploy
   - `TAKO_DATA_DIR=<app data dir>` — set in both dev and deploy
   - Runtime convention vars — `NODE_ENV` for all JS runtimes, `BUN_ENV` for Bun, `DENO_ENV` for Deno

**Reserved:** `ENV` is reserved. If you set it under `[vars]` or `[vars.<env>]`, Tako ignores your value and prints a warning.

**You own log levels:** `LOG_LEVEL` and any other log-verbosity variable your framework reads are not touched by Tako. Set them per environment under `[vars]` / `[vars.<env>]`.

For secrets (API keys, database URLs), use `tako secrets set` instead of `[vars]`. See [Global Config Files](#global-config-files) below.

---

## `[envs.<env>]`

Declare environments, routes, and server targeting. Every non-development environment must define at least one route.

```toml
[envs.production]
route = "api.example.com"
servers = ["la", "nyc"]
idle_timeout = 300

[envs.staging]
routes = [
  "staging.example.com",
  "www.staging.example.com",
  "example.com/api/*",
]
servers = ["staging"]
idle_timeout = 120
```

### Route fields

`route` and `routes` are mutually exclusive — use one or the other.

| Pattern            | Example                 |
| ------------------ | ----------------------- |
| Exact hostname     | `api.example.com`       |
| Wildcard subdomain | `*.api.example.com`     |
| Hostname + path    | `api.example.com/api/*` |
| Wildcard + path    | `*.example.com/admin/*` |

**Route validation rules:**

- Routes must include a hostname — `"/api/*"` is invalid.
- Exact path routes normalize trailing slashes (`example.com/api` and `example.com/api/` are equivalent).
- Each `[envs.<env>]` can use `route` or `routes`, never both.
- Each non-development environment must define at least one route. Empty lists are rejected.
- Development routes must be `{app-name}.test` or `{app-name}.tako.test` (or a subdomain of either).

### `servers`

Array of server names to deploy this environment to. Each name must match an entry in your global `config.toml` (see below). The same server name can be reused across multiple non-development environments.

```toml
servers = ["la", "nyc"]
```

Servers listed under `[envs.development]` are ignored by deploy — development environments only run through `tako dev`.

### `idle_timeout`

Optional idle timeout in seconds. Applies per-instance. Default: `300` (5 minutes).

```toml
idle_timeout = 300
```

### Accepted keys

`[envs.<env>]` accepts only `route`, `routes`, `servers`, and `idle_timeout`. Unknown keys are rejected — env vars belong in `[vars]` / `[vars.<env>]`, not here.

---

## `[servers]` and Workflows

Per-server settings for this app. Right now this section configures workflow workers.

```toml
[servers.workflows]
workers = 1
concurrency = 10

[servers.la.workflows]
workers = 2
```

**Fields under `[servers.workflows]` and `[servers.<name>.workflows]`:**

| Field         | Default | Description                                                                                                                                             |
| ------------- | ------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `workers`     | `0`     | Number of always-on worker processes. `0` means scale-to-zero — the worker is spawned on the first enqueue or cron tick and exits after an idle window. |
| `concurrency` | `10`    | Max parallel runs per worker.                                                                                                                           |

**Precedence:**

1. `[servers.<name>.workflows]` — per-server override
2. `[servers.workflows]` — default for every server in the env
3. Built-in defaults (`workers = 0`, `concurrency = 10`)

`workflows` is a reserved key under `[servers]` — you cannot name a server `workflows`.

If your app has a `workflows/` directory (JS) or declares a worker binary (Go) but no `[servers.*.workflows]` block, the app is implicitly scale-to-zero on every server in the environment.

---

## App Name Resolution

When Tako needs to know the app's identity (deploy, dev, logs, secrets, delete, scale, releases), it uses:

1. Top-level `name` from the selected config, when set.
2. Sanitized version of the selected config file's parent directory name.

Remote deployment identity is always `{app}/{env}`. Setting `name` explicitly is the easiest way to keep the `{app}` segment stable as you rename folders or switch configs.

---

## Config Selection

Every app-scoped command (`dev`, `deploy`, `secrets`, `logs`, `scale`, `delete`, `releases`, `implode`) honors `-c` / `--config`:

```bash
tako deploy -c staging          # loads ./staging.toml
tako deploy -c configs/prod     # loads ./configs/prod.toml
tako dev    -c tako.preview     # loads ./tako.preview.toml
```

The parent directory of the selected file becomes the project directory for that invocation.

---

## Build and Deploy Behavior

Summary of what `tako deploy` does with the fields above. Most of this is informational — there are no knobs below this line.

- Source files are bundled from the git root when available, otherwise from the app directory.
- The build uses a build-dir approach: Tako copies the project into `.tako/build/` (respecting `.gitignore`), symlinks `node_modules/` directories from the original tree, runs build commands, then archives the result without `node_modules/`.
- Deploy always force-excludes `.git/`, `.tako/`, `.env*`, and `node_modules/` from the final archive, on top of `[build].exclude` / per-stage `exclude` and `.gitignore`.
- JS build caches from workspace-root `.turbo/` and app-root `.next/cache/` are restored into the build workspace when present, then excluded from the deploy artifact.
- Artifact include/exclude resolution:
  - Include: `build.include` if set, otherwise `**/*`.
  - Exclude: `build.exclude` plus force-excludes above.
- Asset roots (preset `assets` plus top-level `assets`, deduplicated) are merged into `public/` after build.
- Built artifacts are cached under `.tako/artifacts/` using a deterministic key (source hash, target, preset source/commit, build commands, include/exclude, asset roots, app subdirectory). Cached artifacts are checksum/size verified before reuse.
- Each deploy prunes the local artifact cache to the 90 most recent `{version}.tar.zst` files (across `.tako/artifacts/` and its per-target subdirectories) and removes orphan metadata.
- Non-dry-run deploys take a project-local `.tako/deploy.lock`. If another deploy already holds it, the second invocation exits immediately with the owner PID.
- After extraction on the server, `tako-server` runs the runtime's production install command (e.g. `bun install --production`) before starting instances.

---

## Instance Scaling

Desired instance count per server is **runtime state** on the server, not a `tako.toml` field.

- New deploys start at `0` desired instances on each server.
- `tako scale N --env <env> [--server <name>]` sets the desired count; the value persists across deploys, rollbacks, and server restarts.
- `0` desired = scale-to-zero. Deploy keeps one warm instance running so the app is reachable immediately; instances stop after the environment's `idle_timeout`. Cold starts block the next request up to the startup timeout (default 30s), return `504 App startup timed out` if they exceed it, `502 App failed to start` on setup failure, and `503 App startup queue is full` (with `Retry-After: 1`) when more than 1000 requests queue during a single cold start.
- `N > 0` desired = keep at least `N` instances running on that server.
- `idle_timeout` applies per-instance.
- Instances are never stopped mid-request; explicit scale-down drains in-flight requests first.

---

## Validation Rules

Quick reference of the constraints Tako enforces when loading `tako.toml`.

| Area                            | Rule                                                                                                                      |
| ------------------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| App name                        | Matches `[a-z][a-z0-9-]{0,62}`, no trailing hyphen.                                                                       |
| Top-level `runtime`             | One of `bun`, `node`, `deno`, `go`.                                                                                       |
| Top-level `package_manager`     | One of `npm`, `pnpm`, `yarn`, `bun`.                                                                                      |
| Top-level `preset`              | Runtime-local alias or pinned `alias@<commit>`; namespaced and `github:` forms rejected.                                  |
| `[build]` vs `[[build_stages]]` | Mutually exclusive when `[build]` has `run`. `build.include` / `build.exclude` can't be combined with `[[build_stages]]`. |
| `[build].cwd`                   | Relative path; `..` not allowed.                                                                                          |
| `[[build_stages]].cwd`          | Relative path; `..` allowed, but may not escape the workspace root.                                                       |
| `[[build_stages]].run`          | Required.                                                                                                                 |
| Routes                          | Must include hostname; each env uses `route` or `routes`, not both; non-dev envs require at least one route.              |
| Dev routes                      | Must be `{app}.test`, `{app}.tako.test`, or a subdomain of either.                                                        |
| `[envs.<env>]` keys             | Only `route`, `routes`, `servers`, `idle_timeout`.                                                                        |
| `ENV` var                       | Reserved; setting it in `[vars]` is ignored with a warning.                                                               |
| `servers` under `[envs.<env>]`  | Each name must exist in global `config.toml`; `development` servers are ignored.                                          |
| `workflows` under `[servers]`   | Reserved name — cannot be used as a server name.                                                                          |

---

## Full Annotated Example

```toml
# Stable identity. Remote deploy path is {name}/{env}.
name = "my-app"

# Optional entrypoint override. Written into app.json at deploy.
main = "server/index.mjs"

# Runtime and version. tako init pins the installed version.
runtime = "bun"
runtime_version = "1.2.3"
package_manager = "bun"

# Preset provides defaults for main, assets, and dev command.
preset = "tanstack-start"

# Dev command override. Takes precedence over preset and runtime default.
dev = ["vite", "dev"]

# Extra asset directories merged into public/ after build.
assets = ["dist/client"]

[build]
run = "vinxi build"
install = "bun install"
# cwd = "packages/web"
# include = ["dist/**/*", "public/**/*"]
# exclude = ["**/*.map"]

# Base vars — applied to every environment.
[vars]
LOG_LEVEL = "info"

# Environment-specific overrides.
[vars.production]
API_URL = "https://api.example.com"

[vars.staging]
API_URL = "https://staging-api.example.com"
LOG_LEVEL = "debug"

# Production environment.
[envs.production]
route = "api.example.com"
servers = ["la", "nyc"]
idle_timeout = 300

# Staging environment with multiple routes.
[envs.staging]
routes = [
  "staging.example.com",
  "www.staging.example.com",
  "example.com/api/*",
]
servers = ["staging"]
idle_timeout = 120

# Default workflow config for every server in this app.
[servers.workflows]
workers = 1
concurrency = 10

# Per-server override — the `la` server runs two always-on workers.
[servers.la.workflows]
workers = 2
```

---

## Global Config Files

`tako.toml` is scoped to one project. A few other files live outside the project or alongside it for secrets and server inventory.

### `config.toml` (Global User Config)

Stored at:

- macOS: `~/Library/Application Support/tako/config.toml`
- Linux: `~/.config/tako/config.toml`

This is where your SSH server inventory lives. It is managed by `tako servers add`, `tako servers rm`, and `tako servers ls` — you normally don't edit it by hand.

```toml
[[servers]]
name = "la"
host = "1.2.3.4"
port = 22
description = "LA region primary"
arch = "x86_64"
libc = "glibc"

[[servers]]
name = "nyc"
host = "5.6.7.8"
arch = "aarch64"
libc = "musl"
```

**`[[servers]]` schema:**

| Field         | Required | Description                                                            |
| ------------- | -------- | ---------------------------------------------------------------------- |
| `name`        | yes      | Unique short name referenced by `[envs.<env>].servers` in `tako.toml`. |
| `host`        | yes      | Hostname or IP. Globally unique.                                       |
| `port`        | no       | SSH port. Defaults to `22`.                                            |
| `description` | no       | Free-form label shown in `tako servers ls`.                            |
| `arch`        | auto     | Detected CPU architecture (`x86_64`, `aarch64`).                       |
| `libc`        | auto     | Detected libc (`glibc`, `musl`).                                       |

Both `name` and `host` must be unique across all entries. `arch` and `libc` are detected by `tako servers add` during its SSH probe and stored back into the matching `[[servers]]` entry. If the SSH check is skipped (`--no-test`), deploy will fail for that server until you re-add it with metadata captured.

CLI prompt history is stored separately at `history.toml` next to `config.toml`.

### SSH Authentication

`tako` authenticates to servers using your local keys:

- It looks in `~/.ssh` for common key filenames (`id_ed25519`, `id_rsa`, etc).
- Passphrase-protected keys prompt interactively, or you can set `TAKO_SSH_KEY_PASSPHRASE`.
- If no key file is usable, Tako falls back to `ssh-agent` via `SSH_AUTH_SOCK`.

### `.tako/secrets.json` (Encrypted, Per-Project)

Per-environment encrypted secrets, stored inside the project at `.tako/secrets.json` (JSON format, AES-256-GCM):

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

Each environment stores:

- `salt` — base64-encoded Argon2id salt used for key derivation.
- `secrets` — map of secret name (plaintext) to AES-256-GCM encrypted value.

Encryption keys are file-based, one per environment: `keys/{env}`. Use `tako secrets key derive` to derive a key from a passphrase and `tako secrets key export` to print the current key.

**`.gitignore` behavior during `tako init`:**

- Inside a git repo, `tako init` updates the repo root `.gitignore` with app-relative rules so the app's `.tako/` stays ignored while `.tako/secrets.json` remains trackable.
- Outside a git repo, `tako init` creates or updates `.gitignore` in the app directory with the same intent.

Manage secrets with the `tako secrets` commands rather than editing this file directly.
