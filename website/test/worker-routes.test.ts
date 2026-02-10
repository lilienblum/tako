import { describe, expect, it } from "bun:test";
import { readFile } from "node:fs/promises";
import { join } from "node:path";

describe("worker install routes", () => {
  it("treats install script routes as curl-friendly text/plain", async () => {
    const source = await readFile(join(import.meta.dir, "..", "src", "worker.ts"), "utf8");

    expect(source).toContain('"/install"');
    expect(source).toContain('"/install-server"');
    expect(source).toContain('"/server-install"');
    expect(source).toContain("text/plain; charset=utf-8");
  });
});
