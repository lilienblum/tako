# E2E Bun Fixture

Minimal Bun API-style fixture app used by Docker deploy e2e tests.

## Run Deploy E2E Test

From repo root:

```bash
just e2e e2e/fixtures/js/bun
```

## Notes

- This fixture is source-only (no local build step, no `dist` requirement).
- `package.json main` points at `src/app.ts`, and `tako-server` launches it through the Tako SDK wrapper.
- The app root returns JSON and internal health is handled via `Host: tako.internal` + `/status`.
