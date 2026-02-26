# Scripts

Repository scripts used by installers, CI checks, and local development workflows.

## Scripts

- `install-tako-cli.sh`: POSIX installer for local `tako` CLI.
- `install-tako-server.sh`: POSIX installer for `tako-server` on Linux hosts.
- `install-tako-server-canary.sh`: lightweight wrapper that runs hosted server installer with canary artifact base URL.
  - Both installers resolve component-specific latest tags (`tako-v*`, `tako-server-v*`) via GitHub tags API by default, then download release assets for that tag.
  - Supports systemd and OpenRC for normal install/start.
  - Supports install-refresh mode via `TAKO_RESTART_SERVICE=0` (refreshes binary/users without restarting service; service definition is updated only when a supported manager is active), used in build/container workflows before init/service managers are running.
  - Detects host architecture (`x86_64`/`aarch64`) and libc (`glibc`/`musl`) to download the matching server artifact.
  - Applies `setcap cap_net_bind_service=+ep` to `/usr/local/bin/tako-server` when possible for non-root `:80/:443` binds.
  - Creates both `tako` (server) and `tako-app` (app process) users.
  - Installs restricted maintenance helpers (`/usr/local/bin/tako-server-install-refresh`, `/usr/local/bin/tako-server-service`) and a scoped sudoers policy so the `tako` SSH user can run upgrade/reload commands non-interactively.
  - If `TAKO_SSH_PUBKEY` is unset, prompts for a public key from the terminal (`/dev/tty`) when available, including common piped installs; invalid key lines are re-prompted. If key input cannot be read, installer tries the invoking sudo user's `~/.ssh/authorized_keys` first, then warns/skips if no valid key is found.
  - Installs service definitions based on host init system:
    - systemd unit with `Type=notify`, `ExecReload=/bin/kill -HUP $MAINPID`, and capability bounding for `CAP_NET_BIND_SERVICE`.
    - OpenRC init script with `reload` support and `retry="TERM/1800/KILL/5"` graceful-stop semantics.
  - Installs required runtime dependencies (including Unix-socket-capable `nc` with `-U` support, sqlite runtime libraries, and `mise`) via the host package manager when available.
  - Falls back to the official `mise` installer if distro package managers do not provide `mise`.
- `check_critical_coverage.sh`: coverage gate for selected critical source files.
- `release-notes.sh`: release notes generator used by SDK notes flow.

## Typical Usage

Run from repository root:

```bash
sh scripts/install-tako-cli.sh
sh scripts/install-tako-server.sh
bash scripts/check_critical_coverage.sh
sh scripts/release-notes.sh --component tako --prefix tako-v --output dist/release-notes/tako.md
```

The install scripts are exposed via website redirect endpoints:

- `/install`
- `/install-server`
- `/server-install`
- `/server-install-canary`
