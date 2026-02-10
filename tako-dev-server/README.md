# tako-dev-server

Rust crate for the local development daemon used by `tako dev`.

## Responsibilities

- Local HTTPS ingress for development hosts.
- Host-based routing from public dev hostnames to local app upstream ports.
- Lease registration/renewal over Unix socket.
- Local DNS handling for `tako.local` hosts.

`tako dev` normally starts and manages this daemon automatically.

## Run and Test

From repository root:

```bash
cargo run -p tako-dev-server -- --help
cargo test -p tako-dev-server
```

Example local run:

```bash
cargo run -p tako-dev-server -- --listen 127.0.0.1:47831 --dns-ip 127.77.0.1
```

## Related Docs

- `docs/guides/development.md` (local CA, DNS, troubleshooting)
- `docs/guides/operations.md` (day-2 local runbook)
- `SPEC.md` (`tako dev` behavior contract)
