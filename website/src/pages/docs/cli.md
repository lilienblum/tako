---
layout: ../../layouts/DocsLayout.astro
title: "Tako Docs - CLI Reference"
heading: CLI Reference
current: cli
---

# CLI Reference

Everything you can do with the `tako` command line tool.

## Usage

```bash
tako [--version] [-v|--verbose] [--ci] [--dry-run] <command> [args]
```

## Global Options

These flags work with any command.

`--version` prints the current version and exits. Stable builds print a semver string (e.g. `0.3.1`). Canary builds print `canary-<sha7>`.

`-v`, `--verbose` switches to verbose output -- an append-only execution transcript with timestamps, log levels, and technical detail like file paths and per-host transport context.

`--ci` produces deterministic, non-interactive output. Colors, spinners, and interactive prompts are all disabled. If a required prompt value is missing, the command fails with an actionable error suggesting CLI flags or config. Combine with `--verbose` for detailed transcript output in CI/CD pipelines.

`--dry-run` shows what a command would do without performing any side effects. SSH connections, file uploads, config writes, and remote commands are all skipped. Each skipped action is printed as `⏭ ... (dry run)`. Supported by: `deploy`, `servers add`, `servers rm`, `delete`.

## Output Modes

Tako has three output modes you can mix and match:

**Normal** (default) -- concise, user-focused output with spinners for long-running steps in interactive terminals.

**Verbose** (`--verbose`) -- append-only transcript. Each line is formatted as `HH:MM:SS LEVEL message`. Spinners degrade to log lines, and DEBUG-level messages are shown.

**CI** (`--ci`) -- plain text, no ANSI colors, no spinners, no interactive prompts. Pair with `--verbose` for maximum detail without formatting.

All status, progress, and log output goes to stderr. Only actual command results (URLs, machine-readable data) go to stdout.

## Directory Selection

Several commands accept an optional `[DIR]` positional argument that tells Tako to run as if invoked from that directory:

```bash
tako init [DIR]
tako dev [DIR]
tako deploy [DIR]
tako delete [DIR]
tako logs [DIR]
```

When `DIR` is omitted, the current working directory is used.

---

## `tako init`

Create a `tako.toml` configuration file for your project.

```bash
tako init [DIR]
```

Init walks you through setting up your project. It prompts for:

- **App name** -- defaults to the directory name, sanitized for DNS compatibility.
- **Production route** -- the hostname your app will be served at (defaults to `{name}.example.com`).
- **Runtime** -- detects your runtime automatically (Bun, Node, Deno) and lets you confirm or override.
- **Preset** -- fetches available presets for your runtime and lets you pick one, or use the base runtime preset.
- **Main entrypoint** -- only prompted when neither adapter inference nor the chosen preset provides a default.

The generated `tako.toml` leaves only essential options uncommented (`name`, `runtime`, `runtime_version`, `route`) with all other options included as commented examples. `runtime_version` is pinned from the locally-installed runtime version.

After generating `tako.toml`, init installs the `tako.sh` SDK package using the detected package manager (e.g. `bun add tako.sh`, `npm install tako.sh`).

Init also updates `.gitignore` so `.tako/*` is ignored while `.tako/secrets.json` remains trackable. When the project lives inside a git repo, the repo-root `.gitignore` is updated; otherwise a local `.gitignore` is used.

If `tako.toml` already exists in an interactive terminal, init asks for overwrite confirmation. In non-interactive mode, it silently skips.

The full "Detected" diagnostics summary is only shown when `--verbose` is active.

---

## `tako dev`

Start or attach to a local development session.

```bash
tako dev [--name <NAME>] [DIR]
```

| Flag            | Description                                                            |
| --------------- | ---------------------------------------------------------------------- |
| `--name <NAME>` | Override the app name (defaults to `tako.toml` name or directory name) |

`tako dev` is a client that connects to the `tako-dev-server` daemon. It registers your app, starts it, and streams logs directly to your terminal.

On first run, Tako sets up a local Certificate Authority and HTTPS infrastructure so your app is available at `https://{app}.tako.test/`. On macOS, a loopback proxy is installed so your app is served on the default HTTPS port (443) without needing to specify a port.

When `[envs.development]` defines custom routes in `tako.toml`, those routes are used instead of the default. Dev routes must be `{app}.tako.test` or a subdomain of it.

**Interactive keyboard shortcuts:**

| Key      | Action                                             |
| -------- | -------------------------------------------------- |
| `r`      | Restart the app process                            |
| `b`      | Background the app (hand off to daemon, CLI exits) |
| `Ctrl+c` | Stop the app and quit                              |

When stdout is not a terminal (piped or redirected), output falls back to plain text with no color or raw mode.

`tako dev` watches `tako.toml` for changes and automatically restarts when environment variables change or updates routing when dev routes change. Source hot-reload is runtime-driven (e.g. Bun's built-in watch mode).

### `tako dev stop`

Stop a running or idle dev app.

```bash
tako dev stop [NAME] [--all]
```

| Argument/Flag | Description                                            |
| ------------- | ------------------------------------------------------ |
| `NAME`        | App name to stop (defaults to current directory's app) |
| `--all`       | Stop all registered dev apps                           |

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

Reports on the dev daemon, local DNS, and listener status. On macOS, includes a detailed preflight section covering:

- Loopback proxy install status
- Boot-helper load status
- Dedicated loopback alias status
- Launchd load status
- TCP reachability on loopback ports 443 and 80

If the dev daemon is not running, doctor reports that and hints to start `tako dev`.

---

## `tako deploy`

Build and deploy your application to remote servers.

```bash
tako deploy [--env <ENV>] [-y|--yes] [DIR]
```

| Flag          | Description                                   |
| ------------- | --------------------------------------------- |
| `--env <ENV>` | Target environment (defaults to `production`) |
| `-y`, `--yes` | Skip confirmation prompts                     |

The target environment must be declared in `tako.toml` (`[envs.<name>]`) and must define a `route` or `routes`. The `development` environment is reserved for `tako dev` and cannot be deployed.

In interactive terminals, deploying to `production` requires confirmation unless `--yes` is provided.

**What deploy does:**

1. Validates configuration and secrets
2. Creates a source archive from your project
3. Resolves your build preset and runs build stages
4. Builds target-specific artifacts (locally or in Docker, depending on preset)
5. Uploads artifacts to all servers in the target environment
6. Performs a rolling update on each server (start new instance, health check, swap, drain old instance)
7. Cleans up releases older than 30 days

Deploy resolves app identity from `name` in `tako.toml`, falling back to the sanitized project directory name.

**Server auto-selection:** If you're deploying to `production` and `[envs.production].servers` is empty, Tako will auto-select your only server or prompt you to pick one when you have multiple servers configured.

**Failure handling:** If some servers fail while others succeed, deploy continues and reports failures at the end. Failed deploys automatically roll back on the affected server and clean up partial release directories.

---

## `tako delete`

Remove a deployed app from a specific environment and server.

```bash
tako delete [--env <ENV>] [--server <SERVER>] [-y|--yes] [DIR]
```

Aliases: `tako rm`, `tako remove`, `tako undeploy`, `tako destroy`

| Flag                | Description                    |
| ------------------- | ------------------------------ |
| `--env <ENV>`       | Environment to delete from     |
| `--server <SERVER>` | Specific server to delete from |
| `-y`, `--yes`       | Skip confirmation prompts      |

Delete removes exactly one deployment target, not every server in an environment. In interactive mode, when additional information is needed, Tako shows deployed targets and lets you pick. In non-interactive mode, `--yes`, `--env`, and `--server` are all required.

The `development` environment is reserved for `tako dev` and cannot be used with delete.

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
| `--app <APP>`       | App name (required outside a project directory)            |

Inside a project directory, Tako resolves the app name automatically. Outside a project, use `--app <name>` with `--env <env>`, or pass the full deployment id as `--app <name>/<env>`.

When `--server` is omitted, `--env` is required and Tako scales every server in that environment. When `--server` is provided alone inside a project, the environment defaults to `production`.

The desired instance count is stored on each server and persists across deploys, rollbacks, and restarts.

```bash
# Keep 2 warm instances on every production server
tako scale 2 --env production

# Scale a specific server
tako scale 3 --server lax --env staging

# Scale from outside a project directory
tako scale 2 --app my-api --env production
```

Setting instances to `0` enables on-demand scale-to-zero with cold starts on the next request.

---

## `tako logs`

View or stream logs from remote servers.

```bash
tako logs [--env <ENV>] [--tail] [--days <N>] [DIR]
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

Output is sorted newest-first. Each release shows:

- Release/build id and deployed timestamp (with a relative hint like `{3h ago}` for recent deploys)
- Commit message and cleanliness marker (`[clean]`, `[dirty]`, or `[unknown]`)
- `[current]` marks the active release

Alias: `tako releases list`

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

Rollback reuses current routes, env vars, secrets, and scaling config. It switches the runtime to the target release and performs a standard rolling update. In interactive terminals, rolling back `production` requires confirmation unless `--yes` is provided.

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

Without arguments, Tako launches an interactive setup wizard that guides you through host, name, description, and port configuration.

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

### `tako servers restart`

Restart `tako-server` on a remote host.

```bash
tako servers restart <NAME>
```

| Argument | Description |
| -------- | ----------- |
| `<NAME>` | Server name |

Restarts the entire `tako-server` process, which causes brief downtime for all apps on that server. Use for binary updates, major configuration changes, or system recovery.

### `tako servers upgrade`

Upgrade `tako-server` on a remote host with zero-downtime reload.

```bash
tako servers upgrade <NAME> [--canary|--stable]
```

| Argument/Flag | Description                                            |
| ------------- | ------------------------------------------------------ |
| `<NAME>`      | Server name                                            |
| `--canary`    | Install canary prerelease build                        |
| `--stable`    | Install stable build and set default channel to stable |

Without channel flags, uses the persisted `upgrade_channel` from global config (default: `stable`). The `--canary` and `--stable` flags are mutually exclusive.

Upgrade installs the new binary, acquires an upgrade lock, signals a service-manager reload (`systemctl reload` on systemd, `rc-service reload` on OpenRC), waits for the management socket to report ready, then releases the lock. Requires a supported service manager and root privileges (root login or sudo-capable user).

```bash
tako servers upgrade production
tako servers upgrade staging --canary
```

### `tako servers status`

Show global deployment status across all configured servers.

```bash
tako servers status
```

Alias: `tako servers info`

Prints a snapshot of every server and its deployed apps, including instance counts, build ids, and deploy timestamps. Does not require `tako.toml` and can run from any directory.

If no servers are configured interactively, offers to run the add-server wizard. If no deployed apps are found, reports that explicitly.

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

Aliases: `tako secrets remove`, `tako secrets delete`, `tako secrets del`

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

### `tako secrets key import`

Import an encryption key from masked terminal input.

```bash
tako secrets key import [--env <ENV>]
```

| Flag          | Description                                   |
| ------------- | --------------------------------------------- |
| `--env <ENV>` | Target environment (defaults to `production`) |

Writes the key to `keys/{env}`. Use this to share encryption keys with teammates.

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
- **Cargo** installs use `cargo install tako --locked`
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
| `tako init [DIR]`              | Initialize a new project with `tako.toml` |
| `tako dev [DIR]`               | Start local development session           |
| `tako dev stop [NAME] [--all]` | Stop a dev app                            |
| `tako dev ls`                  | List registered dev apps                  |
| `tako doctor`                  | Print local dev diagnostics               |
| `tako deploy [DIR]`            | Build and deploy to servers               |
| `tako delete [DIR]`            | Remove a deployment                       |
| `tako scale <N>`               | Change instance count                     |
| `tako logs [DIR]`              | View or stream remote logs                |
| `tako releases ls`             | List release history                      |
| `tako releases rollback <ID>`  | Roll back to a previous release           |
| `tako servers add`             | Add a server                              |
| `tako servers rm`              | Remove a server                           |
| `tako servers ls`              | List servers                              |
| `tako servers restart <NAME>`  | Restart tako-server                       |
| `tako servers upgrade <NAME>`  | Upgrade tako-server                       |
| `tako servers status`          | Show deployment status                    |
| `tako secrets set <NAME>`      | Set a secret                              |
| `tako secrets rm <NAME>`       | Remove a secret                           |
| `tako secrets ls`              | List secrets                              |
| `tako secrets sync`            | Sync secrets to servers                   |
| `tako secrets key import`      | Import encryption key                     |
| `tako secrets key export`      | Export encryption key                     |
| `tako upgrade`                 | Upgrade the CLI                           |
| `tako help`                    | Show help                                 |
