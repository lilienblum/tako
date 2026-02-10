# Tako

Tako is an application platform with:

- a CLI (`tako`) for local development and deployment workflows
- a remote runtime/proxy (`tako-server`)
- a local development daemon (`tako-dev-server`)
- a JavaScript/TypeScript SDK (`tako.sh`)

## Prerequisites

- Rust toolchain (stable)
- Bun (for SDK/examples/website tooling)
- `just` (optional, but convenient for repo tasks)

## Quickstart (from source)

From repository root:

```bash
bun install
cargo build
cargo test --workspace
```

Run CLI help from source:

```bash
cargo run -p tako -- --help
```

Run the Bun example with Tako local dev flow:

```bash
just tako examples/js/bun dev
```

## Repository Layout

- `tako/`: CLI crate (`tako`)
- `tako-server/`: remote runtime/proxy crate (`tako-server`)
- `tako-dev-server/`: local dev daemon crate (`tako-dev-server`)
- `tako-core/`: shared protocol types
- `tako-socket/`: shared Unix socket JSONL transport helpers
- `sdk/`: `tako.sh` SDK package
- `examples/`: runnable examples
- `scripts/`: install/check helper scripts
- `website/`: Tako website + installer endpoints
- `docker/`: internal Docker tooling for build/debug workflows

## Documentation Map

- `docs/README.md`: documentation hub and reading paths
- `SPEC.md`: finalized behavior and architecture contract
- Planning/scope: tracked in issues and release planning (no in-repo `TODO.md`)
- `docs/architecture/overview.md`: high-level component/data-flow overview
- `docs/guides/development.md`: local dev setup and troubleshooting
- `docs/guides/deployment.md`: remote deploy model and server requirements
- `docs/guides/operations.md`: day-2 operational runbook

Component-focused docs:

- `tako/README.md`
- `tako-server/README.md`
- `tako-dev-server/README.md`
- `tako-core/README.md`
- `tako-socket/README.md`
- `sdk/README.md`
