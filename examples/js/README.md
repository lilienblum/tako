# JavaScript Examples

JavaScript runtime examples for Tako.

## Available

- `bun/`: Bun app example using the `tako.sh` SDK wrapper.
- `tanstack-start/`: TanStack Start app based on `TanStack/router` `start-basic`, with `tako.sh/vite` server-entry wrapping.

## Run

From repository root:

```bash
just tako examples/js/bun dev
```

Run TanStack Start directly from the example directory:

```bash
cd examples/js/tanstack-start
bun install
bun run dev
```

Build TanStack Start output for deploy:

```bash
cd examples/js/tanstack-start
bun run build
```
