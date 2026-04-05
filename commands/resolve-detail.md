---
description: Resolve all pending Detail bugs
---

# Detail Bug Review

Review and resolve all pending Detail bugs until the dashboard is clean.

**Repository:** Determined automatically from `gh repo view --json nameWithOwner -q .nameWithOwner`, or pass as an argument (e.g. `owner/repo`).

---

## Phase 1 — Fetch all pending bugs

```bash
detail bugs list <owner/repo> --format json
```

If there are no pending bugs, report that the dashboard is clean and stop.

---

## Phase 2 — Triage each bug

For every pending bug, first view the full report:

```bash
detail bugs show <bug_id>
```

Then read the flagged code and its surrounding context. Classify into one of three buckets:

### Bucket A — Fix (genuine issue)

The bug identifies a real security, correctness, or data-integrity problem in production code.

**Action:**

1. Fix the code.
2. Add a brief code comment near the fix if the reason isn't self-evident, so future scans or reviewers understand the intent.
3. Run `cargo test` to verify the fix doesn't break anything.
4. Commit the fix immediately with a conventional commit message referencing the bug:

```
fix: <concise description of what was fixed> (Detail #<bug_id>)
```

5. Close the bug as resolved:

```bash
detail bugs close <bug_id> --state resolved --notes "<brief description of the fix>"
```

### Bucket B — Dismiss as not-a-bug / won't-fix / duplicate

The bug is a false positive, applies only to test code, is intentional, or is a duplicate of another bug.

**Action:**

1. Add a code comment near the flagged line (e.g. `// Detail: <reason this is safe>`) so the next scan or human reviewer doesn't re-flag it.
2. If a code comment was added, commit it:

```
chore: annotate false positive (Detail #<bug_id>)
```

3. Dismiss the bug via the CLI with an appropriate reason and a note explaining why:

```bash
detail bugs close <bug_id> --state dismissed --dismissal-reason <reason> --notes "<explanation>"
```

Valid `--dismissal-reason` values: `not-a-bug`, `wont-fix`, `duplicate`, `other`.

### Bucket C — Needs human input

The fix involves a design tradeoff or ambiguity that the agent can't resolve alone.

**Action:**

1. Present the issue with concrete options:
   - **A (recommended):** ... — why this is preferred
   - **B:** ... — tradeoff
   - **C:** ... — tradeoff
2. Wait for the user to choose before proceeding.
3. After the user decides, apply the chosen fix (or dismissal) following the Bucket A or Bucket B workflow (including commit).

---

## Phase 3 — Verify clean dashboard

After all bugs are handled, re-fetch pending bugs:

```bash
detail bugs list <owner/repo> --format json
```

If there are still pending bugs, go back to Phase 2 for any remaining. Repeat until clean.

---

## Rules

- Always read the full bug report (`detail bugs show`) and the flagged code with surrounding context before deciding — never dismiss blindly.
- Fixes must not break tests. Run `cargo test` before committing each fix.
- Only dismiss bugs with a clear, specific reason — not generic boilerplate.
- Code comments should be concise (one line) and reference the Detail bug when useful (e.g. `// Detail: <reason>`).
- Do not add comments to already-dismissed bugs unless you're also touching that code for a fix.
- Do not push to remote.
- Present Bucket C items one at a time, not batched, so the user can decide incrementally.
