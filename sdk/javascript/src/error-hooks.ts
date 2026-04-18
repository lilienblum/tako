import { createLogger } from "./logger";

let installed = false;
let uncaughtHandler: ((err: unknown) => void) | undefined;
let rejectionHandler: ((reason: unknown) => void) | undefined;

/**
 * Install runtime-level listeners that capture uncaught errors and unhandled
 * promise rejections and emit them as single structured log events via the
 * Tako SDK logger. The full stack travels in the `msg` field so downstream
 * renderers see one event per error instead of one event per stack frame line.
 *
 * On `uncaughtException`: logs then schedules `process.exit(1)` via
 * `setImmediate` so the stdout pipe has a tick to flush.
 * On `unhandledRejection`: logs but does not exit.
 *
 * Idempotent: calling twice registers listeners only once.
 *
 * @param scope - Log scope used on emitted lines (e.g. `"app"`, `"worker"`).
 */
export function installErrorHooks(scope: string): void {
  if (installed) return;
  installed = true;
  const log = createLogger(scope);

  uncaughtHandler = (err: unknown) => {
    const { msg, fieldError } = describeError(err);
    log.error(msg, { error: fieldError, kind: "uncaughtException" });
    setImmediate(() => process.exit(1));
  };
  rejectionHandler = (reason: unknown) => {
    const { msg, fieldError } = describeError(reason);
    log.error(msg, { error: fieldError, kind: "unhandledRejection" });
  };

  process.on("uncaughtException", uncaughtHandler);
  process.on("unhandledRejection", rejectionHandler);
}

function describeError(value: unknown): { msg: string; fieldError: unknown } {
  if (value instanceof Error) {
    const msg =
      value.stack && value.stack.length > 0 ? value.stack : `${value.name}: ${value.message}`;
    return { msg, fieldError: value };
  }
  return { msg: String(value), fieldError: value };
}

/** @internal Reset module state between tests. Do not call from user code. */
export function resetErrorHooksForTests(): void {
  if (uncaughtHandler) process.off("uncaughtException", uncaughtHandler);
  if (rejectionHandler) process.off("unhandledRejection", rejectionHandler);
  uncaughtHandler = undefined;
  rejectionHandler = undefined;
  installed = false;
}
