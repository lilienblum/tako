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

- `--version`: print version and exit (`<semver>` on stable builds, `<semver>-canary-<sha7>` on canary builds).
- `-v`, `--verbose`: enable verbose output.

CLI output conventions:

- default output is concise and user-focused
- `--verbose` adds technical detail (paths, target metadata, per-host transport context)
- in interactive terminals, long-running steps show spinner progress

Directory selection is command-scoped:

- `tako init [DIR]`
- `tako dev [DIR]`
- `tako deploy [DIR]`
- `tako delete [DIR]`

## Top-Level Commands

- `tako init [--force] [--runtime <bun|node|deno>] [DIR]`: initialize `tako.toml` in a project (prompts for app `name` (recommended unique per server), production `route`, runtime, and preset selection when family presets are available); detailed "Detected" summary is shown in verbose mode.
- `tako help`: show all commands with brief descriptions.
- `tako upgrade [--canary|--stable]`: upgrade local CLI installation.
- `tako logs [--env <ENV>]`: stream remote logs (default env: `production`).
- `tako dev [DIR]`: run local development mode.
- `tako dev stop [NAME] [--all]`: stop a running/idle dev app (defaults to current directory's app).
- `tako dev ls`: list all registered dev apps with status.
- `tako doctor`: print local dev diagnostics (DNS, socket, listener, apps, and local forwarding preflight checks).
- `tako deploy [--env <ENV>] [-y|--yes] [DIR]`: build and deploy app.
- `tako releases <subcommand>`: list release history and roll back to a previous release.
- `tako delete [--env <ENV>] [-y|--yes] [DIR]`: delete deployed app (single-server interactive deletes show spinner progress; multi-server deletes use line-based status).
- `tako servers <subcommand>`: manage server inventory and server runtime actions.
- `tako secrets <subcommand>`: manage project secrets and keys.

## `upgrade`

```bash
tako upgrade
tako upgrade --canary
tako upgrade --stable
```

Notes:

- direct canary install endpoint: `curl -fsSL https://tako.sh/install-canary | sh`
- local CLI upgrade strategy is install-aware:
  - Homebrew installs use `brew upgrade tako`
  - Cargo installs under `~/.cargo/bin/tako` use `cargo install tako --locked`
  - fallback uses hosted installer script (`https://tako.sh/install`) via `curl`/`wget` (installs both `tako` and `tako-dev-server`)
  - `--canary` forces hosted installer mode and sets `TAKO_DOWNLOAD_BASE_URL=https://github.com/lilienblum/tako/releases/download/canary`
  - `--stable` forces stable channel and persists it as default
  - without channel flags, upgrade uses persisted global `upgrade_channel` (default: `stable`)
- both upgrade commands print active channel before execution (`You're on {channel} channel`)

## `releases` Subcommands

`tako releases ls`:

```bash
tako releases ls [--env <ENV>]
```

`tako releases rollback`:

```bash
tako releases rollback <RELEASE_ID> [--env <ENV>] [-y|--yes]
```

Notes:

- `tako releases ls` shows release/build history for the current app across mapped servers in the selected environment.
- Release output is sorted newest-first and prints per release in two lines:
  - line 1: release/build id + deployed timestamp (plus `{xh ago}` hint when deployed within 24h)
  - line 2: commit message + cleanliness marker (`[clean]`, `[dirty]`, `[unknown]`)
- `tako releases rollback` reuses current routes/env/secrets/scaling config and rolls app runtime back to the target release id using normal rolling-update behavior.
- In interactive terminals, rollback to `production` prompts for confirmation unless `--yes` is provided.

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

`tako servers upgrade`:

```bash
tako servers upgrade <NAME> [--canary|--stable]
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
  - SSH host key must be present and match in local `~/.ssh/known_hosts` (or configured SSH keys dir).
  - With SSH checks enabled, Tako detects and stores server target metadata (`arch`, `libc`) in that server's `[[servers]]` entry, used for deploy target matching.
  - `--no-test` skips SSH checks and target detection.
- `tako servers rm` aliases: `remove`, `delete`.
- `tako servers ls` alias: `list`.
- `tako servers upgrade <NAME>` installs the updated server binary on the host, then performs an in-place reload via the host service manager (`systemctl reload tako-server` on systemd or `rc-service tako-server reload` on OpenRC) using root privileges (root login or sudo-capable user). A supported service manager is required.
- `tako servers upgrade <NAME> --canary` installs canary prerelease server artifacts by passing `TAKO_DOWNLOAD_BASE_URL=https://github.com/lilienblum/tako/releases/download/canary` to the installer.
- `tako servers upgrade <NAME> --stable` forces stable channel and persists it as default.
- without channel flags, server upgrade uses persisted global `upgrade_channel` (default: `stable`).
- installer-managed hosts configure scoped passwordless sudo for `tako` user maintenance helpers, so `tako servers upgrade`/`restart` run non-interactively by default.
- `tako servers status` prints a single global deployment/runtime snapshot across configured servers.

Deploy note:

- `tako deploy` resolves preset from top-level `preset` or adapter default (top-level `runtime` override, otherwise detected adapter). `preset` in `tako.toml` must be runtime-local (for example `tanstack-start` with `runtime = "bun"`); namespaced aliases like `js/tanstack-start` are rejected and `github:` refs are not supported. Unpinned official aliases are fetched from `master` on each resolve; fetch failures fail resolution, while runtime base aliases (`bun`, `node`, `deno`) fall back to embedded defaults when missing from fetched family manifests. Deploy builds target artifacts locally (Docker or local based on preset `[build].container`) in fixed order: preset stage first, then app `[[build.stages]]`, reuses locally cached verified artifacts on cache hits, then uploads those artifacts to servers. JS runtime base presets (`bun`, `node`, `deno`) default to local build mode (`container = false`) unless preset `container = true` is set.
- `tako dev` command resolution differs by preset mode: with explicit top-level `preset`, it uses preset top-level `dev`; when top-level `preset` is omitted, it ignores preset top-level `dev` and runs runtime-default command with resolved `main` (`bun run node_modules/tako.sh/src/wrapper.ts {main}`, `node {main}`, or `deno run --allow-net --allow-env --allow-read {main}`).
- `tako deploy`/`tako dev`/`tako logs`/`tako secrets sync` resolve app identity from top-level `name` when set, otherwise from sanitized project directory name.
- Preset artifact filters come from preset `[build].exclude` plus app `[build].exclude` (`include` is app-level `[build].include` only).
- Preset runtime fields are top-level `main`/`install`/`start`.
- During artifact prep, deploy verifies resolved `main` exists in the post-build app directory and fails if missing.
- Containerized deploy builds reuse per-target dependency cache volumes (mise + runtime cache mounts, keyed by cache kind + target + builder image) while keeping build containers ephemeral.
- Containerized deploy builds default to `ghcr.io/lilienblum/tako-builder-musl:v1` for `*-musl` targets and `ghcr.io/lilienblum/tako-builder-glibc:v1` for `*-glibc` targets.
- During local builds, when `mise` is available, stage commands run through `mise exec -- sh -lc ...`.
- Bun release dependencies are installed on server before rollout (`bun install --production`).
- On every deploy, Tako prunes local `.tako/artifacts/` cache (best-effort): keeps 30 newest source archives (`*-source.tar.zst`), keeps 90 newest target artifacts (`artifact-cache-*.tar.zst`), and removes orphan target metadata files.
- For private/local route hostnames (`localhost`, `*.localhost`, single-label hosts, and reserved suffixes like `*.local`), deploy provisions self-signed certs on the server instead of ACME.
- If a TLS SNI hostname has no matching cert yet, Tako serves a fallback self-signed default cert so HTTPS requests still complete and unmatched hosts/routes return `404`.
- Remote edge proxy caching stores proxied `GET`/`HEAD` responses only when response `Cache-Control`/`Expires` headers explicitly allow caching (no implicit TTL defaults).

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
  - syncs via `tako-server` management command `update_secrets`; app instances restart automatically when secrets change. Does not write remote `.env` files.
- `tako secrets key import/export` default to `production` when `--env` is omitted.

## Common Examples

Initialize in current directory:

```bash
tako init
```

`tako init` prompts for app name and production route, prompts for runtime (top-level `runtime`), fetches family presets (`Fetching presets...`) and offers base runtime preset + fetched family presets + a custom option; when no family presets are available it skips preset selection and uses the runtime base preset. It only prompts for `main` when neither adapter inference nor preset default provides it. The detailed "Detected" block is shown with `--verbose`.

Run local app (streams logs directly to stdout in interactive terminals):

```bash
tako dev
```

Deploy staging and skip confirmation:

```bash
tako deploy --env staging --yes
```

Remove production app:

```bash
tako delete --env production
```

List release history for staging:

```bash
tako releases ls --env staging
```

Roll back to a previous release:

```bash
tako releases rollback abc1234 --env production --yes
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
