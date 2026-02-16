# AGENTS.md

Instructions for AI agents working on the Tako codebase.

## Key Principles

1. **Keep SPEC.md in sync with code** - Whenever you modify code, update SPEC.md if needed (avoid insignificant implementation details; focus on user-facing behavior and architecture)

2. **Move finalized behavior to SPEC.md** - Keep SPEC.md as the source of truth for implemented behavior; keep planning out of in-repo TODO files

3. **TDD is mandatory for Rust crates and SDK code** - Write tests before implementation. No backend/SDK feature is complete without passing tests. Website (`website/`) changes are explicitly exempt.

4. **Don't over-engineer** - Simple is better than complex. Avoid premature abstractions, extra configurability, or handling scenarios that can't happen

5. **Trust internal code** - Only validate at system boundaries (user input, external APIs). Don't add defensive checks between trusted internal components

6. **Keep README files in sync for touched components** - When code changes setup, commands, or run/test flow, update the relevant README.md files. README content should stay basic and practical (what it is, how to run), not a specification.

## Project Structure

**Rust Crates:**

- `tako-core/` - Shared protocol types (Command, Response enums)
- `tako-server/` - Remote server runtime (proxy, instances, TLS, sockets)
- `tako/` - CLI tool (all commands)

**SDK (current implementation):**

- `sdk/` - `tako.sh` JavaScript/TypeScript SDK package (npm)

**Website:**

- `website/` - Marketing/docs site (do not add automated tests for this component)

## Current Priorities

Routing, deploy locking/parallelism, SNI TLS, and active HTTP health probing are implemented.

Current release cleanup priorities:

1. **Testing & Quality**
   - Add edge case tests (deleted files, network failures)
   - Achieve >80% test coverage on critical paths

## Build & Test Commands

```bash
# Build all
cargo build

# Build release
cargo build --release

# Test all crates
cargo test

# Test specific crate
cargo test -p tako
cargo test -p tako-server

# SDK (current JS/TS implementation)
cd sdk && bun install
bun run build && bun run typecheck
bun test
```

## Commit Messages

Use Conventional Commits for all commits:

- Format: `type(scope): short summary`
- Common types: `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `perf`, `ci`
- If scope is broad/mixed across the repo, use `chore(repo): ...`

## Code References

When referring to code, use format: `file_path:line_number`

Example: "Parse app name in `tako/src/app/name.rs:42`"

## Architecture Overview

### Data Flow

1. Developer: `tako deploy` → build locally → SFTP to server
2. Server: Unpack to `/opt/tako/{app}/releases/{version}/`
3. tako-server: Rolling update via unix socket protocol
4. Proxy: Pingora routes to healthy instances

### Key Components

**tako-core:** Minimal, protocol-only. Don't add features here.

**tako-server:**

- `proxy/` - Pingora HTTP/HTTPS proxy
- `instances/` - App lifecycle management
- `lb/` - Per-app load balancer
- `tls/` - Certificate management (ACME)
- `socket.rs` - Unix socket API

**tako CLI:**

- `commands/` - All CLI commands (init, dev, deploy, server, secret, status, logs)
- `config/` - Configuration parsing and merging
- `ssh/` - Remote server communication
- `runtime/` - Runtime detection (Bun, Node, Deno)

**SDK:**

- Runtime-agnostic fetch handler interface
- Built-in `/_tako/status` endpoint
- Optional config reload support

## When Making Changes

### Before Writing Code

1. Read existing implementation if it exists
2. Check SPEC.md for expected behavior
3. Confirm scope from the current issue/release context
4. Write tests first (TDD) for Rust crates and SDK changes. Skip test creation for `website/` changes.

### After Writing Code

1. Ensure all applicable tests pass (Rust crates and SDK; website has no test requirement)
2. Update SPEC.md if user-facing behavior changed
3. Update affected README.md files if setup/usage/run commands changed
4. Close or update the related issue/task entry
5. Keep implementation details OUT of SPEC.md (focus on what users see/do)

### Example Changes

**If fixing a bug (Rust/SDK):** Add test that reproduces the bug, then fix code. Update SPEC.md only if it reveals a spec gap.

**If fixing a website bug:** Implement the fix directly without adding tests.

**If adding a feature (Rust/SDK):** Confirm scope first, write tests first, implement code, update SPEC.md, then close/update the related issue/task.

**If adding a website feature:** Confirm scope first, implement code without adding tests, update docs/spec as needed, then close/update the related issue/task.

**If changing architecture:** Confirm and record scope first, implement with TDD, then document the result in SPEC.md and close/update the related issue/task.

## Common Tasks

### Adding a new CLI command

1. Add command variant to `tako/src/cli.rs` (clap parser)
2. Create `tako/src/commands/{command}.rs` module
3. Implement command function
4. Write integration tests in `tako/tests/`
5. Update SPEC.md with command description and examples

### Adding a new message type to protocol

1. Add variant to `tako_core::Command` or `tako_core::Response` enum
2. Add handler in `tako_server/src/socket.rs`
3. Add sender in `tako/src/commands/` or SDK as needed
4. Write tests in `tako-core/src/lib.rs`
5. Update SPEC.md "Communication Protocol" section

### Fixing a deployment bug

1. Identify the issue (edge case in SPEC.md?)
2. Add test that reproduces the bug
3. Fix code in `tako/src/commands/deploy.rs` or `tako-server/`
4. Verify fix with test
5. Update SPEC.md if behavior changed or was clarified

## Testing Patterns

Rust crates and SDK code should be tested following TDD. Do not add tests for `website/` changes.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_happy_path() {
        // Test normal operation
    }

    #[test]
    fn test_error_case() {
        // Test error handling
    }

    #[test]
    fn test_edge_case() {
        // Test boundary conditions
    }
}
```

Integration tests in `{crate}/tests/` directory for command-level behavior.

## Documentation Standards

- SPEC.md: High-level, user-focused, finalized behavior with examples
- README.md files: Basic orientation and practical usage (`what this is`, `how to run/test`). Keep concise and non-normative.
- Planning/issues: Track planned work in the issue tracker or release notes; do not add in-repo TODO files
- Code comments: Only where logic isn't self-evident
- Test names: Describe what is being tested (not "test1", "test2")

## Tech Stack Reference

- **Proxy:** Pingora (Cloudflare's library)
- **Async:** Tokio
- **TLS:** OpenSSL callbacks (Pingora) + RCGen (self-signed dev certs)
- **ACME:** instant-acme (Let's Encrypt)
- **SSH:** Russh
- **CLI:** Clap 4.5
- **Config:** TOML 0.8
- **Serialization:** Serde 1.0

## Performance Targets

- Proxy: On par with Nginx, faster than Caddy
- Cold start: ~100-500ms
- Health detection: <3s
- Deploy time: <1 min per rolling update
- Memory: Minimal with on-demand scaling

## Documentation Workflow

```
Scope agreement → Code (TDD) → SPEC.md + README.md updates → Cleanup
```

1. **Scope agreement** - Confirm issue/task and acceptance criteria
2. **Code changes** - Implement with tests for Rust/SDK changes (TDD required there). Do not create tests for `website/` changes.
3. **SPEC.md** - Document finalized behavior after implementation
4. **README.md** - Update affected component readmes for basic usage/run instructions
5. **Cleanup** - Close/update related issue or release task
