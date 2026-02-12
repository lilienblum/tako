# Tako

<img src="website/public/assets/og.svg" alt="Tako logo" />

[![npm: tako.sh](https://img.shields.io/npm/v/tako.sh?label=npm%3A%20tako.sh&color=9BC4B6)](https://www.npmjs.com/package/tako.sh)
[![crate: tako](https://img.shields.io/crates/v/tako?label=crate%3A%20tako&color=E88783)](https://crates.io/crates/tako)
[![crate: tako-server](https://img.shields.io/crates/v/tako-server?label=crate%3A%20tako-server&color=E88783)](https://crates.io/crates/tako-server)

Tako helps you ship apps to your own servers without turning deployment
into a part-time job.

You get:

- a CLI (`tako`) for local dev + deployment
- a remote runtime/proxy (`tako-server`)
- a local development daemon (`tako-dev-server`)
- a JavaScript/TypeScript SDK (`tako.sh`)

## Prerequisites

- Rust toolchain (stable)
- Bun (for SDK/examples/website tooling)
- `just` (optional, but useful for repo tasks)

## Quickstart

From the repo root:

```bash
bun install
git config core.hooksPath .githooks
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

## Repo Layout

- `tako/`: CLI crate (`tako`) and local dev daemon binary (`tako-dev-server`)
- `tako-server/`: remote runtime/proxy crate (`tako-server`)
- `tako-core/`: shared protocol types
- `tako-socket/`: shared Unix socket JSONL transport helpers
- `sdk/`: `tako.sh` SDK package
- `examples/`: runnable examples
- `scripts/`: install/check helper scripts
- `website/`: Tako website + installer endpoints
- `docker/`: internal Docker tooling for build/debug workflows

## Docs

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
- `tako-core/README.md`
- `tako-socket/README.md`
- `sdk/README.md`

## License

MIT
