# tako

Rust crate for the `tako` CLI and `tako-dev-server` local daemon binaries.

## Responsibilities

- Project initialization (`tako init`).
- Local development flow (`tako dev`, `tako doctor`).
- Local development daemon runtime (`tako-dev-server`).
- Deployment orchestration (`tako deploy`).
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
- `delete`

Use `cargo run -p tako --bin tako -- --help` for current flags and subcommand help.

Operational behavior highlights:

- `tako servers status` prints one global snapshot and exits.
- `tako servers upgrade <name>` performs a single-host upgrade handoff using a temporary candidate process and the server-side upgrade lock/mode.
- Status output shows separate lines for concurrently running builds of the same app.
- App heading lines show `app (environment) state`; build/version is shown on the nested `build:` line.
- `tako deploy` packages artifacts from `.tako/artifacts/app` and fails early if that directory is missing or empty.
- `tako deploy` can merge extra public assets from `[tako].assets` (relative directories in `tako.toml`) into `.tako/artifacts/app/static` (or `.tako/artifacts/app/public` when `static/` is absent) without overwriting existing artifact files.
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
- Non-development environments must define `route` or `routes`; development defaults to `{app}.tako.local`.

## Related Docs

- `website/src/pages/docs/quickstart.md` (first-run local + remote setup)
- `website/src/pages/docs/development.md` (local dev workflow)
- `website/src/pages/docs/deployment.md` (remote deploy workflow)
