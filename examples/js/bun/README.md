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

- The app exports a default `{ fetch() }` handler.
- Tako SDK provides built-in `/_tako/status` and `/_tako/health` endpoints.
