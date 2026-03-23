---
name: tako-cli
description: >-
  Tako CLI commands: init, dev, deploy, secrets, typegen, scale, logs,
  rollback, servers, doctor.
type: framework
library: tako.sh
library_version: "0.0.1"
sources:
  - lilienblum/tako:tako/src/cli.rs
---

# Tako CLI

Command-line tool for developing and deploying Tako apps.

## Project Setup

### `tako init`

Initialize a new Tako project. Auto-detects runtime (Go, Bun, Node, Deno) from project files (`go.mod`, `package.json`, `deno.json`).

```bash
tako init
```

Creates `tako.toml`, installs the SDK (`go get tako.sh` or `npm install tako.sh`), and prompts for app name and production route.

### `tako doctor`

Print a local diagnostic report.

```bash
tako doctor
```

## Development

### `tako dev`

Start local development server with built-in HTTPS proxy and `.tako.test` domain.

```bash
tako dev
tako dev --variant staging    # myapp-staging.tako.test
tako dev stop [name]          # stop a running dev app
tako dev ls                   # list registered dev apps
```

Features:

- Local HTTPS via auto-generated certificates
- `.tako.test` domain resolution
- File watching and automatic restart
- Hot reload passthrough for framework dev servers

## Secrets

### `tako secrets set <name> [value] [--env <name>] [--sync]`

Add or update a secret. Prompts for value if omitted. Alias: `add`.

```bash
tako secrets set DATABASE_URL "postgres://..."
tako secrets set API_KEY
tako secrets set API_KEY --sync   # set and sync to servers immediately
```

### `tako secrets rm <name> [--env <name>] [--sync]`

Delete a secret. Aliases: `remove`, `delete`.

### `tako secrets ls`

List all secret names.

### `tako secrets sync [--env <name>]`

Sync secrets to servers. Automatically resolves which servers to sync to based on environment configuration.

### `tako secrets key derive [--env <name>]`

Derive a key from a passphrase (for sharing with teammates).

### `tako secrets key export [--env <name>]`

Export the encryption key.

## Code Generation

### `tako typegen`

Generate typed secret accessors from `.tako/secrets.json`.

```bash
tako typegen
```

- **Go**: generates `tako_secrets.go` with a typed `Secrets` struct
- **JavaScript/TypeScript**: generates `tako.d.ts` with typed declarations

Run after adding or removing secrets. Commit the generated file.

## Deployment

### `tako deploy [--env <env>] [--yes]`

Build locally and deploy to a Tako server.

```bash
tako deploy
tako deploy --env staging
tako deploy --yes             # skip confirmation
```

### `tako delete [--env <env>] [--server <name>] [--yes]`

Delete a deployed app. Aliases: `rm`, `remove`, `undeploy`, `destroy`.

### `tako scale <instances> [--env <env>] [--server <name>]`

Change instance count for a deployed app.

```bash
tako scale 3
tako scale 1 --env staging
```

## Releases

### `tako releases ls [--env <env>]`

List deployment history.

### `tako releases rollback <release-id> [--env <env>] [--yes]`

Rollback to a previous release.

## Logs

### `tako logs [--env <env>] [--tail] [--days N]`

View remote logs. Stream with `--tail` or fetch historical logs.

```bash
tako logs --tail
tako logs --days 3
```

## Servers

### `tako servers add [<host>] [--description <text>]`

Add a deployment server.

### `tako servers ls`

List configured servers.

### `tako servers status`

Show status of all servers and deployed apps.

### `tako servers rm [<name>]`

Remove a server. Alias: `delete`.

### `tako servers upgrade [<name>] [--canary|--stable]`

Upgrade Tako on a server.

## CLI Management

### `tako --version`

Show CLI version.

### `tako upgrade [--canary|--stable]`

Upgrade the Tako CLI itself.

### `tako implode [--yes]`

Uninstall Tako and remove all local data.

## Global Flags

| Flag               | Purpose                                           |
| ------------------ | ------------------------------------------------- |
| `--verbose` / `-v` | Verbose output                                    |
| `--ci`             | Non-interactive, deterministic output             |
| `--dry-run`        | Show what would happen without side effects       |
| `--config` / `-c`  | Use explicit config file instead of `./tako.toml` |
