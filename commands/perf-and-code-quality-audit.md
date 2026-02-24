# Deep Performance & Code Quality Audit

## Scope

- Analyze anything relevant to **performance, scalability, and code quality**.
- Include:
  - Application code paths (hot paths, cold paths)
  - Algorithms & data structures
  - Memory usage & object lifetimes
  - I/O (network, disk, DB, queues)
  - Concurrency & async behavior
  - Caching layers
  - Build & bundling configuration
  - CI/CD performance gates
  - Runtime & deployment assumptions
- Cover infra definitions if present (Docker, k8s, CF Workers, serverless limits).

---

## Method

1. **Map execution paths & cost centers first**  
   (request lifecycle, background jobs, startup path).
2. Identify **real bottlenecks** over theoretical micro-optimizations.
3. Validate each issue with **concrete evidence**:
   - code references
   - complexity analysis
   - benchmarks / logs if present
4. Prefer **user-perceived latency, throughput, and cost** metrics.
5. If profiling/benchmarks cannot run, state that and proceed with static analysis.
6. Avoid “best practices” unless the code demonstrably violates them.

---

## Output Format (Markdown)

### 1) Executive Summary (max 10 bullets)

- Format: **Problem → why it’s slow → what to fix first**
- Include:
  - Top latency drivers
  - Top cost drivers
  - Most likely scale-failure scenario

---

### 2) Execution Paths & Performance Surface

- Request / job lifecycle (text diagram)
- Hot paths vs cold paths
- Synchronous vs async boundaries
- External dependencies (DB, APIs, storage)
- Resource constraints (CPU, memory, time limits)

---

### 3) Findings Table

| ID  | Category | Severity (P0–P3) | Confidence (0–1) | Effort (S/M/L) | Owner | File:Line | Evidence | Impact | Recommended Fix | Follow-up Questions |
| --- | -------- | ---------------- | ---------------- | -------------- | ----- | --------- | -------- | ------ | --------------- | ------------------- |

Severity meaning:

- **P0**: Will fail or degrade severely at realistic scale
- **P1**: Major performance regression under load
- **P2**: Noticeable inefficiency / cost leak
- **P3**: Cleanliness / maintainability / micro-optimization

---

### 4) Detailed Findings

For **each finding**:

#### Evidence

- Code excerpt (file:line)
- Complexity analysis (Big-O) or allocation pattern
- Observed metrics (if available)

#### Why It’s Slow / Risky

- Algorithmic issue
- Excess allocations / copies
- Blocking I/O
- Over-serialization / parsing
- Redundant work
- Missing caching or batching

#### Impact

- Latency (p50 / p95 / p99)
- Throughput limits
- Memory pressure / GC
- Cost amplification
- User-visible symptoms

#### Fix

- **Minimal improvement** (low-risk win)
- **Proper fix** (architectural or deeper refactor)

#### Action Plan

- Ordered steps
- Benchmarks or measurements to add
- Success criteria (numbers, not vibes)
- Rollout notes (feature flags, canaries)

#### Questions / Decision Points (if non-trivial)

Ask only when the fix is not a small refactor:

- Expected scale (QPS, data size, concurrency)?
- Latency vs consistency tradeoff?
- Caching invalidation strategy?
- Memory vs CPU tradeoff?
- Operational impact (warmups, cold starts)?
- Team familiarity with proposed approach?

---

### 5) Performance Anti-Patterns Detected

- N+1 queries
- Unbounded loops or recursion
- Blocking calls in async paths
- Repeated parsing / serialization
- Over-fetching data
- Chatty network patterns

(Only list patterns **actually found**.)

---

### 6) End-to-End Bottleneck Scenarios

Describe realistic slowdowns or failures.

For each scenario:

- Trigger (traffic spike, large input, cold start)
- Where time is spent
- Failure mode (timeouts, OOM, throttling)
- Early mitigations

---

### 7) Top 5 Highest-ROI Improvements

Rank by **latency/cost reduction per effort**.

For each:

- Why it’s top-5
- Effort + dependencies
- Clear “done definition”

---

### 8) Validation Checklist

- Profilers / benchmarks run (if any)
- Static analysis performed
- What could not be measured and why

---

### 9) Blind Spots / Unknowns

- Production traffic shape
- Real data sizes
- Cache hit rates
- Infrastructure limits
- Observability gaps

Explain how each unknown could affect conclusions.

---

### 10) Explicit Statement

If applicable:

> **No P0/P1 performance risks identified based on available evidence.**

---

## Rules

- Evidence-driven only.
- Prefer fewer high-confidence findings over many weak ones.
- No speculative tuning advice.
