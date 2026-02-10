import { describe, expect, it } from "bun:test";
import { readFile } from "node:fs/promises";
import { join } from "node:path";

describe("redirects config", () => {
  it("defines installer redirects for Cloudflare static assets", async () => {
    const redirectsPath = join(import.meta.dir, "..", "public", "_redirects");
    const redirects = await readFile(redirectsPath, "utf8");

    expect(redirects).toContain(
      "/install https://raw.githubusercontent.com/lilienblum/tako/main/scripts/install-tako-cli.sh 302",
    );
    expect(redirects).toContain(
      "/install-server https://raw.githubusercontent.com/lilienblum/tako/main/scripts/install-tako-server.sh 302",
    );
    expect(redirects).toContain(
      "/server-install https://raw.githubusercontent.com/lilienblum/tako/main/scripts/install-tako-server.sh 302",
    );
  });
});
