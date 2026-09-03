# Tako Runtime Protocol

This document records the small set of contracts shared by the CLI, server, and SDKs. Exact message fields and serialized shapes live in typed code and tests.

The protocol version is `0`. Breaking changes are allowed until protocol v1. Change every producer and consumer in the same update. Do not add compatibility shims.

Tako has one active protocol version. The CLI, server, and every deploy artifact must declare that exact version; compatibility does not negotiate version ranges or capability sets. Deploy artifacts record it as `protocol_version` in `app.json`.

Normal deploys check the CLI/server protocol before parallel deployment starts. After upload, the server checks the artifact again before preparing dependencies, running a release command, synchronizing workflows, or starting app processes. `--force` bypasses only these version gates so an operator can attempt an incompatible deploy; normal validation and readiness checks still apply.

Server upgrades run the candidate binary in check-only mode before reload. The candidate checks the invoking CLI version and every active release, then repeats the check after the new process is ready. A mismatch aborts the upgrade. If a failure happens after reload starts, Tako restores and restarts the previous binary before leaving upgrade mode. `tako servers upgrade --force` permits an incompatible attempt but does not disable readiness or rollback.

Before the controlled reload, the CLI writes a one-shot upgrade-owner marker under the server data directory. The candidate consumes it and preserves the matching persisted upgrade fence through state restoration and the post-readiness check. An ordinary reload has no marker, so it still clears a stale upgrade lock.

## Executable owners

| Contract                                            | Owner                                                                                              |
| --------------------------------------------------- | -------------------------------------------------------------------------------------------------- |
| Protocol version, commands, responses, and payloads | [`tako-core/src/protocol.rs`](tako-core/src/protocol.rs)                                           |
| Command serialization                               | [`tako-core/src/protocol/tests.rs`](tako-core/src/protocol/tests.rs)                               |
| Process bootstrap envelope                          | [`tako-core/src/bootstrap.rs`](tako-core/src/bootstrap.rs)                                         |
| Native process environment                          | [`tako-core/src/instance_env.rs`](tako-core/src/instance_env.rs)                                   |
| JSONL framing and limits                            | [`tako-socket/src/lib.rs`](tako-socket/src/lib.rs)                                                 |
| Native readiness handshake                          | [`tako-server/src/instances/spawner/readiness.rs`](tako-server/src/instances/spawner/readiness.rs) |

These sources win if this overview drifts.

## Transport boundaries

Tako has two control paths with different trust boundaries.

- The management path handles deploys and operator commands. Local server control uses the management Unix socket. Remote management exposes the same operations through signed HTTP requests on the configured private management address.
- The internal path is available only to Tako-managed app and worker processes. It handles workflow RPCs and server-side channel publishing. Each command carries its app identity and runtime token.

The Unix transports use newline-delimited JSON. `tako-socket` caps one frame at 1 MiB. A client connects, writes one request, reads one response, and closes unless the owning API explicitly documents a stream.

Management-only commands are rejected on the internal socket. Runtime-only commands are rejected on the management socket. Both paths share `tako_core::Command` so message shapes cannot drift between duplicate enums.

## Process bootstrap

Native app and worker processes receive the bootstrap envelope as JSON on file descriptor 3. The envelope contains the app-scoped internal token, decrypted secrets, and storage bindings. `tako-core::bootstrap` is the only server-side serializer for this shape.

Native HTTP processes receive `HOST=127.0.0.1` and `PORT=0`. The SDK binds an available loopback port, completes any readiness hook, and writes the resolved port to file descriptor 4. The server does not route traffic to the process before this handshake completes.

Tako-managed processes also receive `TAKO_APP_NAME` and `TAKO_INTERNAL_SOCKET`. SDKs require those variables as a pair before making workflow or channel RPCs.

Container processes receive the same bootstrap envelope through `TAKO_BOOTSTRAP_DATA`. HTTP containers bind `HOST=0.0.0.0` and `PORT=3000`; they do not use file descriptors 3 and 4. Container workflow processes receive the internal socket mount and the same app identity contract.

## Health probes

The server probes `GET /status` with `Host: <app>.tako` and an `X-Tako-Internal-Token` header. Tako SDK adapters own this internal endpoint. A healthy response must be successful and echo the token header so an unrelated public `/status` route cannot satisfy the probe.

The token is infrastructure-only. SDKs must not expose it to application code or public responses.

## Changing the protocol

Update the typed owner, every producer and consumer, and the serialization or integration tests in one change. Update this document only when a cross-component boundary or invariant changes. User-facing behavior belongs in the website docs and SDK READMEs.
