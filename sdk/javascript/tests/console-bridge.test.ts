import { afterEach, beforeEach, describe, expect, test } from "bun:test";

import { installConsoleBridge, resetConsoleBridgeForTests } from "../src/tako/console-bridge";

let writes: string[] = [];
let originalWrite: typeof process.stdout.write;

function captured(): Array<Record<string, unknown>> {
  return writes.flatMap((chunk) =>
    chunk
      .split("\n")
      .filter((line) => line.length > 0)
      .map((line) => JSON.parse(line) as Record<string, unknown>),
  );
}

describe("installConsoleBridge", () => {
  beforeEach(() => {
    writes = [];
    originalWrite = process.stdout.write.bind(process.stdout);
    process.stdout.write = ((chunk: unknown): boolean => {
      writes.push(typeof chunk === "string" ? chunk : String(chunk));
      return true;
    }) as typeof process.stdout.write;
    resetConsoleBridgeForTests();
  });

  afterEach(() => {
    process.stdout.write = originalWrite;
    resetConsoleBridgeForTests();
  });

  test("console.log routes to info level", () => {
    installConsoleBridge("app");
    console.log("hello");
    expect(captured()[0]).toMatchObject({ level: "info", scope: "app", msg: "hello" });
  });

  test("console.info routes to info level", () => {
    installConsoleBridge("app");
    console.info("hi");
    expect(captured()[0]!["level"]).toBe("info");
  });

  test("console.warn routes to warn level", () => {
    installConsoleBridge("app");
    console.warn("watch out");
    expect(captured()[0]).toMatchObject({ level: "warn", msg: "watch out" });
  });

  test("console.error routes to error level", () => {
    installConsoleBridge("app");
    console.error("broken");
    expect(captured()[0]).toMatchObject({ level: "error", msg: "broken" });
  });

  test("console.debug routes to debug level", () => {
    installConsoleBridge("app");
    console.debug("trace");
    expect(captured()[0]!["level"]).toBe("debug");
  });

  test("format specifiers (%s, %d) are rendered", () => {
    installConsoleBridge("app");
    console.log("user=%s count=%d", "ada", 3);
    expect(captured()[0]!["msg"]).toBe("user=ada count=3");
  });

  test("multiple args are joined like console does", () => {
    installConsoleBridge("app");
    console.log("a", "b", 1);
    expect(captured()[0]!["msg"]).toBe("a b 1");
  });

  test("multi-line string is emitted as one event with newlines in msg", () => {
    installConsoleBridge("app");
    const stack = "Error: boom\n    at foo (x.ts:1:1)\n    at bar (y.ts:2:2)";
    console.error(stack);
    const lines = captured();
    expect(lines).toHaveLength(1);
    expect(lines[0]!["msg"]).toBe(stack);
  });

  test("Error-only argument uses a single-line summary as msg; stack stays in fields.error", () => {
    installConsoleBridge("app");
    const err = new Error("boom");
    err.stack = "Error: boom\n    at foo (x.ts:1:1)";
    console.error(err);
    const line = captured()[0]!;
    expect(line["msg"]).toBe("Error: boom");
    expect(line["msg"]).not.toContain("\n");
    const fields = line["fields"] as { error: { name: string; message: string; stack: string } };
    expect(fields.error.message).toBe("boom");
    expect(fields.error.stack).toBe(err.stack);
  });

  test("Error mixed with other args: single-line msg + full error on fields.error", () => {
    installConsoleBridge("app");
    const err = new Error("boom");
    err.stack = "Error: boom\n    at foo (x.ts:1:1)";
    console.error("context:", err);
    const line = captured()[0]!;
    expect(line["msg"]).toBe("context: Error: boom");
    expect(line["msg"]).not.toContain("\n");
    const fields = line["fields"] as { error: { message: string; stack: string } };
    expect(fields.error.message).toBe("boom");
    expect(fields.error.stack).toBe(err.stack);
  });

  test("is idempotent — second call is a no-op", () => {
    installConsoleBridge("app");
    const afterFirst = console.log;
    installConsoleBridge("app");
    expect(console.log).toBe(afterFirst);
  });

  test("resetConsoleBridgeForTests restores original methods", () => {
    const originalLog = console.log;
    installConsoleBridge("app");
    expect(console.log).not.toBe(originalLog);
    resetConsoleBridgeForTests();
    expect(console.log).toBe(originalLog);
  });

  test("scope is used on emitted log lines", () => {
    installConsoleBridge("worker");
    console.log("hi");
    expect(captured()[0]!["scope"]).toBe("worker");
  });
});
