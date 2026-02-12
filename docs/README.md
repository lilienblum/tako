# Tako Documentation

User-facing guides for running apps with Tako.

## Quickstart

- `guides/quickstart.md`: fastest path from install to first deploy.
  - Developer machine setup
  - Remote server setup

## How Tako Works

- `architecture/overview.md`: control plane + data plane overview.
- `guides/development.md`: what `tako dev` does locally (HTTPS, DNS, leases, process lifecycle).
- `guides/deployment.md`: what `tako deploy` does remotely (build, upload, rolling update).

## What Tako Can Do

- Rolling deploys with health-based traffic shifts.
- Local HTTPS development on `*.tako.local`.
- Route management and conflict checks during deploy.
- Secrets management for environment-specific configuration.
- Runtime status and log inspection via CLI.

## Built-In Adapters

- Bun adapter quickstart in `guides/quickstart.md` (`Built-in adapters` section).
- SDK package docs in `../sdk/README.md`.

## Reference

- `reference/tako-toml.md`: complete `tako.toml` option reference and examples.

## Operations

- `guides/operations.md`: day-2 runbook for diagnostics and incident response.
