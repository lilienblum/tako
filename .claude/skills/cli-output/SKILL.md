---
name: cli-output
description: "Rules and patterns for Tako CLI output across normal, --verbose, and --ci modes. Use this skill whenever writing or modifying any Tako CLI command output — including print statements, spinners, log lines, prompts, progress indicators, context blocks, or error messages in the `tako/` crate. Also use when adding new commands, reviewing output consistency, or fixing output bugs. Triggers on any work touching `tako/src/output.rs`, `tako/src/commands/`, or CLI user-facing text."
---

# Tako CLI Output

This skill defines how the `tako` CLI presents information to users. Every CLI command has three output personalities — normal (interactive), verbose (`--verbose`), and CI (`--ci`) — and the `output` module in `tako/src/output.rs` handles the branching so command code stays clean.

## Core Invariant

> Normal mode defines the user story. Verbose mode is a superset of the same story plus diagnostics. CI mode preserves the essential outcomes without interactivity or animation.

## The Three Modes

### Normal (default, interactive terminal)

For any user-visible operation that takes time, show a spinner while work is in progress, then replace that same line with the final result when the work completes.

- Colors via the brand palette (see below)
- Spinners for operations that may take >500ms
- Single-line state transitions: spinner text → final result on same line
- `✓` for success, `✗` for failure, `!` for warnings, `•` for bullets
- Section headings in bold+accent for phase grouping
- Prompts (confirm, text, select) vanish after the user answers

### Verbose (`--verbose` / `-v`)

Replace spinner behavior with structured log lines. No spinners, no colors on non-interactive terminals.

Format: `HH:MM:SS.mmm LEVEL [context] message`

For any user-visible operation that takes time, emit two INFO records: one when it starts, one when it completes (with elapsed time). Between them, emit DEBUG/TRACE for internal detail.

**Log level hierarchy:**

- **INFO** — the outer user-visible operation, including completion with elapsed time. Maps 1:1 with what normal mode shows as spinners/results.
- **DEBUG** — meaningful internal steps that help troubleshoot the command. What's connecting, what sizes, what was detected, what paths are being used.
- **TRACE** — noisy, repetitive, or instrumentation-level details inside those internal steps. Timing spans (`timed()`), chunk progress, iteration detail.
- **WARN** — same as normal `!` warnings
- **ERROR** — same as normal `✗` errors

**Style rule:** message text in verbose mode must be plain — no colors, no bold, no ANSI codes. Only the level label is colored (handled by `log()`).

**Headings rule:** headings are not emitted in verbose mode. The `[context]` prefix on log lines replaces the grouping role that headings serve in normal mode — there is no need to repeat a server name as a heading when every log line already carries it as context.

**Context rule:** if a log line relates to a specific target or execution context (server, app, environment), always include `[context]` via `ctx()`. Only omit context for truly global logs.

### CI (`--ci`)

Plain, stable, non-interactive output optimized for logs and automation.

- Same symbols (`✓`, `✗`, `!`, `•`, `┃`) but without color codes
- No spinners — work runs silently, only the final result line appears
- Interactive prompts error out (must use `--yes` or defaults)
- Context blocks still show with `┃` border (plain text)

### Example: how one operation maps across modes

Normal:
```
⠋ Uploading artifact…
✓ Uploaded artifact (711ms)
```

Verbose:
```
10:00:00.100  INFO [prod-la] Uploading artifact…
10:00:00.200 DEBUG [prod-la] Uploading artifact to /var/tako/releases (12.4 MB)
10:00:00.300 TRACE [prod-la] Upload chunk 1/8
10:00:00.400 TRACE [prod-la] Upload chunk 2/8
10:00:00.811  INFO [prod-la] Uploaded artifact (711ms)
```

CI:
```
✓ Uploaded artifact (711ms)
```

## Output API Reference

These are the functions in `output.rs` that command code should use. The module handles mode branching internally.

### Text Output

| Function | Normal mode | Verbose mode | When to use |
|----------|-------------|--------------|-------------|
| `section(title)` | `\n` + bold accent title | INFO log | Phase headings: "Build", "Deploy", "Scale" |
| `heading(title)` | `\n` + bold title | no-op | Sub-headings, e.g. `heading(&format!("Server {}", strong(name)))` |
| `info(message)` | Default-color text | INFO log | General informational lines |
| `bullet(message)` | `  • message` | INFO log (indented) | Sub-items under a heading |
| `success(message)` | `✓ message` | INFO log | Completed action |
| `warning(message)` | `! message` | WARN log | Non-fatal issue |
| `error(message)` | `✗ message` | ERROR log | Failed action |
| `muted(message)` | Dim text | DEBUG log | Low-priority info |
| `hint(message)` | Default-color text | INFO log | Actionable guidance ("Run X to do Y") |

### Text Formatting

| Function | Effect | Example |
|----------|--------|---------|
| `strong(value)` | Bold (no color) | App names, server names, key values |
| `accent(value)` | Accent color (no bold) | Secondary emphasis |
| `brand_success(v)` | Green text | Status words: "active", "trusted" |
| `brand_warning(v)` | Amber text | Status words: "disabled", "untrusted" |
| `brand_error(v)` | Red text | Status words: "unreachable", "error" |
| `brand_muted(v)` | Dim text | Elapsed times, metadata |

### Verbose-Only Logging

These are no-ops in normal/CI mode.

| Function | Level | When to use |
|----------|-------|-------------|
| `log_trace(msg)` | TRACE | Noisy/repetitive detail: timing spans, chunk progress, iteration counts |
| `log_debug(msg)` | DEBUG | Meaningful internal steps: connections, sizes, paths, versions |
| `log_info(msg)` | INFO | Rarely needed directly — prefer the text helpers above |
| `log_warn(msg)` | WARN | Rarely needed — prefer `warning()` |

### Timing

```rust
let _t = output::timed("SSH connect");
// ... work ...
// On drop: TRACE log "SSH connect done (250ms)"
// Or explicitly:
_t.done();           // "SSH connect done (250ms)"
_t.finish("2 hosts"); // "SSH connect 2 hosts (250ms)"
```

`timed()` emits TRACE-level elapsed timing. Wrap any non-trivial async/blocking operation. Timing is always formatted in human-friendly units: `(3ms)`, `(1.2s)`, `(5s)`, `(1m10s)`.

### Context Prefix

```rust
output::log_debug(&output::ctx("prod-la", "Deploy succeeded"));
// Verbose: "[prod-la] Deploy succeeded"
// Normal:  "Deploy succeeded"  (context stripped)
```

**Rule:** in verbose mode, every log line that relates to a specific target must include `[context]` via `ctx()`. Only omit context for truly global logs (e.g., "Fetching tags from GitHub…").

### Context Blocks

A vertical-bar-bordered block for environment/channel info, shown before the main output.

```rust
output::ContextBlock::new()
    .env("production")      // "Using *production* environment"
    .channel("canary")      // "You're on *canary* channel"
    .print();
```

Normal: dim accent-colored `┃` border. CI: plain `┃`. Verbose: INFO log lines.

### Spinners

Pick the right spinner for your use case:

**`with_spinner(loading, success, work)`** — The workhorse. Shows a spinner if work takes >500ms. On success, prints `✓ success`. On error, the spinner stops and a failure line is printed. Sync version.

```rust
output::with_spinner("Validating", "Validated", || {
    validate()?;
    Ok(())
})?;
// Normal: ⠋ Validating... → ✓ Validated (1.2s)
// Verbose: INFO Validating → INFO Validated (1.2s)
// CI: (silent) → ✓ Validated (1.2s)
```

**`with_spinner_async(loading, success, work)`** — Same behavior, async.

**`with_spinner_simple(message, work)`** — Spinner with no result line. Use when the result is communicated by subsequent output (e.g., a heading that follows). Sync.

**`with_spinner_async_simple(message, work)`** — Async version of above.

**`PhaseSpinner::start(message)`** — For major phases (Build, Deploy). Shows elapsed time after 1s. Must be explicitly finished.

```rust
let phase = output::PhaseSpinner::start("Building…");
// ... build steps ...
phase.finish("Build complete");
// Normal: ⠋ Building… (5s) → ✓ Build complete (5.2s)
// Verbose: INFO Building… → INFO Build complete (5.2s)
```

**`PhaseSpinner::start_indented(message)`** — Same but indented (for sub-phases under a server heading).

**`TrackedSpinner::start(message)`** — Spinner whose message can be updated. Use for progress tracking in normal mode.

- **Normal mode**: live updates via `set_message()` (e.g., "Retrieving… 1/2" → "Retrieving… 2/2")
- **Verbose mode**: logs only the initial `start()` message; `set_message()` is a no-op. Per-scope completion should be logged by the calling code via `log_debug()` with `ctx()`.
- **CI mode**: no spinner, no progress updates, only final result.

```rust
// start() message is the verbose INFO line (no counts)
let spinner = output::TrackedSpinner::start("Retrieving…");
// set_message() updates normal-mode spinner only (no-op in verbose)
spinner.set_message(&format!("Retrieving… {}", output::muted_progress(1, 2)));
spinner.set_message(&format!("Retrieving… {}", output::muted_progress(2, 2)));
spinner.finish();
// Per-scope logging done separately:
// log_debug(&ctx("prod-la", "Retrieved 420 lines"));
```

### Prompts

All prompts work only in interactive mode. In CI mode, they use defaults or error.

| Function | Normal | Verbose |
|----------|--------|---------|
| `confirm(prompt, default)` | `(y/n) ›` vanishing prompt | `INFO prompt [Y/n]` then `INFO Confirmed: yes` |
| `text_field(prompt, default)` | `Prompt (default) ›` vanishing | `INFO Prompt?` then `INFO Prompt received` |
| `password_field(prompt)` | `Prompt › ••••` | Same but masked |
| `select(prompt, items)` | Arrow-key list | `INFO Prompt` then `INFO Selected: X` |

### Wizard

For multi-step interactive flows with ESC-to-go-back support:

```rust
let mut wizard = output::Wizard::new();
let name = wizard.text("App name", Some("my-app"))?;
let runtime = wizard.select("Runtime", &["bun", "node", "deno"])?;
```

Each step vanishes after answering. ESC goes back to previous step.

## Color Palette

| Name | RGB | Use |
|------|-----|-----|
| ACCENT | `(125, 196, 228)` | **Primary CLI color.** Spinners, section titles, prompt labels, INFO level, `accent()` emphasis |
| ACCENT_DIM | `(79, 107, 122)` | Context block borders |
| BRAND_GREEN | `(155, 217, 179)` | `✓`, "active", "trusted", "enabled" |
| BRAND_AMBER | `(234, 211, 156)` | `!`, "disabled", "untrusted", "not running" |
| BRAND_RED | `(232, 163, 160)` | `✗`, "unreachable", "error", errors |
| BRAND_TEAL | `(155, 196, 182)` | Dev TUI only — **never use in CLI output** (too close to green/success) |
| BRAND_CORAL | `(232, 135, 131)` | Dev TUI logo only — **never use in CLI output** (too close to red/error) |

Use the semantic helpers (`brand_success`, `brand_warning`, `brand_error`, `accent()`) rather than raw colors.

## Patterns to Follow

### 1. Single-line state transitions

Every action that shows a spinner should transition from its loading state to a final result on the same line. No intermediate rewrites, no multi-line progress.

```
⠋ Connecting…        → ✓ Connected
⠋ Building… (5s)     → ✓ Build complete (5.2s)
⠋ Deploying…         → ✗ Deploy failed: connection refused
```

### 2. Phase flow for deploy-style commands

Show one `✓` per major phase, not per sub-step:

```
✓ Validated
Build                     ← section heading
✓ Build complete (5.2s)
Deploy                    ← section heading
✓ Deployed (3.4s)
  • Revision abc1234 deployed to production
  • 2 server(s) updated
```

### 3. Verbose logging for every remote operation

Every operation behind a spinner should have structured verbose logging at the right level:

```rust
// INFO: outer user-visible operation (maps to spinner)
// This is handled automatically by the spinner helpers.

// DEBUG: meaningful internal steps
output::log_debug(&output::ctx(server, &format!("Uploading artifact to {} ({size})", path)));

// TRACE: timing instrumentation
let _t = output::timed(&output::ctx(server, "Artifact upload"));

// DEBUG: completion detail
output::log_debug(&output::ctx(server, "Upload complete"));
```

### 4. Context block before destructive/important commands

Commands like deploy, delete, rollback should show a context block:

```rust
output::ContextBlock::new()
    .env(&env_name)
    .print();
```

### 5. Accent for emphasis, not quotes

When emphasizing a term in output text, use `accent()` instead of wrapping in quotes.

### 6. stderr for human output, stdout for data

Human-facing CLI output goes to stderr. Structured data and explicit data-output commands go to stdout.

| Command | Stream | Why |
|---------|--------|-----|
| `tako deploy` | stderr | Human-facing progress |
| `tako --version` | stdout | Machine-readable version string |
| `tako completion zsh` | stdout | Shell eval's the output |

## Anti-Patterns to Avoid

- **Ad-hoc ANSI codes** — Use the output helpers. They handle color/no-color branching.
- **`println!` for user-facing output** — Use `eprintln!` or the output helpers (which use stderr).
- **Multiple result lines per spinner** — One spinner → one result line. Use bullets after for details.
- **Spinners for fast operations** — If it's always <100ms, just print the result directly.
- **Interactive prompts without CI fallback** — Every prompt must work in `--ci` mode (use defaults or `--yes`).
- **Styled text in verbose mode messages** — Message text must be plain (no colors, no bold, no ANSI). Only the level label is colored (handled by `log()`).
- **Missing `timed()` on remote operations** — Every SSH/network call should have a timing span.
- **Missing `ctx()` on per-target logs** — Every verbose log that relates to a specific server/target must use `ctx()`.
- **DEBUG for noisy repetitive detail** — Use TRACE for iteration progress, chunk counts, and timing spans. Reserve DEBUG for meaningful troubleshooting steps.

## Quick Decision Tree

Adding output to a command? Walk through this:

1. **Is it a major phase** (Build, Deploy, Delete)? → `section()` + `PhaseSpinner`
2. **Is it a single async operation** that might take >500ms? → `with_spinner_async()`
3. **Is it a single sync operation** that might take >500ms? → `with_spinner()`
4. **Is it a progress-tracking operation** (N of M)? → `TrackedSpinner`
5. **Is it a result detail** under a phase? → `bullet()`
6. **Is it a non-fatal issue**? → `warning()`
7. **Is it a fatal error**? → `error()` then return Err
8. **Is it a meaningful internal step** for debugging? → `log_debug()` with `ctx()` if per-target
9. **Is it noisy/repetitive instrumentation**? → `log_trace()` or `timed()`
10. **Is it environment/channel context**? → `ContextBlock`
11. **Is it low-priority info**? → `muted()`