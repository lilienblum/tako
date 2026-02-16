# TanStack Start Basic Example

TanStack Start example based on `TanStack/router/examples/react/start-basic`, with Tako deploy metadata via `tako.sh/vite`.

## Local TanStack Dev

```bash
cd examples/js/tanstack-start
bun install
bun run dev
```

## Build for Deploy

```bash
cd examples/js/tanstack-start
bun run build
```

After build:

- `dist/.tako-vite.json` contains the compiled server entry metadata.
- `tako.toml` points deploy input to `dist`.
- `assets` merge order populates `dist/public` from `public/` and `dist/client`.

## Notes

- `vite.config.ts` wires TanStack Start and `tako.sh/vite`.
- Deploy uses `dist` as input and archive `app.json` as runtime manifest.
