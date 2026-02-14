---
layout: ../../layouts/DocsLayout.astro
title: Tako Docs - How Tako Works
heading: How Tako Works
current: architecture
---

# How Tako Works

Tako keeps the architecture simple: one management flow to change app state, one traffic flow to serve requests.

## System Boundaries

| Component                   | Runs where           | Owns                                         | Talks to                          |
| --------------------------- | -------------------- | -------------------------------------------- | --------------------------------- |
| `tako` CLI                  | Your machine         | Build/package/deploy commands                | SSH + remote `tako-server` socket |
| `tako-dev-server`           | Your machine         | Local HTTPS ingress + `*.tako.local` routing | Local app process + dev socket    |
| `tako-server`               | Remote server        | App lifecycle, routes, TLS, load balancing   | App instances + management socket |
| App instance                | Local or remote host | Your runtime code                            | Receives proxied HTTP traffic     |
| `tako-core` + `tako-socket` | Shared crates        | Control protocol + socket transport          | CLI and server runtime            |

## Management Flow

The management flow is every operation that changes runtime state.

### Deploy Flow (`tako deploy`)

1. CLI validates config/runtime/secrets.
2. CLI builds and archives locally.
3. CLI uploads release artifacts over SSH to each target server.
4. Artifacts are stored in `/opt/tako/apps/<app>/releases/<version>/`.
5. CLI sends deploy instructions to `tako-server` over Unix socket.
6. `tako-server` performs rolling updates and applies route changes.

### Day-2 Operations

- `tako status` and `tako logs` query runtime state through the same control channel.
- `tako secrets` updates environment-scoped secret material.
- `tako servers` manages server target definitions used by deploy/ops commands.

## Traffic Flow

The traffic flow is the live HTTP/HTTPS request path.

### Remote Request Path

1. Request lands on `tako-server` listener.
2. Route matcher resolves app target by host + path specificity.
3. Built-in load balancer chooses a healthy instance.
4. Request is proxied to that instance.

### Local Request Path (`tako dev`)

1. `tako-dev-server` serves HTTPS on `*.tako.local` (no port juggling).
2. Host-based route maps request to your local app process.
3. Lease lifecycle keeps local routes active while dev session is alive.

## Routing Model

- Routes are declared per environment in [`tako.toml`](/docs/tako-toml).
- Matching uses deterministic host/path specificity.
- Conflict checks run during deploy validation, before traffic changes.

## State and Filesystem Layout

### Remote

- App root: `/opt/tako/apps/<app>/`
- Releases: `/opt/tako/apps/<app>/releases/<version>/`
- Shared runtime data: `/opt/tako/apps/<app>/shared/`
- Active release symlink: `/opt/tako/apps/<app>/current`
- Management socket: `/var/run/tako/tako.sock`

### Local Development

- Dev management socket: `{TAKO_HOME}/dev-server.sock`
- Local DNS + HTTPS ingress for `tako.local` names

## Reliability Model

- Rolling deploys use health-based traffic shifting (no manual cutover work).
- Health endpoints drive instance readiness decisions.
- Built-in load balancing and on-demand instance behavior support low idle footprint.

## Code Ownership Map

- `tako/`: CLI commands, config, SSH, runtime detection, `tako-dev-server`.
- `tako-server/`: runtime, routing, health checks, TLS, deploy orchestration.
- `tako-core/` + `tako-socket/`: shared command protocol and socket transport.
