# tako-core

Shared protocol and status types used across Tako components.

## Scope

- Command/response protocol enums and payloads.
- Shared status models (app/instance/service state).
- Serialization boundary types used by CLI and server runtimes.

Keep this crate protocol-focused and minimal.

## Test

From repository root:

```bash
cargo test -p tako-core
```

## Related Docs

- `docs/architecture/overview.md` (component boundaries and data flow)
