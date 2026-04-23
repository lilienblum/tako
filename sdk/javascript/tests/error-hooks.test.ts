import { afterEach, beforeEach, describe, expect, test } from "bun:test";

import { installErrorHooks, resetErrorHooksForTests } from "../src/tako/error-hooks";

let writes: string[] = [];
let originalWrite: typeof process.stdout.write;
let originalExit: typeof process.exit;
let exitCode: number | undefined;

function captured(): Array<Record<string, unknown>> {
  return writes.flatMap((chunk) =>
    chunk
      .split("\n")
      .filter((line) => line.length > 0)
      .map((line) => JSON.parse(line) as Record<string, unknown>),
  );
}

describe("installErrorHooks", () => {
  beforeEach(() => {
    writes = [];
    exitCode = undefined;
    originalWrite = process.stdout.write.bind(process.stdout);
    process.stdout.write = ((chunk: unknown): boolean => {
      writes.push(typeof chunk === "string" ? chunk : String(chunk));
      return true;
    }) as typeof process.stdout.write;
    // oxlint-disable-next-line typescript/unbound-method -- capturing native exit reference to restore later
    originalExit = process.exit;
    (process as { exit: (code?: number) => never }).exit = ((code?: number) => {
      exitCode = code;
      return undefined as never;
    }) as typeof process.exit;
    resetErrorHooksForTests();
  });

  afterEach(() => {
    process.stdout.write = originalWrite;
    process.exit = originalExit;
    resetErrorHooksForTests();
  });

  test("registers an uncaughtException listener", () => {
    const before = process.listenerCount("uncaughtException");
    installErrorHooks("app");
    expect(process.listenerCount("uncaughtException")).toBe(before + 1);
  });

  test("registers an unhandledRejection listener", () => {
    const before = process.listenerCount("unhandledRejection");
    installErrorHooks("app");
    expect(process.listenerCount("unhandledRejection")).toBe(before + 1);
  });

  test("is idempotent — calling twice only registers once", () => {
    const before = process.listenerCount("uncaughtException");
    installErrorHooks("app");
    installErrorHooks("app");
    expect(process.listenerCount("uncaughtException")).toBe(before + 1);
  });

  test("uncaughtException emits one error log with full stack in msg", async () => {
    installErrorHooks("app");
    const err = new Error("boom");
    err.stack = "Error: boom\n    at foo (x.ts:1:1)\n    at bar (y.ts:2:2)";
    process.emit("uncaughtException", err);
    await new Promise<void>((r) => setImmediate(r));

    const lines = captured();
    expect(lines).toHaveLength(1);
    expect(lines[0]).toMatchObject({
      level: "error",
      scope: "app",
      msg: err.stack,
    });
    const fields = lines[0]!["fields"] as {
      error: { name: string; message: string; stack: string };
      kind: string;
    };
    expect(fields.error.name).toBe("Error");
    expect(fields.error.message).toBe("boom");
    expect(fields.kind).toBe("uncaughtException");
  });

  test("uncaughtException with an error missing .stack falls back to name: message", async () => {
    installErrorHooks("app");
    const err = new Error("no-stack");
    (err as { stack?: string }).stack = undefined;
    process.emit("uncaughtException", err);
    await new Promise<void>((r) => setImmediate(r));
    expect(captured()[0]!["msg"]).toBe("Error: no-stack");
  });

  test("uncaughtException with a non-Error value stringifies it", async () => {
    installErrorHooks("app");
    process.emit("uncaughtException", "plain string" as unknown as Error);
    await new Promise<void>((r) => setImmediate(r));
    expect(captured()[0]!["msg"]).toBe("plain string");
  });

  test("uncaughtException schedules process.exit(1)", async () => {
    installErrorHooks("app");
    process.emit("uncaughtException", new Error("x"));
    await new Promise<void>((r) => setImmediate(r));
    expect(exitCode).toBe(1);
  });

  test("unhandledRejection emits one error log with rejection reason", () => {
    installErrorHooks("app");
    const reason = new Error("rejected");
    reason.stack = "Error: rejected\n    at foo (x.ts:1:1)";
    const p = Promise.reject(reason);
    p.catch(() => {});
    process.emit("unhandledRejection", reason, p);

    const lines = captured();
    expect(lines).toHaveLength(1);
    expect(lines[0]).toMatchObject({
      level: "error",
      scope: "app",
      msg: reason.stack,
    });
    const fields = lines[0]!["fields"] as { kind: string };
    expect(fields.kind).toBe("unhandledRejection");
  });

  test("unhandledRejection does NOT call process.exit", () => {
    installErrorHooks("app");
    const p = Promise.reject(new Error("x"));
    p.catch(() => {});
    process.emit("unhandledRejection", new Error("x"), p);
    expect(exitCode).toBeUndefined();
  });

  test("scope is used on emitted log lines", async () => {
    installErrorHooks("worker");
    process.emit("uncaughtException", new Error("x"));
    await new Promise<void>((r) => setImmediate(r));
    expect(captured()[0]!["scope"]).toBe("worker");
  });
});
