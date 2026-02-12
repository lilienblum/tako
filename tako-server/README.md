# tako-server

Rust crate for the remote Tako runtime and proxy.

## Responsibilities

- Start/stop/manage app instances.
- Maintain route table and app load balancers.
- Terminate HTTP/HTTPS traffic and proxy upstream.
- Redirect HTTP traffic to HTTPS (except ACME challenge and `/_tako/status`).
- Perform active health probing.
- Serve management commands over Unix socket.

Routing policy notes:

- Deploy commands must include at least one non-empty route.
- No implicit catch-all/no-routes mode is supported.

## Key Runtime Paths

- Socket: `/var/run/tako/tako.sock`
- Data root: `/opt/tako`
- App releases: `/opt/tako/apps/<app>/releases/<version>/`

## Run and Test

From repository root:

```bash
cargo run -p tako-server -- --help
cargo test -p tako-server
```

Example local run:

```bash
cargo run -p tako-server -- \
  --socket /tmp/tako.sock \
  --port 8080 \
  --tls-port 8443 \
  --data-dir /tmp/tako-data \
  --no-acme
```

## Related Docs

- `docs/guides/quickstart.md` (remote server install + first deploy setup)
- `docs/guides/deployment.md` (deploy flow and runtime expectations)
- `docs/architecture/overview.md` (runtime component/data-flow context)
