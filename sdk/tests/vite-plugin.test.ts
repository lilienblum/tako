import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdtemp, mkdir, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";

import { takoVitePlugin } from "../src/vite";

let rootDir = "";
let originalPortEnv: string | undefined;

async function readText(relPath: string): Promise<string> {
  return await readFile(path.join(rootDir, relPath), "utf8");
}

describe("tako Vite entry plugin", () => {
  beforeEach(async () => {
    originalPortEnv = process.env.PORT;
    delete process.env.PORT;
    rootDir = await mkdtemp(path.join(tmpdir(), "tako-vite-plugin-"));
  });

  afterEach(async () => {
    if (originalPortEnv === undefined) {
      delete process.env.PORT;
    } else {
      process.env.PORT = originalPortEnv;
    }
    if (rootDir) {
      await rm(rootDir, { recursive: true, force: true });
    }
  });

  test("writes wrapped server entry for a single entry chunk", async () => {
    await mkdir(path.join(rootDir, "dist"), { recursive: true });

    const plugin = takoVitePlugin();
    plugin.configResolved?.({
      root: rootDir,
      build: { outDir: "dist" },
    });
    plugin.generateBundle?.(
      {},
      {
        "server/index.mjs": {
          type: "chunk",
          fileName: "server/index.mjs",
          isEntry: true,
        },
      },
    );
    await plugin.closeBundle?.();

    const wrapper = await readText("dist/tako-entry.mjs");
    expect(wrapper).toContain('import entryModule, * as entryNamespace from "./server/index.mjs";');
    expect(wrapper).toContain("const fetchHandler");
    expect(wrapper).toContain("export default fetchHandler;");
  });

  test("does not force SSR bundling options", () => {
    const plugin = takoVitePlugin();
    expect(plugin.config?.({}, { command: "build" })).toEqual({});
  });

  test("uses PORT env for dev server binding", () => {
    process.env.PORT = "47831";
    const plugin = takoVitePlugin();
    expect(plugin.config?.({}, { command: "serve" })).toEqual({
      server: {
        allowedHosts: [".tako.local"],
        host: "127.0.0.1",
        port: 47831,
        strictPort: true,
      },
    });
  });

  test("adds tako host allowance in serve mode without PORT", () => {
    const plugin = takoVitePlugin();
    expect(plugin.config?.({}, { command: "serve" })).toEqual({
      server: {
        allowedHosts: [".tako.local"],
      },
    });
  });

  test("merges user allowedHosts in serve mode", () => {
    process.env.PORT = "47831";
    const plugin = takoVitePlugin();
    expect(
      plugin.config?.(
        {
          server: {
            allowedHosts: ["localhost"],
          },
        },
        { command: "serve" },
      ),
    ).toEqual({
      server: {
        allowedHosts: ["localhost", ".tako.local"],
        host: "127.0.0.1",
        port: 47831,
        strictPort: true,
      },
    });
  });

  test("keeps allowedHosts true in serve mode", () => {
    process.env.PORT = "47831";
    const plugin = takoVitePlugin();
    expect(
      plugin.config?.(
        {
          server: {
            allowedHosts: true,
          },
        },
        { command: "serve" },
      ),
    ).toEqual({
      server: {
        allowedHosts: true,
        host: "127.0.0.1",
        port: 47831,
        strictPort: true,
      },
    });
  });

  test("ignores PORT env in build mode", () => {
    process.env.PORT = "47831";
    const plugin = takoVitePlugin();
    expect(plugin.config?.({}, { command: "build" })).toEqual({});
  });

  test("prefers entry paths under server when multiple entry chunks exist", async () => {
    const plugin = takoVitePlugin();
    plugin.configResolved?.({
      root: rootDir,
      build: { outDir: "dist" },
    });
    plugin.generateBundle?.(
      {},
      {
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
      },
    );
    await plugin.closeBundle?.();

    const wrapper = await readText("dist/tako-entry.mjs");
    expect(wrapper).toContain('import entryModule, * as entryNamespace from "./server/index.mjs";');
  });

  test("fails clearly when multiple entries are ambiguous", async () => {
    const plugin = takoVitePlugin();
    plugin.configResolved?.({
      root: rootDir,
      build: { outDir: "dist" },
    });
    plugin.generateBundle?.(
      {},
      {
        "entry-a.js": { type: "chunk", fileName: "entry-a.js", isEntry: true },
        "entry-b.js": { type: "chunk", fileName: "entry-b.js", isEntry: true },
      },
    );

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
    plugin.generateBundle?.(
      {},
      {
        "chunk.js": {
          type: "chunk",
          fileName: "chunk.js",
          isEntry: false,
        },
      },
    );

    await expect(plugin.closeBundle?.()).rejects.toThrow("Could not detect server entry chunk");
  });

  test("fails when closeBundle runs before configResolved", async () => {
    const plugin = takoVitePlugin();
    await expect(plugin.closeBundle?.()).rejects.toThrow(
      "takoVitePlugin was not initialized by Vite configResolved hook.",
    );
  });

  test("writes wrapped entry inside the configured outDir", async () => {
    await mkdir(path.join(rootDir, "dist/server"), { recursive: true });

    const plugin = takoVitePlugin();
    plugin.configResolved?.({
      root: rootDir,
      build: { outDir: "dist/server" },
    });
    plugin.generateBundle?.(
      {},
      {
        "server.js": {
          type: "chunk",
          fileName: "server.js",
          isEntry: true,
        },
      },
    );
    await plugin.closeBundle?.();

    const wrapper = await readText("dist/server/tako-entry.mjs");
    expect(wrapper).toContain('import entryModule, * as entryNamespace from "./server.js";');
  });

  test("does not write deploy metadata files", async () => {
    await mkdir(path.join(rootDir, "dist"), { recursive: true });

    const plugin = takoVitePlugin();
    plugin.configResolved?.({
      root: rootDir,
      build: { outDir: "dist" },
    });
    plugin.generateBundle?.(
      {},
      {
        "server/index.mjs": {
          type: "chunk",
          fileName: "server/index.mjs",
          isEntry: true,
        },
      },
    );
    await plugin.closeBundle?.();

    await expect(readText(".tako/build.json")).rejects.toThrow();
    await expect(readText("dist/.tako-vite.json")).rejects.toThrow();
  });
});
