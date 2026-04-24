import { test, expect } from "bun:test";
import { resolve } from "node:path";

test("tako.sh/runtime exports createLogger and loadSecrets", async () => {
  const mod = await import("../src/runtime");
  expect(typeof mod.createLogger).toBe("function");
  expect(typeof mod.loadSecrets).toBe("function");
  expect(typeof mod.Logger).toBe("function");
});

test("tako.sh/runtime bundles cleanly for the browser (no node:* specifiers)", async () => {
  const result = await Bun.build({
    entrypoints: [resolve(import.meta.dir, "../src/runtime.ts")],
    target: "browser",
  });
  if (!result.success) {
    const messages = result.logs.map((log) => log.message).join("\n");
    throw new Error(`runtime.ts failed to bundle for browser:\n${messages}`);
  }
  expect(result.success).toBe(true);
});
