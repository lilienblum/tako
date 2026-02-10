# tako-socket

Shared Unix-socket transport helpers for Tako components.

## Scope

- Newline-delimited JSON message framing.
- Message size/limit checks.
- Generic request/response connection loop utilities.

Used by both local and remote control channels.

## Test

From repository root:

```bash
cargo test -p tako-socket
```

## Related Docs

- `SPEC.md` communication protocol section
