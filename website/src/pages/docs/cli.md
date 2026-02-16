---
layout: ../../layouts/DocsLayout.astro
title: Tako Docs - CLI Reference
heading: CLI Reference
current: cli
---

# CLI Reference

Your quick map of `tako` commands, flags, and common patterns.

## Global Usage

```bash
tako [--version] [-v|--verbose] <command> [args]
```

Global flags:

- `--version`: print version and exit.
- `-v`, `--verbose`: enable verbose output.

Directory selection is command-scoped:

- `tako init [DIR]`
- `tako dev [DIR]`
- `tako deploy [DIR]`
- `tako delete [DIR]`

## Top-Level Commands

- `tako init [--force] [DIR]`: initialize `tako.toml` in a project.
- `tako help`: show all commands with brief descriptions.
- `tako upgrade`: upgrade local CLI using the hosted installer.
- `tako logs [--env <ENV>]`: stream remote logs (default env: `production`).
- `tako dev [--tui | --no-tui] [DIR]`: run local development mode.
- `tako doctor`: print local dev diagnostics (DNS, socket, listener, leases).
- `tako deploy [--env <ENV>] [-y|--yes] [DIR]`: build and deploy app.
- `tako delete [--env <ENV>] [-y|--yes] [DIR]`: delete deployed app.
- `tako servers <subcommand>`: manage server inventory and server runtime actions.
- `tako secrets <subcommand>`: manage project secrets and keys.

## `servers` Subcommands

`tako servers add`:

```bash
tako servers add [HOST] [--name <NAME>] [--description <TEXT>] [--port <PORT>] [--no-test]
```

`tako servers rm`:

```bash
tako servers rm [NAME]
```

`tako servers ls`:

```bash
tako servers ls
```

`tako servers restart`:

```bash
tako servers restart <NAME>
```

`tako servers reload`:

```bash
tako servers reload <NAME>
```

`tako servers upgrade`:

```bash
tako servers upgrade <NAME>
```

`tako servers status`:

```bash
tako servers status
```

Notes:

- `tako servers add`:
  - If `HOST` is omitted, Tako launches an interactive setup wizard.
  - If `HOST` is provided, `--name` is required.
  - `--port` defaults to `22`.
  - By default, tests SSH connection before adding and connects as user `tako`.
  - `--no-test` skips SSH checks and target detection.
- `tako servers rm` aliases: `remove`, `delete`.
- `tako servers ls` alias: `list`.
- `tako servers status` prints a single global deployment/runtime snapshot across configured servers.

## `secrets` Subcommands

`tako secrets set`:

```bash
tako secrets set <NAME> [--env <ENV>]
```

`tako secrets rm`:

```bash
tako secrets rm <NAME> [--env <ENV>]
```

`tako secrets ls`:

```bash
tako secrets ls
```

`tako secrets sync`:

```bash
tako secrets sync [--env <ENV>]
```

`tako secrets key import`:

```bash
tako secrets key import [--env <ENV>]
```

`tako secrets key export`:

```bash
tako secrets key export [--env <ENV>]
```

Notes:

- `tako secrets set` defaults to `--env production` if omitted.
- `tako secrets rm`:
  - with `--env`: removes from one environment.
  - without `--env`: removes from all environments.
- `tako secrets ls` alias: `list`.
- `tako secrets rm` aliases: `remove`, `delete`.
- `tako secrets sync`:
  - with `--env`: syncs only that environment.
  - without `--env`: syncs all environments declared in `tako.toml`.
- `tako secrets key import/export` default to `production` when `--env` is omitted.

## Common Examples

Initialize in current directory:

```bash
tako init
```

Run local app with non-interactive output:

```bash
tako dev --no-tui
```

Deploy staging and skip confirmation:

```bash
tako deploy --env staging --yes
```

Remove production app:

```bash
tako delete --env production
```

Add a server and verify SSH:

```bash
tako servers add 203.0.113.10 --name production
```

Set a production secret:

```bash
tako secrets set DATABASE_URL --env production
```

Sync production secrets:

```bash
tako secrets sync --env production
```
