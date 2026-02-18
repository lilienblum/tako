import { describe, expect, test } from "bun:test";

describe("package exports", () => {
  test("resolves tako.sh/vite without a prebuilt dist directory", async () => {
    const mod = await import("tako.sh/vite");
    expect(typeof mod.takoVitePlugin).toBe("function");
  });
});
