# TanStack Start Basic Example

TanStack Start example based on `TanStack/router/examples/react/start-basic`, with Tako artifact staging via `tako.sh/vite`.

## Local TanStack Dev

```bash
cd examples/js/tanstack-start
bun install
bun run dev
```

## Build and Stage Deploy Artifacts

```bash
cd examples/js/tanstack-start
bun run build
```

After build, deploy artifacts are staged to `.tako/artifacts/app`:

- `.tako/artifacts/app/static`: merged `public/` and client output (`.output/public`)
- `.tako/artifacts/app/server`: Nitro server output (`.output/server`)

## Notes

- `vite.config.ts` wires TanStack Start and `tako.sh/vite`.
- `src/index.ts` is a lightweight Tako runtime entry used for health/status endpoints.
