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
