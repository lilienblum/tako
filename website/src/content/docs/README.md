# The Why

Tako started from one simple question: why did deploying become so dramatic?

The mission is simple: bring back the old <span class="dynamic-phrase">upload and go</span> energy, but with modern safety rails.

- Ship changes quickly.
- See results fast.
- Keep your flow instead of fighting platform glue.

Tako is built to make local development smooth and production deploys boring (the good kind of boring).

## What Tako Can Do

- Rolling deploys with health-based traffic shifts, no babysitting required.
- Built-in load balancer. Scales down to `0`, scales up as far as you need.
- Was it `3000`? `5000`? Or `8081`? With Tako, local setup is portless on `https://*.tako.local`.
- Remote production routes are HTTPS by default (HTTP redirects to HTTPS).
- Subdomains? Custom path routes? Done.
- Secrets and variables per environment. Scoped and ready.
- Runtime status and log inspection via CLI.

> Enjoying Tako already? Give Tako a star on <a href="https://github.com/lilienblum/tako" target="_blank" rel="noopener noreferrer">GitHub</a>.

## Who Tako Is For

- Builders and entrepreneurs who want predictable pricing and predictable performance.
- Teams that want shipping to feel boring and reliable, not risky and ceremonial.
- Teams that are done with surprise invoices and random "how is this `$46,485.99`?" moments.
- People who want a runtime they control, without arbitrary platform limits.
- Folks running lots of low-traffic apps: instances can scale to `0` and start on demand.
- Yes, even "a ton of apps on a tiny VPS" territory, if most of them are idle most of the time.
- Anyone tired of bloated tools and config files that feel like a second full-time job.

## Ok, So Where Do I Sign?

Easy. Start here:

- [Local setup](/docs/quickstart#local-setup)
- [Remote setup](/docs/quickstart#remote-setup)

## How Tako Works

- [Architecture](/docs/architecture): control plane + data plane overview.
- [Development](/docs/development): what `tako dev` does locally (HTTPS, DNS, leases, process lifecycle).
- [Deployment](/docs/deployment): what `tako deploy` does remotely (build, upload, rolling update).

## Built-In Adapters

- Bun adapter quickstart in [Quickstart: Built-in adapters](/docs/quickstart#built-in-adapters).
- SDK package docs in the [SDK README](https://github.com/lilienblum/tako/blob/main/sdk/README.md).

## Reference

- [CLI Reference](/docs/cli): command and subcommand reference with common examples.
- [tako.toml Reference](/docs/tako-toml): complete `tako.toml` option reference and examples.

## Operations

- [Operations](/docs/operations): day-2 runbook for diagnostics and incident response.
