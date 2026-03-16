# Code Scanning Review

Review and resolve all open GitHub code scanning alerts until the dashboard is clean.

**Repository:** Determined automatically from `gh repo view --json nameWithOwner -q .nameWithOwner`, or pass as an argument (e.g. `owner/repo`).

---

## Phase 1 — Fetch all open alerts

```bash
gh api repos/{owner}/{repo}/code-scanning/alerts --paginate \
  --jq '.[] | select(.state == "open") | {number, rule: .rule.id, severity: .rule.severity, file: .most_recent_instance.location.path, start_line: .most_recent_instance.location.start_line, end_line: .most_recent_instance.location.end_line, message: .most_recent_instance.message.text}'
```

If there are no open alerts, report that the dashboard is clean and stop.

---

## Phase 2 — Triage each alert

For every open alert, read the flagged code and its surrounding context. Classify into one of three buckets:

### Bucket A — Fix (genuine issue)

The alert identifies a real security or correctness problem in production code.

**Action:**
1. Fix the code.
2. Add a brief code comment near the fix if the reason isn't self-evident, so future scans or reviewers understand the intent.

### Bucket B — Dismiss as false positive / test-only / intentional

The alert is wrong, applies only to test code, or the flagged pattern is intentional and safe.

**Action:**
1. Dismiss the alert via the API with an appropriate reason (`false positive`, `won't fix`, or `used in tests`) and a short comment explaining why.
2. Add a code comment near the flagged line (e.g. `// CodeQL: <reason this is safe>`) so the next scan or human reviewer doesn't re-flag it.

```bash
gh api -X PATCH repos/{owner}/{repo}/code-scanning/alerts/{number} \
  -f state=dismissed \
  -f dismissed_reason="false positive" \
  -f dismissed_comment="<explanation>"
```

Valid `dismissed_reason` values: `false positive`, `won't fix`, `used in tests`.

### Bucket C — Needs human input

The fix involves a design tradeoff or ambiguity that the agent can't resolve alone.

**Action:**
1. Present the issue with concrete options:
   - **A (recommended):** ... — why this is preferred
   - **B:** ... — tradeoff
   - **C:** ... — tradeoff
2. Wait for the user to choose before proceeding.
3. After the user decides, apply the chosen fix (or dismissal) and continue.

---

## Phase 3 — Verify clean dashboard

After all alerts are handled, re-fetch open alerts:

```bash
gh api repos/{owner}/{repo}/code-scanning/alerts --jq '[.[] | select(.state == "open")] | length'
```

If the count is not `0`, go back to Phase 2 for any remaining alerts. Repeat until clean.

---

## Phase 4 — Commit

Create a single commit with all code changes (fixes + added comments). Use a conventional commit message:

```
fix(security): resolve code scanning alerts
```

Do not push — just commit locally.

---

## Rules

- Always read the flagged code and surrounding context before deciding — never dismiss blindly.
- Fixes must not break tests. Run `cargo test` after making changes.
- Only dismiss alerts with a clear, specific reason — not generic boilerplate.
- Code comments should be concise (one line) and reference the scanning rule when useful (e.g. `// CodeQL[rust/cleartext-logging]: ...`).
- Do not add comments to already-dismissed alerts unless you're also touching that code for a fix.
- Do not push to remote.
- Present Bucket C items one at a time, not batched, so the user can decide incrementally.
