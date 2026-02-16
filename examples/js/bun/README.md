# Bun Example App

Minimal Bun HTTP app for trying `tako` and `tako.sh` together.

## Local Runtime Only (No Tako Proxy)

```bash
cd examples/js/bun
bun install
bun run dev
```

## Run With Tako Dev Flow

From repository root:

```bash
just tako examples/js/bun dev
```

This runs the example through `tako dev` (HTTPS local ingress + routing).

## Notes

- `tako.toml` defines an explicit production route; local dev defaults to `bun-example.tako.local`.
- The app starts Bun on `0.0.0.0:$PORT` (default `3000`) and serves HTTP directly.
- Internal health checks use `Host: tako.internal` with path `/status` via the Tako SDK wrapper.
- `/` returns minimal HTML (`<h1>Tako app</h1>`).
