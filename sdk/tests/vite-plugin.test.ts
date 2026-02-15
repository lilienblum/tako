import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";

import { stageTakoViteArtifacts, takoVitePlugin } from "../src/vite";

let rootDir = "";

async function writeText(relPath: string, content: string): Promise<void> {
  const absPath = path.join(rootDir, relPath);
  await mkdir(path.dirname(absPath), { recursive: true });
  await writeFile(absPath, content, "utf8");
}

async function readText(relPath: string): Promise<string> {
  return readFile(path.join(rootDir, relPath), "utf8");
}

describe("tako Vite plugin artifact staging", () => {
  beforeEach(async () => {
    rootDir = await mkdtemp(path.join(tmpdir(), "tako-vite-plugin-"));
  });

  afterEach(async () => {
    if (rootDir) {
      await rm(rootDir, { recursive: true, force: true });
    }
  });

  test("merges public and client into static and copies server output", async () => {
    await writeText("public/favicon.ico", "from-public");
    await writeText("dist/client/assets/main.js", "console.log('client')");
    await writeText("dist/client/favicon.ico", "from-client");
    await writeText("dist/server/entry-server.js", "export default {};");

    const result = await stageTakoViteArtifacts({
      root: rootDir,
      outDir: "dist",
      publicDir: "public",
    });

    expect(result.clientDir).toBe(path.join(rootDir, "dist/client"));
    expect(result.serverDir).toBe(path.join(rootDir, "dist/server"));
    expect(result.artifactDir).toBe(path.join(rootDir, ".tako/artifacts/app"));

    expect(await readText(".tako/artifacts/app/static/assets/main.js")).toContain("client");
    expect(await readText(".tako/artifacts/app/static/favicon.ico")).toBe("from-client");
    expect(await readText(".tako/artifacts/app/server/entry-server.js")).toContain(
      "export default",
    );
  });

  test("supports custom client and server directories", async () => {
    await writeText("dist/web/index.html", "<!doctype html>");
    await writeText("dist/ssr/entry.js", "export default {};");

    const result = await stageTakoViteArtifacts({
      root: rootDir,
      outDir: "dist",
      publicDir: false,
      options: {
        clientDir: "dist/web",
        serverDir: "dist/ssr",
      },
    });

    expect(result.clientDir).toBe(path.join(rootDir, "dist/web"));
    expect(result.serverDir).toBe(path.join(rootDir, "dist/ssr"));
    expect(await readText(".tako/artifacts/app/static/index.html")).toContain("<!doctype html>");
    expect(await readText(".tako/artifacts/app/server/entry.js")).toContain("export default");
  });

  test("fails with clear error when client output cannot be detected", async () => {
    await writeText("dist/server/entry-server.js", "export default {};");

    await expect(
      stageTakoViteArtifacts({
        root: rootDir,
        outDir: "dist",
        publicDir: false,
      }),
    ).rejects.toThrow("Could not detect Vite client output directory");
  });

  test("plugin stages artifacts from resolved Vite config", async () => {
    await writeText("public/robots.txt", "User-agent: *");
    await writeText("dist/client/index.html", "<html></html>");
    await writeText("dist/server/entry-server.js", "export default {};");

    const plugin = takoVitePlugin();
    plugin.configResolved?.({
      root: rootDir,
      publicDir: path.join(rootDir, "public"),
      build: { outDir: "dist" },
    });
    await plugin.closeBundle?.();

    expect(await readText(".tako/artifacts/app/static/robots.txt")).toContain("User-agent");
    expect(await readText(".tako/artifacts/app/server/entry-server.js")).toContain(
      "export default",
    );
  });
});
