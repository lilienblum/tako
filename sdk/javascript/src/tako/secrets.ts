/**
 * Secrets runtime + fd readers.
 *
 * Tako spawns each app process with a pipe on fd 3 containing a JSON blob of
 * secrets. Each runtime entrypoint reads them at startup and calls
 * `initSecretsFromFd(reader)` before the user's module is imported. The
 * secrets store is then exposed through the `Tako.secrets` proxy.
 *
 * The proxy's `toString`/`toJSON`/inspect return `[REDACTED]` and its
 * property descriptors are non-enumerable, so bulk-spread (`{ ...Tako.secrets }`)
 * returns an empty object — individual access via `Tako.secrets.KEY` still
 * works through the `get` trap.
 */

import { closeSync, fstatSync, openSync, readFileSync } from "node:fs";

/** Module-level secrets store, populated by initSecretsFromFd / injectSecrets. */
let secretStore: Record<string, string> = {};

/** Low-level: replace the store directly. */
export function injectSecrets(raw: Record<string, string>): void {
  secretStore = raw;
}

/** Build the proxy-backed accessor that becomes `Tako.secrets`. */
export function loadSecrets(): Record<string, string> {
  return new Proxy(Object.create(null) as Record<string, string>, {
    get(_target, prop: string | symbol): unknown {
      if (prop === "toString" || prop === "toJSON") return () => "[REDACTED]";
      if (prop === Symbol.for("nodejs.util.inspect.custom")) return () => "[REDACTED]";
      if (prop === Symbol.toPrimitive) return () => "[REDACTED]";
      if (typeof prop === "string") return secretStore[prop];
      return undefined;
    },
    ownKeys(): string[] {
      return Object.keys(secretStore);
    },
    getOwnPropertyDescriptor(_target, prop: string | symbol) {
      if (typeof prop === "string" && prop in secretStore) {
        return { configurable: true, enumerable: false, value: secretStore[prop] };
      }
      return undefined;
    },
    has(_target, prop: string | symbol): boolean {
      return typeof prop === "string" && prop in secretStore;
    },
  });
}

/** Bun + Node: read secrets from the inherited fd 3 directly. */
export function readViaInheritedFd(): string | null {
  try {
    // Guard against blocking on a non-Tako inherited fd (e.g. GitHub Actions).
    const stat = fstatSync(3);
    if (!stat.isFIFO()) return null;
    const data = readFileSync(3, "utf-8");
    closeSync(3);
    return data;
  } catch {
    return null;
  }
}

/** Deno: open fd 3 via `/proc/self/fd/3` (Linux) or `/dev/fd/3` (macOS). */
export function readViaProcSelfFd(): string | null {
  for (const path of ["/proc/self/fd/3", "/dev/fd/3"]) {
    try {
      const fd = openSync(path, "r");
      const data = readFileSync(fd, "utf-8");
      closeSync(fd);
      return data;
    } catch {
      // Try next path.
    }
  }
  return null;
}

/** Run a reader, parse the JSON, and inject into the secrets store. */
export function initSecretsFromFd(reader: () => string | null): void {
  const data = reader();
  if (data === null) return;
  try {
    const secrets = JSON.parse(data);
    if (typeof secrets !== "object" || secrets === null || Array.isArray(secrets)) {
      console.error("Tako: secrets on fd 3 must be a JSON object");
      process.exit(1);
    }
    injectSecrets(Object.assign(Object.create(null), secrets));
  } catch {
    console.error("Tako: invalid secrets JSON on fd 3");
    process.exit(1);
  }
}
