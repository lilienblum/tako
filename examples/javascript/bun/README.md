# Bun Example App

Minimal Bun HTTP app for trying `tako` and `tako.sh` together.

## Local Runtime Only (No Tako Proxy)

```bash
cd examples/javascript/bun
bun install
bun run dev
```

## Run With Tako Dev Flow

From repository root:

```bash
just tako examples/javascript/bun dev
```

This runs the example through `tako dev` (HTTPS local ingress + routing).

## Notes

- `tako.toml` sets `runtime = "bun"` with no top-level `preset`.
  - In `tako dev`, this uses the runtime-default Bun command with resolved `main`.
  - For local direct runs, `bun run dev` uses `bun run index.ts`.
- The app starts Bun on `0.0.0.0:$PORT` (default `3000`) and serves HTTP directly.
- Internal health checks use `Host: tako` with path `/status` via the Tako SDK wrapper.
- Development routes in `tako.toml` are:
  - `bun.tako.test/bun`
  - `foo.bun.tako.test`
  - `*.bun.tako.test`
- Current `tako dev` routing is exact-host only, so use:
  - `https://bun.tako.test/bun`
  - `https://foo.bun.tako.test/`
- Staging routes:
  - `https://tako-testbed.orb.local/bun`
  - `https://<tenant>.bun.tako-testbed.orb.local/`
- Production routes:
  - `https://demo.tako.sh/bun`
  - `https://<tenant>.bun.tako.sh/`
