# channels-workflows fixture

Minimal Bun fixture exercising both channels (SSE, via `defineChannel`)
and workflows (enqueue + durable handler, via `defineWorkflow`).

Flow:

1. Client opens `GET /channels/demo` (handled by the Tako dev proxy).
2. Client `POST /enqueue` with `{ message }` — the fetch handler enqueues
   the `broadcast` workflow.
3. `workflows/broadcast.ts` sleeps briefly then publishes to `demo`.
4. Client receives the message over the SSE stream.

Used by both the CLI e2e suite (`e2e/cli/tests/channels-workflows.test.ts`)
and the deploy/docker harness.
