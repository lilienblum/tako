---
layout: ../../layouts/DocsLayout.astro
title: "Tako CLI reference for local development and self-hosted deploys - Tako Docs"
heading: CLI Reference
current: cli
description: "Complete CLI reference for Tako commands including init, dev, deploy, servers, secrets, status, logs, and global flags."
---

# CLI Reference

Everything you can do with the `tako` command line tool.

## Usage

```bash
tako [--version] [-v|--verbose] [--ci] [--dry-run] [-c|--config <CONFIG>] <command> [args]
```

## Global Options

These flags work with any command.

`--version` prints the current version and exits. Stable builds print a semver string (e.g. `0.3.1`). Canary builds print `<semver>-canary-<sha7>`.

`-v`, `--verbose` switches to verbose output -- an append-only execution transcript with timestamps, log levels, and technical detail like file paths and per-host transport context.

`--ci` produces deterministic, non-interactive output. Colors, spinners, and interactive prompts are disabled, and output stays transcript-style. If a required prompt value is missing, the command fails with an actionable error suggesting CLI flags or config. Combine with `--verbose` for detailed transcript output in CI/CD pipelines.

`--dry-run` shows what a command would do without performing any side effects. SSH connections, file uploads, config writes, and remote commands are all skipped. Each skipped action is printed as `⏭ ... (dry run)`. Production deploy confirmation is auto-skipped. Supported by: `deploy`, `servers add`, `servers rm`, `delete`.

`-c`, `--config <CONFIG>` selects an explicit app config file instead of `./tako.toml`. If the path does not end with `.toml`, Tako appends it automatically. App-scoped commands treat that file's parent directory as the project directory. Omitting `.toml` is the recommended shorthand.

## Output Modes

Tako has four output modes you can mix and match:

**Normal** (default) -- concise interactive output. Commands that already know their plan may render a persistent task tree that shows waiting work up front (`○` with `...` labels), updates running rows in place, keeps completed rows visible, may place a reporter-specific error line underneath a failed task row, can mark remaining incomplete work as `Aborted` after a fatal failure, and may separate top-level sections with blank lines for readability.

**Verbose** (`--verbose`) -- append-only transcript. Each line is formatted as `HH:MM:SS LEVEL message`. Only prints work as it starts or finishes; upcoming tasks are not pre-rendered. DEBUG-level messages are shown.

**CI** (`--ci`) -- plain text, no ANSI colors, no spinners, no interactive prompts. Stays transcript-style and emits only current work plus final results.

**CI + Verbose** (`--ci --verbose`) -- detailed append-only transcript with no colors or timestamps.

On `Ctrl+C`, Tako clears any active prompt or spinner it controls, leaves one blank line, prints `Operation cancelled`, and exits with code `130`.

All status, progress, and log output goes to stderr. Only actual command results (URLs, machine-readable data) go to stdout.

## Config Selection

App-scoped commands default to `./tako.toml`, but you can point Tako at any config file with `-c` / `--config`:

```bash
tako -c apps/web/staging deploy
tako dev -c configs/preview
```

The selected config file's parent directory becomes the project directory for `init`, `dev`, `logs`, `deploy`, `releases`, `delete`, `secrets`, and `scale` when it is using project context. If `-c` is omitted, Tako uses `./tako.toml`.

---

## `tako init`

Create a `tako.toml` configuration file for your project.

```bash
tako init
```

Init walks you through setting up your project. It prompts for:

- **App name** -- defaults to the selected config file's parent directory name, sanitized for DNS compatibility.
- **Production route** -- the hostname your app will be served at (defaults to `{name}.example.com`).
- **Runtime** -- detects your runtime automatically (Bun, Node, Deno, Go) and lets you confirm or override.
- **Preset** -- fetches available presets for your runtime and lets you pick one, or use the base runtime preset. For JavaScript, that can include framework presets like `tanstack-start` or `nextjs`. When "custom preset reference" is selected, `preset` is left commented/unset.
- **Main entrypoint** -- only prompted when neither adapter inference nor the chosen preset provides a default.

The generated `tako.toml` leaves only essential options uncommented (`name`, `runtime`, `runtime_version`, `route`) with all other options included as commented examples. `runtime_version` is pinned from the locally-installed runtime version. A non-base `preset` is uncommented only when one is selected.

After generating `tako.toml`, init installs the `tako.sh` SDK package using the selected runtime's built-in package-manager command (for example `bun add tako.sh` for Bun, `npm install tako.sh` for Node, `go get tako.sh` for Go).

Init also updates `.gitignore` so `.tako/*` is ignored while `.tako/secrets.json` remains trackable. When the project lives inside a git repo, the repo-root `.gitignore` is updated; otherwise a local `.gitignore` is used.

If the selected config file already exists in an interactive terminal, init asks for overwrite confirmation. In non-interactive mode, it leaves the file untouched and prints `Operation cancelled`.

The full "Detected" diagnostics summary is only shown when `--verbose` is active.

---

## `tako dev`

Start or connect to a local development session.

```bash
tako dev [--variant <VARIANT>]
```

| Flag                  | Description                                                                    |
| --------------------- | ------------------------------------------------------------------------------ |
| `--variant <VARIANT>` | Run a DNS variant of the app (e.g. `--variant foo` serves at `myapp-foo.test`) |

Alias for `--variant`: `--var`

`tako dev` is a client that connects to the `tako-dev-server` daemon. It registers the selected config file, starts your app, and streams logs directly to your terminal.

On first run, Tako sets up a local Certificate Authority and HTTPS infrastructure so your app is available at `https://{app}.test/`. On macOS, a dev proxy is installed so your app is served on the default HTTPS port (443) without needing to specify a port. On Linux, iptables redirect rules achieve the same result without an extra proxy binary. In interactive mode, pressing `l` enables LAN mode so the same routes are also available through `.local` aliases.

When `[envs.development]` defines custom routes in `tako.toml`, those routes are used instead of the default. Dev routes must use `.test` or `.tako.test` -- for example `{app}.test` or a subdomain of it.

The app starts immediately when `tako dev` starts (1 local instance) and transitions to idle after 30 minutes of no attached CLI clients. After an idle transition, the next HTTP request triggers wake-on-request. Running `tako dev` again with the same config attaches to the existing session.

**Interactive keyboard shortcuts:**

| Key      | Action                                                |
| -------- | ----------------------------------------------------- |
| `l`      | Toggle LAN mode (`.local` aliases for current routes) |
| `r`      | Restart the app process                               |
| `b`      | Background the app (hand off to daemon, CLI exits)    |
| `Ctrl+c` | Stop the app and quit                                 |

When stdout is not a terminal (piped or redirected), output falls back to plain text with no color or raw mode.

`tako dev` watches `tako.toml` for changes and automatically restarts when environment variables change or updates routing when dev routes change. Source hot-reload is runtime-driven (e.g. Bun's built-in watch mode).

```bash
# Start dev session
tako dev

# Start a variant
tako dev --variant staging
```

### `tako dev stop`

Stop a running or idle dev app.

```bash
tako dev stop [NAME] [--all]
```

| Argument/Flag | Description                                                   |
| ------------- | ------------------------------------------------------------- |
| `NAME`        | App name to stop (defaults to the selected config file's app) |
| `--all`       | Stop all registered dev apps                                  |

### `tako dev ls`

List all registered dev apps with their status.

```bash
tako dev ls
```

Alias: `tako dev list`

---

## `tako doctor`

Print local dev environment diagnostics and exit.

```bash
tako doctor
```

Reports on the dev daemon, local DNS, and listener status. If the dev daemon is not running, doctor reports that and hints to start `tako dev`. Platform-specific sections:

- **macOS:** Dev proxy install status, boot-helper load status, dedicated loopback alias, launchd load status, TCP reachability on `127.77.0.1:443` and `:80`.
- **Linux:** Port redirect status (loopback alias and iptables rules), TCP reachability on `127.77.0.1:443` and `:80`.

---

## `tako deploy`

Build and deploy your application to remote servers.

```bash
tako deploy [--env <ENV>] [-y|--yes]
```

| Flag          | Description                                   |
| ------------- | --------------------------------------------- |
| `--env <ENV>` | Target environment (defaults to `production`) |
| `-y`, `--yes` | Skip confirmation prompts                     |

The target environment must be declared in `tako.toml` (`[envs.<name>]`) and must define a `route` or `routes`. The `development` environment is reserved for `tako dev` and cannot be deployed.

In interactive terminals, deploying to `production` requires confirmation unless `--yes` is provided.

**What deploy does:**

1. Validates configuration, secrets, and server target metadata
2. Resolves source bundle root (git root when available; otherwise app directory)
3. Sets up a clean workdir from your project (respecting `.gitignore`)
4. Resolves preset metadata and runs your build commands
5. Verifies the resolved runtime `main` file exists in the build workspace
6. Packages the deploy artifact (excluding `node_modules/` for JS projects)
7. Checks disk space on target servers before uploading
8. Uploads the artifact to all servers in the target environment
9. Server installs production dependencies (JS runtimes) or runs the binary directly (Go), and performs a rolling update
10. Cleans up releases older than 30 days

Deploy resolves app identity from `name` in the selected config file, falling back to the sanitized selected config parent directory name.

Before server checks and build work start, non-dry-run deploy acquires a project-local `.tako/deploy.lock`. If another local deploy is already running for the same project, the second command fails immediately and reports the owning PID.

**Server auto-selection:** If you're deploying to `production` and `[envs.production].servers` is empty, Tako will auto-select your only server or prompt you to pick one when you have multiple servers configured. The selection is persisted to `tako.toml`.

**Failure handling:** If some servers fail while others succeed, deploy continues and reports failures at the end. Failed deploys automatically roll back on the affected server and clean up partial release directories.

**Upstream transport:** Deploy always uses per-instance private TCP upstreams. The deployed app should listen on the provided `PORT`.

**Version naming:**

- Clean git tree: `{commit_hash}` (e.g., `abc1234`)
- Dirty working tree: `{commit_hash}_{content_hash}` (first 8 chars each)
- No git commit/repo: `nogit_{content_hash}` (first 8 chars)

```bash
# Deploy to production
tako deploy

# Deploy to staging
tako deploy --env staging

# Deploy without confirmation
tako deploy --yes
```

---

## `tako delete`

Remove a deployed app from a specific environment and server.

```bash
tako delete [--env <ENV>] [--server <SERVER>] [-y|--yes]
```

Aliases: `tako rm`, `tako remove`, `tako undeploy`, `tako destroy`

| Flag                | Description                    |
| ------------------- | ------------------------------ |
| `--env <ENV>`       | Environment to delete from     |
| `--server <SERVER>` | Specific server to delete from |
| `-y`, `--yes`       | Skip confirmation prompts      |

Delete removes exactly one deployment target, not every server in an environment. In interactive mode, when additional information is needed, Tako shows deployed targets and lets you pick. In non-interactive mode, `--yes`, `--env`, and `--server` are all required.

The `development` environment is reserved for `tako dev` and cannot be used with delete. Delete is idempotent for absent app state (safe to re-run for cleanup).

```bash
# Interactive: pick from deployed targets
tako delete

# Delete staging from a specific server
tako delete --env staging --server lax --yes
```

---

## `tako scale`

Change the desired instance count for a deployed app.

```bash
tako scale <N> [--env <ENV>] [--server <SERVER>] [--app <APP>]
```

| Argument/Flag       | Description                                                |
| ------------------- | ---------------------------------------------------------- |
| `<N>`               | Desired instance count per targeted server                 |
| `--env <ENV>`       | Environment to scale (required when `--server` is omitted) |
| `--server <SERVER>` | Scale only this specific server                            |
| `--app <APP>`       | App name (required outside project config context)         |

Inside project config context, Tako resolves the app name automatically. Outside that context, use `--app <name>` with `--env <env>`, or pass the full deployment id as `--app <name>/<env>`.

When `--server` is omitted, `--env` is required and Tako scales every server in that environment. When `--server` is provided alone inside a project, the environment defaults to `production`.

The desired instance count is stored on each server and persists across deploys, rollbacks, and restarts.

Setting instances to `0` enables on-demand scale-to-zero with cold starts on the next request. Scaling down drains in-flight requests before stopping excess instances.

```bash
# Keep 2 warm instances on every production server
tako scale 2 --env production

# Scale a specific server
tako scale 3 --server lax --env staging

# Scale from outside project config context
tako scale 2 --app my-api --env production

# Scale to zero (on-demand cold start)
tako scale 0 --env production
```

---

## `tako logs`

View or stream logs from remote servers.

```bash
tako logs [--env <ENV>] [--tail] [--days <N>]
```

| Flag          | Description                                              |
| ------------- | -------------------------------------------------------- |
| `--env <ENV>` | Environment to view logs from (defaults to `production`) |
| `--tail`      | Stream logs continuously (conflicts with `--days`)       |
| `--days <N>`  | Number of days of history to show (default: `3`)         |

**History mode** (default) fetches logs from all mapped servers, sorts them by timestamp, deduplicates consecutive identical messages, and displays them in your pager (`$PAGER`, defaulting to `less -R`).

**Streaming mode** (`--tail`) streams logs continuously until you press `Ctrl+c`. Consecutive identical messages are deduplicated with an "... and N more" suffix.

When multiple servers are present, each line is prefixed with `[server-name]`.

```bash
# View last 3 days of production logs
tako logs

# Stream staging logs
tako logs --env staging --tail

# View last 7 days of production logs
tako logs --days 7
```

---

## `tako releases`

View release history and roll back to previous releases.

### `tako releases ls`

List release history for the current app.

```bash
tako releases ls [--env <ENV>]
```

| Flag          | Description                                     |
| ------------- | ----------------------------------------------- |
| `--env <ENV>` | Environment to query (defaults to `production`) |

Alias: `tako releases list`

Output is sorted newest-first. Each release shows:

- Release/build id and deployed timestamp (with a relative hint like `{3h ago}` for recent deploys)
- Commit message and cleanliness marker (`[clean]`, `[dirty]`, or `[unknown]`)
- `[current]` marks the active release

```bash
tako releases ls
tako releases ls --env staging
```

### `tako releases rollback`

Roll back to a previously deployed release.

```bash
tako releases rollback <RELEASE_ID> [--env <ENV>] [-y|--yes]
```

| Argument/Flag  | Description                                         |
| -------------- | --------------------------------------------------- |
| `<RELEASE_ID>` | Target release/build id to roll back to             |
| `--env <ENV>`  | Environment to roll back (defaults to `production`) |
| `-y`, `--yes`  | Skip confirmation prompt                            |

Rollback reuses current routes, env vars, secrets, and scaling config. It switches the runtime to the target release and performs a standard rolling update. In interactive terminals, rolling back `production` requires confirmation unless `--yes` is provided. Partial failures are reported per server; successful servers remain rolled back.

```bash
tako releases rollback abc1234 --env production --yes
```

---

## `tako servers`

Manage your server inventory and server runtime.

### `tako servers add`

Add a server to your global configuration.

```bash
tako servers add [HOST] [--name <NAME>] [--description <TEXT>] [--port <PORT>] [--no-test]
```

| Argument/Flag          | Description                                         |
| ---------------------- | --------------------------------------------------- |
| `HOST`                 | Server IP or hostname (omit for interactive wizard) |
| `--name <NAME>`        | Server name (required when `HOST` is provided)      |
| `--description <TEXT>` | Optional description shown in `servers ls`          |
| `--port <PORT>`        | SSH port (default: `22`)                            |
| `--no-test`            | Skip SSH connection test and target detection       |

Without arguments, Tako launches an interactive setup wizard that guides you through host, name, description, and port configuration. The wizard supports `Tab` autocomplete suggestions from existing servers and CLI history.

By default, Tako tests the SSH connection (as user `tako`), verifies the host key against `~/.ssh/known_hosts`, and detects server target metadata (`arch`, `libc`) for deploy target matching. Use `--no-test` to skip these checks.

Re-running with the same name/host/port is idempotent.

```bash
# Interactive wizard
tako servers add

# Direct add
tako servers add 203.0.113.10 --name production

# Add with description and custom port
tako servers add 203.0.113.10 --name eu-edge --description "EU region" --port 2222
```

### `tako servers rm`

Remove a server from your global configuration.

```bash
tako servers rm [NAME]
```

Aliases: `tako servers remove`, `tako servers delete`

| Argument | Description                                               |
| -------- | --------------------------------------------------------- |
| `NAME`   | Server name (omit for interactive selector in a terminal) |

Confirms before removal and warns that projects referencing this server will fail. In non-interactive mode, `NAME` is required.

### `tako servers ls`

List all configured servers.

```bash
tako servers ls
```

Alias: `tako servers list`

Displays a table of server name, host, port, and optional description. If no servers are configured, shows a hint to run `tako servers add`.

### `tako servers status`

Show global deployment status across all configured servers.

```bash
tako servers status
```

Alias: `tako servers info`

Prints a snapshot of every server and its deployed apps, including instance counts, build ids, and deploy timestamps. Does not require `tako.toml` and can run from any directory.

If no servers are configured interactively, offers to run the add-server wizard. If no deployed apps are found, reports that explicitly.

### `tako servers restart`

Reload `tako-server` on a remote host without downtime by default.

```bash
tako servers restart <NAME>
tako servers restart <NAME> --force
```

| Argument/Flag | Description                                        |
| ------------- | -------------------------------------------------- |
| `<NAME>`      | Server name                                        |
| `--force`     | Perform a full service restart with brief downtime |

Without `--force`, this sends a graceful reload so the current process can hand off to a replacement process without downtime. `--force` does a full service restart, which may cause brief downtime for all apps on that server.

### `tako servers upgrade`

Upgrade `tako-server` on one or all configured remote servers with zero-downtime reload.

```bash
tako servers upgrade [SERVER_NAME] [--canary|--stable]
```

| Argument/Flag | Description                                            |
| ------------- | ------------------------------------------------------ |
| `SERVER_NAME` | Server name (omit to upgrade all configured servers)   |
| `--canary`    | Install canary prerelease build                        |
| `--stable`    | Install stable build and set default channel to stable |

Without channel flags, uses the persisted `upgrade_channel` from global config (default: `stable`). The `--canary` and `--stable` flags are mutually exclusive.

Upgrade verifies the signed `tako-server-sha256s.txt` release manifest, enforces the matching SHA-256 on the downloaded archive, installs the new binary, acquires an upgrade lock, signals a service-manager reload (`systemctl reload` on systemd, `rc-service reload` on OpenRC), waits for the management socket to report ready, then releases the lock. Reload uses temporary process/listener overlap until the replacement process reports ready, and Tako keeps the previous on-disk binary until then so it can restore it if readiness fails. Requires a supported service manager and root privileges (root login or sudo-capable user).

Custom `TAKO_DOWNLOAD_BASE_URL` overrides must use `https://`. For local testbeds, an explicit `TAKO_ALLOW_INSECURE_DOWNLOAD_BASE=1` override is required before a non-HTTPS base is accepted.

```bash
# Upgrade all servers
tako servers upgrade

# Upgrade a specific server
tako servers upgrade production

# Upgrade to canary
tako servers upgrade staging --canary
```

### `tako servers implode`

Remove `tako-server` and all data from a remote server.

```bash
tako servers implode [NAME] [-y|--yes]
```

| Argument/Flag | Description                                               |
| ------------- | --------------------------------------------------------- |
| `NAME`        | Server name (omit for interactive selector in a terminal) |
| `-y`, `--yes` | Skip confirmation prompts                                 |

Alias: `tako servers uninstall`

Connects via SSH and removes everything Tako installed on the server: services (systemd and OpenRC), binaries, data directory (`/opt/tako/`), socket directories, and service configuration files. After the remote teardown, removes the server from your local config.

Requires root privileges on the server (root login or sudo-capable user).

### `tako servers setup-wildcard`

Configure DNS-01 wildcard certificate support on all servers.

```bash
tako servers setup-wildcard [--env ENV]
```

| Argument/Flag | Description                                  |
| ------------- | -------------------------------------------- |
| `--env`, `-e` | Target environment (defaults to all servers) |

Runs an interactive wizard to collect DNS provider credentials, verifies them locally, then applies the configuration to all servers in parallel. After setup, `tako-server` will automatically download and use `lego` for DNS-01 challenges when wildcard certificates are needed.

Deploy will fail if wildcard routes are configured but DNS credentials have not been set up. Run this command before deploying apps with wildcard routes.

---

## `tako secrets`

Manage per-environment encrypted secrets for your project.

Secrets are stored locally in `.tako/secrets.json` (encrypted with AES-256-GCM). Secret names are plaintext; values are encrypted. Each environment has its own encryption key stored at `keys/{env}`.

### `tako secrets set`

Set or update a secret.

```bash
tako secrets set <NAME> [--env <ENV>] [--sync]
```

| Argument/Flag | Description                                   |
| ------------- | --------------------------------------------- |
| `<NAME>`      | Secret name                                   |
| `--env <ENV>` | Target environment (defaults to `production`) |
| `--sync`      | Immediately sync to servers after setting     |

Alias: `tako secrets add`

In interactive terminals, prompts for the value with masked input. In non-interactive mode, reads a single line from stdin. Creates the environment encryption key if it does not exist.

```bash
tako secrets set DATABASE_URL --env production
tako secrets set API_KEY --env staging --sync
```

### `tako secrets rm`

Remove a secret.

```bash
tako secrets rm <NAME> [--env <ENV>] [--sync]
```

Aliases: `tako secrets remove`, `tako secrets delete`

| Argument/Flag | Description                                                                        |
| ------------- | ---------------------------------------------------------------------------------- |
| `<NAME>`      | Secret name                                                                        |
| `--env <ENV>` | Remove from this environment only (without `--env`, removes from all environments) |
| `--sync`      | Sync to servers after removing                                                     |

When `--sync` is provided without `--env`, secrets are synced to all environments.

### `tako secrets ls`

List all secrets with a presence table across environments.

```bash
tako secrets ls
```

Aliases: `tako secrets list`, `tako secrets show`

Shows which secrets exist in which environments. Warns about missing secrets. Never displays actual values.

### `tako secrets sync`

Push local secrets to remote servers.

```bash
tako secrets sync [--env <ENV>]
```

| Flag          | Description                                                                         |
| ------------- | ----------------------------------------------------------------------------------- |
| `--env <ENV>` | Sync only this environment (without `--env`, syncs all environments in `tako.toml`) |

Decrypts secrets locally and sends them to `tako-server` on each mapped server. App instances restart automatically when secrets change.

Environments with no mapped servers are skipped with a warning.

```bash
# Sync all environments
tako secrets sync

# Sync just production
tako secrets sync --env production
```

### `tako secrets key derive`

Derive an encryption key from a passphrase (for sharing with teammates).

```bash
tako secrets key derive [--env <ENV>]
```

| Flag          | Description                                   |
| ------------- | --------------------------------------------- |
| `--env <ENV>` | Target environment (defaults to `production`) |

Writes the key to `keys/{env}`.

### `tako secrets key export`

Export an encryption key to clipboard.

```bash
tako secrets key export [--env <ENV>]
```

| Flag          | Description                                   |
| ------------- | --------------------------------------------- |
| `--env <ENV>` | Target environment (defaults to `production`) |

Reads from `keys/{env}` and copies the base64-encoded key to your clipboard.

---

## `tako upgrade`

Upgrade the local `tako` CLI to the latest release.

```bash
tako upgrade [--canary|--stable]
```

| Flag       | Description                                                   |
| ---------- | ------------------------------------------------------------- |
| `--canary` | Install latest canary build                                   |
| `--stable` | Install latest stable build and set default channel to stable |

The `--canary` and `--stable` flags are mutually exclusive. Without either flag, upgrade uses the persisted `upgrade_channel` from global config (default: `stable`).

Before running, upgrade prints the active channel (`You're on {channel} channel`).

Upgrade strategy is install-aware:

- **Homebrew** installs use `brew upgrade tako`
- **Hosted installer** (default/fallback) downloads and runs `https://tako.sh/install.sh`

`--canary` always uses the hosted installer path and pulls from the canary release channel.

```bash
tako upgrade
tako upgrade --canary
tako upgrade --stable
```

You can also install canary directly:

```bash
curl -fsSL https://tako.sh/install-canary.sh | sh
```

---

## `tako implode`

Remove the local Tako CLI and all associated data.

```bash
tako implode [-y|--yes]
```

| Flag          | Description              |
| ------------- | ------------------------ |
| `-y`, `--yes` | Skip confirmation prompt |

Alias: `tako uninstall`

Removes the Tako config directory, data directory (CA certs, encryption keys, dev server state), and CLI binaries (`tako`, `tako-dev-server`, `tako-dev-proxy`). Stops the dev server before removal.

Also removes system-level services installed by `tako dev` (requires sudo):

- **macOS:** dev proxy LaunchDaemons, `/Library/Application Support/Tako/`, `/etc/resolver/test`, `/etc/resolver/tako.test`, CA certificate in system keychain, loopback alias.
- **Linux:** systemd redirect service, resolved drop-in, CA certificate in system trust store, iptables rules, loopback alias.

If nothing exists to remove, reports that and exits.

---

## `tako typegen`

Generate typed accessors for the current project.

```bash
tako typegen
```

Generates `tako.d.ts` for JavaScript/TypeScript projects and `tako_secrets.go` for Go projects. In JS/TS projects, `tako.d.ts` types both `Tako.secrets` and the stable app env surface exposed through `process.env` and `import.meta.env` (for example `ENV`, `TAKO_ENV`, `TAKO_BUILD`, and `TAKO_DATA_DIR`).

---

## `tako version`

Show version information.

```bash
tako version
```

Same as `--version` flag. Stable builds print a semver string; canary builds print `<semver>-canary-<sha7>`.

---

## `tako help`

Show all available commands with brief descriptions.

```bash
tako help
```

Running `tako` with no arguments also prints help.

---

## Quick Reference

| Command                        | What it does                              |
| ------------------------------ | ----------------------------------------- |
| `tako init`                    | Initialize a new project with `tako.toml` |
| `tako dev`                     | Start local development session           |
| `tako dev stop [NAME] [--all]` | Stop a dev app                            |
| `tako dev ls`                  | List registered dev apps                  |
| `tako doctor`                  | Print local dev diagnostics               |
| `tako deploy`                  | Build and deploy to servers               |
| `tako delete`                  | Remove a deployment                       |
| `tako scale <N>`               | Change instance count                     |
| `tako logs`                    | View or stream remote logs                |
| `tako releases ls`             | List release history                      |
| `tako releases rollback <ID>`  | Roll back to a previous release           |
| `tako servers add`             | Add a server                              |
| `tako servers rm`              | Remove a server                           |
| `tako servers ls`              | List servers                              |
| `tako servers status`          | Show deployment status                    |
| `tako servers restart <NAME>`  | Reload tako-server (`--force` to restart) |
| `tako servers upgrade [NAME]`  | Upgrade tako-server                       |
| `tako servers implode [NAME]`  | Remove tako-server and all data           |
| `tako servers setup-wildcard`  | Configure DNS-01 wildcard support         |
| `tako secrets set <NAME>`      | Set a secret                              |
| `tako secrets rm <NAME>`       | Remove a secret                           |
| `tako secrets ls`              | List secrets                              |
| `tako secrets sync`            | Sync secrets to servers                   |
| `tako secrets key derive`      | Derive encryption key from passphrase     |
| `tako secrets key export`      | Export encryption key                     |
| `tako upgrade`                 | Upgrade the CLI                           |
| `tako implode`                 | Remove Tako CLI and all local data        |
| `tako typegen`                 | Generate typed secret accessors           |
| `tako version`                 | Show version                              |
| `tako help`                    | Show help                                 |
