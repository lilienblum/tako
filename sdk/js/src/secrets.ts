/**
 * Tako Secrets
 *
 * In production: secrets are pushed by tako-server via `POST /secrets`
 * on `Host: tako`. They never touch disk.
 *
 * In dev mode: secrets are read from a file via `TAKO_SECRETS_FILE` as a
 * fallback (tako dev writes this file).
 *
 * The returned secrets object is a Proxy that reads from a mutable store,
 * so secrets become available as soon as they're pushed — even if the
 * proxy was created before the push arrived.
 *
 * toString/toJSON/inspect return "[REDACTED]" to prevent bulk leaks.
 */

import { readFileSync } from "node:fs";

/** Module-level secrets store, populated by injectSecrets() or dev fallback. */
let secretStore: Record<string, string> = {};

// Dev-mode fallback: if TAKO_SECRETS_FILE is set (by tako dev), load from file.
if (process.env.TAKO_SECRETS_FILE) {
  try {
    const content = readFileSync(process.env.TAKO_SECRETS_FILE, "utf-8");
    secretStore = JSON.parse(content);
  } catch {
    // Ignore — secrets will be empty
  }
  delete process.env.TAKO_SECRETS_FILE;
}

/**
 * Called by the `POST /secrets` endpoint handler to populate secrets.
 */
export function injectSecrets(raw: Record<string, string>): void {
  secretStore = raw;
}

/**
 * Creates a Proxy-backed secrets accessor.
 *
 * The proxy reads from `secretStore` on every access, so secrets are
 * available as soon as `injectSecrets()` is called — even if the proxy
 * was created earlier.
 */
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
        return {
          configurable: true,
          enumerable: true,
          value: secretStore[prop],
        };
      }
      return undefined;
    },
    has(_target, prop: string | symbol): boolean {
      return typeof prop === "string" && prop in secretStore;
    },
  });
}
