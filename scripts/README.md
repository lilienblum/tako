# Scripts

Repository scripts used by installers, CI checks, and local development workflows.

## Scripts

- `install-tako-cli.sh`: POSIX installer for local `tako` CLI.
- `install-tako-server.sh`: POSIX installer for `tako-server` on Linux hosts.
  - Configures systemd when available, including `KillMode=control-group` and `TimeoutStopSec=30min` for graceful shutdown windows.
  - Detects container/non-systemd environments and falls back to manual-start guidance instead of failing.
  - Detects host architecture (`x86_64`/`aarch64`) and libc (`glibc`/`musl`) to download the matching server artifact.
  - Applies `setcap cap_net_bind_service=+ep` to `/usr/local/bin/tako-server` when possible for non-root `:80/:443` binds.
  - Installs required runtime dependencies (including Unix-socket-capable `nc` with `-U` support, plus sqlite runtime libraries) via the host package manager.
- `check_critical_coverage.sh`: coverage gate for selected critical source files.

## Typical Usage

Run from repository root:

```bash
sh scripts/install-tako-cli.sh
sh scripts/install-tako-server.sh
bash scripts/check_critical_coverage.sh
```

The install scripts are exposed via website redirect endpoints:

- `/install`
- `/install-server`
