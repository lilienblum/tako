/**
 * Bootstrap runtime + fd readers.
 *
 * Tako spawns each app process with a pipe on fd 3 containing a JSON
 * envelope `{"token": ..., "secrets": {...}}`. The runtime entrypoint
 * reads it at startup and calls `initBootstrapFromFd(reader)` before
 * the user's module is imported.
 *
 * The token is kept in module scope and used by the SDK to authenticate
 * server-issued `Host: tako.internal` requests — it is not exposed to
 * user code, and it does NOT leak to processes the app spawns (unlike
 * an env var would).
 *
 * Secrets are exposed through the `Tako.secrets` proxy. Its
 * `toString`/`toJSON`/inspect return `[REDACTED]` and its property
 * descriptors are non-enumerable, so bulk-spread (`{ ...Tako.secrets }`)
 * returns an empty object — individual access via `Tako.secrets.KEY`
 * still works through the `get` trap.
 */

import { closeSync, fstatSync, openSync, readFileSync } from "node:fs";

interface BootstrapEnvelope {
  token: string | null;
  secrets: Record<string, string>;
}

let bootstrap: BootstrapEnvelope = { token: null, secrets: {} };

/** Low-level: replace the whole bootstrap state (tests + explicit init). */
export function injectBootstrap(next: BootstrapEnvelope): void {
  bootstrap = {
    token: next.token,
    secrets: Object.assign(Object.create(null), next.secrets ?? {}),
  };
}

/** Returns the internal auth token, or `null` when running outside Tako. */
export function getInternalToken(): string | null {
  return bootstrap.token;
}

/** Build the proxy-backed accessor that becomes `Tako.secrets`. */
export function loadSecrets(): Record<string, string> {
  return new Proxy(Object.create(null) as Record<string, string>, {
    get(_target, prop: string | symbol): unknown {
      if (prop === "toString" || prop === "toJSON") return () => "[REDACTED]";
      if (prop === Symbol.for("nodejs.util.inspect.custom")) return () => "[REDACTED]";
      if (prop === Symbol.toPrimitive) return () => "[REDACTED]";
      if (typeof prop === "string") return bootstrap.secrets[prop];
      return undefined;
    },
    ownKeys(): string[] {
      return Object.keys(bootstrap.secrets);
    },
    getOwnPropertyDescriptor(_target, prop: string | symbol) {
      if (typeof prop === "string" && prop in bootstrap.secrets) {
        return { configurable: true, enumerable: false, value: bootstrap.secrets[prop] };
      }
      return undefined;
    },
    has(_target, prop: string | symbol): boolean {
      return typeof prop === "string" && prop in bootstrap.secrets;
    },
  });
}

/** Bun + Node: read the envelope from the inherited fd 3 directly. */
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

/** Run a reader, parse the JSON envelope, and populate token + secrets. */
export function initBootstrapFromFd(reader: () => string | null): void {
  const data = reader();
  if (data === null) return;
  let parsed: unknown;
  try {
    parsed = JSON.parse(data);
  } catch {
    console.error("Tako: invalid bootstrap JSON on fd 3");
    process.exit(1);
  }
  if (
    typeof parsed !== "object" ||
    parsed === null ||
    Array.isArray(parsed) ||
    typeof (parsed as { token?: unknown }).token !== "string" ||
    typeof (parsed as { secrets?: unknown }).secrets !== "object" ||
    (parsed as { secrets: unknown }).secrets === null ||
    Array.isArray((parsed as { secrets: unknown }).secrets)
  ) {
    console.error("Tako: bootstrap on fd 3 must be {token: string, secrets: object}");
    process.exit(1);
  }
  const envelope = parsed as { token: string; secrets: Record<string, string> };
  injectBootstrap({ token: envelope.token, secrets: envelope.secrets });
}
