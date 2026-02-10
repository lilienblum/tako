import { describe, expect, it } from "bun:test";
import { readFile } from "node:fs/promises";
import { join } from "node:path";

const websiteFiles = [
  join(import.meta.dir, "..", "src", "pages", "index.astro"),
  join(import.meta.dir, "..", "src", "pages", "docs", "index.astro"),
  join(import.meta.dir, "..", "src", "layouts", "DocsLayout.astro"),
];

describe("brand theme", () => {
  it("uses custom primary and secondary palette on all public pages", async () => {
    for (const file of websiteFiles) {
      const source = await readFile(file, "utf8");
      expect(source).toContain("--primary: #E88783;");
      expect(source).toContain("--secondary: #9BC4B6;");
      expect(source).not.toContain("#F67675");
      expect(source).not.toContain("246, 118, 117");
    }
  });
});
