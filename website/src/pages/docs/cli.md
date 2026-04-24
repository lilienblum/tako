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

| Flag                    | Description                                                                                                                                 |
| ----------------------- | ------------------------------------------------------------------------------------------------------------------------------------------- |
| `--version`             | Print version information and exit. Versions render as `<base>-<sha7>` (the package version `0.0.0` while Tako is pre-v1, plus commit sha). |
| `-v`, `--verbose`       | Switch to an append-only execution transcript with timestamps, log levels, and technical detail.                                            |
| `--ci`                  | Produce deterministic, non-interactive output. No colors, no spinners, no prompts.                                                          |
| `--dry-run`             | Show what a command would do without performing any side effects. Supported by `deploy`, `servers add`, `servers rm`, and `delete`.         |
| `-c`, `--config CONFIG` | Select an explicit app config file instead of `./tako.toml`. The `.toml` suffix is optional.                                                |

## Output Modes

Tako has four output modes you can mix and match:

**Normal** (default) -- concise interactive output. Commands that know their plan upfront render a persistent task tree that shows waiting work as muted `○ ...` rows, updates running rows in place, keeps completed rows visible, and may render reporter-specific error lines under failed tasks. Remaining incomplete work after a fatal failure is marked `Aborted` rather than left pending.

**Verbose** (`--verbose`) -- append-only transcript. Each line is formatted as `HH:MM:SS LEVEL message`. Only prints work as it starts or finishes; upcoming tasks are not pre-rendered. DEBUG-level messages are shown. Prompts still render but as transcript-style lines.

**CI** (`--ci`) -- plain text, no ANSI colors, no spinners, no interactive prompts. Stays transcript-style and emits only current work plus final results. If a required prompt value is missing, the command fails with an actionable error suggesting CLI flags or config.

**CI + Verbose** (`--ci --verbose`) -- detailed append-only transcript with no colors or timestamps. Best for CI/CD logs you want to search later.

On `Ctrl+C`, Tako clears any active prompt or spinner it controls, leaves one blank line, prints `Operation cancelled`, and exits with code `130`.

All status, progress, and log output goes to stderr. Only actual command results (URLs, machine-readable data) go to stdout, so you can pipe results without capturing progress noise.

## Config Selection

App-scoped commands default to `./tako.toml`. Use `-c` / `--config` to point at any other config file:

```bash
tako -c apps/web/staging deploy
tako dev -c configs/preview
```

The selected file's parent directory becomes the project directory. The commands that honor `-c` are: `init`, `dev`, `logs`, `deploy`, `releases`, `delete`, `secrets`, and `scale` (in project context).

If the path you pass doesn't end in `.toml`, Tako appends it automatically -- `tako -c apps/web/staging` resolves to `apps/web/staging.toml`. This lets you keep several configs (one per environment, one per variant, etc.) in a single folder.

---

## `tako init`

Initialize a new `tako.toml` for your project.

```bash
tako init
```

Init walks you through the minimum setup your app needs:

- **App name** -- defaults to the selected config file's parent directory name, sanitized for DNS.
- **Production route** -- the hostname your app serves (defaults to `{name}.example.com`).
- **Runtime** -- detects Bun, Node, Deno, or Go automatically and lets you confirm or override.
- **Preset** -- fetches available presets for the chosen runtime (e.g. `tanstack-start`, `nextjs`). You can also pick the base runtime preset or "custom preset reference", which leaves `preset` commented out.
- **Main entrypoint** -- only prompted when neither adapter inference nor the selected preset provides a default.

The generated `tako.toml` leaves only the essentials uncommented (`name`, `runtime`, `runtime_version`, production `route`) with every other option preserved as commented examples. `runtime_version` is pinned from the locally-installed runtime (`<runtime> --version`). A non-base `preset` is uncommented only when one is selected.

After writing `tako.toml`, init installs the `tako.sh` SDK via the runtime's package manager (for example `bun add tako.sh` for Bun, `go get tako.sh` for Go).

Init also updates `.gitignore` so `.tako/*` is ignored while `.tako/secrets.json` stays tracked. The repo-root `.gitignore` is edited when the project lives inside a git repo; otherwise a local one is created.

If the selected config file already exists, interactive runs ask for confirmation before overwriting. Non-interactive runs leave the file untouched and print `Operation cancelled`. The full "Detected" diagnostics summary is only shown in `--verbose` mode.

---

## `tako dev`

Start or connect to a local development session for the current app.

```bash
tako dev [--variant <VARIANT>]
```

| Flag                  | Description                                                                                |
| --------------------- | ------------------------------------------------------------------------------------------ |
| `--variant <VARIANT>` | Run a DNS variant of the app. Alias: `--var`. Example: `--variant foo` → `myapp-foo.test`. |

`tako dev` is a thin client. It ensures the persistent `tako-dev-server` daemon is running, then registers the selected config file so the daemon manages the process lifecycle, logs, and routing. Running `tako dev` again for the same config attaches to the existing session instead of starting a new one.

**Portless HTTPS URLs**

Tako serves each registered app at `https://{app}.test/`.

- **macOS** -- Tako installs a socket-activated launchd dev proxy that listens on `127.77.0.1:80` and `127.77.0.1:443` and forwards to the daemon on `127.0.0.1:47831`. A one-time sudo prompt explains what will change before the install runs.
- **Linux** -- Tako configures iptables NAT redirect rules and a loopback alias (`127.77.0.1`) so port 443 works without root on the listener side.
- **NixOS** -- Tako prints a `configuration.nix` snippet for the redirect rules instead of running imperative setup.

HTTPS is terminated by the dev daemon using a local CA that is generated on first run and installed into the system trust store (with your consent). The public CA cert is written to `{TAKO_HOME}/ca/ca.crt` for tools that honor `NODE_EXTRA_CA_CERTS`.

**Routing**

When `[envs.development]` doesn't set `routes`, Tako registers `https://{app}.test/`. When you configure explicit routes, they replace the default entirely -- the default `{app}.test` host is not added, leaving that slug free for other apps. Dev routes must use `.test` or `.tako.test` (or a subdomain of either). Wildcard host entries are ignored; dev routing matches exact hostnames only.

`.tako.test` works as a DNS fallback even when your system already owns `/etc/resolver/test`.

**LAN mode**

Pressing `l` in the interactive UI toggles LAN mode. The same dev routes are also served via `.local` aliases and advertised over mDNS (Bonjour on macOS, Avahi on Linux) so phones and tablets resolve them by name. Wildcard routes can't be advertised via mDNS -- Tako surfaces a warning and suggests adding an explicit subdomain route as the fix.

**Interactive keyboard shortcuts**

| Key      | Action                                                        |
| -------- | ------------------------------------------------------------- |
| `r`      | Restart the app process                                       |
| `l`      | Toggle LAN mode                                               |
| `b`      | Background the app -- hand off to the daemon and exit the CLI |
| `Ctrl+C` | Stop the app, unregister routes, and quit                     |

When stdout is not a TTY, dev falls back to plain `println`-style output and no raw mode.

**Auto-behavior**

- Watches `tako.toml` and restarts the app when effective dev env vars change or dev routes change.
- Source hot-reload is runtime-driven (e.g. Bun watch/dev scripts). Tako does not watch source files itself.
- The app starts immediately (1 local instance) and transitions to `idle` after 30 minutes without an attached client. The next HTTP request wakes it back up.
- Dev logs are written to a shared per-app stream and replayed when a new client attaches.

**Examples**

```bash
tako dev
tako dev --variant staging
tako dev -c apps/web/preview
```

---

## `tako dev stop`

Stop a running dev app.

```bash
tako dev stop [NAME] [--all]
```

| Argument / Flag | Description                                                                                   |
| --------------- | --------------------------------------------------------------------------------------------- |
| `NAME`          | Name of the registered app to stop. When omitted, stops the app for the selected config file. |
| `--all`         | Stop every registered dev app.                                                                |

**Examples**

```bash
tako dev stop
tako dev stop myapp
tako dev stop --all
```

---

## `tako dev ls`

List every registered dev app with its status (`running`, `idle`, `stopped`).

```bash
tako dev ls
```

Alias: `tako dev list`.

---

## `tako doctor`

Print a local diagnostic report and exit.

```bash
tako doctor
```

Reports dev daemon listen info, DNS resolver status, and platform-specific preflight:

- **macOS preflight** -- dev proxy install status, dev boot-helper status, loopback alias presence, launchd load status, and TCP reachability on the dev proxy's loopback `:80` and `:443`.
- **Linux preflight** -- iptables redirect rule presence, loopback alias presence, and systemd-resolved configuration.

If the dev daemon is not running, doctor reports `status: not running` with a hint to start `tako dev` and exits successfully (since `doctor` is a reporting tool, not a gate).

---

## `tako deploy`

Build the current app and deploy it to the servers mapped to an environment.

```bash
tako deploy [--env <ENV>] [-y|--yes]
```

| Flag          | Description                                                                                      |
| ------------- | ------------------------------------------------------------------------------------------------ |
| `--env <ENV>` | Target environment. Defaults to `production`. Must be declared in `tako.toml` (`[envs.<name>]`). |
| `-y`, `--yes` | Skip the production confirmation prompt.                                                         |

`development` is reserved for `tako dev` and cannot be used with `tako deploy`. The target environment must define `route` or `routes`.

**Confirmation**

Interactive `production` deploys require explicit confirmation unless `--yes` is provided. `--dry-run` auto-skips the prompt because no side effects run.

**Deploy flow (summary)**

1. Pre-deployment validation (secrets present, valid target metadata per server).
2. Resolve source bundle root (git root when available, otherwise the app directory).
3. Resolve `main` from `tako.toml`, preset defaults, or JS index fallbacks.
4. Resolve app preset, fetching unpinned official aliases from `master`.
5. Copy the project into `.tako/build` (respecting `.gitignore`) and symlink `node_modules/` in.
6. Run `[[build_stages]]` → `[build]` → runtime default, in that precedence order. Each stage runs `install` then `run`. Merge asset roots into `public/`.
7. Archive the build dir (excluding `node_modules/`) into a target-specific artifact and cache it locally.
8. Deploy in parallel to every mapped server: upload, extract, query/send secrets, then rolling update.
9. Update `current` symlink, prune releases older than 30 days, and report.

Interactive progress shows tasks and sub tasks as a live tree: `Connecting` and `Building` start together after planning, then one `Deploying to <server>` task per target server with sub tasks for `Uploading`, `Preparing`, and `Starting`. Verbose and CI deploys stay transcript-style.

**Version naming**

| Situation                | Version format                                      |
| ------------------------ | --------------------------------------------------- |
| Clean git tree           | `{commit}` (e.g. `abc1234`)                         |
| Dirty working tree       | `{commit}_{source_hash8}` (e.g. `abc1234_d9f01a2b`) |
| No git commit or no repo | `nogit_{source_hash8}`                              |

**Target selection**

- If `[envs.<env>].servers` is set in `tako.toml`, deploy targets those servers.
- For `production` with no mapping, a single configured server is auto-selected and persisted into `[envs.production].servers`. With multiple servers and an interactive terminal, Tako prompts you to pick one and persists the choice.
- If no servers are configured, deploy offers to run the add-server wizard.

**Rolling update (per server)**

Start one new instance, wait up to 30s for health, add it to the LB, drain the old instance (30s), repeat until all desired instances are replaced, update `current`, and clean up releases older than 30 days. If any instance fails, new ones are killed and the old ones keep serving. Partial failures across multiple servers are reported at the end.

**Examples**

```bash
tako deploy
tako deploy --env staging
tako deploy --env production -y
tako --dry-run deploy --env staging
```

---

## `tako delete`

Remove a deployed app from exactly one environment/server target.

```bash
tako delete [--env <ENV>] [--server <SERVER>] [-y|--yes]
```

| Flag                | Description                                                                          |
| ------------------- | ------------------------------------------------------------------------------------ |
| `--env <ENV>`       | Environment declared in `tako.toml` (`[envs.<name>]`). `development` is not allowed. |
| `--server <SERVER>` | Server name from `config.toml` `[[servers]]`.                                        |
| `-y`, `--yes`       | Skip the confirmation prompt. Required in non-interactive terminals.                 |

Target resolution:

- In project context with neither flag, Tako lists deployed targets (e.g. `production from hkg`) and asks you to pick one.
- With only `--env`, Tako prompts for a matching server; with only `--server`, it prompts for a matching environment.
- With both flags, Tako skips discovery and goes straight to the confirmation step.
- Outside project context, Tako discovers deployed targets across configured servers; in non-interactive mode, flags must identify a single target.

The confirmation prompt always names the app, environment, and server. Deletes are idempotent -- running them again when state is already gone is safe.

Aliases: `tako rm`, `tako remove`, `tako undeploy`, `tako destroy`.

**Examples**

```bash
tako delete
tako delete --env staging --server hkg -y
tako rm --env production --server la
```

---

## `tako scale`

Change the desired instance count for a deployed app.

```bash
tako scale <N> [--env <ENV>] [--server <SERVER>] [--app <APP>]
```

| Argument / Flag     | Description                                                                         |
| ------------------- | ----------------------------------------------------------------------------------- |
| `<N>`               | Desired instance count per targeted server.                                         |
| `--env <ENV>`       | Required when `--server` is omitted; scales every server in `[envs.<env>].servers`. |
| `--server <SERVER>` | Scale only that server. In project context, defaults `--env` to `production`.       |
| `--app <APP>`       | Required outside project context. Accepts `<name>` with `--env`, or `<name>/<env>`. |

Resolution rules:

- In project context, Tako resolves the app name from the selected config file (or the config's parent directory when top-level `name` is unset).
- Outside project context, `--app` is required. Use `--app <app> --env <env>` or the combined form `--app <app>/<env>`.
- When both `--env` and `--server` are provided, the server must belong to that environment.

The desired count is persisted in server-side runtime state, so it survives deploys, rollbacks, and server restarts. Scaling to `0` drains and stops excess instances after in-flight requests finish or the drain timeout fires.

**Examples**

```bash
tako scale 3
tako scale 2 --env staging
tako scale 0 --server hkg
tako scale 5 --app myapp/production
```

---

## `tako logs`

View or stream application logs from every server in an environment.

```bash
tako logs [--env <ENV>] [--tail] [--days <N>]
```

| Flag          | Description                                                                        |
| ------------- | ---------------------------------------------------------------------------------- |
| `--env <ENV>` | Environment to query. Defaults to `production`; must exist in the selected config. |
| `--tail`      | Stream logs continuously until `Ctrl+C`. Conflicts with `--days`.                  |
| `--days <N>`  | History window in days for the default mode. Defaults to `3`.                      |

Logs from every mapped server are fetched in parallel. Lines are prefixed with `[server-name]` when multiple servers are present. Consecutive identical messages are deduplicated with a `... and N more` suffix.

**History mode (default)**

Shows the last `N` days across all servers, sorted by timestamp. Displayed in `$PAGER` (default `less -R`) when interactive, otherwise written to stdout.

**Streaming mode (`--tail`)**

Streams new lines as they arrive. Same dedup behavior as history mode. `Ctrl+C` exits with code `130`.

If `production` has no servers configured and the terminal is interactive, logs offers to run the add-server wizard.

**Examples**

```bash
tako logs
tako logs --env staging --days 7
tako logs --tail
tako logs --env staging --tail
```

---

## `tako releases ls`

List release/build history for the current app across mapped environment servers.

```bash
tako releases ls [--env <ENV>]
```

Alias: `tako releases list`.

| Flag          | Description                                                               |
| ------------- | ------------------------------------------------------------------------- |
| `--env <ENV>` | Environment to list. Defaults to `production`; must exist in `tako.toml`. |

Output is release-centric, sorted newest-first. Each entry has two lines:

- Line 1: release/build id and deployed timestamp. When deployed within the last 24 hours, a muted relative hint like `{3h ago}` is appended.
- Line 2: commit message plus a cleanliness marker (`[clean]`, `[dirty]`, or `[unknown]`).

`[current]` marks the release the server's `current` symlink points to. Older releases may show `[unknown]` or `(no commit message)` if they predate current metadata.

---

## `tako releases rollback`

Roll the current environment back to a previous release.

```bash
tako releases rollback <RELEASE_ID> [--env <ENV>] [-y|--yes]
```

| Argument / Flag | Description                                    |
| --------------- | ---------------------------------------------- |
| `<RELEASE_ID>`  | Release/build id shown by `tako releases ls`.  |
| `--env <ENV>`   | Defaults to `production`.                      |
| `-y`, `--yes`   | Skip the confirmation prompt for `production`. |

Rollback reuses the current app's routes, env, secrets, and scaling config, then runs the standard rolling update flow to switch to the target release. It runs per server in parallel; successful servers remain rolled back even if others fail.

**Example**

```bash
tako releases rollback abc1234 --env production
```

---

## `tako servers add`

Register a remote server in global `config.toml` under `[[servers]]`.

```bash
tako servers add [HOST] [--name <NAME>] [--description <TEXT>] [--port <PORT>] [--no-test]
```

| Argument / Flag        | Description                                                                        |
| ---------------------- | ---------------------------------------------------------------------------------- |
| `HOST`                 | SSH host. When omitted in an interactive terminal, launches the add-server wizard. |
| `--name <NAME>`        | Required when `HOST` is provided (there is no implicit default to the hostname).   |
| `--description <TEXT>` | Optional human-readable metadata shown in `tako servers ls`.                       |
| `--port <PORT>`        | SSH port. Defaults to 22.                                                          |
| `--no-test`            | Skip SSH connection checks and target metadata detection.                          |

**Wizard flow**

When `HOST` is omitted, Tako prompts for host, required server name, optional description, and SSH port, then asks a final `Looks good?` confirmation. Choosing `No` restarts the wizard.

The wizard offers `Tab` autocomplete from existing servers and persisted CLI history (`history.toml`). Host-related suggestions rank first for name/port prompts, then global history. Successful adds are recorded to `history.toml`.

**SSH and detection**

Tako connects as the `tako` user and tests the SSH connection before writing config. While connected, it detects and stores the target's `arch` and `libc` in the `[[servers]]` entry so deploys can pick the correct artifact. When `--no-test` is used, SSH checks and detection are skipped; deploys will fail for that server until metadata is captured by re-adding with checks enabled.

If `tako-server` isn't installed on the target, Tako warns and expects you to install it manually.

Re-running with the same `host`/`name`/`port` is idempotent -- Tako reports "already configured" and succeeds.

**Examples**

```bash
tako servers add
tako servers add la.example.com --name la
tako servers add 10.0.0.4 --name backup --description "cold standby" --port 2222
tako servers add la.example.com --name la --no-test
```

---

## `tako servers rm`

Remove a server entry from `config.toml`.

```bash
tako servers rm [NAME]
```

Aliases: `tako servers remove`, `tako servers delete`.

When `NAME` is omitted in an interactive terminal, Tako opens a server selector. In non-interactive mode, `NAME` is required. Tako warns that any project referencing this server will fail before confirming removal.

**Example**

```bash
tako servers rm la
```

---

## `tako servers ls`

List every configured server from `config.toml`.

```bash
tako servers ls
```

Alias: `tako servers list`.

Output is a simple table with Name, Host, Port, and optional Description. When no servers are configured, Tako prints a hint to run `tako servers add`.

---

## `tako servers status`

Show a single snapshot of global deployment status across configured servers.

```bash
tako servers status
```

Alias: `tako servers info`.

Output groups one block per server, with nested blocks per running build:

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
```

Each app block uses tree connectors (`┌`, `│`, `└`) and shows `app-name (environment) state`. State is color-coded: `running` is success, `idle` is muted, `deploying` and `stopped` are warnings, `error` is an error. Timestamps render in the user's local time zone.

`tako servers status` does not require `tako.toml` and runs from any directory. It prints one snapshot and exits. If no servers are configured and the terminal is interactive, status offers to run the add-server wizard.

---

## `tako servers restart`

Reload or restart `tako-server` on a host.

```bash
tako servers restart <NAME> [--force]
```

| Flag      | Description                                                               |
| --------- | ------------------------------------------------------------------------- |
| `--force` | Full service restart. May cause brief downtime for all apps on that host. |

**Default (reload)** -- sends `SIGHUP` via `systemctl reload tako-server` (systemd) or `rc-service tako-server reload` (OpenRC). The current process spawns a replacement, the new process takes over the management socket and listener ports, then the old process drains and exits. Use this for normal config refresh and control-plane restarts.

**`--force`** -- `systemctl restart tako-server` or `rc-service tako-server restart`. Use for recovery when graceful reload isn't appropriate.

App routing, config, and per-app data survive reloads, restarts, and crashes because `tako-server` persists runtime registration in SQLite. On systemd, `KillMode=control-group` and `TimeoutStopSec=30min` allow app processes up to 30 minutes of graceful shutdown before forced termination; OpenRC uses `retry="TERM/1800/KILL/5"` for the same effect.

**Example**

```bash
tako servers restart la
tako servers restart la --force
```

---

## `tako servers upgrade`

Upgrade `tako-server` on one or all configured servers via service-manager reload.

```bash
tako servers upgrade [SERVER_NAME]
```

When `SERVER_NAME` is omitted, every configured server is upgraded.

The upgrade uses a signed checksum manifest for integrity: the CLI verifies the `tako-server-sha256s.txt` release manifest with an embedded public key, selects the expected SHA-256 for the target archive, and the remote host verifies that SHA-256 before extracting the archive into `/usr/local/bin/tako-server`.

**Flow**

1. Verify `tako-server` is active on the host.
2. Install the new server binary (signed SHA-256 check on both sides).
3. Acquire the durable single-owner upgrade lock (`enter_upgrading`) and set server mode to `upgrading`, which temporarily rejects mutating management commands (`deploy`, `stop`, `delete`, `update-secrets`).
4. Reload via `systemctl reload tako-server` or `rc-service tako-server reload` (both send `SIGHUP` for graceful reload, starting the replacement before the old process exits).
5. Wait for the primary management socket to report ready.
6. Release upgrade mode (`exit_upgrading`).

**Rollback and failure modes**

- The previous on-disk `tako-server` binary is kept until the replacement process reports ready. If readiness doesn't arrive, the previous binary is restored.
- If the reload was sent but the socket didn't become ready in time, Tako warns that upgrade mode may still be enabled until the primary recovers.

**Custom download sources**

Set `TAKO_DOWNLOAD_BASE_URL` to point upgrades at a custom mirror. Non-HTTPS URLs are rejected unless `TAKO_ALLOW_INSECURE_DOWNLOAD_BASE=1` is set (intended for local testing only).

**Examples**

```bash
tako servers upgrade
tako servers upgrade la
TAKO_DOWNLOAD_BASE_URL=https://mirror.example.com tako servers upgrade
```

---

## `tako servers implode`

Remove `tako-server` and all its data from a remote server.

```bash
tako servers implode [NAME] [-y|--yes]
```

Alias: `tako servers uninstall`.

When `NAME` is omitted in an interactive terminal, Tako prompts you to pick from the configured servers.

**What gets removed**

- `tako-server` and `tako-server-standby` services (systemd + OpenRC).
- systemd service files, drop-ins, and OpenRC init scripts. `systemctl daemon-reload` runs afterwards on systemd hosts.
- Binaries: `/usr/local/bin/tako-server`, `tako-server-service`, `tako-server-install-refresh`.
- Data directory (`/opt/tako/`) and the management socket directory (`/var/run/tako/`).
- The server's entry in your local `config.toml`.

Before touching anything, Tako displays exactly what will be removed and asks for confirmation (skipped with `-y`).

**Example**

```bash
tako servers implode la
tako servers uninstall nyc -y
```

---

## `tako servers setup-wildcard`

Configure DNS-01 wildcard certificate support on every configured server.

```bash
tako servers setup-wildcard [-e|--env <ENV>]
```

| Flag              | Description                                                                                 |
| ----------------- | ------------------------------------------------------------------------------------------- |
| `-e`, `--env ENV` | Only configure servers mapped to the given environment. Defaults to all configured servers. |

The wizard prompts for DNS provider and credentials, verifies them locally against the provider API, and applies the configuration to each server in parallel:

- Writes credentials to `/opt/tako/dns-credentials.env` (mode `0600`).
- Merges `dns.provider` into `/opt/tako/config.json`.
- Writes a systemd drop-in that injects the env file and restarts `tako-server`.
- Polls `tako-server` until the provider reports active.

`tako-server` downloads and installs `lego` on-demand the first time it issues a wildcard certificate.

---

## `tako secrets set`

Set or update a secret for an environment.

```bash
tako secrets set <NAME> [--env <ENV>] [--sync]
```

Alias: `tako secrets add`.

| Argument / Flag | Description                                                                 |
| --------------- | --------------------------------------------------------------------------- |
| `<NAME>`        | Secret name.                                                                |
| `--env <ENV>`   | Target environment. Defaults to `production`.                               |
| `--sync`        | Sync all environment secrets to servers immediately after the local change. |

In interactive terminals, Tako prompts for the value with masked input. In non-interactive mode, it reads a single line from stdin. Values are encrypted locally in `.tako/secrets.json` using `keys/{env}` (created if missing).

`--sync` triggers a rolling restart of running HTTP instances so fresh processes pick up the new values via fd 3.

**Examples**

```bash
tako secrets set DATABASE_URL
tako secrets set STRIPE_KEY --env staging --sync
echo "$VALUE" | tako secrets set API_TOKEN
```

---

## `tako secrets rm`

Remove a secret.

```bash
tako secrets rm <NAME> [--env <ENV>] [--sync]
```

Aliases: `tako secrets remove`, `tako secrets delete`, `tako secrets del`.

Removes the secret from `.tako/secrets.json`. Omitting `--env` removes it from every environment. With `--sync`, Tako pushes the change to the target environment (or every environment if `--env` is also omitted).

**Example**

```bash
tako secrets rm OLD_API_KEY
tako secrets rm STRIPE_KEY --env staging --sync
```

---

## `tako secrets ls`

List every secret and show which environments have it set.

```bash
tako secrets ls
```

Aliases: `tako secrets list`, `tako secrets show`.

Output is a presence table: one row per secret, one column per declared environment. Values are never displayed. Tako warns about secrets that appear in some environments but not others.

---

## `tako secrets sync`

Push local secrets to the servers mapped to each environment.

```bash
tako secrets sync [--env <ENV>]
```

Source of truth is always `.tako/secrets.json`. By default, every environment declared in `tako.toml` is processed. With `--env`, only that environment runs.

For each target environment, Tako decrypts with `keys/{env}` and sends `update_secrets` to `tako-server`. Secrets never touch the server's disk as plaintext -- `tako-server` stores them encrypted in SQLite and reconciles the app's workflow runtime plus rolling-restarts HTTP instances so new processes receive the updated values via fd 3.

Tako shows a spinner with the total target server count and reports elapsed time on completion. Environments with no mapped servers are skipped with a warning. If no servers are configured and the terminal is interactive, sync offers to run the add-server wizard.

**Examples**

```bash
tako secrets sync
tako secrets sync --env staging
```

---

## `tako secrets key derive`

Derive an environment encryption key from a passphrase.

```bash
tako secrets key derive [--env <ENV>]
```

Writes `keys/{env}` (defaults to `production`). Use this to share a shared key with teammates via a passphrase they can re-enter.

---

## `tako secrets key export`

Copy an environment encryption key to the clipboard.

```bash
tako secrets key export [--env <ENV>]
```

Reads `keys/{env}` (defaults to `production`). Useful for transferring a machine-generated key to another teammate securely.

---

## `tako upgrade`

Upgrade the local `tako` CLI.

```bash
tako upgrade
```

Upgrade strategy:

- **Homebrew installs** -- runs `brew upgrade tako`.
- **Default / fallback** -- re-runs the hosted installer (`https://tako.sh/install.sh`) via `curl` or `wget`.

`tako upgrade` upgrades only the local CLI. For upgrading `tako-server` on remote hosts, use [`tako servers upgrade`](#tako-servers-upgrade).

---

## `tako implode`

Remove the local Tako CLI and every trace of local Tako data.

```bash
tako implode [-y|--yes]
```

Alias: `tako uninstall`.

**What gets removed**

- **User-level**: config directory, data directory, CLI binaries (`tako`, `tako-dev-server`, and `tako-dev-proxy` on macOS).
- **System-level (requires sudo)** -- platform-specific services and config installed by `tako dev`:
  - **macOS** -- dev proxy LaunchDaemons (`sh.tako.dev-proxy`, `sh.tako.dev-bootstrap`), `/Library/Application Support/Tako/`, `/etc/resolver/test`, `/etc/resolver/tako.test`, the CA certificate in the system keychain, and the loopback alias `127.77.0.1`.
  - **Linux** -- `tako-dev-redirect.service`, the systemd-resolved drop-in (`tako-dev.conf`), the CA certificate in the system trust store, iptables NAT redirect rules, and the loopback alias `127.77.0.1`.

If nothing exists to remove, Tako reports "nothing to remove" and exits. Otherwise it displays every item (including system-level ones that require sudo) and asks for confirmation (skipped with `-y`). A best-effort dev server stop unregisters all dev apps first, then Tako removes system items via `sudo`, then user-level directories and binaries. Partial removals are reported with the items that couldn't be deleted.

---

## `tako typegen`

Generate typed accessors for the current project.

```bash
tako typegen
```

**Output files**

- `tako.gen.ts` -- runtime state + typed `Secrets` interface for JS/TS apps.
- `tako_secrets.go` -- typed secret accessors for Go apps.

**Placement (JS/TS)**

- If an existing `tako.gen.ts` is found, it's overwritten in place.
- Otherwise `tako.gen.ts` goes inside `src/` or `app/` when either exists, or the project root as a last resort.
- Legacy `tako.d.ts` files left over from the pre-v0-global design are removed.

**Scaffolding**

For JS/TS projects, if `channels/` or `workflows/` directories exist, typegen also:

- Scaffolds a `demo.ts` in empty dirs.
- Adds a missing default `defineChannel(...)` / `defineWorkflow(...)` export to existing definition files that have no default export yet.

---

## `tako version`

Show version information (same output as `--version`).

```bash
tako version
```

Versions render as `<base>-<sha7>`: the package version (always `0.0.0` while Tako is pre-v1) plus the 7-character source commit.

---

## Quick reference

| Command                       | Description                                                 |
| ----------------------------- | ----------------------------------------------------------- |
| `tako init`                   | Create a `tako.toml` for the current project.               |
| `tako dev`                    | Start or attach to a local dev session for the current app. |
| `tako dev stop`               | Stop a running dev app (or `--all`).                        |
| `tako dev ls`                 | List every registered dev app.                              |
| `tako doctor`                 | Print a local diagnostic report.                            |
| `tako deploy`                 | Build and deploy to an environment's mapped servers.        |
| `tako delete`                 | Remove a deployed app from one env/server target.           |
| `tako scale`                  | Change the desired instance count per server.               |
| `tako logs`                   | View or stream logs across an environment's servers.        |
| `tako releases ls`            | List release history for an environment.                    |
| `tako releases rollback`      | Roll back to a previous release.                            |
| `tako servers add`            | Register a remote server.                                   |
| `tako servers rm`             | Remove a server entry.                                      |
| `tako servers ls`             | List configured servers.                                    |
| `tako servers status`         | Snapshot deployment status across servers.                  |
| `tako servers restart`        | Reload or force-restart `tako-server` on a host.            |
| `tako servers upgrade`        | Upgrade `tako-server` on one or all configured hosts.       |
| `tako servers implode`        | Remove `tako-server` and all data from a remote host.       |
| `tako servers setup-wildcard` | Configure DNS-01 wildcard certificate support.              |
| `tako secrets set`            | Create or update a secret.                                  |
| `tako secrets rm`             | Remove a secret.                                            |
| `tako secrets ls`             | Show which environments each secret is set in.              |
| `tako secrets sync`           | Push local secrets to mapped servers.                       |
| `tako secrets key derive`     | Derive an environment key from a passphrase.                |
| `tako secrets key export`     | Export an environment key to the clipboard.                 |
| `tako upgrade`                | Upgrade the local CLI.                                      |
| `tako implode`                | Remove the local CLI and all Tako data.                     |
| `tako typegen`                | Emit typed accessors (`tako.gen.ts`, `tako_secrets.go`).    |
| `tako version`                | Show version information.                                   |
