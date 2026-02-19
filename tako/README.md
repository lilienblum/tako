# tako

Rust crate for the `tako` CLI and `tako-dev-server` local daemon binaries.

## Responsibilities

- Project initialization (`tako init`).
- Local development flow (`tako dev`, `tako doctor`).
- Local development daemon runtime (`tako-dev-server`).
- Deployment orchestration (`tako deploy`).
- Release history and rollback (`tako releases ls`, `tako releases rollback`).
- Remote operational commands (`logs`, `delete`, `servers`, `secrets`).
- Config loading/validation, runtime detection, and SSH interactions.

## Command Surface

Primary subcommands:

- `init`
- `logs`
- `dev`
- `doctor`
- `servers`
- `secrets`
- `upgrade`
- `deploy`
- `releases`
- `delete`

Use `cargo run -p tako --bin tako -- --help` for current flags and subcommand help.

Operational behavior highlights:

- `tako servers status` prints one global snapshot and exits.
- `tako servers upgrade <name>` performs a single-host upgrade handoff using a temporary candidate process and the server-side upgrade lock/mode.
- Status output shows separate lines for concurrently running builds of the same app.
- App heading lines show `app (environment) state`; build/version is shown on the nested `build:` line.
- `tako deploy` packages source files from the app's source root (git root when available; otherwise app directory), filtered by `.gitignore`.
- `tako deploy` always excludes `.git/`, `.tako/`, `.env*`, `node_modules/`, and `target/` from source bundles.
- `tako deploy` resolves preset from top-level `preset` when set, otherwise falls back to adapter base preset from top-level `runtime` (when set) or adapter detection (`unknown` falls back to `bun`); unpinned official aliases are fetched from `master` on each resolve and the resolved source metadata is written to `.tako/build.lock.json`.
- `tako deploy` builds per-target artifacts locally before upload, using Docker only when preset `[build].container` resolves to `true`; built-in JS base presets (`bun`, `node`, `deno`) default to local build mode (`container = false`) unless explicitly overridden.
- Container builds stay ephemeral; dependency downloads are reused via per-target Docker cache volumes keyed by target label and builder image.
- Containerized deploy builds default to `ghcr.io/lilienblum/tako-builder-musl:v1` for `*-musl` targets and `ghcr.io/lilienblum/tako-builder-glibc:v1` for `*-glibc` targets.
- `tako deploy` caches target artifacts in `.tako/artifacts` and reuses verified cache hits when build inputs are unchanged; invalid cache entries are rebuilt automatically.
- Local runtime version resolution is mise-aware: Tako probes `mise exec -- <tool> --version` when `mise` is installed, then falls back to `mise.toml` and `latest`; local build stage commands also run through `mise exec -- sh -lc ...` when `mise` is available.
- `tako deploy` merges build assets (preset assets + `build.assets`) into app `public/` after target build, in listed order.
- `tako deploy` writes `app.json` in the deployed app directory and `tako-server` uses it to resolve the runtime start command.
- `tako releases ls` shows release/build history for the current app and environment with commit metadata when available.
- `tako releases rollback <release-id>` rolls target servers back to a previous release id using the normal rolling-update path.
- `tako servers add` captures per-server target metadata (`arch`, `libc`) during SSH checks and stores it in `~/.tako/config.toml` under `[server_targets.<name>]`.
- `tako deploy` requires valid target metadata for each selected server and does not probe targets during deploy.
- `tako deploy` validates startup even for `instances = 0` (on-demand) by briefly starting one instance; deploy fails if startup health checks fail.

## Run and Test

From repository root:

```bash
cargo run -p tako --bin tako -- --help
cargo run -p tako --bin tako-dev-server -- --help
cargo test -p tako
```

Run a focused command from source:

```bash
cargo run -p tako --bin tako -- deploy --help
```

## Config Requirements

- `tako.toml` is required for `dev`, `deploy`, `logs`, and `secrets` workflows.
- Top-level `name` in `tako.toml` is optional; when omitted, app identity falls back to sanitized project directory name.
- Setting `name` explicitly is recommended for stable identity and uniqueness per server; renaming identity later creates a new app path and requires manual cleanup of old deployments.
- Non-development environments must define `route` or `routes`; development defaults to `{app}.tako.local`.

## Related Docs

- `website/src/pages/docs/quickstart.md` (first-run local + remote setup)
- `website/src/pages/docs/development.md` (local dev workflow)
- `website/src/pages/docs/deployment.md` (remote deploy workflow)
