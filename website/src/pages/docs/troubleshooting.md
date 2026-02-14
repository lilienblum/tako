---
layout: ../../layouts/DocsLayout.astro
title: Tako Docs - Troubleshooting
heading: Troubleshooting
current: troubleshooting
---

# Troubleshooting

Day-2 checks for local and remote Tako environments.

Use this when things feel weird and you want a clean, repeatable response path.

## Fast Triage (Do This First)

1. Run `tako servers status` (remote state snapshot).
2. Run `tako logs --env <environment>` (live logs across mapped servers).
3. Reproduce once and note if failure is local-only, one-host-only, or all hosts.

This quickly separates config errors from host/runtime failures.

## Local Development Issues (`tako dev`)

Baseline checks:

1. `tako doctor`
2. `tako dev`
3. Open:
   - macOS: `https://{app}.tako.local/`
   - non-macOS: `https://{app}.tako.local:47831/`

If local URL fails:

- Ensure `tako dev` is currently running.
- Re-run `tako doctor` and fix preflight issues it reports.
- On macOS, verify `/etc/resolver/tako.local` points to `127.0.0.1:53535`.

Important local behavior:

- `tako dev` uses fixed HTTPS port `127.0.0.1:47831`.
- On macOS, Tako configures local forwarding and split DNS so you can use `https://{app}.tako.local/` without port suffix.
- On first trust/setup flow, elevated access may be required for local CA trust + forwarding setup.

Config-related local failures:

- `[envs.development]` routes must be `{app}.tako.local` or subdomains.
- Dev routing uses exact hostnames; wildcard host entries are ignored.
- If configured dev routes contain no exact hostnames, `tako dev` fails validation.

## Deploy Failures and Partial Success

When `tako deploy` reports mixed success:

1. Identify failed hosts from deploy output.
2. Run `tako servers status` and inspect only failing host blocks.
3. Confirm host prerequisites:
   - `tako-server` installed and running
   - writable `/opt/tako`
   - management socket reachable (`/var/run/tako/tako.sock`)
4. Re-run deploy after fixing host-level issues.

Expected deploy behavior:

- Partial failures are possible: some servers can succeed while others fail.
- Rolling updates are health-gated and auto-rollback on failed health transition.
- Failed partial release directories are auto-cleaned on deploy failure.

## High-Value Failure Modes

- `Deploy lock left behind`:
  - Symptom: deploy fails immediately due to existing lock.
  - Fix: remove stale lock directory on affected host:
    - `/opt/tako/apps/{app}/.deploy_lock`
- `Low disk space under /opt/tako`:
  - Symptom: deploy fails before upload with required vs available sizes.
  - Fix: free space, then redeploy.
- `503 App is starting`:
  - Symptom: traffic arrives before instance becomes healthy.
  - Fix: check startup logs and health probe readiness.
- `Route mismatch / wrong app`:
  - Verify env route config in [`tako.toml` reference](/docs/tako-toml-reference).
  - Ensure environment has valid `route` or `routes` values.

## Config and State Edge Cases

From spec-defined behavior:

- `~/.tako/` deleted: auto-recreated on next command.
- `.tako/` deleted: auto-recreated on next deploy.
- `tako.toml` deleted: config-requiring commands fail with guidance to run `tako init`.
- `.tako/secrets` deleted: warning is shown; restore secrets before deploy.
- `~/.tako/config.toml` corrupted: parse error with line context.

## Files and Paths Worth Inspecting

- Local:
  - `{TAKO_HOME}/dev-server.sock`
  - `{TAKO_HOME}/ca/ca.crt`
- Remote:
  - `/var/run/tako/tako.sock`
  - `/opt/tako/apps/<app>/current`
  - `/opt/tako/apps/<app>/releases/<version>/`
  - `/opt/tako/apps/<app>/.deploy_lock`

## Escalation Bundle

If issue remains unresolved, capture:

1. `tako servers status` output
2. `tako logs --env <environment>` output
3. host scope (`one host` vs `all hosts`)
4. route/env/server mapping from [`tako.toml` reference](/docs/tako-toml-reference)
