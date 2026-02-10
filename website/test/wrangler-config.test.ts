import { describe, expect, it } from "bun:test";
import { readFile } from "node:fs/promises";
import { join } from "node:path";

describe("wrangler config", () => {
  it("uses wrangler.jsonc with assets binding", async () => {
    const p = join(import.meta.dir, "..", "wrangler.jsonc");
    const raw = await readFile(p, "utf8");
    const cfg = JSON.parse(raw);

    expect(cfg.name).toBe("tako-website");
    expect(cfg.main).toBe("src/worker.ts");
    expect(cfg.assets?.directory).toBe("./dist");
    expect(cfg.assets?.binding).toBe("ASSETS");
  });
});

