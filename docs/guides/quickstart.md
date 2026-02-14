# Quickstart

This is the fastest path from "I just installed Tako" to "my app is live."

## Local setup

Install the CLI:

```bash
curl -fsSL https://tako.sh/install | sh
tako --version
```

From your app directory:

- Install the SDK (Bun example):

```bash
bun add tako.sh
```

- Check local prerequisites:

```bash
tako doctor
```

- Start local HTTPS development:

```bash
tako dev
```

## Remote setup

On each deployment host (as `root` or with `sudo`), install the runtime:

```bash
curl -fsSL https://tako.sh/install-server | sh
tako-server --version
```

From your developer machine, add that host to Tako:

```bash
tako servers add --name production <host-or-ip>
```

Configure explicit routes and map the server in `tako.toml`:

```toml
[envs.production]
route = "my-app.example.com"

[servers.production]
env = "production"
```

`tako dev` uses `{app}.tako.local` by default. Add `[envs.development]` only when you want custom local routes.

Deploy:

```bash
tako deploy --env production
```

Need every config option? See the full [tako.toml reference](/docs/tako-toml).
Need uninstall steps? See [Uninstall CLI](/docs/install#uninstall-cli-curl) and [Uninstall Server Runtime](/docs/install#uninstall-server-curl).

## How Tako works

1. `tako dev` runs your app locally and routes HTTPS traffic through `tako-dev-server`.
2. `tako deploy` builds your app locally, then uploads releases to remote hosts over SSH.
3. `tako-server` runs instances, probes health, and shifts traffic to healthy targets.
4. Runtime status and logs stay available through `tako servers status` and `tako logs`.

## What Tako can do

- Local HTTPS dev URLs on `*.tako.local`.
- Rolling deploys with health checks.
- Environment-aware route configuration.
- Secrets distribution during deploy.
- Per-environment status and logs.

## Built-in adapters

### Bun

Use the Bun adapter from `tako.sh`:

```ts
import { serve } from "tako.sh/bun";

serve({
  fetch() {
    return new Response("Hello from Bun + Tako");
  },
});
```

The adapter exposes built-in endpoints used by Tako health/status checks:

- `/_tako/health`
- `/_tako/status`
