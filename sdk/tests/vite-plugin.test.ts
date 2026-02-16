import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdtemp, mkdir, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";

import { takoVitePlugin } from "../src/vite";

let rootDir = "";

async function readJson(relPath: string): Promise<unknown> {
  const content = await readFile(path.join(rootDir, relPath), "utf8");
  return JSON.parse(content);
}

async function readText(relPath: string): Promise<string> {
  return await readFile(path.join(rootDir, relPath), "utf8");
}

describe("tako Vite plugin metadata", () => {
  beforeEach(async () => {
    rootDir = await mkdtemp(path.join(tmpdir(), "tako-vite-plugin-"));
  });

  afterEach(async () => {
    if (rootDir) {
      await rm(rootDir, { recursive: true, force: true });
    }
  });

  test("writes wrapped compiled_main metadata for a single entry chunk", async () => {
    await mkdir(path.join(rootDir, "dist"), { recursive: true });

    const plugin = takoVitePlugin();
    plugin.configResolved?.({
      root: rootDir,
      build: { outDir: "dist" },
    });
    plugin.generateBundle?.({}, {
      "server/index.mjs": {
        type: "chunk",
        fileName: "server/index.mjs",
        isEntry: true,
      },
    });
    await plugin.closeBundle?.();

    const metadata = (await readJson("dist/.tako-vite.json")) as {
      compiled_main: string;
      entries: string[];
    };
    expect(metadata.compiled_main).toBe("tako-entry.mjs");
    expect(metadata.entries).toEqual(["server/index.mjs"]);

    const wrapper = await readText("dist/tako-entry.mjs");
    expect(wrapper).toContain('import entryModule from "./server/index.mjs";');
    expect(wrapper).toContain('"tako.internal"');
    expect(wrapper).toContain('pathname === "/status"');
    expect(wrapper).not.toContain("/_tako/status");
  });

  test("forces SSR bundling for deployable server output", () => {
    const plugin = takoVitePlugin();
    expect(plugin.config?.()).toEqual({
      ssr: {
        noExternal: true,
      },
    });
  });

  test("prefers entry paths under server when multiple entry chunks exist", async () => {
    const plugin = takoVitePlugin();
    plugin.configResolved?.({
      root: rootDir,
      build: { outDir: "dist" },
    });
    plugin.generateBundle?.({}, {
      "client/index.js": {
        type: "chunk",
        fileName: "client/index.js",
        isEntry: true,
      },
      "server/index.mjs": {
        type: "chunk",
        fileName: "server/index.mjs",
        isEntry: true,
      },
    });
    await plugin.closeBundle?.();

    const metadata = (await readJson("dist/.tako-vite.json")) as {
      compiled_main: string;
    };
    expect(metadata.compiled_main).toBe("tako-entry.mjs");

    const wrapper = await readText("dist/tako-entry.mjs");
    expect(wrapper).toContain('import entryModule from "./server/index.mjs";');
  });

  test("fails clearly when multiple entries are ambiguous", async () => {
    const plugin = takoVitePlugin();
    plugin.configResolved?.({
      root: rootDir,
      build: { outDir: "dist" },
    });
    plugin.generateBundle?.({}, {
      "entry-a.js": { type: "chunk", fileName: "entry-a.js", isEntry: true },
      "entry-b.js": { type: "chunk", fileName: "entry-b.js", isEntry: true },
    });

    await expect(plugin.closeBundle?.()).rejects.toThrow(
      "Could not choose a single server entry chunk",
    );
  });

  test("fails clearly when no entry chunks exist", async () => {
    const plugin = takoVitePlugin();
    plugin.configResolved?.({
      root: rootDir,
      build: { outDir: "dist" },
    });
    plugin.generateBundle?.({}, {
      "chunk.js": {
        type: "chunk",
        fileName: "chunk.js",
        isEntry: false,
      },
    });

    await expect(plugin.closeBundle?.()).rejects.toThrow(
      "Could not detect server entry chunk",
    );
  });

  test("fails when closeBundle runs before configResolved", async () => {
    const plugin = takoVitePlugin();
    await expect(plugin.closeBundle?.()).rejects.toThrow(
      "takoVitePlugin was not initialized by Vite configResolved hook.",
    );
  });

  test("writes metadata to dist root when SSR outDir ends with server", async () => {
    await mkdir(path.join(rootDir, "dist/server"), { recursive: true });

    const plugin = takoVitePlugin();
    plugin.configResolved?.({
      root: rootDir,
      build: { outDir: "dist/server" },
    });
    plugin.generateBundle?.({}, {
      "server.js": {
        type: "chunk",
        fileName: "server.js",
        isEntry: true,
      },
    });
    await plugin.closeBundle?.();

    const metadata = (await readJson("dist/.tako-vite.json")) as {
      compiled_main: string;
    };
    expect(metadata.compiled_main).toBe("server/tako-entry.mjs");

    const wrapper = await readText("dist/server/tako-entry.mjs");
    expect(wrapper).toContain('import entryModule from "./server.js";');
  });
});
