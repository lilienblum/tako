---
description: Suggest 5 blog post ideas with one or two marked as recommended
---

$ARGUMENTS

# Blog Ideas

Generate 5 fresh blog post ideas for the Tako website. Mark the strongest 1–2 as **Recommended**. Report only — do not write any posts.

## Input

The user may pass a theme, angle, or constraint as `$ARGUMENTS` (e.g. "focus on local dev", "comparisons only", "secrets feature"). If empty, range broadly across Tako's surface area.

## Process

### Step 1 — Gather context

1. Read `SPEC.md` to understand current Tako capabilities, recent additions, and roadmap hints.
2. List existing posts in `website/src/content/blog/` and read titles + first paragraphs to avoid duplication and detect coverage gaps.
3. Skim recent commits (`git log --oneline -n 30`) for shipped-but-unannounced features.
4. Check memory for the platform vision (`project_platform_vision.md`) and competitor landscape if present.
5. If `$ARGUMENTS` references a feature, read the relevant source to make sure the angle is accurate.

### Step 2 — Generate ideas

Aim for variety across these post types — don't return five of the same kind:

- **Announcement** — a recently shipped feature that hasn't been written about yet
- **Deep dive** — how an existing feature works under the hood
- **Comparison** — Tako vs. a specific competitor (only if no recent post covers it)
- **Tutorial** — concrete "build X with Tako" walkthrough
- **Opinion / philosophy** — why Tako does something differently (no Docker, fetch handlers, SFTP, etc.)
- **Vision** — where the platform is heading (channels, queues, workflows, image opt, edge)

Reject ideas that:

- Duplicate an existing post's angle (different wording on the same thesis doesn't count as new)
- Make claims that aren't backed by SPEC.md or shipped code
- Are generic dev-tooling content with no Tako-specific hook

### Step 3 — Score and recommend

Mark 1–2 ideas as **Recommended** based on:

- **Timeliness** — shipped recently, no post yet, momentum to ride
- **Coverage gap** — fills an obvious hole in the existing blog
- **Reach** — comparison or opinion posts tend to attract more traffic than tutorials
- **Effort/payoff** — easy to write well, hard to get wrong

If two ideas tie, prefer the one closer to a recently shipped feature.

## Output format

```
### Blog ideas

1. **Title** — _type_ ⭐ Recommended
   Angle: one sentence on the thesis or hook.
   Why now: timeliness, coverage gap, or audience reason.
   Anchor: SPEC.md section, source file, commit, or competitor reference.

2. **Title** — _type_
   Angle: …
   Why now: …
   Anchor: …

… (5 total)
```

After the list, add one short line noting which existing posts each idea is closest to (so the user can verify no overlap). Example:

```
**Adjacent existing posts:**
- Idea 2 sits near `pingora-vs-caddy-vs-traefik.md` but covers latency tradeoffs, not feature comparison.
```

## Rules

- Exactly 5 ideas. No more, no less.
- Mark at most 2 as **Recommended**. One is fine. Zero is not — pick the best of the five if nothing stands out.
- Every idea must cite an anchor (SPEC section, file, commit, or named competitor). No vague "developers care about X" pitches.
- Don't propose posts that already exist under a different title — check `website/src/content/blog/` first.
- Don't write the post. This command produces ideas only; the user will invoke `/blog-post` separately for whichever they pick.
- Keep each idea to the four-line block above. No long pitches, no draft outlines.
