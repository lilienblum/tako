import { describe, expect, it } from "bun:test";
import { access, readFile } from "node:fs/promises";
import { join } from "node:path";

describe("docs layout", () => {
  it("exists and is used by docs pages", async () => {
    const layoutPath = join(import.meta.dir, "..", "src", "layouts", "DocsLayout.astro");
    await access(layoutPath);

    const pages = [
      join(import.meta.dir, "..", "src", "pages", "docs", "install.astro"),
      join(import.meta.dir, "..", "src", "pages", "docs", "spec.astro"),
      join(import.meta.dir, "..", "src", "pages", "docs", "development.astro"),
      join(import.meta.dir, "..", "src", "pages", "docs", "deployment.astro"),
    ];

    for (const p of pages) {
      const s = await readFile(p, "utf8");
      expect(s).toContain("DocsLayout");
    }
  });
});
