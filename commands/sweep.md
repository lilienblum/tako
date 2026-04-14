---
description: Security, performance, and code quality audit
---

# Sweep

Combined security, performance, and code quality audit. Commit progress along the way.

## Phase 1 — Find everything

Analyze the codebase for issues across all categories:

- Security (injection, auth, secrets, supply chain, trust boundaries)
- Performance (hot paths, blocking I/O, unnecessary allocations, algorithmic complexity)
- Code quality (dead code, redundant logic, error handling gaps, inconsistencies, overengineering)
- Configuration & operational (misconfigs, missing limits, unsafe defaults)

**Method:**

1. Map trust boundaries and execution paths first.
2. Focus on realistic exploitability and real bottlenecks — skip theoretical or stylistic nits.
3. Every issue must have concrete evidence (file:line).
4. If tools/profilers can't run, say so and continue with static analysis.

## Phase 2 — Fix what's obvious

For each issue found, decide:

- **Auto-fix**: If the fix is trivial, safe, clearly correct, and doesn't require architectural decisions or my input — fix it immediately. Examples: missing input validation, obvious resource leaks, dead code removal, simple hardening.
- **Skip**: If the fix requires my input, is risky, or involves tradeoffs — don't touch it, carry it to Phase 3.

Commit each batch of auto-fixes as you go (use the current branch). Group related fixes into logical commits with conventional commit messages.

After fixing, run a second audit pass to catch regressions or newly exposed issues.

## Phase 3 — Report remaining issues

Present **only P0 and P1 issues** that remain after auto-fixes. Use this exact format:

```
### Remaining Issues

1. **[Security|Perf|Quality] Short title** — `file:line`
   What: one-line description of the problem
   Risk: what happens if not fixed
   Recommendation: what to do (if there are options, list them as A/B/C so I can pick)
   Input needed: what decision I need to make (or "none — just needs implementation time")

2. ...
```

Keep it flat — no nested sections, no executive summaries, no tables. Just the numbered list.

If auto-fixes were made in Phase 2, list them briefly at the top:

```
### Auto-fixed
- Short description of fix (`file:line`)
- ...
```

If no P0/P1 issues remain:

> **No high-priority issues found.**

## Rules

- Evidence only — no speculative warnings.
- Flag overengineering: abstractions with a single implementation, indirection that doesn't pay for itself, configurability nobody uses, error handling for impossible states, solving problems that aren't real yet. Simple code that does the job beats clever code that anticipates hypotheticals.
- Fewer high-confidence findings over many weak ones.
- Don't report best practices unless the code demonstrably violates them in a way that matters.
- Suspicious-but-unprovable items go in a short "Blind spots" list at the end, not in findings.
- Ignore issues in code paths that exist only for development or testing (e.g. `#[cfg(test)]` modules, test helper commands, dev-only socket commands). Focus on code that runs in production builds.

## Do not report

Before adding an item to the report, check this list. If any apply, drop it — do not promote it to Phase 3.

- **"Not exploitable in practice"** — if you have to write this in the Risk line, the finding doesn't belong in the report. Move it to Blind spots or delete it.
- **Textbook hardening with no concrete attacker model** — e.g., non-constant-time comparison of a high-entropy env-scoped token over a network, timing side channels behind JIT/GC noise, redundant length checks where the type system already guarantees bounds. Only report a timing/hardening issue if you can describe a concrete attacker and realistic exploit path.
- **Defense-in-depth suggestions for a layer that already sits behind a trusted gate** — if a separate component (proxy, firewall, auth middleware) enforces the real boundary, don't ask the inner layer to duplicate the check "just in case." Trust the architecture; flag the outer gate if it's weak.
- **Cross-layer validation where the project's convention says otherwise** — if security guards live in Rust (or Go, or a specific service) by design, don't report JS/TS/peripheral code for not re-validating. Respect the trust boundary the codebase has chosen.
- **Code-simplification suggestions dressed as security** — "this check is redundant with X" or "this fallback is unnecessary" is a refactor, not a finding. Skip.
- **Widening-not-tightening "fixes"** — adding entries to an allow-list (e.g., extra loopback hosts), adding more accepted inputs, or making a gate more permissive is never a security fix. Skip.
- **Findings where the recommendation is "document better"** — docs gaps are not P0/P1 security or perf issues.
