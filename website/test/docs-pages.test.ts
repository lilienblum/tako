import { describe, expect, it } from "bun:test";
import { access, readFile } from "node:fs/promises";
import { join } from "node:path";

describe("/docs pages", () => {
  it("has a docs index page", async () => {
    const p = join(import.meta.dir, "..", "src", "pages", "docs", "index.astro");
    await access(p);
    const s = await readFile(p, "utf8");
    expect(s).toContain('href="/docs/spec"');
    expect(s).toContain('href="/docs/install"');
    expect(s).toContain('href="/docs/development"');
    expect(s).toContain('href="/docs/deployment"');
    expect(s).toContain('href="/docs/operations"');
    expect(s).toContain('href="/docs/architecture"');
  });
});
