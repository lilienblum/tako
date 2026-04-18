# Demo App

TanStack Start app for trying `tako` and `tako.sh` together. Shows tenant-aware content using wildcard subdomain routing.

Live at [demo.tako.sh](https://demo.tako.sh).

## Local Dev (Vite)

```bash
cd examples/javascript/demo
bun install
bun run dev
```

## Run With Tako Dev Flow

From repository root:

```bash
just tako examples/javascript/demo dev
```

## Build

```bash
cd examples/javascript/demo
bun run build
```

## Notes

- `tako.toml` sets `preset = "tanstack-start"` with `runtime = "bun"`.
- Tenant is detected server-side from the `Host` header — no env var needed.
  - `foo.demo.tako.sh` → tenant `foo`
  - `demo.tako.sh` → no tenant
- Development routes: `demo.test`, `*.demo.test`
- Production routes: `demo.tako.sh`, `*.demo.tako.sh`
