# tako-server

Rust crate for the remote Tako runtime and proxy.

## Responsibilities

- Start/stop/manage app instances.
- Maintain route table and app load balancers.
- Terminate HTTP/HTTPS traffic and proxy upstream.
- Redirect HTTP traffic to HTTPS (except ACME challenge checks).
- Cache proxied `GET`/`HEAD` upstream responses in-memory when response cache directives explicitly allow caching.
- Perform health probing using `Host: tako` + `/status` against each app instance.
- Perform active health probing.
- Serve management commands over Unix socket.
- Report per-build runtime status (multiple concurrently running builds during rollout).
- Validate on-demand deploy startup when the desired instance count is `0` before finalizing idle state.
- Validate app ids, release ids, and deploy paths at the management socket boundary.
- Persist app runtime registration (config/routes + release metadata) to SQLite and restore it on restart.
- Read non-secret env vars from release `app.json` and secrets from per-app `secrets.json` (0600) under the data directory.
- Persist server upgrade mode in SQLite and reject mutating commands while upgrading.
- Use a single-owner durable upgrade lock so only one upgrade controller can enter upgrading mode at a time.
- Expose `server_info`, `enter_upgrading`, and `exit_upgrading` management commands for upgrade orchestration.
- Enable zero-downtime reload handoff with SIGHUP child spawn, `SO_REUSEPORT` listener overlap, and pid-specific management sockets (`tako-{pid}.sock`) behind stable symlink `tako.sock`.

Routing policy notes:

- Deploy commands must include at least one non-empty route.
- No implicit catch-all/no-routes mode is supported.

## Key Runtime Paths

- Socket: `/var/run/tako/tako.sock`
- Data root: `/opt/tako`
- Runtime state DB: `/opt/tako/runtime-state.sqlite3`
- App releases: `/opt/tako/apps/<app>/<env>/releases/<version>/`

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
