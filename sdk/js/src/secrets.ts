/**
 * Tako Secrets
 *
 * Loads secrets from a file written by tako-server or tako dev,
 * and exposes them as properties with serialization protection.
 *
 * Secrets are never in process.env — they exist only in this object's
 * private closure, and toString/toJSON/inspect return "[REDACTED]".
 */

import { readFileSync, unlinkSync } from "node:fs";

/**
 * Creates a Secrets object with getter-based access to secret values.
 *
 * Each secret is a plain string accessible via property access.
 * The container itself resists serialization to prevent bulk leaks.
 */
export function loadSecrets(): Record<string, string> {
  const secretsFile = process.env.TAKO_SECRETS_FILE;
  if (!secretsFile) {
    return createSecrets({});
  }

  let raw: Record<string, string>;
  try {
    const content = readFileSync(secretsFile, "utf-8");
    raw = JSON.parse(content);
  } catch {
    return createSecrets({});
  }

  // Clean up the env var so child processes don't inherit the path
  delete process.env.TAKO_SECRETS_FILE;

  return createSecrets(raw);
}

function createSecrets(raw: Record<string, string>): Record<string, string> {
  const obj = Object.create(null);

  // Define toString/toJSON/inspect on the object itself
  Object.defineProperty(obj, "toString", {
    value: () => "[REDACTED]",
    enumerable: false,
  });
  Object.defineProperty(obj, "toJSON", {
    value: () => "[REDACTED]",
    enumerable: false,
  });
  Object.defineProperty(obj, Symbol.for("nodejs.util.inspect.custom"), {
    value: () => "[REDACTED]",
    enumerable: false,
  });

  // Define a getter for each secret
  for (const [key, value] of Object.entries(raw)) {
    let secretValue = value;
    Object.defineProperty(obj, key, {
      get: () => secretValue,
      enumerable: true,
      configurable: false,
    });
  }

  return Object.freeze(obj);
}
