import { describe, expect, it } from "bun:test";
import { readFile } from "node:fs/promises";
import { join } from "node:path";

describe("installer scripts", () => {
  it("repo CLI setup script matches hosted /install script", async () => {
    const a = await readFile(join(import.meta.dir, "..", "public", "install"), "utf8");
    const b = await readFile(join(import.meta.dir, "..", "..", "scripts", "setup-tako-cli.sh"), "utf8");

    expect(a).toBe(b);
  });

  it("repo server setup script matches hosted /install-server script", async () => {
    const a = await readFile(join(import.meta.dir, "..", "public", "install-server"), "utf8");
    const b = await readFile(join(import.meta.dir, "..", "..", "scripts", "setup-tako-server.sh"), "utf8");

    expect(a).toBe(b);
  });
});
