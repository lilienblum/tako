# Go workflow worker

Register workflow handlers before calling `RunWorker(ctx)`. The worker reads
`TAKO_WORKER_CONCURRENCY` (default 500) and runs at most that many handlers
concurrently. Cancellation stops new claims and waits for active handlers to
finish; heartbeats and completion RPCs continue during this drain. Handlers must
eventually return for graceful shutdown to finish.

`TAKO_WORKER_IDLE_TIMEOUT_MS` applies only when there are no active handlers.
The idle interval starts again when the last handler finishes.

Run this module's tests from `sdk/go` with `go test ./...`. The concurrency and
drain test also supports `go test -race -run TestWorkerHonorsConcurrencyAndDrains`.
