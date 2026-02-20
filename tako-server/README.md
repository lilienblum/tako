# tako-server

Rust crate for the remote Tako runtime and proxy.

## Responsibilities

- Start/stop/manage app instances.
- Maintain route table and app load balancers.
- Terminate HTTP/HTTPS traffic and proxy upstream.
- Redirect HTTP traffic to HTTPS (except ACME challenge and internal `tako.internal/status` checks).
- Cache proxied `GET`/`HEAD` upstream responses in-memory when response cache directives explicitly allow caching.
- Internal runtime status uses `Host: tako.internal` + `/status`; non-internal host requests are routed to apps.
- Perform active health probing.
- Serve management commands over Unix socket.
- Report per-build runtime status (multiple concurrently running builds during rollout).
- Validate on-demand (`instances = 0`) deploy startup before finalizing idle state.
- Persist app runtime registration (config/routes/env) to SQLite and restore it on restart.
- Persist server upgrade mode in SQLite and reject mutating commands while upgrading.
- Use a single-owner durable upgrade lock so only one upgrade controller can enter upgrading mode at a time.
- Expose `server_info`, `enter_upgrading`, and `exit_upgrading` management commands for upgrade orchestration.
- Enable `SO_REUSEPORT` listeners for HTTP/HTTPS so temporary upgrade candidates can bind alongside the active process.
- Support `--instance-port-offset` for temporary candidate processes to avoid app-port collisions during overlap.

Routing policy notes:

- Deploy commands must include at least one non-empty route.
- No implicit catch-all/no-routes mode is supported.

## Key Runtime Paths

- Socket: `/var/run/tako/tako.sock`
- Data root: `/opt/tako`
- Runtime state DB: `/opt/tako/runtime-state.sqlite3`
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

- `website/src/pages/docs/quickstart.md` (remote server install + first deploy setup)
- `website/src/pages/docs/deployment.md` (deploy flow and runtime expectations)
- `website/src/pages/docs/architecture.md` (runtime component/data-flow context)
