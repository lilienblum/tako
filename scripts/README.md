# Scripts

Repository scripts used by installers, CI checks, and local development workflows.

## Scripts

- `install-tako-cli.sh`: POSIX installer for local `tako` CLI.
- `install-tako-server.sh`: POSIX installer for `tako-server` on Linux hosts.
  - Requires usable systemd for normal install/start and exits with an error when unavailable.
  - Supports install-refresh mode via `TAKO_RESTART_SERVICE=0` (writes unit/install artifacts without restarting service), used in build/container workflows before systemd is running.
  - Detects host architecture (`x86_64`/`aarch64`) and libc (`glibc`/`musl`) to download the matching server artifact.
  - Applies `setcap cap_net_bind_service=+ep` to `/usr/local/bin/tako-server` when possible for non-root `:80/:443` binds.
  - Creates both `tako` (server) and `tako-app` (app process) users, and removes any legacy sudoers/upgrade-helper artifacts.
  - Installs systemd unit with `Type=notify`, `ExecReload=/bin/kill -HUP $MAINPID`, and capability set including `CAP_SETUID`/`CAP_SETGID` for app-user spawning.
  - Installs required runtime dependencies (including Unix-socket-capable `nc` with `-U` support, sqlite runtime libraries, and `mise`) via the host package manager when available.
  - Falls back to the official `mise` installer if distro package managers do not provide `mise`.
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
