import { afterEach, beforeEach, describe, expect, test } from "bun:test";

import { createLogger, resetLoggerOutputWriterForTests } from "../src/logger";
import { installStdioBridge, resetStdioBridgeForTests } from "../src/tako/stdio-bridge";

let writes: string[] = [];
let originalStdoutWrite: typeof process.stdout.write;
let originalStderrWrite: typeof process.stderr.write;

function captured(): Array<Record<string, unknown>> {
  return writes.flatMap((chunk) =>
    chunk
      .split("\n")
      .filter((line) => line.length > 0)
      .map((line) => JSON.parse(line) as Record<string, unknown>),
  );
}

describe("installStdioBridge", () => {
  beforeEach(() => {
    writes = [];
    originalStdoutWrite = process.stdout.write.bind(process.stdout);
    originalStderrWrite = process.stderr.write.bind(process.stderr);

    process.stdout.write = ((chunk: unknown): boolean => {
      writes.push(typeof chunk === "string" ? chunk : String(chunk));
      return true;
    }) as typeof process.stdout.write;
    process.stderr.write = ((chunk: unknown): boolean => {
      writes.push(typeof chunk === "string" ? chunk : String(chunk));
      return true;
    }) as typeof process.stderr.write;

    resetLoggerOutputWriterForTests();
    resetStdioBridgeForTests();
  });

  afterEach(() => {
    process.stdout.write = originalStdoutWrite;
    process.stderr.write = originalStderrWrite;
    resetStdioBridgeForTests();
    resetLoggerOutputWriterForTests();
  });

  test("raw stderr write with embedded newlines is emitted as one warn event", async () => {
    installStdioBridge("app");

    process.stderr.write("Error: boom\n    at foo (x.ts:1:1)\n    at bar (y.ts:2:2)\n");
    await new Promise<void>((resolve) => queueMicrotask(resolve));

    const lines = captured();
    expect(lines).toHaveLength(1);
    expect(lines[0]).toMatchObject({
      level: "warn",
      scope: "app",
      msg: "Error: boom\n    at foo (x.ts:1:1)\n    at bar (y.ts:2:2)",
    });
  });

  test("consecutive synchronous stderr writes coalesce into one event", async () => {
    installStdioBridge("app");

    process.stderr.write("Error: boom\n");
    process.stderr.write("    at foo (x.ts:1:1)\n");
    process.stderr.write("    at bar (y.ts:2:2)\n");
    await new Promise<void>((resolve) => queueMicrotask(resolve));

    const lines = captured();
    expect(lines).toHaveLength(1);
    expect(lines[0]!["msg"]).toBe("Error: boom\n    at foo (x.ts:1:1)\n    at bar (y.ts:2:2)");
  });

  test("logger output bypasses the bridge and does not recurse", async () => {
    installStdioBridge("app");

    createLogger("app").info("hello");
    await new Promise<void>((resolve) => queueMicrotask(resolve));

    const lines = captured();
    expect(lines).toHaveLength(1);
    expect(lines[0]).toMatchObject({
      level: "info",
      scope: "app",
      msg: "hello",
    });
  });

  test("is idempotent", () => {
    /* oxlint-disable typescript/unbound-method -- identity comparison on monkey-patched methods */
    installStdioBridge("app");
    const stdoutAfterFirst = process.stdout.write;
    const stderrAfterFirst = process.stderr.write;

    installStdioBridge("app");

    expect(process.stdout.write).toBe(stdoutAfterFirst);
    expect(process.stderr.write).toBe(stderrAfterFirst);
    /* oxlint-enable typescript/unbound-method */
  });
});
