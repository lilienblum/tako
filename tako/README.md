# tako (CLI)

Rust crate for the `tako` command-line tool.

## Responsibilities

- Project initialization (`tako init`).
- Local development flow (`tako dev`, `tako doctor`).
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

Use `cargo run -p tako -- --help` for current flags and subcommand help.

## Run and Test

From repository root:

```bash
cargo run -p tako -- --help
cargo test -p tako
```

Run a focused command from source:

```bash
cargo run -p tako -- deploy --help
```

## Related Docs

- `SPEC.md` (authoritative command behavior)
- `docs/guides/development.md` (local dev workflow)
- `docs/guides/deployment.md` (remote deploy workflow)
