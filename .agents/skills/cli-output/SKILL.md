---
name: cli-output
description: "Rules and patterns for Tako CLI output across normal, --verbose, and --ci modes. Use this skill whenever writing or modifying any Tako CLI command output — including print statements, spinners, log lines, prompts, progress indicators, or error messages in the `tako/` crate. Also use when adding new commands, reviewing output consistency, or fixing output bugs. Triggers on any work touching `tako/src/output.rs`, `tako/src/commands/`, or CLI user-facing text."
---

# Tako CLI Output

Two output systems coexist in code. Only one renders at a time based on the mode.

## Architecture

- **Pretty output** (`output::info()`, `output::success()`, spinners, etc.) — renders in normal mode, no-op in verbose/CI.
- **Tracing** (`tracing::debug!()`, `tracing::info!()`, etc.) — renders in verbose/CI mode, no-op in normal mode (no subscriber installed).

Both systems are called side-by-side in command code. Each is invisible in the mode it doesn't belong to.

## The Three Modes

### Normal (default, interactive terminal)

Pretty output renders — spinners, colors, symbols, vanishing prompts. Tracing calls are no-ops (no subscriber installed).

- Colors via the brand palette
- Spinners for operations >500ms
- Single-line state transitions: spinner → result
- `✓` success, `✗` failure, `!` warnings, `•` bullets
- Section headings in bold+accent
- Prompts vanish after the user answers

### Verbose (`--verbose` / `-v`)

Tracing renders — all levels (TRACE through ERROR) with local timestamps and colored level labels. Pretty output functions are no-ops.

Format: `HH:MM:SS.mmm LEVEL message`

Prompts remain interactive but use transcript style (no screen erasing). Prompts are NOT wrapped in tracing log-level prefixes — they print as plain `eprintln!` text.

### CI (`--ci`)

Same as verbose but without ANSI colors. Prompts use defaults (non-interactive).

### Example: how one operation maps across modes

Normal:
```
⠋ Uploading artifact…
✓ Uploaded artifact (711ms)
```

Verbose:
```
10:00:00.100  INFO Uploading artifact…
10:00:00.200 DEBUG [prod-la] Uploading artifact to /var/tako/releases (12.4 MB)
10:00:00.300 TRACE [prod-la] Artifact upload done (711ms)
10:00:00.811  INFO Uploaded artifact (711ms)
```

CI:
```
10:00:00.100  INFO Uploading artifact…
10:00:00.811  INFO Uploaded artifact (711ms)
```

## Pretty Output API (normal mode only)

These functions print in normal mode, no-op in verbose/CI. Use `output::is_pretty()` to check.

### Text Output

| Function | Normal mode | Verbose/CI |
|----------|-------------|------------|
| `section(title)` | `\n` + bold accent title | no-op |
| `heading(title)` | `\n` + bold title | no-op |
| `info(message)` | Default-color text | no-op |
| `bullet(message)` | `  • message` | no-op |
| `success(message)` | `✓ message` | no-op |
| `warning(message)` | `! message` | no-op |
| `error(message)` | `✗ message` | no-op |
| `muted(message)` | Dim text | no-op |
| `hint(message)` | Default-color text | no-op |

### Text Formatting

| Function | Effect | Example |
|----------|--------|---------|
| `strong(value)` | Bold (no color) | App names, server names, key values |
| `accent(value)` | Accent color (no bold) | Secondary emphasis |
| `brand_success(v)` | Green text | Status words: "active", "trusted" |
| `brand_warning(v)` | Amber text | Status words: "disabled", "untrusted" |
| `brand_error(v)` | Red text | Status words: "unreachable", "error" |
| `brand_muted(v)` | Dim text | Elapsed times, metadata |

### Environment & Channel Context

Environment and channel info use standard output helpers — no special block structure.

- **Environment** (auto-resolved): `output::warning()` with accented env name — draws attention since env selection has operational impact.
  ```rust
  output::warning(&format!("Using {} environment", output::accent(&env)));
  // Normal: ! Using production environment
  ```
- **Channel** (canary): `output::muted()` — low-priority informational context.
  ```rust
  output::muted("You're on canary channel");
  // Normal: You're on canary channel (dim)
  ```

### Spinners

Spinners bridge both output systems: in normal mode they show visual animation, in verbose/CI they emit `tracing::info!` for start and completion (with elapsed time). Command code does NOT need to duplicate tracing calls for spinner operations.

**`with_spinner(loading, success, work)`** — Shows spinner if >1s. On success: `✓ success (elapsed)`.

```rust
output::with_spinner("Validating", "Validated", || {
    validate()?;
    Ok(())
})?;
// Normal: ⠋ Validating... → ✓ Validated (1.2s)
// Verbose: INFO Validating → INFO Validated (1.2s)
```

**`with_spinner_async(loading, success, work)`** — Same, async.

**`with_spinner_simple(message, work)`** — Spinner with no result line.

**`with_spinner_async_simple(message, work)`** — Async version.

**`PhaseSpinner::start(message)`** — Major phases (Build, Deploy). Shows elapsed after 1s.

```rust
let phase = output::PhaseSpinner::start("Building…");
// ... build steps ...
phase.finish("Build complete");
// Normal: ⠋ Building… (5s) → ✓ Build complete (5.2s)
// Verbose: INFO Building… → INFO Build complete (5.2s)
```

**`TrackedSpinner::start(message)`** — Updatable message. `set_message()` is a no-op in verbose/CI.

### Prompts

All prompts work only in interactive mode. In CI mode, they use defaults or error.

**Prompts are NOT log lines.** In verbose mode, prompts print as plain `eprintln!` text — no timestamp, no level prefix. The answer appears on the next line.

| Function | Normal | Verbose |
|----------|--------|---------|
| `confirm(prompt, default)` | Vanishing `(y/n) ›` | Plain text transcript (no log prefix) |
| `text_field(prompt, default)` | Vanishing input | Plain text transcript (no log prefix) |
| `password_field(prompt)` | Masked `••••` | Same but masked |
| `select(prompt, items)` | Arrow-key list | Numbered list (no log prefix) |

## Tracing API (verbose/CI mode only)

Use standard tracing macros. They are no-ops in normal mode (no subscriber installed).

```rust
tracing::info!("Uploading artifact");
tracing::debug!("[{name}] Artifact size: {size}");
tracing::trace!("Upload chunk 1/8");
tracing::warn!("Retrying after timeout");
tracing::error!("Upload failed: {err}");
```

### Level guidelines

- **TRACE** — Noisy/repetitive detail, timing spans (`timed()`)
- **DEBUG** — Meaningful internal steps: connections, sizes, paths, versions
- **INFO** — User-visible operation milestones (rarely needed directly — spinners handle this)
- **WARN** — Non-fatal issues
- **ERROR** — Failures

### Message capitalization

Tracing messages that start with a regular word must be capitalized. Messages that start with a name (e.g. `tako-server`, a variable) are fine as-is.

```rust
// Good:
tracing::debug!("Downloading binary…");
tracing::debug!("[{name}] Deploy succeeded");
tracing::info!("tako-server restarted");

// Bad:
tracing::debug!("downloading binary…");
tracing::debug!("[{name}] deploy succeeded");
```

### Scope prefix `[name]`

Use a `[name]` message prefix for per-target context. Do NOT use tracing structured fields (`server = %name`).

The scope is typically a server name. Pass labels to the SSH client via `SshConfig::with_label()` or `SshClient::connect_with_label()` so SSH-level logs also carry the scope.

```rust
tracing::debug!("[{name}] Deploy succeeded");
// Output: 10:00:00.100 DEBUG [prod] Deploy succeeded

let ssh_config = SshConfig::from_server(&server.host, server.port).with_label(server_name);
let mut ssh = SshClient::new(ssh_config);
// SSH logs automatically use: [prod] Connecting to 1.2.3.4:22
```

For `timed()` labels, use the same pattern:

```rust
let _t = output::timed(&format!("[{name}] Artifact upload"));
```

### Start/finish records

Fast operations (< ~2s) need only **one** record — typically the result or a single summary line. Don't emit separate "Starting X…" and "X done" messages for each small step.

For longer operations that do warrant a start+finish pair, the **start** message must end with `…` (ellipsis):

```rust
// Good: single record for fast operation
tracing::debug!("[{name}] Server status: active");

// Good: start+finish for slow operation
tracing::debug!("[{name}] Downloading canary binary…");
// ...work...
tracing::debug!("[{name}] New server process ready (pid: 1234)");

// Bad: start+finish for fast operation
tracing::debug!("[{name}] Checking status…");    // unnecessary
tracing::debug!("[{name}] Status: active");       // this alone is enough
```

### Timing

```rust
let _t = output::timed("SSH connect");
// On drop: tracing::trace!("SSH connect done (250ms)")
```

`timed()` emits TRACE-level elapsed timing via tracing. Only visible in verbose/CI mode. For per-target timing, include context in the label:

```rust
let _t = output::timed(&format!("[{server_name}] Artifact upload"));
```

## Patterns to Follow

### 1. Coexist pretty + tracing

```rust
// Tracing for verbose/CI (no-op in normal)
tracing::info!("Deploying to {name}");

// Pretty output for normal (no-op in verbose/CI)
output::section("Deploy");

// Spinner bridges both modes automatically
let result = output::with_spinner_async("Uploading", "Uploaded", async {
    // Detail tracing inside spinner body
    tracing::debug!("[{name}] Uploading {size} to {path}");
    upload().await
}).await?;

// Pretty result details (no-op in verbose/CI)
output::bullet(&format!("Revision {} deployed", output::strong(rev)));
```

### 2. Single-line state transitions

Every spinner transitions from loading to result on the same line:
```
⠋ Connecting…        → ✓ Connected
⠋ Building… (5s)     → ✓ Build complete (5.2s)
```

### 3. Phase flow for deploy-style commands

One `✓` per major phase:
```
✓ Validated
Build
✓ Build complete (5.2s)
Deploy
✓ Deployed (3.4s)
  • Revision abc1234 deployed to production
```

### 4. Environment warning before destructive commands

```rust
output::warning(&format!("Using {} environment", output::accent(&env_name)));
```

### 5. Accent for emphasis, not quotes

Use `accent()` instead of wrapping in quotes.

### 6. stderr for human output, stdout for data

Human-facing CLI output goes to stderr. Structured data goes to stdout.

## Color Palette

| Name | RGB | Use |
|------|-----|-----|
| ACCENT | `(125, 196, 228)` | Primary CLI color: spinners, section titles, prompt labels |
| BRAND_GREEN | `(155, 217, 179)` | `✓`, "active", "trusted", "enabled" |
| BRAND_AMBER | `(234, 211, 156)` | `!`, "disabled", "untrusted" |
| BRAND_RED | `(232, 163, 160)` | `✗`, "unreachable", "error" |
| BRAND_TEAL | `(155, 196, 182)` | Dev TUI only |
| BRAND_CORAL | `(232, 135, 131)` | Dev TUI logo only |

## Anti-Patterns to Avoid

- **Ad-hoc ANSI codes** — Use the output helpers.
- **`println!` for user-facing output** — Use `eprintln!` or output helpers (stderr).
- **Multiple result lines per spinner** — One spinner → one result. Use bullets for details.
- **Spinners for fast operations** — If always <100ms, print result directly.
- **Interactive prompts without CI fallback** — Every prompt must work in `--ci`.
- **Missing `timed()` on remote operations** — Every SSH/network call should have a timing span.
- **DEBUG for noisy repetitive detail** — Use TRACE. Reserve DEBUG for meaningful steps.
- **Sharing formatted messages between modes** — Normal and verbose modes construct messages independently. Never pass an ANSI-formatted string (from `strong()`, `accent()`, etc.) to a function that reaches tracing. Spinner loading/success messages must be plain text.
- **Using `strip_ansi` to clean messages** — Don't strip ANSI as a workaround. Keep messages plain from the start.
- **Duplicating spinner tracing** — Spinners emit tracing::info! for start/completion automatically. Don't add redundant tracing::info! calls outside spinners for the same operation.
- **Tracing structured fields** — Don't use `server = %name` structured fields. Use `[name]` message prefix instead.
- **Wrapping prompts in tracing** — Prompts use `eprintln!` in verbose mode, never `tracing::info!`.
- **Dumping large remote commands** — Remote commands are auto-truncated in SSH client logging. Don't log full multi-line scripts.
- **Start+finish for fast operations** — Operations under ~2s need only one record (the result). Don't emit separate "Starting X…" and "X done" pairs for quick steps.
- **Start messages without `…`** — Every start message that has a corresponding finish must end with `…` (ellipsis).

## Quick Decision Tree

1. **Major phase** (Build, Deploy)? → `section()` + `PhaseSpinner`
2. **Single async operation >500ms**? → `with_spinner_async()`
3. **Single sync operation >500ms**? → `with_spinner()`
4. **Progress tracking** (N of M)? → `TrackedSpinner`
5. **Result detail** under a phase? → `bullet()`
6. **Non-fatal issue**? → `warning()`
7. **Fatal error**? → `error()` then return Err
8. **Meaningful internal step** for debugging? → `tracing::debug!()` with `[scope]` prefix
9. **Noisy/repetitive instrumentation**? → `tracing::trace!()` or `timed()`
10. **Environment context** (auto-resolved)? → `warning()` with `accent()` env name
11. **Channel context** (canary)? → `muted()`
12. **Low-priority info**? → `muted()`
