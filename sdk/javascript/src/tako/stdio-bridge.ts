import { createLogger, resetLoggerOutputWriterForTests, setLoggerOutputWriter } from "../logger";

type Level = "info" | "warn";
type Writer = typeof process.stdout.write;

let installed = false;
let originalStdoutWrite: Writer | undefined;
let originalStderrWrite: Writer | undefined;

type PendingState = {
  buffer: string;
  scheduled: boolean;
};

/**
 * Replace raw `process.stdout.write` / `process.stderr.write` with a buffered
 * structured logger bridge. This catches framework/runtime output that bypasses
 * `console.*` and would otherwise be split into one log event per line by the
 * parent process's stdio reader.
 *
 * Synchronous bursts of writes are coalesced into a single event on the next
 * microtask, preserving embedded newlines in `msg`.
 */
export function installStdioBridge(scope: string): void {
  if (installed) return;
  installed = true;

  originalStdoutWrite = process.stdout.write.bind(process.stdout) as Writer;
  originalStderrWrite = process.stderr.write.bind(process.stderr) as Writer;
  setLoggerOutputWriter((chunk) => originalStdoutWrite!(chunk));

  const stdoutState: PendingState = { buffer: "", scheduled: false };
  const stderrState: PendingState = { buffer: "", scheduled: false };
  const stdoutLog = createLogger(scope);
  const stderrLog = createLogger(scope);

  process.stdout.write = wrapWrite(stdoutState, stdoutLog, "info") as typeof process.stdout.write;
  process.stderr.write = wrapWrite(stderrState, stderrLog, "warn") as typeof process.stderr.write;
}

function wrapWrite(
  state: PendingState,
  log: ReturnType<typeof createLogger>,
  level: Level,
): Writer {
  return ((chunk: unknown, encoding?: unknown, cb?: unknown): boolean => {
    const callback =
      typeof encoding === "function"
        ? (encoding as (() => void) | undefined)
        : typeof cb === "function"
          ? (cb as (() => void) | undefined)
          : undefined;

    state.buffer += normalizeChunk(chunk);
    scheduleFlush(state, log, level);
    if (callback) queueMicrotask(callback);
    return true;
  }) as Writer;
}

function normalizeChunk(chunk: unknown): string {
  if (typeof chunk === "string") return chunk;
  if (chunk instanceof Uint8Array) return Buffer.from(chunk).toString("utf8");
  return String(chunk);
}

function scheduleFlush(
  state: PendingState,
  log: ReturnType<typeof createLogger>,
  level: Level,
): void {
  if (state.scheduled) return;
  state.scheduled = true;
  queueMicrotask(() => {
    state.scheduled = false;
    const msg = stripTrailingLineBreaks(state.buffer);
    state.buffer = "";
    if (msg.length === 0) return;
    log[level](msg);
  });
}

function stripTrailingLineBreaks(value: string): string {
  return value.replace(/(?:\r?\n)+$/u, "");
}

/** @internal Reset module state between tests. Do not call from user code. */
export function resetStdioBridgeForTests(): void {
  if (originalStdoutWrite) {
    process.stdout.write = originalStdoutWrite;
  }
  if (originalStderrWrite) {
    process.stderr.write = originalStderrWrite;
  }
  originalStdoutWrite = undefined;
  originalStderrWrite = undefined;
  installed = false;
  resetLoggerOutputWriterForTests();
}
