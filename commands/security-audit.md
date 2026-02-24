# Deep Security Audit

## Scope

- Analyze anything relevant to security. Do not limit yourself to a predefined checklist.
- Include:
  - Application code
  - Configuration
  - Build/release tooling
  - CI/CD workflows
  - Dependency & supply-chain exposure
  - Secrets handling
  - Runtime assumptions
  - Operational hardening
- Include cloud & infra definitions if present (Terraform, Helm, Docker, k8s, CF Workers, etc.).
- Identify both single-point vulnerabilities and **multi-step attack chains**.

---

## Method

1. **Map trust boundaries and attack surface first**  
   (describe entry points, identities, and data flows; text diagrams welcome).
2. Prioritize **realistic exploitability** over theoretical issues.
3. Validate each finding with **concrete evidence from the repo** (file + line).
4. If checks/tools cannot run, explicitly state that and continue with manual analysis.
5. For each **P0/P1**, provide at least one plausible attacker abuse path.
6. Do **not** report best practices unless the repo clearly demonstrates risky behavior.

---

## Output Format (Markdown)

### 1) Executive Summary (max 10 bullets)

- Format: **Risk → why exploitable → what to do first**
- Include:
  - Count of P0 / P1 findings
  - One “most likely real-world incident” scenario

---

### 2) Trust Boundaries & Attack Surface

- Entry points (HTTP, queues, cron, webhooks, admin panels, workers, CLIs)
- Data stores & message buses
- Secrets & identity systems
- Third-party integrations
- Privileged identities (CI tokens, deploy keys, service accounts)
- Preconditions an attacker needs

---

### 3) Findings Table

| ID  | Severity (P0–P3) | Confidence (0–1) | Effort (S/M/L) | Owner | File:Line | Exploit Path | Impact | Recommended Fix | Follow-up Questions |
| --- | ---------------- | ---------------- | -------------- | ----- | --------- | ------------ | ------ | --------------- | ------------------- |

---

### 4) Detailed Findings

For **each finding**:

#### Evidence

- Minimal code/config excerpts with file:line references

#### Exploit / Abuse Scenario

- Step-by-step attacker flow
- Concrete, realistic assumptions

#### Why It Works

- Which trust boundary is violated
- Missing validation / auth / isolation

#### Fix

- **Minimum viable patch** (fast mitigation)
- **Proper fix** (long-term, correct solution)

#### Action Plan

- Ordered steps to implement the fix
- Tests / checks to verify it’s fixed
- Rollout notes (flags, migrations, backward compatibility)

#### Questions / Decision Points (only if non-trivial)

Ask only if the fix requires architectural, product, or operational change:

- Security goal: who are we defending against?
- Data sensitivity involved?
- Client / integration breakage risk?
- Key or identity management implications?
- Operational impact (deploy risk, on-call, alerts)?
- Blast radius if delayed?

---

### 5) Attack-Chain Scenarios (Multi-Step)

Combine findings into realistic chains.

For each chain:

- Preconditions
- Attack steps
- Blast radius
- Detection signals (logs / alerts)
- Fixes that break the chain early

---

### 6) Top 5 Highest-Risk Fixes (Ranked)

Rank by **risk reduction per effort**.

For each:

- Why it’s top-5
- Effort + dependencies
- Clear “done definition”

---

### 7) Validation Checklist

- Tools/commands run (if any)
- Manual inspection performed
- Explicit statement if tools could not run and why

---

### 8) Blind Spots / Unknowns

List what cannot be verified from the repo:

- Runtime environment (IAM, k8s policies, WAF, network ACLs)
- Secrets storage outside repo
- Production logging & alerting
- Private dependencies / registries

Explain how each unknown could materially change risk.

---

### 9) Explicit Statement

If applicable, include:

> **No critical findings (P0/P1) identified based on available evidence.**

---

## Rules

- Evidence only — no speculative warnings.
- Prefer fewer high-confidence findings over many weak ones.
- If something is suspicious but unprovable, put it in **Blind Spots**, not Findings.
