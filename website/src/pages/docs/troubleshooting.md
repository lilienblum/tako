---
layout: ../../layouts/DocsLayout.astro
title: Tako Docs - Troubleshooting
heading: Troubleshooting
current: troubleshooting
---

# Troubleshooting

Day-2 checks for local and remote Tako environments.

Use this when things feel weird and you want a clean, repeatable response path.

## Scope

Use this guide when:

- local development URLs are not reachable,
- deploys partially fail,
- traffic is routed but responses are unhealthy,
- you need a predictable first-response sequence.

## Local Development Checks

1. `tako doctor`
2. `tako dev`
3. Open `https://{app}.tako.local/` (macOS) or `https://{app}.tako.local:47831/` (other platforms).

If DNS or HTTPS is failing:

- verify `/etc/resolver/tako.local` points to `127.0.0.1:53535`,
- ensure `tako dev` is currently running,
- re-run `tako doctor` and fix any preflight failures it reports.

## Deploy Triage

When `tako deploy` reports mixed success:

1. Identify failed hosts from deploy output.
2. Run `tako servers status` and inspect the failed hosts' sections.
3. Confirm remote prerequisites (`tako-server`, writable `/opt/tako`, socket access).
4. Re-run deploy once host-level issues are fixed.

## Runtime Health Validation

After deploy (or during incident response):

1. `tako servers status`
2. `tako logs --env <environment>`
3. Verify request path/host routing with the public endpoint.

Symptoms and first checks:

- `503 App is starting`: verify instance health/startup logs and wait for probe success.
- Route mismatch: confirm [`tako.toml`](/docs/tako-toml) route declarations for the selected environment.
- Intermittent failures: inspect logs and compare behavior across target servers.

## Files and Paths Worth Inspecting

- Local: `{TAKO_HOME}/dev-server.sock`
- Local: `{TAKO_HOME}/ca/ca.crt`
- Remote: `/var/run/tako/tako.sock`
- Remote: `/opt/tako/apps/<app>/current`
- Remote: `/opt/tako/apps/<app>/releases/<version>/`

## Escalation Notes

If a host remains unhealthy after prerequisite checks and redeploy:

1. collect `tako servers status` output,
2. collect `tako logs --env <environment>` output,
3. capture whether the issue is host-specific or environment-wide,
4. include route/env/server mapping from [`tako.toml`](/docs/tako-toml) in the incident report.
