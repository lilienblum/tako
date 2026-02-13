# CLI Reference

Your quick map of `tako` commands, flags, and common patterns.

## Related Docs

- [Install](/docs/install): install, upgrade, and uninstall commands.
- [Quickstart](/docs/quickstart): first deploy flow.
- [tako.toml Reference](/docs/tako-toml): config file options and merge rules.
- [Operations](/docs/operations): diagnostics and incident response runbook.

## Global Usage

```bash
tako [--version] [-v|--verbose] <command> [args]
```

Global flags:

- `--version`: print version and exit.
- `-v`, `--verbose`: enable verbose output.

## Top-Level Commands

- `tako init [--force] [DIR]`: initialize `tako.toml` in a project.
- `tako logs [--env <ENV>]`: stream remote logs (default env: `production`).
- `tako dev [--tui | --no-tui] [DIR]`: run local development mode.
- `tako doctor`: print local dev diagnostics (DNS, socket, listener, leases).
- `tako deploy [--env <ENV>] [-y|--yes] [DIR]`: build and deploy app.
- `tako delete [--env <ENV>] [-y|--yes] [DIR]`: delete deployed app.
  - Aliases: `tako rm`, `tako remove`, `tako undeploy`, `tako destroy`
- `tako servers <subcommand>`: manage server inventory and server runtime actions.
- `tako secrets <subcommand>`: manage project secrets and keys.
- `tako upgrade`: upgrade local CLI using the hosted installer.

## `servers` Subcommands

```bash
tako servers add [HOST] [--name <NAME>] [--description <TEXT>] [--port <PORT>] [--no-test]
tako servers rm [NAME]
tako servers ls
tako servers restart <NAME>
tako servers reload <NAME>
tako servers status [NAME]
```

Notes:

- `tako servers add`:
  - If `HOST` is omitted, Tako launches an interactive setup wizard.
  - If `HOST` is provided, `--name` is required.
  - `--port` defaults to `22`.
  - `--no-test` skips SSH connection testing.
- `tako servers rm` aliases: `remove`, `delete`.
- `tako servers ls` alias: `list`.
- `tako servers status` without `NAME` shows global deployment/runtime status across configured servers.
- `tako servers status <NAME>` shows install/service/app summary for one server.

## `secrets` Subcommands

```bash
tako secrets set <NAME> [--env <ENV>]
tako secrets rm <NAME> [--env <ENV>]
tako secrets ls
tako secrets sync [--env <ENV>]
tako secrets key import [--env <ENV>]
tako secrets key export [--env <ENV>]
```

Notes:

- `tako secrets set` defaults to `--env production` if omitted.
- `tako secrets rm`:
  - with `--env`: removes from one environment.
  - without `--env`: removes from all environments (with confirmation).
- `tako secrets ls` alias: `list`.
- `tako secrets rm` aliases: `remove`, `delete`.
- `tako secrets key import/export` default to `production` when `--env` is omitted.

## Common Examples

```bash
# initialize in current directory
tako init

# run local app with non-interactive output
tako dev --no-tui

# deploy staging and skip confirmation
tako deploy --env staging --yes

# remove production app
tako delete --env production

# add a server and verify SSH
tako servers add 203.0.113.10 --name production

# set + sync secrets for production
tako secrets set DATABASE_URL --env production
tako secrets sync --env production
```
