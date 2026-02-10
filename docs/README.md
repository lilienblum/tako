# Tako Documentation

This folder is the canonical home for practical project documentation.

## Quick Navigation

- `guides/development.md`: local development workflow, HTTPS/DNS setup, environment variables.
- `guides/deployment.md`: remote deploy model, server prerequisites, release layout.
- `guides/operations.md`: operational runbook for diagnostics and incident triage.
- `architecture/overview.md`: component map and data/control plane flows.
- `reference/spec.md`: entrypoint to the authoritative specification.

## Suggested Reading Paths

### New contributor

1. `../README.md`
2. `architecture/overview.md`
3. `guides/development.md`
4. `../SPEC.md`

### Deploying an app

1. `guides/deployment.md`
2. `guides/operations.md`
3. `../SPEC.md`

### Debugging production behavior

1. `guides/operations.md`
2. `guides/deployment.md`
3. `../SPEC.md`

## Documentation Rules

- `../SPEC.md` remains the source of truth for finalized user-facing behavior.
- Guides in this folder should stay practical: setup, run, diagnose, and operate.
- Keep README files in each component concise and link here for deeper workflows.
