/**
 * Readiness-fd writers, one per runtime ABI.
 *
 * Tako spawns an app process with a pipe on fd 4 expecting the resolved
 * HTTP port as `{port}\n`. Bun and Node write the inherited fd directly;
 * Deno's node compat can't, so it opens the fd via `/proc/self/fd/N`
 * (Linux) or `/dev/fd/N` (macOS).
 */

import { closeSync, openSync, writeSync } from "node:fs";

/** Default: write to the inherited fd directly (Bun + Node). */
export function writeViaInheritedFd(fd: number, port: number): void {
  try {
    writeSync(fd, `${port}\n`);
    closeSync(fd);
  } catch {
    // Not running under Tako or readiness pipe unavailable.
  }
}

/** Deno: open the fd through /proc/self/fd/N or /dev/fd/N. */
export function writeViaProcSelfFd(fd: number, port: number): void {
  for (const path of [`/proc/self/fd/${fd}`, `/dev/fd/${fd}`]) {
    try {
      const newFd = openSync(path, "w");
      writeSync(newFd, `${port}\n`);
      closeSync(newFd);
      return;
    } catch {
      // Try next path or give up.
    }
  }
}
