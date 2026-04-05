---
description: Reconcile declared PostHog events with 30d data and report gaps
---

# Analytics audit

Reconcile `posthog.capture(...)` calls in `website/src` with events actually recorded in PostHog over the last 30 days. Surface dead instrumentation, stale events, and low-signal tracking. Report only — do not auto-fix.

## Prerequisites

- `posthog-cli` installed (`npm i -g @posthog/cli`, or run via `bunx @posthog/cli`)
- Authenticated via `posthog-cli login`, or `POSTHOG_CLI_TOKEN` + `POSTHOG_CLI_ENV_ID` exported in the shell (`ENV_ID` is the PostHog project/environment ID)
- Personal API key must have `query:read` scope

If any prerequisite is missing, stop and ask me to set it up — do not guess.

## Phase 1 — Extract declared events

Grep `website/src` for every `posthog.capture(...)` call. For each match, record:

- Event name (first string argument)
- File and line
- Property keys declared in the second-argument object literal, if present

Present the declared set as a flat list before continuing.

## Phase 2 — Query actual events

Run the following HogQL via `posthog-cli query`. Note: the `query` subcommand is marked "subject to change" in the upstream README — if output parsing fails, fall back to `curl` against `POST /api/projects/:project_id/query/` with a `HogQLQuery` payload (stable HTTP API).

```sql
SELECT event, count() AS c
FROM events
WHERE timestamp > now() - interval 30 day
GROUP BY event
ORDER BY c DESC
```

Capture event names, counts, and separately note `$autocapture` and `$pageview` volumes for the ratio check in Phase 3.

## Phase 3 — Reconcile and report

Bucket every declared-or-observed event:

- **Dead in code** — declared in `website/src`, 0 fires in 30d. Likely a broken selector, removed element, or JS error.
- **Stale in PostHog** — fired in PostHog, not in code. Deleted feature, or a code path the grep missed.
- **Low-signal** — declared and fired, but <10 fires in 30d. Candidate for removal.
- **Healthy** — declared, fired, meaningful volume.

Also compute:

- **Autocapture ratio** — `$autocapture` volume ÷ total manual event volume. >10x means autocapture dominates and manual events may be redundant. <0.1x means autocapture is noise and can be turned off (`autocapture: false` in `posthog.init`).
- **$pageview coverage** — total pageviews, and whether any custom "viewed" events duplicate it.

## Output format

```
### Analytics audit — last 30 days

**Totals**
- Manual events: N fires across M declared (of K observed)
- $autocapture: N fires
- $pageview: N fires
- Autocapture ratio: Nx manual

**Dead in code** (declared, 0 fires)
1. `event_name` — `file:line` — likely cause: …

**Stale in PostHog** (fired, not in code)
1. `event_name` — N fires — possible source: …

**Low-signal** (<10 fires / 30d)
1. `event_name` — N fires — `file:line`

**Healthy**
- `event_name` — N fires
- …

**Recommendations**
- Concrete, file-grounded actions only.
```

## Rules

- Evidence only — every finding cites either `file:line` or a concrete 30-day event count.
- Do not auto-fix. Surface findings, the human decides.
- If `posthog-cli query` fails or returns an unexpected format, fall back to `curl` + HogQL against the HTTP API. If that also fails, stop and report the failure — never guess at data.
- 30-day window is the default. Respect an explicit window override if the user provides one.
- Ignore `$`-prefixed PostHog internal events, except `$autocapture` and `$pageview` which feed the ratio/coverage checks.
- No generic analytics advice. Only concrete, grounded findings for this site.
