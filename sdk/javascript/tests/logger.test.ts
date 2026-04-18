import { afterEach, beforeEach, describe, expect, test } from "bun:test";

import { createLogger, Logger } from "../src/logger";

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

function installStdoutCapture(): void {
  writes = [];
  originalWrite = process.stdout.write.bind(process.stdout);
  process.stdout.write = ((chunk: unknown): boolean => {
    writes.push(typeof chunk === "string" ? chunk : String(chunk));
    return true;
  }) as typeof process.stdout.write;
}

describe("Logger", () => {
  beforeEach(() => {
    installStdoutCapture();
    Logger.resetForTests();
  });

  afterEach(() => {
    process.stdout.write = originalWrite;
    Logger.resetForTests();
  });

  test("createLogger returns a Logger instance", () => {
    expect(createLogger("vite")).toBeInstanceOf(Logger);
  });

  test("info() emits root keys ts/level/scope/msg with no fields when empty", () => {
    createLogger("vite").info("listening");
    const [line] = captured();
    expect(line).toEqual({
      ts: expect.any(Number),
      level: "info",
      scope: "vite",
      msg: "listening",
    });
    expect(line).not.toHaveProperty("fields");
  });

  test("warn() and error() emit the correct level", () => {
    const log = createLogger("vite");
    log.warn("slow");
    log.error("oops");
    expect(captured().map((l) => l.level)).toEqual(["warn", "error"]);
  });

  test("per-call fields go under fields, not at the root", () => {
    createLogger("vite").info("bound", { port: 5173 });
    const [line] = captured();
    expect(line).toMatchObject({ msg: "bound", fields: { port: 5173 } });
    expect(line).not.toHaveProperty("port");
  });

  test("system fields at root are not overridden by user fields named the same", () => {
    createLogger("vite").info("hi", {
      ts: 0,
      level: "fatal",
      scope: "fake",
      msg: "fake",
    });
    const [line] = captured();
    expect(line.level).toBe("info");
    expect(line.scope).toBe("vite");
    expect(line.msg).toBe("hi");
    expect(line.ts).not.toBe(0);
    // The conflicting names still travel through fields — they're user data.
    expect(line.fields).toMatchObject({ ts: 0, level: "fatal", scope: "fake", msg: "fake" });
  });

  describe("child()", () => {
    test("with a source arg returns a logger with the new scope", () => {
      createLogger("tako").child("vite").info("hi");
      expect(captured()[0]).toMatchObject({ scope: "vite", msg: "hi" });
    });

    test("with fields adds them under fields", () => {
      createLogger("app").child(undefined, { requestId: "abc" }).info("done");
      expect(captured()[0]).toMatchObject({ scope: "app", fields: { requestId: "abc" } });
    });

    test("does not mutate the parent", () => {
      const parent = createLogger("app");
      parent.child("auth", { userId: 7 }).info("c");
      parent.info("p");
      const [childLine, parentLine] = captured();
      expect(childLine).toMatchObject({ scope: "auth", fields: { userId: 7 } });
      expect(parentLine.scope).toBe("app");
      expect(parentLine).not.toHaveProperty("fields");
    });
  });

  describe("setGlobals()", () => {
    test("globals appear under fields on every log", () => {
      createLogger("a").setGlobals({ pkgVersion: "1.2.3" });
      createLogger("b").info("hi");
      expect(captured()[0]).toMatchObject({ scope: "b", fields: { pkgVersion: "1.2.3" } });
    });

    test("merges with previous globals", () => {
      const log = createLogger("a");
      log.setGlobals({ pkgVersion: "1.2.3" });
      log.setGlobals({ region: "us-east" });
      log.info("hi");
      expect(captured()[0]).toMatchObject({
        fields: { pkgVersion: "1.2.3", region: "us-east" },
      });
    });

    test("per-call fields override globals", () => {
      const log = createLogger("a");
      log.setGlobals({ region: "us-east" });
      log.info("hi", { region: "eu-west" });
      expect(captured()[0]).toMatchObject({ fields: { region: "eu-west" } });
    });

    test("child fields override globals", () => {
      const log = createLogger("a");
      log.setGlobals({ region: "us-east" });
      log.child(undefined, { region: "eu-west" }).info("hi");
      expect(captured()[0]).toMatchObject({ fields: { region: "eu-west" } });
    });
  });

  describe("auto-populated fields", () => {
    test("populates build/instance from TAKO_BUILD/TAKO_INSTANCE_ID into fields", () => {
      const original = { build: process.env.TAKO_BUILD, inst: process.env.TAKO_INSTANCE_ID };
      process.env.TAKO_BUILD = "deploy-abc";
      process.env.TAKO_INSTANCE_ID = "inst-7";
      try {
        Logger.resetForTests();
        createLogger("vite").info("hi");
        expect(captured()[0]).toMatchObject({
          fields: { build: "deploy-abc", instance: "inst-7" },
        });
      } finally {
        if (original.build === undefined) delete process.env.TAKO_BUILD;
        else process.env.TAKO_BUILD = original.build;
        if (original.inst === undefined) delete process.env.TAKO_INSTANCE_ID;
        else process.env.TAKO_INSTANCE_ID = original.inst;
        Logger.resetForTests();
      }
    });

    test("omits build/instance when env vars are unset", () => {
      const original = { build: process.env.TAKO_BUILD, inst: process.env.TAKO_INSTANCE_ID };
      delete process.env.TAKO_BUILD;
      delete process.env.TAKO_INSTANCE_ID;
      try {
        Logger.resetForTests();
        createLogger("vite").info("hi");
        const [line] = captured();
        expect(line).not.toHaveProperty("fields");
      } finally {
        if (original.build !== undefined) process.env.TAKO_BUILD = original.build;
        if (original.inst !== undefined) process.env.TAKO_INSTANCE_ID = original.inst;
        Logger.resetForTests();
      }
    });

    test("user setGlobals() can override auto-populated fields", () => {
      const original = process.env.TAKO_BUILD;
      process.env.TAKO_BUILD = "deploy-abc";
      try {
        Logger.resetForTests();
        const log = createLogger("vite");
        log.setGlobals({ build: "override-xyz" });
        log.info("hi");
        expect(captured()[0]).toMatchObject({ fields: { build: "override-xyz" } });
      } finally {
        if (original === undefined) delete process.env.TAKO_BUILD;
        else process.env.TAKO_BUILD = original;
        Logger.resetForTests();
      }
    });
  });

  describe("Error serialization", () => {
    test("Error in fields expands to name/message/stack", () => {
      const err = new Error("boom");
      createLogger("vite").error("fail", { error: err });
      const [line] = captured();
      const fields = line.fields as { error: { name: string; message: string; stack: string } };
      expect(fields.error.name).toBe("Error");
      expect(fields.error.message).toBe("boom");
      expect(typeof fields.error.stack).toBe("string");
      expect(fields.error.stack.length).toBeGreaterThan(0);
    });

    test("Error subclasses use the actual class name", () => {
      class MyErr extends Error {
        override name = "MyErr";
      }
      createLogger("vite").error("fail", { error: new MyErr("x") });
      const fields = captured()[0]!.fields as { error: { name: string } };
      expect(fields.error.name).toBe("MyErr");
    });

    test("non-Error values pass through unchanged", () => {
      createLogger("vite").info("hi", { count: 5, label: "x" });
      expect(captured()[0]).toMatchObject({ fields: { count: 5, label: "x" } });
    });
  });

  describe("toViteLogger", () => {
    test("exposes the Vite Logger shape", () => {
      const vl = createLogger("vite").toViteLogger();
      expect(typeof vl.info).toBe("function");
      expect(typeof vl.warn).toBe("function");
      expect(typeof vl.warnOnce).toBe("function");
      expect(typeof vl.error).toBe("function");
      expect(typeof vl.clearScreen).toBe("function");
      expect(typeof vl.hasErrorLogged).toBe("function");
      expect(vl.hasWarned).toBe(false);
    });

    test("info/warn/error route through the tako logger", () => {
      const vl = createLogger("vite").toViteLogger();
      vl.info("i");
      vl.warn("w");
      vl.error("e");
      expect(captured().map((l) => [l.level, l.msg])).toEqual([
        ["info", "i"],
        ["warn", "w"],
        ["error", "e"],
      ]);
    });

    test("warn() sets hasWarned", () => {
      const vl = createLogger("vite").toViteLogger();
      vl.warn("x");
      expect(vl.hasWarned).toBe(true);
    });

    test("warnOnce deduplicates identical messages", () => {
      const vl = createLogger("vite").toViteLogger();
      vl.warnOnce("same");
      vl.warnOnce("same");
      vl.warnOnce("other");
      expect(captured()).toHaveLength(2);
    });

    test("hasErrorLogged tracks errors by reference", () => {
      const vl = createLogger("vite").toViteLogger();
      const err = new Error("boom");
      expect(vl.hasErrorLogged(err)).toBe(false);
      vl.error("boom", { error: err });
      expect(vl.hasErrorLogged(err)).toBe(true);
    });

    test("strips leading and trailing whitespace/newlines from Vite messages", () => {
      const vl = createLogger("vite").toViteLogger();
      vl.info("\n  VITE v8 ready\n");
      expect(captured()[0]).toMatchObject({ msg: "  VITE v8 ready" });
    });

    test("drops empty or whitespace-only messages from Vite", () => {
      const vl = createLogger("vite").toViteLogger();
      vl.info("");
      vl.info("\n");
      vl.info("   ");
      vl.info("real");
      expect(captured()).toHaveLength(1);
      expect(captured()[0]).toMatchObject({ msg: "real" });
    });

    test("core logger still emits empty and newline-containing messages", () => {
      const log = createLogger("vite");
      log.info("");
      log.info("line1\nline2");
      const lines = captured();
      expect(lines).toHaveLength(2);
      expect(lines[0]!.msg).toBe("");
      expect(lines[1]!.msg).toBe("line1\nline2");
    });
  });
});
