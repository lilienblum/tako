import type { Logger as ViteLogger } from "vite";

type Level = "debug" | "info" | "warn" | "error";
type Fields = Record<string, unknown>;

function autoPopulate(): Fields {
  const fields: Fields = {};
  const build = process.env["TAKO_BUILD"];
  const instance = process.env["TAKO_INSTANCE_ID"];
  if (build !== undefined) fields["build"] = build;
  if (instance !== undefined) fields["instance"] = instance;
  return fields;
}

function expandErrors(fields: Fields): Fields {
  const out: Fields = {};
  for (const [key, value] of Object.entries(fields)) {
    out[key] =
      value instanceof Error
        ? { name: value.name, message: value.message, stack: value.stack ?? "" }
        : value;
  }
  return out;
}

/**
 * Structured JSON logger used across the Tako SDK and user apps.
 *
 * Writes one JSON object per line to `stdout` with `ts`, `level`, `scope`,
 * `msg`, and an optional `fields` bag merged from three scopes: process
 * globals (see {@link setGlobals}), logger-local fields (see {@link child}),
 * and per-call fields. `Error` values in `fields` are serialized to
 * `{ name, message, stack }` automatically.
 *
 * Obtain an instance with {@link createLogger} or read `Tako.logger`.
 */
export class Logger {
  static #globals: Fields = autoPopulate();

  readonly #scope: string;
  readonly #localFields: Fields;

  /**
   * @param scope - Origin tag emitted as `scope` on every line (e.g. `"app"`).
   * @param localFields - Fields attached to every line from this instance.
   */
  constructor(scope: string, localFields: Fields = {}) {
    this.#scope = scope;
    this.#localFields = localFields;
  }

  /**
   * Merge fields into the process-global bag. Every log line from every
   * `Logger` instance will include these under `fields`. Intended for
   * startup-time configuration (package version, region, etc.) — do not call
   * per-request, global state leaks across concurrent work.
   */
  setGlobals(fields: Fields): void {
    Logger.#globals = { ...Logger.#globals, ...fields };
  }

  /**
   * Return a new sub-logger. Pass `scope` to rebrand the log origin, and/or
   * `fields` to attach fields to every log line from the sub-logger. The
   * parent is not mutated.
   */
  child(scope?: string, fields?: Fields): Logger {
    return new Logger(scope ?? this.#scope, { ...this.#localFields, ...fields });
  }

  /**
   * Emit a `debug`-level line.
   * @param msg - Human-readable message.
   * @param fields - Optional per-call fields merged into the `fields` bag.
   */
  debug(msg: string, fields?: Fields): void {
    this.#emit("debug", msg, fields);
  }
  /**
   * Emit an `info`-level line.
   * @param msg - Human-readable message.
   * @param fields - Optional per-call fields merged into the `fields` bag.
   */
  info(msg: string, fields?: Fields): void {
    this.#emit("info", msg, fields);
  }
  /**
   * Emit a `warn`-level line.
   * @param msg - Human-readable message.
   * @param fields - Optional per-call fields merged into the `fields` bag.
   */
  warn(msg: string, fields?: Fields): void {
    this.#emit("warn", msg, fields);
  }
  /**
   * Emit an `error`-level line. Pass an `Error` in `fields` to auto-serialize it.
   * @param msg - Human-readable message.
   * @param fields - Optional per-call fields merged into the `fields` bag.
   */
  error(msg: string, fields?: Fields): void {
    this.#emit("error", msg, fields);
  }

  /**
   * Return a Vite-compatible `Logger` adapter. Pass to `customLogger` in a
   * Vite config to route Vite's own logs through this logger.
   *
   * Normalizes Vite's pretty-print conventions at this bridge: strips
   * leading/trailing blank lines from messages and drops whitespace-only
   * calls (Vite uses those as spacers in its default text logger). The core
   * `Logger` itself stays verbatim — this is adapter-only.
   */
  toViteLogger(): ViteLogger {
    const seenWarnings = new Set<string>();
    const seenErrors = new WeakSet<object>();
    // CodeQL[js/polynomial-redos]: split/join avoids the \s/\n overlap that
    // makes anchored regexes like /^\s*\n|\n\s*$/g polynomial on all-newline
    // input. Strips fully blank outer lines while preserving intra-line
    // indentation on the first/last content lines.
    const normalize = (msg: string): string | null => {
      const lines = msg.split("\n");
      let first = 0;
      while (first < lines.length && lines[first]!.trim() === "") first++;
      if (first === lines.length) return null;
      let last = lines.length - 1;
      while (lines[last]!.trim() === "") last--;
      return lines.slice(first, last + 1).join("\n");
    };
    const self: ViteLogger = {
      hasWarned: false,
      info: (msg) => {
        const n = normalize(msg);
        if (n === null) return;
        this.#emit("info", n);
      },
      warn: (msg) => {
        const n = normalize(msg);
        if (n === null) return;
        self.hasWarned = true;
        this.#emit("warn", n);
      },
      warnOnce: (msg) => {
        const n = normalize(msg);
        if (n === null) return;
        if (seenWarnings.has(n)) return;
        seenWarnings.add(n);
        self.hasWarned = true;
        this.#emit("warn", n);
      },
      error: (msg, opts) => {
        const err = opts?.error;
        if (err) seenErrors.add(err);
        const n = normalize(msg);
        if (n === null) return;
        this.#emit("error", n);
      },
      clearScreen: () => {},
      hasErrorLogged: (err) => seenErrors.has(err as object),
    };
    return self;
  }

  #emit(level: Level, msg: string, callFields?: Fields): void {
    const merged = expandErrors({
      ...Logger.#globals,
      ...this.#localFields,
      ...callFields,
    });
    const payload: Record<string, unknown> = {
      ts: Date.now(),
      level,
      scope: this.#scope,
      msg,
    };
    if (Object.keys(merged).length > 0) {
      payload["fields"] = merged;
    }
    process.stdout.write(`${JSON.stringify(payload)}\n`);
  }

  /** @internal Reset static state between tests. Do not call from user code. */
  static resetForTests(): void {
    Logger.#globals = autoPopulate();
  }
}

/**
 * Create a new {@link Logger} at the given scope.
 *
 * Prefer this over `new Logger(...)` so the constructor signature can evolve
 * without breaking callers.
 *
 * @param scope - Origin tag emitted as `scope` on every line.
 */
export function createLogger(scope: string): Logger {
  return new Logger(scope);
}
