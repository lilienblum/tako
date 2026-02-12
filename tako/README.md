# tako

Rust crate for the `tako` CLI and `tako-dev-server` local daemon binaries.

## Responsibilities

- Project initialization (`tako init`).
- Local development flow (`tako dev`, `tako doctor`).
- Local development daemon runtime (`tako-dev-server`).
- Deployment orchestration (`tako deploy`).
- Remote operational commands (`status`, `logs`, `servers`, `secrets`).
- Config loading/validation, runtime detection, and SSH interactions.

## Command Surface

Primary subcommands:

- `init`
- `status`
- `logs`
- `dev`
- `doctor`
- `servers`
- `secrets`
- `upgrade`
- `deploy`

Use `cargo run -p tako --bin tako -- --help` for current flags and subcommand help.

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

## Related Docs

- `docs/guides/quickstart.md` (first-run local + remote setup)
- `docs/guides/development.md` (local dev workflow)
- `docs/guides/deployment.md` (remote deploy workflow)
