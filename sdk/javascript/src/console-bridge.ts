import { format } from "node:util";

import { createLogger, type Logger } from "./logger";

type ConsoleMethod = "log" | "info" | "warn" | "error" | "debug";

let installed = false;
const originals: Partial<Record<ConsoleMethod, (...args: unknown[]) => void>> = {};

/**
 * Replace `console.log/info/warn/error/debug` with wrappers that emit one
 * structured log event per call via the Tako SDK logger. Multi-line strings
 * (such as pre-formatted error stacks from frameworks) travel as a single
 * event with newlines preserved in the `msg` field, so the daemon renders
 * them as one log entry instead of one-per-line.
 *
 * `Error` arguments are detected: the first `Error` in the args becomes
 * `fields.error` (auto-serialized to `{name, message, stack}`). If the only
 * argument is an `Error`, its `stack` becomes the `msg` so the renderer
 * shows the full trace.
 *
 * Idempotent: calling twice leaves the console as it was after the first call.
 *
 * @param scope - Log scope used on emitted lines (e.g. `"app"`, `"worker"`).
 */
export function installConsoleBridge(scope: string): void {
  if (installed) return;
  installed = true;
  const log = createLogger(scope);

  wrap("log", (...args) => emit(log, "info", args));
  wrap("info", (...args) => emit(log, "info", args));
  wrap("warn", (...args) => emit(log, "warn", args));
  wrap("error", (...args) => emit(log, "error", args));
  wrap("debug", (...args) => emit(log, "debug", args));
}

function wrap(method: ConsoleMethod, impl: (...args: unknown[]) => void): void {
  originals[method] = console[method] as (...args: unknown[]) => void;
  (console as unknown as Record<ConsoleMethod, (...args: unknown[]) => void>)[method] = impl;
}

function emit(log: Logger, level: "info" | "warn" | "error" | "debug", args: unknown[]): void {
  const error = args.find((a): a is Error => a instanceof Error);
  let msg: string;
  if (args.length === 1 && error) {
    msg = error.stack && error.stack.length > 0 ? error.stack : `${error.name}: ${error.message}`;
  } else {
    msg = format(...args);
  }
  const fields = error ? { error } : undefined;
  log[level](msg, fields);
}

/** @internal Reset module state between tests. Do not call from user code. */
export function resetConsoleBridgeForTests(): void {
  for (const [method, original] of Object.entries(originals) as Array<
    [ConsoleMethod, (...args: unknown[]) => void]
  >) {
    (console as unknown as Record<ConsoleMethod, (...args: unknown[]) => void>)[method] = original;
  }
  for (const key of Object.keys(originals) as ConsoleMethod[]) {
    delete originals[key];
  }
  installed = false;
}
