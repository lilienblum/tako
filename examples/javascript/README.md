# JavaScript Examples

JavaScript runtime examples for Tako.

## Available

- `demo/`: TanStack Start app using the `tako.sh` SDK with tenant-aware routing.
- `tanstack-start/`: TanStack Start app based on `TanStack/router` `start-basic`, with `tako.sh/vite` server-entry wrapping.
- `nextjs/`: Next.js app example.

## Run

From repository root:

```bash
just tako examples/javascript/demo dev
```

Build demo for deploy:

```bash
cd examples/javascript/demo
bun run build
```

Run TanStack Start directly from the example directory:

```bash
cd examples/javascript/tanstack-start
bun install
bun run dev
```
