# Architecture

This document gives a high-level view of Tako's runtime architecture and data flows.

## Related Docs

- `../guides/quickstart.md`: first-run local and remote setup.
- `../guides/development.md`: local dev runtime behavior and troubleshooting.
- `../guides/deployment.md`: deploy flow and remote runtime prerequisites.

## System Components

### `tako` (CLI)

- Entry point for `init`, `dev`, `doctor`, `deploy`, `status`, `logs`, `servers`, and `secrets`.
- Detects runtime/build behavior from project files.
- Builds and packages app artifacts locally.
- Connects to remote hosts over SSH for deploy/status/log management.

### `tako-dev-server` (local daemon)

- Local HTTPS ingress for development (`*.tako.local`).
- Host-based routing to local app upstream ports.
- Lease-based app registration over Unix socket (`{TAKO_HOME}/dev-server.sock`).
- Local DNS authority for `tako.local` names.

### `tako-server` (remote runtime)

- Runs app instances on remote hosts.
- Terminates TLS and proxies HTTP/HTTPS requests.
- Maintains route mappings and app state.
- Exposes a Unix socket management API (`/var/run/tako/tako.sock`).

### Shared crates

- `tako-core`: command/response protocol types and shared status models.
- `tako-socket`: reusable newline-delimited JSON over Unix-socket helpers.

### Support crates and assets

- `sdk/`: runtime adapters and Tako endpoint integration for JS/TS apps.
- `website/`: docs/marketing site and install endpoints.
- `docker/`: internal Docker assets used by tests/tooling.

### SDK (`tako.sh`)

- Runtime adapters for Bun/Node/Deno.
- Built-in health/status endpoints used by local and remote health checks.

## Control Plane vs Data Plane

### Control plane

- CLI-driven actions over SSH + Unix socket commands.
- Typical operations: deploy, reload, status, secrets sync.

### Data plane

- Inbound HTTP/HTTPS request routing to app instances.
- Health checks and load-balancer target selection.

## Key Data Flows

### 1) Local development (`tako dev`)

1. CLI ensures `tako-dev-server` is running.
2. CLI registers lease(s) for development hostnames.
3. App process runs locally on an ephemeral upstream port.
4. `tako-dev-server` receives HTTPS traffic and routes by `Host` header.
5. Idle lifecycle rules stop/restart the app process as needed.

### 2) Deployment (`tako deploy`)

1. CLI validates config/runtime/secrets.
2. CLI builds and archives app locally.
3. CLI deploys to all target servers in parallel over SSH.
4. Each server receives archive under `/opt/tako/apps/<app>/releases/<version>/`.
5. CLI notifies `tako-server` with deploy command (app, version, routes, scaling settings).
6. `tako-server` applies rolling update and route table changes.

### 3) Remote request handling

1. Client request arrives at `tako-server` HTTP/HTTPS listener.
2. Route matching chooses target app based on host/path specificity.
3. App load balancer selects healthy instance.
4. Request is proxied to selected app instance.

## Routing Model

- Routes are declared per environment in `tako.toml`.
- Matching uses host + path with deterministic specificity ordering.
- Route ownership and conflict checks are enforced during deploy validation.

## Storage Layout (remote)

- App root: `/opt/tako/apps/<app>/`
- Release artifacts: `/opt/tako/apps/<app>/releases/<version>/`
- Shared runtime data: `/opt/tako/apps/<app>/shared/`
- Active release symlink: `/opt/tako/apps/<app>/current`

## Protocol/Interface Boundaries

- CLI ↔ server runtime: `tako-core` message types over Unix socket (invoked through SSH commands).
- CLI ↔ remote host: SSH command execution + SCP artifact upload.
- Local client ↔ dev daemon: Unix socket control API.
- Proxy ↔ app: HTTP upstream requests (and health probes).

## Ownership By Directory

- `tako/`: CLI command implementation, config, SSH, runtime detection, and local dev daemon binary (`src/bin/tako-dev-server`).
- `tako-server/`: production runtime, routing, health checks, TLS, deploy orchestration.
- `tako-core/` and `tako-socket/`: shared protocol and transport boundaries.
